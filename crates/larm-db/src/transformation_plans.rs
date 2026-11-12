//! Immutable P093 plan records. Storing a preview creates no job, reservation
//! or model output. Admission and physical input verification remain separate.
use std::sync::Arc;

use larm_domain::{
    bridge::{is_uuid_v7, timestamp},
    conformance::hash,
    error::{ErrorCode, LarmError},
    ports::{AdapterQualification, AdapterQualificationGuard, MutationContext},
    transformations::{SealedTransformationPlan, TransformationExecutionBinding},
};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    events::{append, NewEvent},
    pool::db_error,
};

pub const PLAN_KIND: &str = "transformation.plan";

pub(crate) mod admission;

#[derive(Clone)]
pub struct SqlTransformationPlans {
    pub pool: SqlitePool,
    pub qualification: Option<Arc<dyn AdapterQualification>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRecord {
    schema_version: u8,
    actor_id: String,
    correlation_id: String,
    plan: SealedTransformationPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation: Option<PlanActivation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanActivation {
    job_id: String,
    activated_at: String,
    correlation_id: String,
}

struct QualifiedPlan {
    guard: Box<dyn AdapterQualificationGuard>,
    package_hash: String,
}

fn invalid(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(code, "El plan de transformación no está disponible", reason)
}

fn valid_context(ctx: &MutationContext) -> bool {
    !ctx.actor_id.trim().is_empty()
        && ctx.actor_id.len() <= 256
        && !ctx.actor_id.chars().any(char::is_control)
        && is_uuid_v7(&ctx.correlation_id)
}

fn validate_plan(
    plan: &SealedTransformationPlan,
) -> Result<&TransformationExecutionBinding, LarmError> {
    plan.verify_seal().map_err(|_| {
        invalid(
            ErrorCode::IntegrityFailed,
            "transformation_plan_seal_invalid",
        )
    })?;
    plan.execution_binding.as_ref().ok_or_else(|| {
        invalid(
            ErrorCode::InvalidStateTransition,
            "transformation_execution_binding_missing",
        )
    })
}

/// Resolve the committed P026 receipt/profile inside the business transaction.
/// Retain the physical host guard until COMMIT; JSON flags confer no authority.
async fn qualify(
    conn: &mut SqliteConnection,
    binding: &TransformationExecutionBinding,
    adapter_id: &str,
    ctx: &MutationContext,
    gate: Option<&Arc<dyn AdapterQualification>>,
) -> Result<QualifiedPlan, LarmError> {
    let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(&binding.adapter_installation_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|error| db_error("leyendo adaptador de transformación", &error))?
        .ok_or_else(|| invalid(ErrorCode::NotFound, "transformation_adapter_missing"))?;
    let installation = crate::adapters::to_installation(&row)?;
    if installation.family != "transformation" {
        return Err(invalid(
            ErrorCode::ValidationFailed,
            "transformation_adapter_family_mismatch",
        ));
    }
    if installation.adapter_id != adapter_id
        || installation
            .manifest
            .get("id")
            .and_then(serde_json::Value::as_str)
            != Some(adapter_id)
    {
        return Err(invalid(
            ErrorCode::ValidationFailed,
            "transformation_adapter_identity_mismatch",
        ));
    }
    let run = crate::conformance::require_consumer_current(conn, &installation).await?;
    let profile = crate::conformance_profiles::load(conn, &installation.id)
        .await?
        .ok_or_else(|| {
            invalid(
                ErrorCode::InvalidStateTransition,
                "transformation_profile_missing",
            )
        })?;
    profile.check_installation(&installation)?;
    if run.id != binding.conformance_run_id
        || run.digest()? != binding.conformance_run_sha256
        || hash(&profile)? != binding.profile_sha256
    {
        return Err(invalid(
            ErrorCode::StaleRevision,
            "transformation_execution_binding_stale",
        ));
    }
    let gate = gate.ok_or_else(|| {
        invalid(
            ErrorCode::InvalidStateTransition,
            "conformance_physical_gate_unavailable",
        )
    })?;
    // The installed backend profile owns its approved roots/grants. The HTTP
    // actor remains the separate, persisted owner of the plan and eventual job.
    let backend_ctx = MutationContext {
        actor_id: profile.actor_id,
        correlation_id: ctx.correlation_id.clone(),
    };
    let guard = gate.acquire(&installation, &run, &backend_ctx).await?;
    Ok(QualifiedPlan {
        guard,
        package_hash: installation.package_hash,
    })
}

fn decode(payload: &str, id: &str, digest: &str, resources: &str) -> Result<PlanRecord, LarmError> {
    let record: PlanRecord = serde_json::from_str(payload).map_err(|_| {
        invalid(
            ErrorCode::IntegrityFailed,
            "transformation_plan_record_invalid",
        )
    })?;
    let resource_ids: Vec<String> = serde_json::from_str(resources).map_err(|_| {
        invalid(
            ErrorCode::IntegrityFailed,
            "transformation_plan_record_invalid",
        )
    })?;
    if record.schema_version != 1
        || record.plan.response.plan_id != id
        || record.plan.response.plan_hash != digest
        || record.plan.response.resource_ids != resource_ids
        || record.activation.as_ref().is_some_and(|activation| {
            !is_uuid_v7(&activation.job_id)
                || !is_uuid_v7(&activation.correlation_id)
                || activation.activated_at.len() != 24
                || !activation.activated_at.ends_with('Z')
                || chrono::DateTime::parse_from_rfc3339(&activation.activated_at).is_err()
        })
        || !valid_context(&MutationContext {
            actor_id: record.actor_id.clone(),
            correlation_id: record.correlation_id.clone(),
        })
    {
        return Err(invalid(
            ErrorCode::IntegrityFailed,
            "transformation_plan_record_invalid",
        ));
    }
    validate_plan(&record.plan)?;
    Ok(record)
}

impl SqlTransformationPlans {
    /// Persist a host-bound preview and its event atomically, without admitting
    /// an execution. A repeated ID cannot overwrite actor, binding or contents.
    pub async fn store_plan(
        &self,
        plan: &SealedTransformationPlan,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        if !valid_context(ctx) {
            return Err(invalid(
                ErrorCode::ValidationFailed,
                "transformation_plan_context_invalid",
            ));
        }
        let binding = validate_plan(plan)?;
        if chrono::DateTime::parse_from_rfc3339(&plan.response.expires_at).map_err(|_| {
            invalid(
                ErrorCode::ValidationFailed,
                "transformation_plan_expiry_invalid",
            )
        })? <= chrono::Utc::now()
        {
            return Err(invalid(
                ErrorCode::StaleRevision,
                "transformation_plan_expired",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN plan de transformación", &error))?;
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando planes de transformación", &error))?;
        let guard = qualify(
            &mut tx,
            binding,
            &plan.request.adapter_id,
            ctx,
            self.qualification.as_ref(),
        )
        .await?;
        let existing: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT kind,plan_hash,payload,resource_ids FROM operation_journal WHERE id=?",
        )
        .bind(&plan.response.plan_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("leyendo plan de transformación", &error))?;
        if let Some((kind, digest, payload, resources)) = existing {
            if kind != PLAN_KIND {
                return Err(invalid(
                    ErrorCode::Conflict,
                    "transformation_plan_id_collision",
                ));
            }
            let record = decode(&payload, &plan.response.plan_id, &digest, &resources)?;
            if record.actor_id != ctx.actor_id || digest != plan.response.plan_hash {
                return Err(invalid(
                    ErrorCode::Conflict,
                    "transformation_plan_id_collision",
                ));
            }
            guard.guard.recheck().await?;
            tx.commit()
                .await
                .map_err(|error| db_error("COMMIT replay de plan", &error))?;
            return Ok(());
        }
        let now = timestamp(chrono::Utc::now());
        let record = PlanRecord {
            schema_version: 1,
            actor_id: ctx.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            plan: plan.clone(),
            activation: None,
        };
        let payload = serde_json::to_string(&record)
            .map_err(|error| db_error("serializando plan de transformación", &error))?;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,?,?,'PLANNED',?,?,?)")
            .bind(&plan.response.plan_id).bind(PLAN_KIND)
            .bind(serde_json::to_string(&plan.response.resource_ids).map_err(|error| db_error("recursos del plan", &error))?)
            .bind(&plan.response.plan_hash).bind(payload).bind(now)
            .execute(&mut *tx).await.map_err(|error| db_error("guardando plan de transformación", &error))?;
        append(&mut tx, &NewEvent {
            event_type: "transformation.state_changed", resource_type: "transformation_plan",
            resource_id: &plan.response.plan_id, resource_version: 1,
            data: serde_json::json!({"action":"planned","state":"PLANNED","input_variant_id":plan.request.input_variant_id,"plan_hash":plan.response.plan_hash}),
        }, ctx).await?;
        guard.guard.recheck().await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT plan de transformación", &error))?;
        Ok(())
    }

    /// Private lookup by owning actor. This returns historical data, never an
    /// authorization to execute; admission must recheck its phase and evidence.
    pub async fn plan(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<Option<SealedTransformationPlan>, LarmError> {
        if !is_uuid_v7(id) || !valid_context(ctx) {
            return Err(invalid(
                ErrorCode::ValidationFailed,
                "transformation_plan_lookup_invalid",
            ));
        }
        let row: Option<(String, String, String)> = sqlx::query_as("SELECT plan_hash,payload,resource_ids FROM operation_journal WHERE id=? AND kind=? AND json_extract(payload,'$.actor_id')=?")
            .bind(id).bind(PLAN_KIND).bind(&ctx.actor_id).fetch_optional(&self.pool).await
            .map_err(|error| db_error("leyendo plan por propietario", &error))?;
        row.map(|(digest, payload, resources)| {
            decode(&payload, id, &digest, &resources).map(|record| record.plan)
        })
        .transpose()
    }
}
