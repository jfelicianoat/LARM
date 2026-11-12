//! Admisión durable de instalaciones runtime (P031).
//!
//! El plan ya debe estar calculado por el caso de uso y ligado al adapter.
//! Esta capa vuelve a comprobar su binding de dominio y escribe la fila
//! INSTALL_PLANNED, el job, locks, reservas, evento/outbox e idempotencia en
//! una transacción SQLite única. No ejecuta comandos ni inicia procesos.

use super::lock;
use crate::events::{append, NewEvent};
use crate::guard::{acquire_in, write_journal};
use crate::pool::db_error;
use crate::reservations::admit_in;
use async_trait::async_trait;
use larm_domain::adapter_updates::is_sha256;
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{is_uuid_v7, timestamp, JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
use larm_domain::ports::{JobSource, MutationContext, VolumeDemand};
use larm_domain::runtime::{
    InstallationStage, InstallationState, RegistrationMode, RuntimeOwnership, RuntimeVersion,
    RuntimeVersionLifecycle,
};
use larm_domain::runtime_install::{
    plan_hash, request_from_plan, InstallRequest, RuntimeInstallAdmission, RuntimeInstallJob,
    RuntimeInstallJobRepository, INSTALL_IDEMPOTENCY_OPERATION, INSTALL_JOB_KIND,
    INSTALL_OPERATION, INSTALL_SPEC,
};
use larm_domain::runtime_profiles::RuntimeProfileLock;
use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeSet;

const JOB_STATUS_PREFIX: &str = "/api/v1/jobs/";

async fn update_version_lifecycle(
    conn: &mut SqliteConnection,
    version_id: &str,
    expected: &[RuntimeVersionLifecycle],
    next: RuntimeVersionLifecycle,
    action: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let Some(mut version) = super::load::<RuntimeVersion>(conn, version_id).await? else {
        return Err(integrity("runtime_install_version_missing"));
    };
    if !expected.contains(&version.lifecycle) {
        return Ok(());
    }
    version.lifecycle = next;
    version.row_version = version
        .row_version
        .checked_add(1)
        .ok_or_else(|| invalid("row_version_overflow"))?;
    version.updated_at = timestamp(chrono::Utc::now());
    let changed = sqlx::query(
        "UPDATE runtime_version SET lifecycle=?,row_version=?,updated_at=? WHERE id=? AND row_version=?",
    )
    .bind(next.as_str())
    .bind(version.row_version)
    .bind(&version.updated_at)
    .bind(version_id)
    .bind(version.row_version - 1)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("actualizando lifecycle de runtime_version", &e))?
    .rows_affected();
    if changed != 1 {
        return Err(conflict("runtime_install_version_state_conflict"));
    }
    super::mutation(conn, &version, action, ctx).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct RuntimeInstallCatalogRow {
    runtime_id: String,
    version: String,
    runtime_family: String,
    adapter_installation_id: String,
    package_hash: String,
    state: String,
    trust: String,
    adapter_id: String,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "No se pudo encolar la instalación del runtime",
        reason,
    )
}

fn conflict(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "No se pudo encolar la instalación del runtime",
        reason,
    )
}

fn integrity(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El journal de instalación está corrupto",
        reason,
    )
}

fn canonical_time(value: &str, reason: &str) -> Result<chrono::DateTime<chrono::Utc>, LarmError> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid(reason))?
        .with_timezone(&chrono::Utc);
    if timestamp(parsed) != value {
        return Err(invalid(reason));
    }
    Ok(parsed)
}

fn stored_receipt(row: (String, i64, String)) -> Result<JobReceipt, LarmError> {
    let (body_hash, status, response) = row;
    if !is_sha256(&body_hash) || status != 202 {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "Respuesta de idempotencia corrupta",
            "runtime_install_idempotency_corrupt",
        ));
    }
    let receipt: JobReceipt = serde_json::from_str(&response).map_err(|_| {
        LarmError::new(
            ErrorCode::IntegrityFailed,
            "Respuesta de idempotencia corrupta",
            "runtime_install_idempotency_corrupt",
        )
    })?;
    if !is_uuid_v7(&receipt.job_id)
        || receipt.status_url != format!("{JOB_STATUS_PREFIX}{}", receipt.job_id)
        || receipt.state != JobPhase::Queued
    {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "Respuesta de idempotencia corrupta",
            "runtime_install_idempotency_corrupt",
        ));
    }
    Ok(receipt)
}

struct InstallPlanBinding {
    runtime_version_id: String,
    adapter_id: String,
    adapter_installation_id: String,
    adapter_package_hash: String,
    relative_destination: Option<String>,
}

fn spec_fields(plan: &larm_domain::artifacts::Plan) -> Result<InstallPlanBinding, LarmError> {
    let mut values = plan
        .steps
        .iter()
        .filter(|step| step.request.contains_key(INSTALL_SPEC));
    let step = values
        .next()
        .ok_or_else(|| invalid("runtime_install_spec_missing"))?;
    if values.next().is_some() {
        return Err(invalid("runtime_install_spec_ambiguous"));
    }
    if step.phase != "BIND_INSTALLATION" {
        return Err(invalid("runtime_install_binding_phase_invalid"));
    }
    let adapter_id = step
        .adapter_id
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| invalid("runtime_install_adapter_binding_missing"))?;
    let value = &step.request[INSTALL_SPEC];
    let runtime_version_id = value
        .get("runtime_version_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("runtime_install_version_binding_missing"))?
        .to_owned();
    let adapter_installation_id = value
        .get("adapter_installation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("runtime_install_adapter_installation_missing"))?
        .to_owned();
    let adapter_package_hash = value
        .get("adapter_package_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("runtime_install_adapter_hash_missing"))?
        .to_owned();
    let relative_destination = value
        .get("relative_destination")
        .cloned()
        .ok_or_else(|| invalid("runtime_install_destination_binding_missing"))?;
    let relative_destination = serde_json::from_value(relative_destination)
        .map_err(|_| invalid("runtime_install_destination_binding_invalid"))?;
    Ok(InstallPlanBinding {
        runtime_version_id,
        adapter_id,
        adapter_installation_id,
        adapter_package_hash,
        relative_destination,
    })
}

fn validate_binding(
    installation: &larm_domain::runtime::RuntimeInstallation,
    plan: &larm_domain::artifacts::Plan,
) -> Result<InstallRequest, LarmError> {
    installation.validate()?;
    let request = request_from_plan(plan)?;
    request.validate()?;
    let binding = spec_fields(plan)?;
    if !is_uuid_v7(&plan.plan_id)
        || plan.operation != INSTALL_OPERATION
        || plan.plan_hash != plan_hash(plan)?
        || !is_sha256(&plan.plan_hash)
        || !is_uuid_v7(&binding.runtime_version_id)
        || !is_uuid_v7(&binding.adapter_installation_id)
        || !is_sha256(&binding.adapter_package_hash)
        || plan.resource_ids.iter().collect::<BTreeSet<_>>().len() != plan.resource_ids.len()
        || plan.resource_ids.iter().any(|id| !is_uuid_v7(id))
        || !plan.resource_ids.contains(&request.runtime_id)
        || !plan.resource_ids.contains(&binding.runtime_version_id)
        || !plan.resource_ids.contains(&request.storage_location_id)
        || installation.stage != InstallationStage::InstallPlanned
        || installation.state != InstallationState::InstallPlanned
        || installation.runtime_version_id != binding.runtime_version_id
        || installation.storage_location_id.as_deref() != Some(request.storage_location_id.as_str())
        || installation.strategy != request.strategy
        || installation.external_host_id != request.external_host_id
        || installation.relative_path != binding.relative_destination
        || installation.lock_hash.as_deref() != Some(request.profile_lock_hash.as_str())
    {
        return Err(invalid("runtime_install_binding_mismatch"));
    }
    let adopted = request.strategy == larm_domain::runtime::InstallationStrategy::ExternalAdoption;
    if (adopted
        && (installation.registration_mode != RegistrationMode::Adopted
            || installation.ownership != RuntimeOwnership::External))
        || (!adopted
            && (installation.registration_mode != RegistrationMode::Managed
                || installation.ownership != RuntimeOwnership::LarmManaged))
    {
        return Err(invalid("runtime_install_ownership_mismatch"));
    }
    let expiry = canonical_time(&plan.expires_at, "runtime_install_expiry_invalid")?;
    if expiry <= chrono::Utc::now() {
        return Err(conflict("runtime_install_plan_expired"));
    }
    if plan.approval_required {
        return Err(LarmError::new(
            ErrorCode::ApprovalRequired,
            "Este plan requiere aprobación antes de entrar en cola",
            "runtime_install_approval_required",
        ));
    }
    Ok(request)
}

async fn validate_catalog_binding(
    conn: &mut SqliteConnection,
    request: &InstallRequest,
    binding: &InstallPlanBinding,
) -> Result<(), LarmError> {
    let row: Option<RuntimeInstallCatalogRow> = sqlx::query_as(
        "SELECT rv.runtime_id, rv.version_string AS version, ra.runtime_family, ra.adapter_installation_id, \
         ai.package_hash, ai.state, ai.trust, ai.adapter_id \
         FROM runtime_version rv \
         JOIN runtime r ON r.id = rv.runtime_id \
         JOIN runtime_adapter ra ON ra.id = r.adapter_id \
         JOIN adapter_installation ai ON ai.id = ra.adapter_installation_id \
         WHERE rv.id = ? AND r.id = ?",
    )
    .bind(&binding.runtime_version_id)
    .bind(&request.runtime_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("comprobando binding de runtime.install", &e))?;
    let Some(RuntimeInstallCatalogRow {
        runtime_id,
        version,
        runtime_family,
        adapter_installation_id,
        package_hash,
        state,
        trust,
        adapter_id,
    }) = row
    else {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "La versión o el adaptador del plan ya no están disponibles",
            "runtime_install_catalog_binding_changed",
        ));
    };
    let profile_payload: Option<String> =
        sqlx::query_scalar("SELECT payload FROM runtime_profile_lock WHERE lock_sha256=?")
            .bind(&request.profile_lock_hash)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| db_error("leyendo runtime profile lock de runtime.install", &e))?;
    let Some(profile_payload) = profile_payload else {
        return Err(conflict("runtime_install_profile_lock_missing"));
    };
    let profile: RuntimeProfileLock = serde_json::from_str(&profile_payload)
        .map_err(|_| integrity("runtime_install_profile_lock_corrupt"))?;
    profile
        .validate()
        .map_err(|_| integrity("runtime_install_profile_lock_corrupt"))?;
    if profile.key.runtime != runtime_family || profile.key.upstream_version != version {
        return Err(conflict("runtime_install_profile_lock_binding_changed"));
    }
    let strategy = serde_json::to_value(request.strategy)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| invalid("runtime_install_strategy_invalid"))?;
    let strategies = profile
        .fields
        .get("installation_strategies")
        .and_then(Value::as_array)
        .ok_or_else(|| integrity("runtime_install_profile_lock_corrupt"))?;
    if !strategies
        .iter()
        .any(|value| value.as_str() == Some(strategy.as_str()))
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El perfil runtime no anuncia la estrategia solicitada",
            "runtime_install_strategy_not_in_profile",
        ));
    }
    if profile
        .fields
        .get("install_strategy")
        .and_then(Value::as_str)
        .is_some_and(|declared| declared != strategy)
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La estrategia no coincide con la fijada por el perfil runtime",
            "runtime_install_strategy_profile_mismatch",
        ));
    }
    if runtime_id != request.runtime_id
        || version != request.upstream_version
        || adapter_installation_id != binding.adapter_installation_id
        || package_hash != binding.adapter_package_hash
        || adapter_id != binding.adapter_id
        || !matches!(state.as_str(), "INSTALLED" | "VALIDATED")
        || trust == "REVOKED"
    {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "El binding del adaptador o de la versión cambió desde la planificación",
            "runtime_install_catalog_binding_changed",
        ));
    }
    Ok(())
}

async fn volume_demands(
    conn: &mut SqliteConnection,
    installation: &larm_domain::runtime::RuntimeInstallation,
    plan: &larm_domain::artifacts::Plan,
) -> Result<(Vec<VolumeDemand>, chrono::DateTime<chrono::Utc>), LarmError> {
    let target: Option<(String, String)> =
        sqlx::query_as("SELECT volume_key, state FROM storage_location WHERE id = ?")
            .bind(
                installation
                    .storage_location_id
                    .as_deref()
                    .unwrap_or_default(),
            )
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| db_error("leyendo destino runtime", &e))?;
    let (target_volume, target_state) = target.ok_or_else(|| {
        LarmError::new(
            ErrorCode::NotFound,
            "La ubicación de instalación no existe",
            "storage_location_not_found",
        )
    })?;
    if target_state != "ONLINE" {
        return Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "La ubicación de instalación no está disponible",
            "runtime_install_location_unavailable",
        ));
    }

    if plan.storage_estimate.per_volume.is_empty() {
        return Err(LarmError::new(
            ErrorCode::UnknownSize,
            "El plan no tiene una estimación reservable por volumen",
            "runtime_install_size_unknown",
        ));
    }
    if plan.storage_estimate.peak_extra_bytes.is_none()
        || plan.storage_estimate.required_free_bytes.is_none()
    {
        return Err(LarmError::new(
            ErrorCode::UnknownSize,
            "El plan no tiene una cota superior para reservar",
            "runtime_install_size_unknown",
        ));
    }
    let mut demands = Vec::with_capacity(plan.storage_estimate.per_volume.len());
    let mut seen = BTreeSet::new();
    let mut sum_required = 0_u64;
    let mut snapshot_at: Option<chrono::DateTime<chrono::Utc>> = None;
    for estimate in &plan.storage_estimate.per_volume {
        if estimate.volume_key.is_empty() || !seen.insert(estimate.volume_key.as_str()) {
            return Err(invalid("runtime_install_volume_estimate_invalid"));
        }
        if !estimate.eligible {
            return Err(LarmError::new(
                ErrorCode::StorageInsufficient,
                "El plan informa espacio insuficiente",
                "insufficient_space",
            ));
        }
        let required = estimate
            .peak_extra_bytes
            .get()
            .checked_add(estimate.margin_bytes.get())
            .ok_or_else(|| invalid("runtime_install_size_overflow"))?;
        sum_required = sum_required
            .checked_add(required)
            .ok_or_else(|| invalid("runtime_install_size_overflow"))?;
        let observed: (Option<i64>, Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT MIN(free_bytes), MAX(reserved_bytes), \
             CASE WHEN COUNT(*) = COUNT(last_seen_at) THEN MIN(last_seen_at) ELSE NULL END \
             FROM storage_location WHERE volume_key = ? AND state = 'ONLINE'",
        )
        .bind(&estimate.volume_key)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo snapshot de volumen runtime", &e))?;
        let (free, policy_minimum, seen_at) = observed;
        let (free, policy_minimum) = free.zip(policy_minimum).ok_or_else(|| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "El volumen del plan no está registrado en línea",
                "runtime_install_volume_unavailable",
            )
        })?;
        if free < 0 || policy_minimum < 0 {
            return Err(invalid("runtime_install_volume_snapshot_invalid"));
        }
        let seen_at = seen_at.ok_or_else(|| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "Falta la hora de la medición de espacio",
                "runtime_install_volume_snapshot_missing",
            )
        })?;
        let seen_at = canonical_time(&seen_at, "runtime_install_volume_snapshot_invalid")?;
        snapshot_at = Some(snapshot_at.map_or(seen_at, |current| current.min(seen_at)));
        demands.push(VolumeDemand {
            volume_key: estimate.volume_key.clone(),
            required_bytes: required,
            os_free: u64::try_from(free)
                .map_err(|_| invalid("runtime_install_volume_snapshot_invalid"))?,
            policy_minimum: u64::try_from(policy_minimum)
                .map_err(|_| invalid("runtime_install_volume_snapshot_invalid"))?,
        });
    }
    if !seen.contains(target_volume.as_str())
        || plan
            .storage_estimate
            .required_free_bytes
            .map(|bytes| bytes.get())
            != Some(sum_required)
    {
        return Err(invalid("runtime_install_estimate_binding_mismatch"));
    }
    Ok((
        demands,
        snapshot_at.ok_or_else(|| invalid("runtime_install_volume_snapshot_missing"))?,
    ))
}

#[derive(Clone)]
pub struct SqlRuntimeInstallJobs {
    pub pool: SqlitePool,
}

type RuntimeInstallJobRow = (String, String, String, String, String);

fn runtime_install_job_from_row(
    id: &str,
    (phase, saved_hash, resource_ids, payload, updated_at): RuntimeInstallJobRow,
) -> Result<RuntimeInstallJob, LarmError> {
    let payload: Value =
        serde_json::from_str(&payload).map_err(|_| integrity("runtime_install_job_corrupt"))?;
    let phase: JobPhase = serde_json::from_value(Value::String(phase))
        .map_err(|_| integrity("runtime_install_job_phase_corrupt"))?;
    let plan: larm_domain::artifacts::Plan = serde_json::from_value(payload["plan"].clone())
        .map_err(|_| integrity("runtime_install_job_plan_corrupt"))?;
    let installation: larm_domain::runtime::RuntimeInstallation =
        serde_json::from_value(payload["installation"].clone())
            .map_err(|_| integrity("runtime_install_job_record_corrupt"))?;
    installation
        .validate()
        .map_err(|_| integrity("runtime_install_job_record_invalid"))?;
    let actor_id = payload["actor_id"]
        .as_str()
        .ok_or_else(|| integrity("runtime_install_job_actor_corrupt"))?
        .to_owned();
    let correlation_id = payload["correlation_id"]
        .as_str()
        .ok_or_else(|| integrity("runtime_install_job_correlation_corrupt"))?
        .to_owned();
    let request_hash = payload["request_hash"]
        .as_str()
        .ok_or_else(|| integrity("runtime_install_job_request_hash_corrupt"))?
        .to_owned();
    let resource_ids: Vec<String> = serde_json::from_str(&resource_ids)
        .map_err(|_| integrity("runtime_install_job_resources_corrupt"))?;
    if id != plan.plan_id
        || saved_hash != plan.plan_hash
        || plan_hash(&plan)? != saved_hash
        || resource_ids != plan.resource_ids
        || !is_uuid_v7(&correlation_id)
        || !is_sha256(&request_hash)
    {
        return Err(integrity("runtime_install_job_binding_corrupt"));
    }
    let result = payload
        .get("result")
        .filter(|value| !value.is_null())
        .cloned();
    let error = match payload.get("error") {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value(value.clone())
                .map_err(|_| integrity("runtime_install_job_error_corrupt"))?,
        ),
        _ => None,
    };
    Ok(RuntimeInstallJob {
        job_id: id.to_owned(),
        state: phase,
        plan_hash: saved_hash,
        resource_ids,
        plan,
        installation,
        actor_id,
        correlation_id,
        request_hash,
        updated_at,
        result,
        error,
    })
}

#[async_trait]
impl RuntimeInstallJobRepository for SqlRuntimeInstallJobs {
    async fn replay(
        &self,
        principal_id: &str,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<Option<JobReceipt>, LarmError> {
        if principal_id.is_empty()
            || principal_id.len() > 256
            || principal_id.chars().any(char::is_control)
            || idempotency_key.is_empty()
            || idempotency_key.len() > 200
            || idempotency_key.chars().any(char::is_control)
            || !is_sha256(request_hash)
        {
            return Err(invalid("runtime_install_idempotency_invalid"));
        }
        let now = timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN replay runtime.install", &e))?;
        let existing: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash, status_code, response_json FROM idempotency \
             WHERE principal_id = ? AND operation = ? AND key = ? AND expires_at > ?",
        )
        .bind(principal_id)
        .bind(INSTALL_IDEMPOTENCY_OPERATION)
        .bind(idempotency_key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo replay runtime.install", &e))?;
        let Some((saved_hash, status, response)) = existing else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay runtime.install vacío", &e))?;
            return Ok(None);
        };
        if saved_hash != request_hash {
            return Err(LarmError::new(
                ErrorCode::IdempotencyConflict,
                "La clave idempotente ya se usó con otra solicitud",
                "idempotency_key_conflict",
            ));
        }
        let receipt = stored_receipt((saved_hash, status, response))?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT replay runtime.install", &e))?;
        if self.get_job(&receipt.job_id).await?.is_none() {
            return Err(integrity("runtime_install_job_missing"));
        }
        Ok(Some(receipt))
    }

    async fn enqueue_plan(
        &self,
        admission: RuntimeInstallAdmission<'_>,
    ) -> Result<(JobReceipt, bool), LarmError> {
        let RuntimeInstallAdmission {
            installation,
            plan,
            principal_id,
            idempotency_key,
            request_hash,
            idempotency_expires_at,
            ctx,
            session,
        } = admission;
        if principal_id.is_empty()
            || principal_id.len() > 256
            || principal_id.chars().any(char::is_control)
            || idempotency_key.is_empty()
            || idempotency_key.len() > 200
            || idempotency_key.chars().any(char::is_control)
            || !is_sha256(request_hash)
            || ctx.actor_id.is_empty()
            || ctx.actor_id.len() > 256
            || ctx.actor_id.chars().any(char::is_control)
            || !is_uuid_v7(&ctx.correlation_id)
        {
            return Err(invalid("runtime_install_idempotency_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN runtime.install", &e))?;
        lock(&mut tx).await?;
        let now = timestamp(chrono::Utc::now());
        let existing: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash, status_code, response_json FROM idempotency \
             WHERE principal_id = ? AND operation = ? AND key = ? AND expires_at > ?",
        )
        .bind(principal_id)
        .bind(INSTALL_IDEMPOTENCY_OPERATION)
        .bind(idempotency_key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo idempotencia runtime.install", &e))?;
        if let Some((saved_hash, status, response)) = existing {
            if saved_hash != request_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "La clave idempotente ya se usó con otra solicitud",
                    "idempotency_key_conflict",
                ));
            }
            let receipt = stored_receipt((saved_hash, status, response))?;
            let found: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM operation_journal WHERE id = ? AND kind = ?")
                    .bind(&receipt.job_id)
                    .bind(INSTALL_JOB_KIND)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| db_error("verificando job runtime.install", &e))?;
            if found.is_none() {
                return Err(LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El registro idempotente no tiene job",
                    "runtime_install_job_missing",
                ));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay runtime.install", &e))?;
            let job = self
                .get_job(&receipt.job_id)
                .await?
                .ok_or_else(|| integrity("runtime_install_job_missing"))?;
            // El replay conserva el recibo original 202 (QUEUED), aunque el
            // recurso ya haya avanzado o terminado. La lectura valida el
            // journal y el binding, pero el estado actual vive en GET /jobs.
            let _ = job;
            return Ok((receipt, false));
        }
        let expiry = canonical_time(idempotency_expires_at, "idempotency_expiry_invalid")?;
        if expiry <= chrono::Utc::now() {
            return Err(invalid("idempotency_expiry_invalid"));
        }
        validate_binding(installation, plan)?;
        let binding = spec_fields(plan)?;
        let request = request_from_plan(plan)?;
        validate_catalog_binding(&mut tx, &request, &binding).await?;
        let session_binding = session.binding();
        if session_binding.runtime_id != request.runtime_id
            || session_binding.actor_id != ctx.actor_id
            || ctx.actor_id != principal_id
        {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La sesión cualificada pertenece a otro actor o runtime",
                "runtime_install_session_mismatch",
            ));
        }
        session.recheck().await?;
        let (demands, snapshot_at) = volume_demands(&mut tx, installation, plan).await?;
        let receipt = JobReceipt {
            job_id: plan.plan_id.clone(),
            status_url: format!("{JOB_STATUS_PREFIX}{}", plan.plan_id),
            state: JobPhase::Queued,
        };
        let record = json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "installation": installation,
            "plan": plan,
            "request_hash": request_hash,
        });
        let step = JournalStep {
            id: plan.plan_id.clone(),
            kind: INSTALL_JOB_KIND.into(),
            phase: "QUEUED".into(),
            plan_hash: plan.plan_hash.clone(),
            resource_ids: plan.resource_ids.clone(),
            payload: record.clone(),
            from_phases: Vec::new(),
        };
        let duplicate: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM operation_journal WHERE id = ?")
                .bind(&plan.plan_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("comprobando job runtime.install", &e))?;
        if duplicate.is_some() {
            return Err(conflict("runtime_install_job_identity_exists"));
        }
        super::insert(&mut tx, installation).await?;
        update_version_lifecycle(
            &mut tx,
            &installation.runtime_version_id,
            &[RuntimeVersionLifecycle::Candidate],
            RuntimeVersionLifecycle::Installing,
            "installation_queued",
            ctx,
        )
        .await?;
        let locks: Vec<_> = demands
            .iter()
            .map(|demand| {
                (
                    Resource::new(ResourceKind::Volume, demand.volume_key.clone()),
                    LockMode::Exclusive,
                )
            })
            .collect();
        acquire_in(&mut tx, &plan.plan_id, 1, &locks, Some(&step)).await?;
        admit_in(
            &mut tx,
            &plan.plan_id,
            &demands,
            snapshot_at,
            Some(plan.expires_at.clone()),
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_queued","job_id":plan.plan_id,"stage":"INSTALL_PLANNED"}),
            },
            ctx,
        )
        .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES(?,?,?,?,202,?,?)",
        )
        .bind(principal_id)
        .bind(INSTALL_IDEMPOTENCY_OPERATION)
        .bind(idempotency_key)
        .bind(request_hash)
        .bind(serde_json::to_string(&receipt).map_err(|e| db_error("serializando respuesta", &e))?)
        .bind(idempotency_expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando idempotencia runtime.install", &e))?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT runtime.install", &e))?;
        Ok((receipt, true))
    }

    async fn start_job(&self, id: &str) -> Result<RuntimeInstallJob, LarmError> {
        let job = self.get_job(id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            )
        })?;
        if job.state != JobPhase::Queued {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El job de instalación ya no está en cola",
                "runtime_install_start_state_conflict",
            ));
        }

        let request = validate_binding(&job.installation, &job.plan)?;
        let binding = spec_fields(&job.plan)?;
        let now = chrono::Utc::now();
        let now_text = timestamp(now);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN start runtime.install", &e))?;
        super::lock(&mut tx).await?;
        validate_catalog_binding(&mut tx, &request, &binding).await?;

        for estimate in &job.plan.storage_estimate.per_volume {
            let required = estimate
                .peak_extra_bytes
                .get()
                .checked_add(estimate.margin_bytes.get())
                .ok_or_else(|| invalid("runtime_install_size_overflow"))?;
            let required_i64 =
                i64::try_from(required).map_err(|_| invalid("runtime_install_size_overflow"))?;
            let lock_key = Resource::new(ResourceKind::Volume, estimate.volume_key.clone()).key();
            let held: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM resource_lock WHERE resource_id = ? AND owner_id = ? \
                 AND generation = 1 AND mode = 'EXCLUSIVE'",
            )
            .bind(lock_key)
            .bind(&job.job_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("verificando lock runtime.install", &e))?;
            let reserved: Option<i64> = sqlx::query_scalar(
                "SELECT remaining_bytes FROM volume_reservation WHERE job_id = ? AND volume_key = ? \
                 AND remaining_bytes >= ? AND (expires_at IS NULL OR expires_at > ?)",
            )
            .bind(&job.job_id)
            .bind(&estimate.volume_key)
            .bind(required_i64)
            .bind(&now_text)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("verificando reserva runtime.install", &e))?;
            if held.is_none() || reserved.is_none() {
                return Err(LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El job perdió su lock o su reserva de volumen",
                    "runtime_install_guard_missing",
                )
                .with_resource(job.job_id.clone()));
            }
        }

        InstallationStage::InstallPlanned.transition(InstallationStage::Installing, false)?;
        let mut installation =
            super::load::<larm_domain::runtime::RuntimeInstallation>(&mut tx, &job.installation.id)
                .await?
                .ok_or_else(|| integrity("runtime_install_record_missing"))?;
        if installation != job.installation {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro de instalación cambió desde la admisión",
                "runtime_install_record_changed",
            ));
        }
        installation.stage = InstallationStage::Installing;
        installation.state = InstallationState::Installing;
        installation.row_version = installation
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        installation.updated_at = now_text.clone();
        installation.validate()?;
        let changed = sqlx::query(
            "UPDATE runtime_installation SET stage = 'INSTALLING', state = 'INSTALLING', \
             row_version = ?, updated_at = ? WHERE id = ? AND row_version = ? \
             AND stage = 'INSTALL_PLANNED' AND state = 'INSTALL_PLANNED'",
        )
        .bind(installation.row_version)
        .bind(&installation.updated_at)
        .bind(&installation.id)
        .bind(job.installation.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("iniciando runtime.install", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El registro de instalación ya no está planificado",
                "runtime_install_record_state_conflict",
            ));
        }

        let payload = json!({
            "actor_id": job.actor_id,
            "correlation_id": job.correlation_id,
            "installation": installation,
            "plan": job.plan,
            "request_hash": job.request_hash,
        });
        write_journal(
            &mut tx,
            &JournalStep {
                id: job.job_id.clone(),
                kind: INSTALL_JOB_KIND.into(),
                phase: "RUNNING".into(),
                plan_hash: job.plan_hash.clone(),
                resource_ids: job.resource_ids.clone(),
                payload,
                from_phases: vec!["QUEUED".into()],
            },
        )
        .await?;
        let ctx = MutationContext {
            actor_id: job.actor_id,
            correlation_id: job.correlation_id,
        };
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_started","job_id":job.job_id,"stage":"INSTALLING"}),
            },
            &ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT start runtime.install", &e))?;
        self.get_job(id)
            .await?
            .ok_or_else(|| integrity("runtime_install_job_missing_after_start"))
    }

    async fn complete_job(
        &self,
        id: &str,
        worker_receipt: &JobReceipt,
    ) -> Result<RuntimeInstallJob, LarmError> {
        if !is_uuid_v7(&worker_receipt.job_id)
            || worker_receipt.status_url.is_empty()
            || worker_receipt.state != JobPhase::Completed
        {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El worker no confirmó la finalización de la instalación",
                "runtime_install_worker_not_completed",
            ));
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN complete runtime.install", &e))?;
        super::lock(&mut tx).await?;
        let row: Option<RuntimeInstallJobRow> = sqlx::query_as(
            "SELECT phase,plan_hash,resource_ids,payload,updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(INSTALL_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo job runtime.install para completar", &e))?;
        let job = row
            .map(|row| runtime_install_job_from_row(id, row))
            .transpose()?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                )
            })?;
        if job.state != JobPhase::Running {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El job de instalación no está ejecutándose",
                "runtime_install_complete_state_conflict",
            ));
        }

        job.installation
            .stage
            .transition(InstallationStage::Installed, false)?;
        let mut installation =
            super::load::<larm_domain::runtime::RuntimeInstallation>(&mut tx, &job.installation.id)
                .await?
                .ok_or_else(|| integrity("runtime_install_record_missing"))?;
        if installation != job.installation {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro de instalación cambió durante el job",
                "runtime_install_record_changed",
            ));
        }
        installation.stage = InstallationStage::Installed;
        installation.state = InstallationState::Installed;
        installation.row_version = installation
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        installation.updated_at = timestamp(chrono::Utc::now());
        installation.validate()?;
        let changed = sqlx::query(
            "UPDATE runtime_installation SET stage = 'INSTALLED', state = 'INSTALLED', \
             row_version = ?, updated_at = ? WHERE id = ? AND row_version = ? \
             AND stage = 'INSTALLING' AND state = 'INSTALLING'",
        )
        .bind(installation.row_version)
        .bind(&installation.updated_at)
        .bind(&installation.id)
        .bind(job.installation.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("completando runtime.install", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación ya no está ejecutándose",
                "runtime_install_record_state_conflict",
            ));
        }

        let result = json!({
            "worker_job_id": worker_receipt.job_id,
            "state": "COMPLETED",
        });
        let payload = json!({
            "actor_id": job.actor_id,
            "correlation_id": job.correlation_id,
            "installation": installation,
            "plan": job.plan,
            "request_hash": job.request_hash,
            "result": result,
            "error": null,
        });
        write_journal(
            &mut tx,
            &JournalStep {
                id: job.job_id.clone(),
                kind: INSTALL_JOB_KIND.into(),
                phase: "COMPLETED".into(),
                plan_hash: job.plan_hash.clone(),
                resource_ids: job.resource_ids.clone(),
                payload,
                from_phases: vec!["RUNNING".into()],
            },
        )
        .await?;
        sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva runtime.install", &e))?;
        sqlx::query("DELETE FROM resource_lock WHERE owner_id = ? AND generation = 1")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando locks runtime.install", &e))?;
        let ctx = MutationContext {
            actor_id: job.actor_id,
            correlation_id: job.correlation_id,
        };
        update_version_lifecycle(
            &mut tx,
            &installation.runtime_version_id,
            &[RuntimeVersionLifecycle::Installing],
            RuntimeVersionLifecycle::Validating,
            "installation_bytes_installed",
            &ctx,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_completed","job_id":job.job_id,"stage":"INSTALLED"}),
            },
            &ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT complete runtime.install", &e))?;
        self.get_job(id)
            .await?
            .ok_or_else(|| integrity("runtime_install_job_missing_after_complete"))
    }

    async fn fail_job(&self, id: &str, error: &LarmError) -> Result<RuntimeInstallJob, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN fail runtime.install", &e))?;
        super::lock(&mut tx).await?;
        let row: Option<RuntimeInstallJobRow> = sqlx::query_as(
            "SELECT phase,plan_hash,resource_ids,payload,updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(INSTALL_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo job runtime.install para fallo", &e))?;
        let job = row
            .map(|row| runtime_install_job_from_row(id, row))
            .transpose()?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                )
            })?;
        if !matches!(job.state, JobPhase::Queued | JobPhase::Running) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El job de instalación ya terminó",
                "runtime_install_fail_state_conflict",
            ));
        }

        let mut installation =
            super::load::<larm_domain::runtime::RuntimeInstallation>(&mut tx, &job.installation.id)
                .await?
                .ok_or_else(|| integrity("runtime_install_record_missing"))?;
        if installation != job.installation {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro de instalación cambió durante el job",
                "runtime_install_record_changed",
            ));
        }
        installation
            .stage
            .transition(InstallationStage::Failed, false)?;
        installation.stage = InstallationStage::Failed;
        installation.state = InstallationState::Failed;
        installation.row_version = installation
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        installation.updated_at = timestamp(chrono::Utc::now());
        installation.validate()?;
        let changed = sqlx::query(
            "UPDATE runtime_installation SET stage = 'FAILED', state = 'FAILED', \
             row_version = ?, updated_at = ? WHERE id = ? AND row_version = ? \
             AND stage IN ('INSTALL_PLANNED','INSTALLING')",
        )
        .bind(installation.row_version)
        .bind(&installation.updated_at)
        .bind(&installation.id)
        .bind(job.installation.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando runtime.install fallido", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación ya no está en un estado fallable",
                "runtime_install_record_state_conflict",
            ));
        }

        let error_value = serde_json::to_value(error)
            .map_err(|_| integrity("runtime_install_job_error_serialize"))?;
        let payload = json!({
            "actor_id": job.actor_id,
            "correlation_id": job.correlation_id,
            "installation": installation,
            "plan": job.plan,
            "request_hash": job.request_hash,
            "result": null,
            "error": error_value,
        });
        write_journal(
            &mut tx,
            &JournalStep {
                id: job.job_id.clone(),
                kind: INSTALL_JOB_KIND.into(),
                phase: "FAILED".into(),
                plan_hash: job.plan_hash.clone(),
                resource_ids: job.resource_ids.clone(),
                payload,
                from_phases: vec!["QUEUED".into(), "RUNNING".into()],
            },
        )
        .await?;
        sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva runtime.install", &e))?;
        sqlx::query("DELETE FROM resource_lock WHERE owner_id = ? AND generation = 1")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando locks runtime.install", &e))?;
        let ctx = MutationContext {
            actor_id: job.actor_id,
            correlation_id: job.correlation_id,
        };
        update_version_lifecycle(
            &mut tx,
            &installation.runtime_version_id,
            &[
                RuntimeVersionLifecycle::Installing,
                RuntimeVersionLifecycle::Validating,
            ],
            RuntimeVersionLifecycle::Broken,
            "installation_failed",
            &ctx,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_failed","job_id":job.job_id,"stage":"FAILED","error_code":error.code.as_str()}),
            },
            &ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT fail runtime.install", &e))?;
        self.get_job(id)
            .await?
            .ok_or_else(|| integrity("runtime_install_job_missing_after_fail"))
    }

    async fn cancel_queued_job(
        &self,
        id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstallJob, LarmError> {
        if principal_id.is_empty() || principal_id != ctx.actor_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El actor de la cancelación no coincide con la credencial",
                "runtime_install_actor_mismatch",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cancel runtime.install", &e))?;
        super::lock(&mut tx).await?;
        let row: Option<RuntimeInstallJobRow> = sqlx::query_as(
            "SELECT phase,plan_hash,resource_ids,payload,updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(INSTALL_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo job runtime.install para cancelar", &e))?;
        let job = row
            .map(|row| runtime_install_job_from_row(id, row))
            .transpose()?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                )
            })?;
        if job.actor_id != principal_id {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            ));
        }
        if job.state == JobPhase::Cancelled {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay cancel runtime.install", &e))?;
            return Ok(job);
        }
        if job.state != JobPhase::Queued {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Sólo se puede cancelar una instalación que aún está en cola",
                "runtime_install_cancel_state_conflict",
            ));
        }

        let mut installation =
            super::load::<larm_domain::runtime::RuntimeInstallation>(&mut tx, &job.installation.id)
                .await?
                .ok_or_else(|| integrity("runtime_install_record_missing"))?;
        if installation != job.installation {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro de instalación cambió desde la admisión",
                "runtime_install_record_changed",
            ));
        }
        installation
            .stage
            .transition(InstallationStage::Failed, false)?;
        installation.stage = InstallationStage::Failed;
        installation.state = InstallationState::Failed;
        installation.row_version = installation
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        installation.updated_at = timestamp(chrono::Utc::now());
        installation.validate()?;
        let changed = sqlx::query(
            "UPDATE runtime_installation SET stage = 'FAILED', state = 'FAILED', \
             row_version = ?, updated_at = ? WHERE id = ? AND row_version = ? \
             AND stage = 'INSTALL_PLANNED' AND state = 'INSTALL_PLANNED'",
        )
        .bind(installation.row_version)
        .bind(&installation.updated_at)
        .bind(&installation.id)
        .bind(job.installation.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando runtime.install cancelado", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación ya no está planificada",
                "runtime_install_record_state_conflict",
            ));
        }

        let payload = json!({
            "actor_id": job.actor_id,
            "correlation_id": job.correlation_id,
            "installation": installation,
            "plan": job.plan,
            "request_hash": job.request_hash,
            "result": {"state":"CANCELLED"},
            "error": null,
        });
        write_journal(
            &mut tx,
            &JournalStep {
                id: job.job_id.clone(),
                kind: INSTALL_JOB_KIND.into(),
                phase: "CANCELLED".into(),
                plan_hash: job.plan_hash.clone(),
                resource_ids: job.resource_ids.clone(),
                payload,
                from_phases: vec!["QUEUED".into()],
            },
        )
        .await?;
        sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva runtime.install cancelado", &e))?;
        sqlx::query("DELETE FROM resource_lock WHERE owner_id = ? AND generation = 1")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando locks runtime.install cancelado", &e))?;
        update_version_lifecycle(
            &mut tx,
            &installation.runtime_version_id,
            &[RuntimeVersionLifecycle::Installing],
            RuntimeVersionLifecycle::Candidate,
            "installation_cancelled_before_start",
            ctx,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_cancelled","job_id":job.job_id,"stage":"FAILED","error_code":"runtime_install_cancelled"}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cancel runtime.install", &e))?;
        self.get_job(id)
            .await?
            .ok_or_else(|| integrity("runtime_install_job_missing_after_cancel"))
    }

    async fn cancel_running_job(
        &self,
        id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstallJob, LarmError> {
        if principal_id.is_empty() || principal_id != ctx.actor_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El actor de la cancelación no coincide con la credencial",
                "runtime_install_actor_mismatch",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cancel running runtime.install", &e))?;
        super::lock(&mut tx).await?;
        let row: Option<RuntimeInstallJobRow> = sqlx::query_as(
            "SELECT phase,plan_hash,resource_ids,payload,updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(INSTALL_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo job runtime.install para cancelar", &e))?;
        let job = row
            .map(|row| runtime_install_job_from_row(id, row))
            .transpose()?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                )
            })?;
        if job.actor_id != principal_id {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            ));
        }
        if job.state == JobPhase::Cancelled {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay cancel running runtime.install", &e))?;
            return Ok(job);
        }
        if job.state != JobPhase::Running {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Sólo se puede cancelar una instalación en ejecución",
                "runtime_install_cancel_state_conflict",
            ));
        }

        let mut installation =
            super::load::<larm_domain::runtime::RuntimeInstallation>(&mut tx, &job.installation.id)
                .await?
                .ok_or_else(|| integrity("runtime_install_record_missing"))?;
        if installation != job.installation {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro de instalación cambió desde la admisión",
                "runtime_install_record_changed",
            ));
        }
        installation
            .stage
            .transition(InstallationStage::Failed, false)?;
        installation.stage = InstallationStage::Failed;
        installation.state = InstallationState::Failed;
        installation.row_version = installation
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        installation.updated_at = timestamp(chrono::Utc::now());
        installation.validate()?;
        let changed = sqlx::query(
            "UPDATE runtime_installation SET stage = 'FAILED', state = 'FAILED', \
             row_version = ?, updated_at = ? WHERE id = ? AND row_version = ? \
             AND stage = 'INSTALLING' AND state = 'INSTALLING'",
        )
        .bind(installation.row_version)
        .bind(&installation.updated_at)
        .bind(&installation.id)
        .bind(job.installation.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando runtime.install cancelado", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación ya no está en ejecución",
                "runtime_install_record_state_conflict",
            ));
        }

        let payload = json!({
            "actor_id": job.actor_id,
            "correlation_id": job.correlation_id,
            "installation": installation,
            "plan": job.plan,
            "request_hash": job.request_hash,
            "result": {"state":"CANCELLED"},
            "error": null,
        });
        write_journal(
            &mut tx,
            &JournalStep {
                id: job.job_id.clone(),
                kind: INSTALL_JOB_KIND.into(),
                phase: "CANCELLED".into(),
                plan_hash: job.plan_hash.clone(),
                resource_ids: job.resource_ids.clone(),
                payload,
                from_phases: vec!["RUNNING".into()],
            },
        )
        .await?;
        sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva runtime.install cancelado", &e))?;
        sqlx::query("DELETE FROM resource_lock WHERE owner_id = ? AND generation = 1")
            .bind(&job.job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando locks runtime.install cancelado", &e))?;
        update_version_lifecycle(
            &mut tx,
            &installation.runtime_version_id,
            &[
                RuntimeVersionLifecycle::Installing,
                RuntimeVersionLifecycle::Validating,
            ],
            RuntimeVersionLifecycle::Broken,
            "installation_cancelled",
            ctx,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.state_changed",
                resource_type: "runtime_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: json!({"action":"installation_cancelled","job_id":job.job_id,"stage":"FAILED","error_code":"runtime_install_cancelled"}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cancel running runtime.install", &e))?;
        self.get_job(id)
            .await?
            .ok_or_else(|| integrity("runtime_install_job_missing_after_cancel"))
    }

    async fn get_job(&self, id: &str) -> Result<Option<RuntimeInstallJob>, LarmError> {
        let row: Option<RuntimeInstallJobRow> = sqlx::query_as(
            "SELECT phase,plan_hash,resource_ids,payload,updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(INSTALL_JOB_KIND)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo job runtime.install", &e))?;
        row.map(|row| runtime_install_job_from_row(id, row))
            .transpose()
    }

    async fn queued_job_ids(&self, limit: u32) -> Result<Vec<String>, LarmError> {
        if limit == 0 || limit > 64 {
            return Err(invalid("runtime_install_queue_limit_invalid"));
        }
        sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind = ? AND phase = 'QUEUED' \
             ORDER BY updated_at, id LIMIT ?",
        )
        .bind(INSTALL_JOB_KIND)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando jobs runtime.install en cola", &e))
    }

    async fn running_job_ids(&self, limit: u32) -> Result<Vec<String>, LarmError> {
        if limit == 0 || limit > 64 {
            return Err(invalid("runtime_install_queue_limit_invalid"));
        }
        sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind = ? AND phase = 'RUNNING' \
             ORDER BY random() LIMIT ?",
        )
        .bind(INSTALL_JOB_KIND)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando jobs runtime.install en ejecución", &e))
    }
}

#[async_trait]
impl JobSource for SqlRuntimeInstallJobs {
    async fn job(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<Option<JobState>, LarmError> {
        let Some(job) = self.get_job(job_id).await? else {
            return Ok(None);
        };
        if job.actor_id != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        Ok(Some(JobState {
            job_id: job.job_id,
            state: job.state,
            resource_type: "runtime_installation".into(),
            resource_id: Some(job.installation.id),
            progress: Default::default(),
            result: job.result,
            error: job.error,
            evidence_refs: Vec::new(),
        }))
    }
}
