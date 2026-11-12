//! Atomic initial admission and the explicitly authorized 3.13.0 restart.
use std::collections::{BTreeMap, BTreeSet};

use larm_domain::{
    approval::{ApprovalRecord, ApprovalUse},
    bridge::{JobPhase, JobReceipt},
    estimate::{margin, Confidence},
    guard::{normalize, LockMode, Resource, ResourceKind},
    ports::VolumeDemand,
    transformations::{
        verify_observed_input_hashes, ExecuteTransformationPlan,
        TransformationActivationIdempotency, TransformationAdmission, TransformationInputGuard,
        TransformationJob, TransformationRestart, TransformationSpaceSnapshot, TransformationState,
    },
};
use serde_json::json;

use super::*;

const OPERATION: &str = "P093_post__transformations";
const RESTART_OPERATION: &str = "P093_post__transformations_id_restart";

#[derive(sqlx::FromRow)]
struct InputCatalogue {
    revision_id: String,
    lifecycle: String,
    bundle_hash: Option<String>,
    availability: String,
    state: String,
    row_version: i64,
    manifest_hash: String,
    required_artifact_ids: String,
    optional_artifact_ids: String,
}

fn conflict(reason: &str) -> LarmError {
    invalid(ErrorCode::Conflict, reason)
}

fn inserted(result: sqlx::sqlite::SqliteQueryResult, reason: &str) -> Result<(), LarmError> {
    if result.rows_affected() != 1 {
        return Err(conflict(reason));
    }
    Ok(())
}

async fn confirm_guards(
    conn: &mut SqliteConnection,
    job: &str,
    resources: &[(Resource, LockMode)],
    generation: i64,
) -> Result<(), LarmError> {
    let actual: Vec<(String, String, i64)> = sqlx::query_as("SELECT resource_id,mode,generation FROM resource_lock WHERE owner_id=? ORDER BY resource_id")
        .bind(job).fetch_all(conn).await.map_err(|e|db_error("confirmando guards de transformación",&e))?;
    let mut expected: Vec<_> = resources
        .iter()
        .map(|(r, m)| (r.key(), m.as_str().to_owned(), generation))
        .collect();
    expected.sort();
    if actual != expected {
        return Err(conflict("transformation_guards_not_persisted"));
    }
    Ok(())
}

async fn confirm_reservations(
    conn: &mut SqliteConnection,
    job: &str,
    demands: &[VolumeDemand],
) -> Result<(), LarmError> {
    let actual: Vec<(String, i64, i64, Option<String>)> = sqlx::query_as("SELECT volume_key,total_bytes,remaining_bytes,expires_at FROM volume_reservation WHERE job_id=? ORDER BY volume_key")
        .bind(job).fetch_all(conn).await.map_err(|e|db_error("confirmando reservas de transformación",&e))?;
    let mut expected: Vec<_> = demands
        .iter()
        .map(|d| {
            (
                d.volume_key.clone(),
                d.required_bytes as i64,
                d.required_bytes as i64,
                None,
            )
        })
        .collect();
    expected.sort();
    if actual != expected {
        return Err(conflict("transformation_reservations_not_persisted"));
    }
    Ok(())
}

fn check_expiry(plan: &SealedTransformationPlan) -> Result<(), LarmError> {
    if chrono::DateTime::parse_from_rfc3339(&plan.response.expires_at)
        .map_err(|_| conflict("transformation_plan_expiry_invalid"))?
        <= chrono::Utc::now()
    {
        return Err(invalid(
            ErrorCode::StaleRevision,
            "transformation_plan_expired",
        ));
    }
    Ok(())
}

pub(crate) async fn catalogue(
    conn: &mut SqliteConnection,
    plan: &SealedTransformationPlan,
    input: &dyn TransformationInputGuard,
) -> Result<Vec<(Resource, LockMode)>, LarmError> {
    verify_observed_input_hashes(&plan.input_artifacts, input.artifacts())
        .map_err(|_| invalid(ErrorCode::IntegrityFailed, "transformation_input_changed"))?;
    let identity = input.identity();
    if identity.variant_id != plan.request.input_variant_id
        || identity.bundle_row_version < 1
        || ![
            &identity.variant_id,
            &identity.revision_id,
            &identity.bundle_id,
        ]
        .into_iter()
        .all(|id| is_uuid_v7(id))
        || ![
            &identity.variant_bundle_hash,
            &identity.bundle_manifest_hash,
        ]
        .into_iter()
        .all(|s| larm_domain::adapter_updates::is_sha256(s))
    {
        return Err(conflict("transformation_input_identity_invalid"));
    }
    let row: Option<InputCatalogue> = sqlx::query_as(
        "SELECT v.revision_id,v.lifecycle,v.bundle_hash,v.availability,b.state,b.row_version,b.manifest_hash,b.required_artifact_ids,b.optional_artifact_ids FROM model_variant v JOIN model_bundle b ON b.variant_id=v.id WHERE v.id=? AND b.id=?")
        .bind(&identity.variant_id).bind(&identity.bundle_id).fetch_optional(&mut *conn).await
        .map_err(|e| db_error("leyendo input de transformación", &e))?;
    let Some(row) = row else {
        return Err(conflict("transformation_input_catalogue_missing"));
    };
    if row.revision_id != identity.revision_id
        || row.bundle_hash.as_deref() != Some(&identity.variant_bundle_hash)
        || row.state != "VERIFIED"
        || row.manifest_hash != identity.bundle_manifest_hash
        || row.row_version != identity.bundle_row_version
        || !matches!(
            row.lifecycle.as_str(),
            "VERIFIED" | "PREPARED" | "TECHNICALLY_VALIDATED" | "AVAILABLE" | "RUNNING"
        )
        || matches!(row.availability.as_str(), "OFFLINE" | "CORRUPT")
    {
        return Err(invalid(
            ErrorCode::StaleRevision,
            "transformation_input_catalogue_changed",
        ));
    }
    let required: Vec<String> = serde_json::from_str(&row.required_artifact_ids)
        .map_err(|e| db_error("miembros requeridos", &e))?;
    let optional: Vec<String> = serde_json::from_str(&row.optional_artifact_ids)
        .map_err(|e| db_error("miembros opcionales", &e))?;
    let selected: BTreeSet<&str> = plan
        .input_artifacts
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect();
    let members: BTreeSet<&str> = required
        .iter()
        .chain(&optional)
        .map(String::as_str)
        .collect();
    if !required.iter().all(|id| selected.contains(id.as_str()))
        || !selected.is_subset(&members)
        || members.len() != required.len() + optional.len()
    {
        return Err(conflict("transformation_input_membership_changed"));
    }
    let locations: BTreeMap<&str, _> = input
        .locations()
        .iter()
        .map(|l| (l.artifact_id.as_str(), l))
        .collect();
    if locations.len() != input.locations().len() || locations.len() != selected.len() {
        return Err(conflict("transformation_input_locations_invalid"));
    }
    let mut locks = vec![(
        Resource::new(ResourceKind::Variant, &identity.variant_id),
        LockMode::Exclusive,
    )];
    for artifact in &plan.input_artifacts {
        let source = locations
            .get(artifact.artifact_id.as_str())
            .ok_or_else(|| conflict("transformation_input_location_missing"))?;
        if !is_uuid_v7(&source.artifact_location_id)
            || !is_uuid_v7(&source.storage_location_id)
            || source.canonical_storage_path.is_empty()
            || source.volume_key.is_empty()
            || source.volume_key.starts_with("unknown:")
            || larm_domain::model::bundle_path(&source.relative_path).is_err()
        {
            return Err(conflict("transformation_input_location_invalid"));
        }
        let matches:bool=sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM model_artifact a JOIN artifact_location l ON l.artifact_id=a.id JOIN storage_location s ON s.id=l.storage_location_id WHERE a.id=? AND (a.variant_id=? OR EXISTS(SELECT 1 FROM artifact_reference r WHERE r.artifact_id=a.id AND r.variant_id=?)) AND a.filename=? AND l.id=? AND l.storage_location_id=? AND l.relative_path=? AND l.state='ONLINE' AND l.sha256=? AND l.file_size=? AND s.volume_key=? AND s.canonical_path=? AND s.state IN ('ONLINE','READ_ONLY','LOW_SPACE','DEGRADED'))")
            .bind(&artifact.artifact_id).bind(&identity.variant_id).bind(&identity.variant_id).bind(&artifact.relative_path)
            .bind(&source.artifact_location_id).bind(&source.storage_location_id).bind(&source.relative_path)
            .bind(&artifact.sha256).bind(artifact.size_bytes.get() as i64).bind(&source.volume_key)
            .bind(&source.canonical_storage_path).fetch_one(&mut *conn).await.map_err(|e|db_error("comprobando archivo de entrada",&e))?;
        if !matches {
            return Err(invalid(
                ErrorCode::StaleRevision,
                "transformation_input_location_changed",
            ));
        }
        locks.push((
            Resource::new(ResourceKind::Volume, &source.volume_key),
            LockMode::Shared,
        ));
        locks.push((
            Resource::new(ResourceKind::Artifact, &artifact.artifact_id),
            LockMode::Exclusive,
        ));
    }
    Ok(locks)
}

async fn demands(
    conn: &mut SqliteConnection,
    plan: &SealedTransformationPlan,
    snapshot: &TransformationSpaceSnapshot,
) -> Result<Vec<VolumeDemand>, LarmError> {
    let estimate = &plan.response.storage_estimate;
    let keys: BTreeSet<&str> = snapshot
        .demands
        .iter()
        .map(|d| d.volume_key.as_str())
        .collect();
    if estimate.confidence == Confidence::Unknown
        && estimate.upper_bound_bytes.is_none()
        && plan.request.hard_budget_bytes.is_none()
    {
        return Err(invalid(
            ErrorCode::UnknownSize,
            "transformation_size_unknown",
        ));
    }
    if keys.len() != snapshot.demands.len()
        || keys.len() != estimate.per_volume.len()
        || keys.is_empty()
    {
        return Err(conflict("transformation_space_snapshot_invalid"));
    }
    let mut total = 0u64;
    let mut required = 0u64;
    let mut expected = Vec::new();
    let mut seen = BTreeSet::new();
    for volume in &estimate.per_volume {
        if !seen.insert(volume.volume_key.as_str())
            || !volume.eligible
            || volume.volume_key.is_empty()
            || volume.volume_key.starts_with("unknown:")
        {
            return Err(conflict("transformation_space_plan_invalid"));
        }
        let m = margin(volume.peak_extra_bytes.get(), snapshot.margin_policy)
            .map_err(|_| conflict("transformation_size_overflow"))?;
        if m != volume.margin_bytes.get() {
            return Err(invalid(
                ErrorCode::StaleRevision,
                "transformation_margin_policy_changed",
            ));
        }
        let amount = volume
            .peak_extra_bytes
            .get()
            .checked_add(m)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| conflict("transformation_size_overflow"))?;
        total = total
            .checked_add(volume.peak_extra_bytes.get())
            .ok_or_else(|| conflict("transformation_size_overflow"))?;
        required = required
            .checked_add(amount)
            .ok_or_else(|| conflict("transformation_size_overflow"))?;
        let demand = snapshot
            .demands
            .iter()
            .find(|d| d.volume_key == volume.volume_key)
            .ok_or_else(|| conflict("transformation_space_volume_missing"))?;
        let root_min: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(reserved_bytes),0) FROM storage_location WHERE volume_key=?",
        )
        .bind(&volume.volume_key)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("mínimo por volumen", &e))?;
        if root_min < 0
            || demand.required_bytes != amount
            || demand.policy_minimum != snapshot.minimum_free_bytes.max(root_min as u64)
            || demand.os_free > i64::MAX as u64
        {
            return Err(conflict("transformation_space_demand_mismatch"));
        }
        expected.push(demand.clone());
    }
    if estimate.peak_extra_bytes.map(|v| v.get()) != Some(total)
        || estimate.upper_bound_bytes.map(|v| v.get()) != Some(total)
        || estimate.required_free_bytes.map(|v| v.get()) != Some(required)
    {
        return Err(conflict("transformation_space_totals_invalid"));
    }
    if plan
        .request
        .hard_budget_bytes
        .is_some_and(|b| total > b.get())
    {
        return Err(invalid(
            ErrorCode::StorageInsufficient,
            "transformation_hard_budget_insufficient",
        ));
    }
    Ok(expected)
}

async fn approval(
    conn: &mut SqliteConnection,
    req: &ExecuteTransformationPlan,
    plan: &SealedTransformationPlan,
    package_hash: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let Some(id) = req.approval_id.as_deref() else {
        return if plan.response.approval_required {
            Err(invalid(
                ErrorCode::ApprovalRequired,
                "transformation_approval_required",
            ))
        } else {
            Ok(())
        };
    };
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='approval'",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("aprobación de transformación", &e))?;
    let Some((phase, payload)) = row else {
        return Err(invalid(
            ErrorCode::ApprovalRequired,
            "transformation_approval_missing",
        ));
    };
    let record: ApprovalRecord = serde_json::from_str(&payload).map_err(|_| {
        invalid(
            ErrorCode::ApprovalRequired,
            "transformation_approval_invalid",
        )
    })?;
    if phase != "APPROVED" || record.id != id {
        return Err(invalid(
            ErrorCode::ApprovalRequired,
            "transformation_approval_unavailable",
        ));
    }
    record.check(
        &ApprovalUse {
            actor_id: &ctx.actor_id,
            operation_digest: &plan.response.plan_hash,
            resource_ids: &plan.response.resource_ids,
            code_hash: Some(package_hash),
        },
        chrono::Utc::now(),
    )?;
    let at = timestamp(chrono::Utc::now());
    let changed=sqlx::query("UPDATE operation_journal SET phase='CONSUMED',payload=json_set(payload,'$.consumed_at',?),updated_at=? WHERE id=? AND kind='approval' AND phase='APPROVED'")
        .bind(&at).bind(&at).bind(id).execute(conn).await.map_err(|e|db_error("consumiendo aprobación de transformación",&e))?;
    if changed.rows_affected() != 1 {
        return Err(conflict("transformation_approval_raced"));
    }
    Ok(())
}

async fn previous_attempt(
    conn: &mut SqliteConnection,
    restart: &TransformationRestart,
    ctx: &MutationContext,
) -> Result<TransformationJob, LarmError> {
    let job = crate::transformations::load(conn, &restart.job_id)
        .await?
        .ok_or_else(|| invalid(ErrorCode::NotFound, "transformation_restart_job_missing"))?;
    if job
        .metadata
        .get("actor_id")
        .and_then(serde_json::Value::as_str)
        != Some(ctx.actor_id.as_str())
    {
        return Err(invalid(
            ErrorCode::Forbidden,
            "transformation_restart_actor_mismatch",
        ));
    }
    if job.row_version != restart.if_match as i64 {
        return Err(invalid(
            ErrorCode::StaleRevision,
            "transformation_restart_version_changed",
        ));
    }
    if !matches!(
        job.state,
        TransformationState::Failed | TransformationState::FailedRecoverable
    ) || job.output_variant_id.is_some()
    {
        return Err(invalid(
            ErrorCode::InvalidStateTransition,
            "transformation_restart_state_invalid",
        ));
    }
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operation_journal WHERE id=? AND kind='transformation.job' AND plan_hash=? AND phase=? AND json_extract(payload,'$.state')=phase AND json_extract(payload,'$.actor_id')=? AND json_extract(payload,'$.correlation_id')=?)")
        .bind(&job.id).bind(&job.plan_hash).bind(if job.state==TransformationState::Failed {"FAILED"} else {"FAILED_RECOVERABLE"})
        .bind(&ctx.actor_id).bind(job.metadata.get("correlation_id").and_then(serde_json::Value::as_str))
        .fetch_one(&mut *conn).await.map_err(|e|db_error("validando intento anterior",&e))?;
    if !valid {
        return Err(invalid(
            ErrorCode::IntegrityFailed,
            "transformation_restart_journal_invalid",
        ));
    }
    // The supervisor must close/drain the old attempt and release its resources
    // before restart. A terminal flag alone cannot authorize removing them.
    let outstanding:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource_lock WHERE owner_id=?) OR EXISTS(SELECT 1 FROM volume_reservation WHERE job_id=?)")
        .bind(&job.id).bind(&job.id).fetch_one(conn).await.map_err(|e|db_error("recursos del intento anterior",&e))?;
    if outstanding {
        return Err(conflict(
            "transformation_previous_attempt_resources_pending",
        ));
    }
    Ok(job)
}

impl SqlTransformationPlans {
    /// Accept one initial execution atomically. The live input/space providers
    /// are server-owned; the public HTTP request supplies neither of them.
    pub async fn activate_plan(
        &self,
        req: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
        observed: TransformationAdmission<'_>,
    ) -> Result<JobReceipt, LarmError> {
        self.admit(req, ctx, key, Some(observed), None, false)
            .await?
            .ok_or_else(|| conflict("transformation_admission_missing"))
    }

    pub async fn restart_plan(
        &self,
        restart: &TransformationRestart,
        req: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
        observed: TransformationAdmission<'_>,
    ) -> Result<JobReceipt, LarmError> {
        self.admit(req, ctx, key, Some(observed), Some(restart), false)
            .await?
            .ok_or_else(|| conflict("transformation_admission_missing"))
    }

    /// Exact durable replay before any new physical acquisition. Absence is
    /// not an admission: the caller must still supply current input/space.
    pub async fn admission_replay(
        &self,
        req: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
        restart: Option<&TransformationRestart>,
    ) -> Result<Option<JobReceipt>, LarmError> {
        self.admit(req, ctx, key, None, restart, false).await
    }
    /// Public native-engine producer: the delivery hint and business mutation
    /// commit together. The hint never grants a claim or native authority.
    pub async fn admit_for_dispatch(
        &self,
        req: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
        observed: TransformationAdmission<'_>,
        restart: Option<&TransformationRestart>,
    ) -> Result<JobReceipt, LarmError> {
        self.admit(req, ctx, key, Some(observed), restart, true)
            .await?
            .ok_or_else(|| conflict("transformation_admission_missing"))
    }

    async fn admit(
        &self,
        req: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
        observed: Option<TransformationAdmission<'_>>,
        restart: Option<&TransformationRestart>,
        dispatch: bool,
    ) -> Result<Option<JobReceipt>, LarmError> {
        if !valid_context(ctx)
            || !is_uuid_v7(&req.plan_id)
            || !larm_domain::adapter_updates::is_sha256(&req.plan_hash)
            || req.approval_id.as_ref().is_some_and(|id| !is_uuid_v7(id))
            || key.key.is_empty()
            || key.key.len() > 240
            || !key
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
            || !larm_domain::adapter_updates::is_sha256(&key.body_hash)
            || restart.is_some_and(|r| {
                !is_uuid_v7(&r.job_id) || r.if_match == 0 || r.if_match > i64::MAX as u64
            })
        {
            return Err(invalid(
                ErrorCode::ValidationFailed,
                "transformation_activation_invalid",
            ));
        }
        let activation_digest = hash(req)?;
        let operation = if restart.is_some() {
            RESTART_OPERATION
        } else {
            OPERATION
        };
        let body_hash = match restart {
            Some(r) => hash(
                &json!({"job_id":r.job_id,"if_match":r.if_match.to_string(),"request":req,"body_hash":key.body_hash}),
            )?,
            None => key.body_hash.clone(),
        };
        let replay_code = if restart.is_some() {
            ErrorCode::Conflict
        } else {
            ErrorCode::IdempotencyConflict
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN admisión de transformación", &e))?;
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando admisión", &e))?;
        let at = timestamp(chrono::Utc::now());
        let replay:Option<(String,i64,String)>=sqlx::query_as("SELECT body_hash,status_code,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?")
            .bind(&ctx.actor_id).bind(operation).bind(&key.key).bind(&at).fetch_optional(&mut *tx).await.map_err(|e|db_error("leyendo replay de transformación",&e))?;
        if let Some((digest, status, response)) = replay {
            if digest != body_hash {
                return Err(invalid(replay_code, "idempotency_body_mismatch"));
            }
            let receipt: JobReceipt = serde_json::from_str(&response)
                .map_err(|e| db_error("recibo de transformación", &e))?;
            let stored:Option<(String,Option<String>)>=sqlx::query_as("SELECT json_extract(metadata,'$.actor_id'),json_extract(metadata,'$.initial_activation_digest') FROM transformation_job WHERE id=?")
                .bind(&receipt.job_id).fetch_optional(&mut *tx).await.map_err(|e|db_error("comprobando replay de transformación",&e))?;
            if status != 202
                || !stored.as_ref().is_some_and(|(actor, initial)| {
                    actor == &ctx.actor_id
                        && match restart {
                            Some(r) => receipt.job_id == r.job_id,
                            None => initial.as_deref() == Some(activation_digest.as_str()),
                        }
                })
                || receipt.status_url != format!("/api/v1/jobs/{}", receipt.job_id)
                || receipt.state != JobPhase::Queued
            {
                return Err(invalid(replay_code, "transformation_replay_mismatch"));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de transformación", &e))?;
            return Ok(Some(receipt));
        }
        let Some(observed) = observed else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT consulta de replay", &e))?;
            return Ok(None);
        };
        let row:Option<(String,String,String,String)>=sqlx::query_as("SELECT phase,plan_hash,payload,resource_ids FROM operation_journal WHERE id=? AND kind=?")
            .bind(&req.plan_id).bind(PLAN_KIND).fetch_optional(&mut *tx).await.map_err(|e|db_error("leyendo plan a ejecutar",&e))?;
        let Some((phase, digest, payload, resources)) = row else {
            return Err(invalid(ErrorCode::NotFound, "transformation_plan_missing"));
        };
        let mut record = decode(&payload, &req.plan_id, &digest, &resources)?;
        if record.actor_id != ctx.actor_id {
            return Err(invalid(
                ErrorCode::Forbidden,
                "transformation_plan_actor_mismatch",
            ));
        }
        if phase != "PLANNED" || record.activation.is_some() {
            return Err(conflict("transformation_plan_already_consumed"));
        }
        if digest != req.plan_hash {
            return Err(invalid(
                ErrorCode::StaleRevision,
                "transformation_plan_changed",
            ));
        }
        check_expiry(&record.plan)?;
        let binding = validate_plan(&record.plan)?;
        let previous = match restart {
            Some(r) => Some(previous_attempt(&mut tx, r, ctx).await?),
            None => None,
        };
        let proof =
            verify_observed_input_hashes(&record.plan.input_artifacts, observed.input.artifacts())
                .map_err(|_| invalid(ErrorCode::IntegrityFailed, "transformation_input_changed"))?;
        if previous.as_ref().is_some_and(|job| {
            job.input_variant_id != record.plan.request.input_variant_id
                || job
                    .metadata
                    .get("input_fingerprint_sha256")
                    .and_then(serde_json::Value::as_str)
                    != record.plan.input_fingerprint().ok().as_deref()
        }) {
            return Err(invalid(
                ErrorCode::IntegrityFailed,
                "transformation_restart_original_input_changed",
            ));
        }
        let qualified = qualify(
            &mut tx,
            binding,
            &record.plan.request.adapter_id,
            ctx,
            self.qualification.as_ref(),
        )
        .await?;
        observed.input.recheck().await?;
        let mut resources = catalogue(&mut tx, &record.plan, observed.input).await?;
        let target: Option<(String, String, String)> =
            sqlx::query_as("SELECT volume_key,state,filesystem FROM storage_location WHERE id=?")
                .bind(&record.plan.request.storage_location_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("destino de transformación", &e))?;
        let Some((volume, state, filesystem)) = target else {
            return Err(conflict("transformation_destination_missing"));
        };
        if state != "ONLINE"
            || volume.is_empty()
            || volume.starts_with("unknown:")
            || !record
                .plan
                .response
                .storage_estimate
                .per_volume
                .iter()
                .any(|v| v.volume_key == volume)
        {
            return Err(conflict("transformation_destination_unavailable"));
        }
        if filesystem.eq_ignore_ascii_case("FAT32")
            && record
                .plan
                .response
                .storage_estimate
                .upper_bound_bytes
                .is_none_or(|b| b.get() > u64::from(u32::MAX))
        {
            return Err(invalid(
                ErrorCode::Incompatible,
                "transformation_file_bound_exceeds_fat32",
            ));
        }
        let job_id = previous
            .as_ref()
            .map(|j| j.id.clone())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let generation = match previous.as_ref() {
            Some(j) => match j.metadata.get("guard_generation") {
                Some(value) => value.as_i64().filter(|g| *g >= 0).ok_or_else(|| {
                    invalid(
                        ErrorCode::IntegrityFailed,
                        "transformation_guard_generation_invalid",
                    )
                })?,
                None => 0,
            }
            .checked_add(1)
            .filter(|g| *g > 0)
            .ok_or_else(|| conflict("transformation_guard_generation_overflow"))?,
            None => 0,
        };
        let output_id = uuid::Uuid::now_v7().to_string();
        for v in &record.plan.response.storage_estimate.per_volume {
            resources.push((
                Resource::new(ResourceKind::Volume, &v.volume_key),
                LockMode::Shared,
            ));
        }
        resources.push((
            Resource::new(ResourceKind::Variant, &output_id),
            LockMode::Exclusive,
        ));
        let resources = normalize(&resources);
        crate::guard::acquire_in(&mut tx, &job_id, generation, &resources, None).await?;
        confirm_guards(&mut tx, &job_id, &resources, generation).await?;
        let mut job = if let Some(previous) = &previous {
            let mut job = previous.clone();
            job.retry_with_preflight(&record.plan, &proof, &binding.adapter_installation_id, &at)
                .map_err(|_| {
                    invalid(
                        ErrorCode::IntegrityFailed,
                        "transformation_restart_precondition_failed",
                    )
                })?;
            job.metadata
                .insert("correlation_id".into(), json!(ctx.correlation_id));
            job
        } else {
            TransformationJob::from_plan(
                &job_id,
                &binding.adapter_installation_id,
                &record.plan,
                &at,
                ctx,
            )
            .map_err(|_| conflict("transformation_job_invalid"))?
        };
        if previous.is_none() {
            job.metadata
                .insert("initial_activation_digest".into(), json!(activation_digest));
        }
        job.metadata
            .insert("guard_generation".into(), json!(generation));
        job.metadata
            .insert("planned_output_variant_id".into(), json!(output_id));
        job.metadata
            .insert("resource_locks".into(), json!(resources));
        job.metadata.insert(
            "input_location_ids".into(),
            json!(observed
                .input
                .locations()
                .iter()
                .map(|s| &s.artifact_location_id)
                .collect::<Vec<_>>()),
        );
        job.metadata
            .insert("input_identity".into(), json!(observed.input.identity()));
        job.metadata
            .insert("input_locations".into(), json!(observed.input.locations()));
        let created = if let Some(previous) = &previous {
            sqlx::query("UPDATE transformation_job SET updated_at=?,row_version=?,metadata=?,adapter_id=?,tool_version=?,parameters=?,plan_hash=?,state='QUEUED',attempt=?,error=NULL,output_variant_id=NULL WHERE id=? AND row_version=? AND state IN ('FAILED','FAILED_RECOVERABLE') AND output_variant_id IS NULL")
                .bind(&job.updated_at).bind(job.row_version).bind(json!(job.metadata).to_string()).bind(&job.adapter_id).bind(&job.tool_version).bind(job.parameters.to_string()).bind(&job.plan_hash).bind(i64::from(job.attempt)).bind(&job.id).bind(previous.row_version)
                .execute(&mut *tx).await.map_err(|e|db_error("reiniciando transformación",&e))?
        } else {
            sqlx::query("INSERT INTO transformation_job(id,created_at,updated_at,row_version,metadata,input_variant_id,output_variant_id,adapter_id,tool_version,parameters,plan_hash,state,attempt,error) VALUES(?,?,?,1,?,?,NULL,?,?,?,?,'QUEUED',0,NULL)")
            .bind(&job.id).bind(&job.created_at).bind(&job.updated_at).bind(json!(job.metadata).to_string())
            .bind(&job.input_variant_id).bind(&job.adapter_id).bind(&job.tool_version).bind(job.parameters.to_string()).bind(&job.plan_hash)
            .execute(&mut *tx).await.map_err(|e|db_error("creando transformación",&e))?
        };
        inserted(created, "transformation_job_not_persisted")?;
        let journal = json!({"schema_version":"1.0","actor_id":ctx.actor_id,"correlation_id":ctx.correlation_id,"state":"QUEUED","attempt":job.attempt,"plan_hash":job.plan_hash,"plan":record.plan,"guard_generation":generation,"planned_output_variant_id":output_id,
            "input_identity":observed.input.identity(),"input_locations":observed.input.locations()});
        let journaled = if let Some(previous) = &previous {
            sqlx::query("UPDATE operation_journal SET resource_ids=?,phase='QUEUED',plan_hash=?,payload=?,updated_at=? WHERE id=? AND kind='transformation.job' AND plan_hash=? AND phase IN ('FAILED','FAILED_RECOVERABLE')")
                .bind(json!(resources.iter().map(|(r,_)|r.key()).collect::<Vec<_>>()).to_string()).bind(&job.plan_hash).bind(journal.to_string()).bind(&at).bind(&job.id).bind(&previous.plan_hash)
                .execute(&mut *tx).await.map_err(|e|db_error("journal de reinicio",&e))?
        } else {
            sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'transformation.job',?,'QUEUED',?,?,?)")
            .bind(&job.id).bind(json!(resources.iter().map(|(r,_)|r.key()).collect::<Vec<_>>()).to_string())
            .bind(&job.plan_hash).bind(journal.to_string()).bind(&at).execute(&mut *tx).await.map_err(|e|db_error("journal de transformación",&e))?
        };
        inserted(journaled, "transformation_journal_not_persisted")?;
        record.activation = Some(PlanActivation {
            job_id: job_id.clone(),
            activated_at: at.clone(),
            correlation_id: ctx.correlation_id.clone(),
        });
        let consumed=sqlx::query("UPDATE operation_journal SET phase='QUEUED',payload=?,updated_at=? WHERE id=? AND kind=? AND phase='PLANNED' AND plan_hash=?")
            .bind(serde_json::to_string(&record).map_err(|e|db_error("consumo del plan",&e))?).bind(&at).bind(&req.plan_id).bind(PLAN_KIND).bind(&req.plan_hash)
            .execute(&mut *tx).await.map_err(|e|db_error("consumiendo plan de transformación",&e))?;
        if consumed.rows_affected() != 1 {
            return Err(conflict("transformation_plan_consumption_raced"));
        }
        append(&mut tx,&NewEvent {event_type:"transformation.state_changed",resource_type:"transformation_job",resource_id:&job_id,resource_version:job.row_version,
            data:json!({"action":if restart.is_some() {"restarted"} else {"queued"},"state":"QUEUED","input_variant_id":job.input_variant_id,"plan_id":req.plan_id,"plan_hash":req.plan_hash,"attempt":job.attempt})},ctx).await?;
        let receipt = JobReceipt {
            job_id: job_id.clone(),
            status_url: format!("/api/v1/jobs/{job_id}"),
            state: JobPhase::Queued,
        };
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id).bind(operation).bind(&key.key).bind(&at).execute(&mut *tx).await.map_err(|e|db_error("retirando clave expirada",&e))?;
        let remembered=sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES(?,?,?,?,202,?,?)")
            .bind(&ctx.actor_id).bind(operation).bind(&key.key).bind(&body_hash).bind(json!(receipt).to_string())
            .bind(timestamp(chrono::Utc::now()+chrono::Duration::days(30))).execute(&mut *tx).await.map_err(|e|db_error("idempotencia de transformación",&e))?;
        inserted(remembered, "transformation_idempotency_not_persisted")?;
        qualified.guard.recheck().await?;
        observed.input.recheck().await?;
        let space = observed.space.observe().await?;
        let expected = demands(&mut tx, &record.plan, &space).await?;
        approval(&mut tx, req, &record.plan, &qualified.package_hash, ctx).await?;
        crate::reservations::admit_in(&mut tx, &job_id, &expected, space.observed_at, None).await?;
        confirm_reservations(&mut tx, &job_id, &expected).await?;
        check_expiry(&record.plan)?;
        if dispatch {
            crate::transformations::dispatch::enqueue(&mut tx, &job, ctx).await?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT admisión de transformación", &e))?;
        Ok(Some(receipt))
    }
}
