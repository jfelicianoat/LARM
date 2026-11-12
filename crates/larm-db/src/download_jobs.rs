//! Cola SQLite, recuperación y consulta de jobs de descarga P051.

use async_trait::async_trait;
use larm_domain::artifacts::{ArtifactLocation, ArtifactState, Ownership};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{ApiError, JobPhase, JobState};
use larm_domain::download::{DownloadJob, DownloadState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{ArtifactProvenance, ArtifactSizeConfidence, ArtifactSizeEstimate, Trust};
use larm_domain::ports::{DownloadQueueService, JobSource, MutationContext};
use larm_domain::units::DecimalU64;
use serde_json::Value;
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};

use crate::events::{append, NewEvent};
use crate::pool::db_error;

#[path = "download_controls.rs"]
mod controls;

#[derive(Clone)]
pub struct SqlDownloadJobs {
    pub pool: SqlitePool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadClaim {
    pub job: DownloadJob,
    pub actor_id: String,
    pub correlation_id: String,
}

/// Resultado que el consumidor entrega tras verificar e ingerir un artefacto
/// en el CAS. La capa DB vuelve a cotejarlo con el snapshot aprobado del plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedArtifact {
    pub artifact_id: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub trust: Trust,
    pub source_id: String,
    pub revision_key: String,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo actualizar el trabajo de descarga",
        reason,
    )
}

fn phase_for(state: DownloadState) -> JobPhase {
    match state {
        DownloadState::Planned => JobPhase::Queued,
        DownloadState::Queued => JobPhase::Queued,
        DownloadState::Running
        | DownloadState::Pausing
        | DownloadState::Paused
        | DownloadState::Verifying
        | DownloadState::Cancelling => JobPhase::Running,
        DownloadState::Completed => JobPhase::Completed,
        DownloadState::Failed => JobPhase::Failed,
        DownloadState::Cancelled => JobPhase::Cancelled,
    }
}

fn state_text(state: DownloadState) -> &'static str {
    match state {
        DownloadState::Planned => "PLANNED",
        DownloadState::Queued => "QUEUED",
        DownloadState::Running => "RUNNING",
        DownloadState::Pausing => "PAUSING",
        DownloadState::Paused => "PAUSED",
        DownloadState::Verifying => "VERIFYING",
        DownloadState::Completed => "COMPLETED",
        DownloadState::Cancelling => "CANCELLING",
        DownloadState::Cancelled => "CANCELLED",
        DownloadState::Failed => "FAILED",
    }
}

fn decode_row(row: &sqlx::sqlite::SqliteRow) -> Result<DownloadJob, LarmError> {
    let metadata_text: String = row
        .try_get("metadata")
        .map_err(|e| db_error("metadata de descarga", &e))?;
    let progress_text: String = row
        .try_get("progress")
        .map_err(|e| db_error("progreso de descarga", &e))?;
    let error_text: Option<String> = row
        .try_get("error")
        .map_err(|e| db_error("error de descarga", &e))?;
    let metadata: Value =
        serde_json::from_str(&metadata_text).map_err(|e| db_error("metadata de descarga", &e))?;
    let progress: Value =
        serde_json::from_str(&progress_text).map_err(|e| db_error("progreso de descarga", &e))?;
    let error: Option<Value> = error_text
        .map(|text| serde_json::from_str(&text).map_err(|e| db_error("error de descarga", &e)))
        .transpose()?;
    let job: DownloadJob = serde_json::from_value(serde_json::json!({
        "id": row.try_get::<String, _>("id").map_err(|e| db_error("id de descarga", &e))?,
        "created_at": row.try_get::<String, _>("created_at").map_err(|e| db_error("created_at de descarga", &e))?,
        "updated_at": row.try_get::<String, _>("updated_at").map_err(|e| db_error("updated_at de descarga", &e))?,
        "row_version": row.try_get::<i64, _>("row_version").map_err(|e| db_error("row_version de descarga", &e))?,
        "metadata": metadata,
        "priority": row.try_get::<String, _>("priority").map_err(|e| db_error("priority de descarga", &e))?,
        "variant_id": row.try_get::<String, _>("variant_id").map_err(|e| db_error("variant_id de descarga", &e))?,
        "storage_location_id": row.try_get::<String, _>("storage_location_id").map_err(|e| db_error("storage_location_id de descarga", &e))?,
        "plan_hash": row.try_get::<String, _>("plan_hash").map_err(|e| db_error("plan_hash de descarga", &e))?,
        "state": row.try_get::<String, _>("state").map_err(|e| db_error("state de descarga", &e))?,
        "attempt": u32::try_from(row.try_get::<i64, _>("attempt").map_err(|e| db_error("attempt de descarga", &e))?).map_err(|e| db_error("attempt de descarga", &e))?,
        "progress": progress,
        "error": error,
    }))
    .map_err(|e| db_error("registro de descarga", &e))?;
    job.validate()?;
    Ok(job)
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE download_job SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando cola de descargas", &e))?;
    Ok(())
}

async fn load(conn: &mut SqliteConnection, job_id: &str) -> Result<Option<DownloadJob>, LarmError> {
    let row = sqlx::query("SELECT * FROM download_job WHERE id=?")
        .bind(job_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo trabajo de descarga", &e))?;
    row.as_ref().map(decode_row).transpose()
}

fn actor_context(job: &DownloadJob) -> Result<MutationContext, LarmError> {
    let actor_id = job
        .metadata
        .get("actor_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("download_job_actor_missing"))?;
    let correlation_id = job
        .metadata
        .get("correlation_id")
        .and_then(Value::as_str)
        .filter(|id| larm_domain::bridge::is_uuid_v7(id))
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    Ok(MutationContext {
        actor_id: actor_id.into(),
        correlation_id,
    })
}

async fn emit_state(
    conn: &mut SqliteConnection,
    job: &DownloadJob,
    ctx: &MutationContext,
    action: &str,
) -> Result<(), LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "download.state_changed",
            resource_type: "download_job",
            resource_id: &job.id,
            resource_version: job.row_version,
            data: serde_json::json!({
                "action": action,
                "state": job.state,
                "variant_id": job.variant_id,
                "storage_location_id": job.storage_location_id,
                "attempt": job.attempt,
            "priority": job.priority,
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

impl SqlDownloadJobs {
    /// Persistent resource ownership is checked before worker IO and CAS
    /// promotion, and checked again in the completion writer transaction.
    pub async fn check_acquisition_fence(
        &self,
        job_id: &str,
        record: &larm_domain::download::DownloadPlanRecord,
    ) -> Result<(), LarmError> {
        if record.response.acquisition.is_none() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN fence descarga", &e))?;
        let bound:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM download_job j JOIN operation_journal o ON o.id=json_extract(j.metadata,'$.plan_id') WHERE j.id=? AND j.variant_id=? AND j.plan_hash=? AND o.plan_hash=j.plan_hash AND o.phase='QUEUED' AND json_extract(o.payload,'$.execution.job_id')=j.id)")
            .bind(job_id).bind(&record.response.variant_id).bind(record.response.plan_hash.as_deref())
            .fetch_one(&mut *tx).await.map_err(|e|db_error("revalidando fence de job",&e))?;
        if !bound {
            return Err(invalid("download_resource_fence_lost"));
        }
        crate::download_bundle::check_fence(&mut tx, record).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT fence descarga", &e))?;
        Ok(())
    }

    /// Estado durable actual para resolver una carrera con pause/cancel. Las
    /// transiciones posteriores vuelven a comprobar el estado bajo lock.
    pub async fn current_state(&self, job_id: &str) -> Result<Option<DownloadState>, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(job_id) {
            return Err(invalid("download_job_identity_invalid"));
        }
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| db_error("leyendo estado actual de descarga", &error))?;
        state
            .map(|state| {
                serde_json::from_value(Value::String(state))
                    .map_err(|error| db_error("estado actual de descarga", &error))
            })
            .transpose()
    }

    /// Actualiza prioridad sin alterar el snapshot aprobado, el plan ni la
    /// reserva física. El cambio y su evento de outbox son atómicos.
    pub async fn update_priority(
        &self,
        job_id: &str,
        expected_row_version: i64,
        priority: larm_domain::download::DownloadPriority,
        ctx: &MutationContext,
    ) -> Result<DownloadJob, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(job_id) || expected_row_version < 1 {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Identificador o versión de descarga no válido",
                "download_job_revision_invalid",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN prioridad descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if job.metadata.get("actor_id").and_then(Value::as_str) != Some(ctx.actor_id.as_str()) {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            ));
        }
        if job.row_version != expected_row_version {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La descarga cambió; actualiza la cola antes de volver a intentarlo",
                "stale_revision",
            )
            .with_resource(job_id));
        }
        if !matches!(
            job.state,
            DownloadState::Queued
                | DownloadState::Paused
                | DownloadState::Pausing
                | DownloadState::Running
        ) {
            return Err(invalid("download_priority_state_not_allowed").with_resource(job_id));
        }
        if job.priority == priority {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT prioridad sin cambios", &e))?;
            return Ok(job);
        }
        let next_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let priority_text = serde_json::to_value(priority)
            .map_err(|e| db_error("serializando prioridad de descarga", &e))?
            .as_str()
            .ok_or_else(|| invalid("download_priority_invalid"))?
            .to_owned();
        let changed = sqlx::query(
            "UPDATE download_job SET priority=?,row_version=row_version+1,updated_at=? \
             WHERE id=? AND row_version=? AND state IN ('QUEUED','PAUSED','PAUSING','RUNNING')",
        )
        .bind(&priority_text)
        .bind(&now)
        .bind(job_id)
        .bind(expected_row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando prioridad de descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_priority_update_conflict").with_resource(job_id));
        }
        job.priority = priority;
        job.row_version = next_version;
        job.updated_at = now;
        emit_state(&mut tx, &job, ctx, "priority_changed").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT prioridad descarga", &e))?;
        Ok(job)
    }

    async fn list_owned(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        actor_id: &str,
    ) -> Result<Vec<DownloadJob>, LarmError> {
        let limit = i64::from(limit);
        let rows = if let Some((created_at, id)) = after {
            sqlx::query(
                "SELECT * FROM download_job \
                 WHERE json_extract(metadata,'$.actor_id')=? \
                   AND (created_at>? OR (created_at=? AND id>?)) \
                 ORDER BY created_at,id LIMIT ?",
            )
            .bind(actor_id)
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT * FROM download_job \
                 WHERE json_extract(metadata,'$.actor_id')=? \
                 ORDER BY created_at,id LIMIT ?",
            )
            .bind(actor_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| db_error("listando descargas", &error))?;
        rows.iter().map(decode_row).collect()
    }

    /// Reclama un único job en orden HIGH/NORMAL/BACKGROUND y FIFO dentro
    /// de cada prioridad. El cambio de estado y el evento son atómicos.
    pub async fn claim_next(&self) -> Result<Option<DownloadClaim>, LarmError> {
        self.claim_next_with_source_limit(i64::MAX).await
    }

    /// Reclama respetando además el máximo de descargas activas por fuente.
    pub async fn claim_next_with_source_limit(
        &self,
        per_source_limit: i64,
    ) -> Result<Option<DownloadClaim>, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cola descargas", &e))?;
        lock(&mut tx).await?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT queued.id FROM download_job AS queued WHERE queued.state='QUEUED' \
             AND (SELECT COUNT(*) FROM download_job AS active \
                  WHERE (active.state IN ('RUNNING','PAUSING','VERIFYING') \
                     OR (active.state='CANCELLING' AND json_extract(active.progress,'$.cancel_dispatch')='ACTIVE')) \
                    AND json_extract(active.metadata,'$.source_id')=json_extract(queued.metadata,'$.source_id')) < ? \
             ORDER BY CASE queued.priority WHEN 'HIGH' THEN 0 WHEN 'NORMAL' THEN 1 ELSE 2 END, queued.created_at, queued.id \
             LIMIT 1",
        )
        .bind(per_source_limit)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("seleccionando descarga pendiente", &e))?;
        let Some(id) = id else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT cola descargas", &e))?;
            return Ok(None);
        };
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed = sqlx::query(
            "UPDATE download_job SET state='RUNNING',attempt=attempt+1,row_version=row_version+1,updated_at=?, \
             progress=json_set(progress,'$.phase','RUNNING') WHERE id=? AND state='QUEUED'",
        )
        .bind(now)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("reclamando descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_claim_conflict").with_resource(&id));
        }
        let job = load(&mut tx, &id)
            .await?
            .ok_or_else(|| invalid("download_job_missing_after_claim"))?;
        let ctx = actor_context(&job)?;
        emit_state(&mut tx, &job, &ctx, "claimed").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT claim download", &e))?;
        Ok(Some(DownloadClaim {
            actor_id: ctx.actor_id,
            correlation_id: ctx.correlation_id,
            job,
        }))
    }

    /// Pasa un job RUNNING a VERIFYING antes de aceptar cualquier archivo
    /// staged. El estado y su evento durable se escriben en una transacción.
    pub async fn begin_verification(&self, job_id: &str) -> Result<DownloadJob, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN verificación descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if !job
            .state
            .can_transition_to(larm_domain::download::DownloadState::Verifying)
        {
            return Err(invalid("download_job_not_running_for_verification").with_resource(job_id));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed = sqlx::query(
            "UPDATE download_job SET state='VERIFYING',error=NULL,row_version=row_version+1,updated_at=?,progress=json_set(progress,'$.phase','VERIFYING') WHERE id=? AND state='RUNNING'",
        )
        .bind(&now)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("iniciando verificación de descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_verification_conflict").with_resource(job_id));
        }
        job.state = larm_domain::download::DownloadState::Verifying;
        job.error = None;
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now;
        job.progress
            .insert("phase".into(), Value::String("VERIFYING".into()));
        let ctx = actor_context(&job)?;
        emit_state(&mut tx, &job, &ctx, "verification_started").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT verificación descarga", &e))?;
        Ok(job)
    }

    /// Marca un job activo y su plan sellado como FAILED y libera su reserva
    /// en la misma transacción. Los jobs antiguos sin plan conservan su fallo.
    pub async fn fail_download(&self, job_id: &str, error: &LarmError) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN fallo descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if !job
            .state
            .can_transition_to(larm_domain::download::DownloadState::Failed)
        {
            return Err(
                invalid("download_job_cannot_fail_from_current_state").with_resource(job_id)
            );
        }
        let error_value = serde_json::to_value(error)
            .map_err(|e| db_error("serializando error de descarga", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed = sqlx::query(
            "UPDATE download_job SET state='FAILED',error=?,row_version=row_version+1,updated_at=?,progress=json_set(progress,'$.phase','FAILED') WHERE id=? AND state=?",
        )
        .bind(error_value.to_string())
        .bind(&now)
        .bind(job_id)
        .bind(state_text(job.state))
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("fallando descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_failure_conflict").with_resource(job_id));
        }
        if let Some(plan_id) = job.metadata.get("plan_id").and_then(Value::as_str) {
            let changed = sqlx::query("UPDATE operation_journal SET phase='FAILED',updated_at=?,payload=json_set(payload,'$.execution.phase','FAILED') WHERE id=? AND kind='download.plan' AND phase='QUEUED' AND plan_hash=? AND json_extract(payload,'$.execution.job_id')=?")
                .bind(&now).bind(plan_id).bind(&job.plan_hash).bind(job_id)
                .execute(&mut *tx).await.map_err(|e|db_error("fallando journal de descarga",&e))?;
            if changed.rows_affected() != 1 {
                return Err(invalid("download_failure_journal_conflict").with_resource(job_id));
            }
        }
        sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva de descarga fallida", &e))?;
        job.state = larm_domain::download::DownloadState::Failed;
        job.error = Some(
            serde_json::from_value(error_value)
                .map_err(|e| db_error("decodificando error de descarga", &e))?,
        );
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now;
        job.progress
            .insert("phase".into(), Value::String("FAILED".into()));
        let ctx = actor_context(&job)?;
        crate::download_bundle::release(&mut tx, &job).await?;
        emit_state(&mut tx, &job, &ctx, "failed").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT fallo descarga", &e))?;
        Ok(())
    }

    /// Cierra PAUSING→PAUSED después de que staging haya sincronizado los
    /// bytes del último bloque y el worker cualificado se haya detenido.
    pub async fn finish_pause(&self, job_id: &str) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN pausa descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if job.state != DownloadState::Pausing {
            return Err(invalid("download_job_not_pausing").with_resource(job_id));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        job.state = DownloadState::Paused;
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now.clone();
        job.progress
            .insert("phase".into(), Value::String("PAUSED".into()));
        let changed = sqlx::query(
            "UPDATE download_job SET state='PAUSED',progress=?,row_version=row_version+1,updated_at=? WHERE id=? AND state='PAUSING'",
        )
        .bind(serde_json::to_string(&job.progress).map_err(|e| db_error("progreso pausa descarga", &e))?)
        .bind(&now)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("cerrando pausa descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_pause_conflict").with_resource(job_id));
        }
        let ctx = actor_context(&job)?;
        emit_state(&mut tx, &job, &ctx, "paused").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT pausa descarga", &e))
    }

    /// Cierra CANCELLING→CANCELLED, libera la reserva y actualiza el journal
    /// del plan en una transacción atómica.
    pub async fn finish_cancel(&self, job_id: &str) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cancelación descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if job.state != DownloadState::Cancelling {
            return Err(invalid("download_job_not_cancelling").with_resource(job_id));
        }
        let plan_id = job
            .metadata
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("download_job_plan_missing"))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        job.state = DownloadState::Cancelled;
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now.clone();
        job.progress
            .insert("phase".into(), Value::String("CANCELLED".into()));
        let changed = sqlx::query(
            "UPDATE download_job SET state='CANCELLED',progress=?,row_version=row_version+1,updated_at=? WHERE id=? AND state='CANCELLING'",
        )
        .bind(serde_json::to_string(&job.progress).map_err(|e| db_error("progreso cancelación descarga", &e))?)
        .bind(&now)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("cerrando cancelación descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_cancel_conflict").with_resource(job_id));
        }
        let journal_changed = sqlx::query(
            "UPDATE operation_journal SET phase='CANCELLED',updated_at=?,payload=json_set(payload,'$.execution.phase','CANCELLED','$.execution.cancelled_at',?) WHERE id=? AND kind='download.plan' AND phase IN ('QUEUED','FAILED') AND plan_hash=? AND json_extract(payload,'$.execution.job_id')=?",
        )
        .bind(&now)
        .bind(&now)
        .bind(plan_id)
        .bind(&job.plan_hash)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("cancelando journal de descarga", &e))?;
        if journal_changed.rows_affected() != 1 {
            return Err(invalid("download_plan_cancel_conflict").with_resource(plan_id));
        }
        sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva cancelada", &e))?;
        let ctx = actor_context(&job)?;
        crate::download_bundle::release(&mut tx, &job).await?;
        emit_state(&mut tx, &job, &ctx, "cancelled").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cancelación descarga", &e))
    }

    /// Confirma artefactos ya verificados y copiados al CAS junto con su
    /// procedencia, el job COMPLETED, el journal y la liberación de reserva.
    /// Ninguna parte del catálogo se publica si falla una escritura de outbox.
    pub async fn complete_download(
        &self,
        job_id: &str,
        artifacts: &[DownloadedArtifact],
    ) -> Result<(), LarmError> {
        self.complete_download_qualified(job_id, artifacts, None)
            .await
    }

    /// La primera adquisición exige su snapshot y cualificación vigentes hasta
    /// COMMIT. La entrada anterior sólo admite los planes físicos heredados.
    pub async fn complete_download_qualified(
        &self,
        job_id: &str,
        artifacts: &[DownloadedArtifact],
        qualification: Option<&std::sync::Arc<dyn larm_domain::ports::AdapterQualification>>,
    ) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN completar descarga", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if job.state != DownloadState::Verifying {
            return Err(invalid("download_job_not_verifying").with_resource(job_id));
        }
        let ctx = actor_context(&job)?;
        let plan_id = job
            .metadata
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("download_job_plan_missing"))?
            .to_owned();
        let source_id = job
            .metadata
            .get("source_id")
            .and_then(Value::as_str)
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("download_job_source_missing"))?
            .to_owned();
        let artifact_snapshot = job
            .metadata
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("download_job_artifacts_missing"))?;
        let selected_artifact_count = artifact_snapshot
            .iter()
            .filter(|artifact| artifact.get("selected").and_then(Value::as_bool) == Some(true))
            .count();
        let selected: BTreeMap<String, Value> = artifact_snapshot
            .iter()
            .filter(|artifact| artifact.get("selected").and_then(Value::as_bool) == Some(true))
            .map(|artifact| {
                let id = artifact
                    .get("artifact_id")
                    .and_then(Value::as_str)
                    .filter(|id| larm_domain::bridge::is_uuid_v7(id))
                    .ok_or_else(|| invalid("download_job_artifact_identity_invalid"))?;
                Ok((id.to_owned(), artifact.clone()))
            })
            .collect::<Result<_, LarmError>>()?;
        if selected.is_empty()
            || selected.len() != selected_artifact_count
            || selected.len() != artifacts.len()
        {
            return Err(invalid("download_completion_artifact_set_mismatch").with_resource(job_id));
        }
        let unique: BTreeSet<&str> = artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect();
        if unique.len() != artifacts.len() || unique.len() != selected.len() {
            return Err(invalid("download_completion_artifact_set_mismatch").with_resource(job_id));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let plan_payload: Option<(String, String)> = sqlx::query_as(
            "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='download.plan' AND plan_hash=?",
        )
        .bind(&plan_id)
        .bind(&job.plan_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo journal de descarga", &e))?;
        let Some((plan_phase, plan_payload)) = plan_payload else {
            return Err(invalid("download_plan_journal_missing").with_resource(plan_id));
        };
        let plan_payload: Value = serde_json::from_str(&plan_payload)
            .map_err(|e| db_error("payload del plan de descarga", &e))?;
        if plan_phase != "QUEUED"
            || plan_payload
                .pointer("/execution/job_id")
                .and_then(Value::as_str)
                != Some(job_id)
        {
            return Err(invalid("download_plan_job_binding_mismatch").with_resource(plan_id));
        }
        let sealed_record: larm_domain::download::DownloadPlanRecord = serde_json::from_value(
            plan_payload
                .get("record")
                .cloned()
                .ok_or_else(|| invalid("download_plan_journal_missing"))?,
        )
        .map_err(|e| db_error("registro sellado del plan", &e))?;
        let acquisition: Option<larm_domain::acquisition::AcquisitionPlanBinding> =
            serde_json::from_value(
                job.metadata
                    .get("acquisition")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|e| db_error("vínculo de adquisición del job", &e))?;
        if acquisition != sealed_record.response.acquisition {
            return Err(invalid("download_acquisition_snapshot_mismatch"));
        }
        let acquisition_guard =
            crate::acquisition::validate_download_in(&mut tx, &sealed_record, &ctx, qualification)
                .await?;
        let sealed_candidates = &sealed_record.response.source_candidates;
        let distribution: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT r.revision_key,d.source_id,d.license_id FROM model_variant v JOIN model_revision r ON r.id=v.revision_id JOIN model_distribution d ON d.id=r.distribution_id WHERE v.id=?",
        )
        .bind(&job.variant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("resolviendo procedencia de descarga", &e))?;
        let Some((revision_key, distribution_source_id, license_id)) = distribution else {
            return Err(
                invalid("download_variant_provenance_missing").with_resource(&job.variant_id)
            );
        };
        if !sealed_candidates.is_empty()
            && (sealed_record.response.source_id.as_deref()
                != Some(distribution_source_id.as_str())
                || sealed_record.revision_key != revision_key)
        {
            return Err(invalid("download_source_revision_mismatch").with_resource(&job.variant_id));
        }
        if sealed_candidates.is_empty() && distribution_source_id != source_id {
            return Err(invalid("download_source_revision_mismatch").with_resource(&job.variant_id));
        }
        let hard_budget = job
            .metadata
            .get("hard_budget_bytes")
            .and_then(Value::as_str)
            .map(str::parse::<u64>)
            .transpose()
            .map_err(|_| invalid("download_hard_budget_invalid"))?;
        let mut total_bytes = 0_u64;
        let mut locations = Vec::with_capacity(artifacts.len());
        let mut provenance = Vec::with_capacity(artifacts.len());
        let mut result_artifacts = Vec::with_capacity(artifacts.len());
        for downloaded in artifacts {
            let Some(snapshot) = selected.get(&downloaded.artifact_id) else {
                return Err(invalid("download_completion_artifact_not_selected")
                    .with_resource(&downloaded.artifact_id));
            };
            let relative_path =
                larm_domain::artifacts::normalize_relative(&downloaded.relative_path)?;
            let expected_blob_path = format!(
                "blobs/sha256/{}/{}",
                downloaded.sha256.get(..2).unwrap_or_default(),
                downloaded.sha256
            );
            if relative_path != expected_blob_path
                || !larm_domain::adapter_updates::is_sha256(&downloaded.sha256)
                || DecimalU64::new(downloaded.size_bytes).is_none()
            {
                return Err(invalid("download_completion_artifact_invalid")
                    .with_resource(&downloaded.artifact_id));
            }
            let expected_sha = snapshot.get("sha256").and_then(Value::as_str);
            if !larm_domain::bridge::is_uuid_v7(&downloaded.source_id)
                || downloaded.revision_key.trim().is_empty()
                || downloaded.revision_key.len() > 4096
            {
                return Err(invalid("download_completion_source_identity_invalid")
                    .with_resource(&downloaded.artifact_id));
            }
            let from_primary = downloaded.source_id == distribution_source_id
                && downloaded.revision_key == revision_key;
            if from_primary {
                // The source_candidates array contains mirrors only; the primary
                // distribution is authorized independently by the sealed plan.
            } else if sealed_candidates.is_empty() {
                if downloaded.source_id != source_id || downloaded.revision_key != revision_key {
                    return Err(invalid("download_source_revision_mismatch")
                        .with_resource(&downloaded.artifact_id));
                }
            } else {
                let candidate = sealed_candidates
                    .iter()
                    .find(|candidate| {
                        candidate.source_id == downloaded.source_id
                            && candidate.revision_key == downloaded.revision_key
                    })
                    .ok_or_else(|| {
                        invalid("download_source_revision_mismatch")
                            .with_resource(&downloaded.artifact_id)
                    })?;
                let mapping = candidate
                    .artifacts
                    .iter()
                    .find(|mapping| mapping.target_artifact_id == downloaded.artifact_id)
                    .ok_or_else(|| {
                        invalid("download_source_artifact_mapping_mismatch")
                            .with_resource(&downloaded.artifact_id)
                    })?;
                if mapping.relative_path
                    != snapshot
                        .get("relative_path")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    || mapping.sha256 != downloaded.sha256
                    || mapping.size_bytes.get() != downloaded.size_bytes
                {
                    return Err(invalid("download_source_artifact_mapping_mismatch")
                        .with_resource(&downloaded.artifact_id));
                }
            }
            let size_estimate: ArtifactSizeEstimate = serde_json::from_value(
                snapshot
                    .get("size_estimate")
                    .cloned()
                    .ok_or_else(|| invalid("download_size_estimate_missing"))?,
            )
            .map_err(|e| db_error("size_estimate del plan", &e))?;
            size_estimate.validate()?;
            if size_estimate.confidence == ArtifactSizeConfidence::Unknown && hard_budget.is_none()
            {
                return Err(invalid("download_unknown_size_without_hard_budget")
                    .with_resource(&downloaded.artifact_id));
            }
            let trust_matches = match (expected_sha, downloaded.trust) {
                (Some(expected), Trust::Verified) => expected == downloaded.sha256,
                (None, Trust::Declared) => true,
                _ => false,
            };
            let size_matches = match size_estimate.confidence {
                ArtifactSizeConfidence::Exact => size_estimate
                    .bytes
                    .is_some_and(|expected| expected.get() == downloaded.size_bytes),
                ArtifactSizeConfidence::High
                | ArtifactSizeConfidence::Estimated
                | ArtifactSizeConfidence::Unknown => size_estimate
                    .upper_bound_bytes
                    .is_none_or(|upper| downloaded.size_bytes <= upper.get()),
            };
            if !trust_matches || !size_matches {
                return Err(LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El artefacto descargado no coincide con el plan confirmado",
                    if trust_matches {
                        "download_size_outside_approved_bound"
                    } else {
                        "download_hash_or_trust_mismatch"
                    },
                )
                .with_resource(&downloaded.artifact_id));
            }
            total_bytes = total_bytes
                .checked_add(downloaded.size_bytes)
                .filter(|total| *total <= i64::MAX as u64)
                .ok_or_else(|| invalid("download_total_size_overflow"))?;
            let Some(expected_row) = sqlx::query(
                "SELECT filename,required,sha256,size_estimate,actual_size FROM model_artifact WHERE id=? AND variant_id=?",
            )
            .bind(&downloaded.artifact_id)
            .bind(&job.variant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("validando artefacto descargado", &e))? else {
                return Err(invalid("download_artifact_catalog_mismatch")
                    .with_resource(&downloaded.artifact_id));
            };
            let filename: String = expected_row
                .try_get("filename")
                .map_err(|e| db_error("filename", &e))?;
            let required: i64 = expected_row
                .try_get("required")
                .map_err(|e| db_error("required", &e))?;
            let catalog_sha: Option<String> = expected_row
                .try_get("sha256")
                .map_err(|e| db_error("sha256", &e))?;
            let catalog_estimate: String = expected_row
                .try_get("size_estimate")
                .map_err(|e| db_error("size_estimate", &e))?;
            let catalog_estimate: ArtifactSizeEstimate = serde_json::from_str(&catalog_estimate)
                .map_err(|e| db_error("size_estimate", &e))?;
            if snapshot.get("relative_path").and_then(Value::as_str) != Some(filename.as_str())
                || snapshot.get("required").and_then(Value::as_bool) != Some(required != 0)
                || snapshot.get("sha256").and_then(Value::as_str) != catalog_sha.as_deref()
                || size_estimate != catalog_estimate
            {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "El manifiesto del artefacto cambió tras confirmar la descarga",
                    "download_artifact_snapshot_stale",
                )
                .with_resource(&downloaded.artifact_id));
            }
            let previous_actual_size: Option<i64> = expected_row
                .try_get("actual_size")
                .map_err(|e| db_error("actual_size", &e))?;
            let actual_size =
                i64::try_from(downloaded.size_bytes).map_err(|e| db_error("actual_size", &e))?;
            if previous_actual_size.is_some_and(|previous| previous != actual_size) {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "El tamaño descargado contradice el catálogo existente",
                    "download_actual_size_conflict",
                )
                .with_resource(&downloaded.artifact_id));
            }
            if previous_actual_size.is_none() {
                let changed = sqlx::query(
                    "UPDATE model_artifact SET actual_size=?,updated_at=?,row_version=row_version+1 WHERE id=? AND variant_id=? AND actual_size IS NULL",
                )
                .bind(actual_size)
                .bind(&now)
                .bind(&downloaded.artifact_id)
                .bind(&job.variant_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("actualizando tamaño descargado", &e))?;
                if changed.rows_affected() != 1 {
                    return Err(invalid("download_actual_size_conflict")
                        .with_resource(&downloaded.artifact_id));
                }
                let version: i64 =
                    sqlx::query_scalar("SELECT row_version FROM model_artifact WHERE id=?")
                        .bind(&downloaded.artifact_id)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| db_error("row_version de artefacto", &e))?;
                append(
                    &mut tx,
                    &NewEvent {
                        event_type: "model.state_changed",
                        resource_type: "model_artifact",
                        resource_id: &downloaded.artifact_id,
                        resource_version: version,
                        data: serde_json::json!({
                            "action": "downloaded",
                            "actual_size": downloaded.size_bytes.to_string(),
                        }),
                    },
                    &ctx,
                )
                .await?;
            }
            locations.push(ArtifactLocation {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: now.clone(),
                updated_at: now.clone(),
                row_version: 1,
                metadata: serde_json::json!({
                    "download_job_id": job.id,
                    "acquisition_mode": "DOWNLOAD",
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
                artifact_id: downloaded.artifact_id.clone(),
                storage_location_id: job.storage_location_id.clone(),
                relative_path,
                file_size: DecimalU64::new(downloaded.size_bytes),
                sha256: Some(downloaded.sha256.clone()),
                state: ArtifactState::Online,
                verified_at: Some(now.clone()),
                ownership: Ownership::LarmManaged,
            });
            provenance.push(ArtifactProvenance {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: now.clone(),
                updated_at: now.clone(),
                row_version: 1,
                metadata: serde_json::json!({
                    "downloaded_at": now,
                    "download_job_id": job.id,
                    "source_id": downloaded.source_id,
                    "revision_key": downloaded.revision_key,
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
                artifact_id: downloaded.artifact_id.clone(),
                source_id: Some(downloaded.source_id.clone()),
                revision_key: Some(downloaded.revision_key.clone()),
                parents: Vec::new(),
                tool_id: None,
                tool_version: None,
                parameters: Default::default(),
                license_id: license_id.clone(),
                license_hash: None,
                trust: downloaded.trust,
                actor_id: ctx.actor_id.clone(),
                evidence_refs: vec![
                    format!("download_job:{job_id}"),
                    format!("sha256:{}", downloaded.sha256),
                ],
            });
            result_artifacts.push(serde_json::json!({
                "artifact_id": downloaded.artifact_id,
                "size_bytes": downloaded.size_bytes.to_string(),
                "sha256": downloaded.sha256,
                "trust": downloaded.trust,
                "source_id": downloaded.source_id,
                "revision_key": downloaded.revision_key,
            }));
        }
        if hard_budget.is_some_and(|budget| total_bytes > budget) {
            return Err(LarmError::new(
                ErrorCode::StorageInsufficient,
                "La descarga superó el presupuesto confirmado",
                "download_hard_budget_exceeded",
            )
            .with_resource(job_id));
        }
        crate::download_bundle::reconcile(&mut tx, &sealed_record, artifacts, job_id, &ctx).await?;
        crate::artifacts::insert_locations_in(&mut tx, &locations, &ctx).await?;
        for item in &provenance {
            crate::models::insert_provenance_in(&mut tx, item, &ctx).await?;
        }
        let result = serde_json::json!({
            "artifacts": result_artifacts,
            "completed_at": now,
            "total_bytes": total_bytes.to_string(),
        });
        job.metadata.insert("result".into(), result.clone());
        job.state = DownloadState::Completed;
        job.error = None;
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now.clone();
        job.progress
            .insert("phase".into(), Value::String("COMPLETED".into()));
        job.progress.insert(
            "completed_bytes".into(),
            Value::String(total_bytes.to_string()),
        );
        job.progress
            .insert("completed_artifacts".into(), Value::from(artifacts.len()));
        let changed = sqlx::query(
            "UPDATE download_job SET state='COMPLETED',metadata=?,progress=?,error=NULL,row_version=row_version+1,updated_at=? WHERE id=? AND state='VERIFYING'",
        )
        .bind(serde_json::to_string(&job.metadata).map_err(|e| db_error("metadata de descarga", &e))?)
        .bind(serde_json::to_string(&job.progress).map_err(|e| db_error("progreso de descarga", &e))?)
        .bind(&now)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("completando job de descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_job_completion_conflict").with_resource(job_id));
        }
        let journal_changed = sqlx::query(
            "UPDATE operation_journal SET phase='COMPLETED',updated_at=?,payload=json_set(payload,'$.execution.phase','COMPLETED','$.execution.completed_at',?,'$.execution.result',json(?)) WHERE id=? AND kind='download.plan' AND phase='QUEUED' AND plan_hash=? AND json_extract(payload,'$.execution.job_id')=?",
        )
        .bind(&now)
        .bind(&now)
        .bind(result.to_string())
        .bind(&plan_id)
        .bind(&job.plan_hash)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("completando journal de descarga", &e))?;
        if journal_changed.rows_affected() != 1 {
            return Err(invalid("download_plan_completion_conflict").with_resource(plan_id));
        }
        sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reserva de descarga completada", &e))?;
        crate::download_bundle::release(&mut tx, &job).await?;
        emit_state(&mut tx, &job, &ctx, "completed").await?;
        if let Some(guard) = &acquisition_guard {
            guard.recheck().await?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT descarga completada", &e))?;
        Ok(())
    }

    /// Registra un failover de mirror y reinicia el checkpoint del artefacto
    /// dentro de la misma transacción que escribe el evento y el outbox.
    pub async fn record_mirror_failure(
        &self,
        job_id: &str,
        artifact_id: &str,
        failed_source_id: &str,
        next_source_id: &str,
        next_revision_key: &str,
        reason_code: &str,
    ) -> Result<(), LarmError> {
        if !larm_domain::bridge::is_uuid_v7(job_id)
            || !larm_domain::bridge::is_uuid_v7(artifact_id)
            || !larm_domain::bridge::is_uuid_v7(failed_source_id)
            || !larm_domain::bridge::is_uuid_v7(next_source_id)
            || next_revision_key.trim().is_empty()
            || next_revision_key.len() > 4096
            || !matches!(
                reason_code,
                "timeout" | "io_error" | "http_408" | "http_429" | "http_5xx"
            )
        {
            return Err(invalid("download_mirror_failure_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN failover de mirror", &e))?;
        lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Descarga desconocida",
                "download_job_not_found",
            )
        })?;
        if job.state != DownloadState::Running {
            return Err(invalid("download_mirror_failure_job_not_running").with_resource(job_id));
        }
        let ctx = actor_context(&job)?;
        let plan_id = job
            .metadata
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("download_job_plan_missing"))?;
        let payload: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM operation_journal WHERE id=? AND kind='download.plan' AND phase='QUEUED' AND plan_hash=? AND json_extract(payload,'$.execution.job_id')=?",
        )
        .bind(plan_id)
        .bind(&job.plan_hash)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo plan de failover", &e))?;
        let payload: Value = payload
            .ok_or_else(|| invalid("download_plan_job_binding_mismatch"))
            .and_then(|value| {
                serde_json::from_str(&value).map_err(|error| db_error("payload del plan", &error))
            })?;
        let record: larm_domain::download::DownloadPlanRecord = serde_json::from_value(
            payload
                .get("record")
                .cloned()
                .ok_or_else(|| invalid("download_plan_journal_missing"))?,
        )
        .map_err(|error| db_error("registro del plan de failover", &error))?;
        let candidates = record.response.source_candidates;
        if candidates.is_empty() {
            return Err(invalid("download_mirror_failure_legacy_plan"));
        }
        let primary_source_id = record
            .response
            .source_id
            .as_deref()
            .ok_or_else(|| invalid("download_job_source_missing"))?;
        let primary_revision_key = &record.revision_key;
        let failed_is_primary = failed_source_id == primary_source_id;
        let failed_position = if failed_is_primary {
            0
        } else {
            candidates
                .iter()
                .position(|candidate| candidate.source_id == failed_source_id)
                .map(|index| index + 1)
                .ok_or_else(|| invalid("download_mirror_source_not_in_plan"))?
        };
        let next = if failed_position == 0 {
            candidates.first()
        } else {
            candidates.get(failed_position)
        }
        .filter(|candidate| candidate.source_id == next_source_id)
        .ok_or_else(|| invalid("download_mirror_next_source_not_in_plan"))?;
        if next.revision_key != next_revision_key
            || (failed_is_primary && primary_revision_key.trim().is_empty())
            || (!failed_is_primary
                && !candidates[failed_position - 1]
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.target_artifact_id == artifact_id))
            || !next
                .artifacts
                .iter()
                .any(|artifact| artifact.target_artifact_id == artifact_id)
        {
            return Err(invalid("download_mirror_artifact_not_in_plan").with_resource(artifact_id));
        }
        let approved = job
            .metadata
            .get("artifacts")
            .and_then(Value::as_array)
            .is_some_and(|artifacts| {
                artifacts.iter().any(|artifact| {
                    artifact.get("artifact_id").and_then(Value::as_str) == Some(artifact_id)
                        && artifact.get("selected").and_then(Value::as_bool) == Some(true)
                })
            });
        if !approved {
            return Err(
                invalid("download_checkpoint_artifact_not_selected").with_resource(artifact_id)
            );
        }
        let failed_revision_key = if failed_is_primary {
            primary_revision_key
        } else {
            &candidates[failed_position - 1].revision_key
        };
        let checkpoint_source = job
            .progress
            .get("artifacts")
            .and_then(Value::as_object)
            .and_then(|artifacts| artifacts.get(artifact_id))
            .and_then(Value::as_object)
            .and_then(|checkpoint| {
                Some((
                    checkpoint.get("source_id")?.as_str()?,
                    checkpoint.get("revision_key")?.as_str()?,
                ))
            });
        if checkpoint_source.is_some_and(|(source, revision)| {
            source != failed_source_id || revision != failed_revision_key
        }) {
            return Err(invalid("download_mirror_failed_source_checkpoint_mismatch")
                .with_resource(artifact_id));
        }
        let artifacts = job
            .progress
            .entry("artifacts")
            .or_insert_with(|| Value::Object(Default::default()));
        let Some(artifacts) = artifacts.as_object_mut() else {
            return Err(invalid("download_progress_invalid"));
        };
        artifacts.insert(
            artifact_id.into(),
            serde_json::json!({
                "durable_offset": "0",
                "resume_validator": null,
                "source_id": next.source_id,
                "revision_key": next.revision_key,
            }),
        );
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let next_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        let changed = sqlx::query(
            "UPDATE download_job SET progress=?,updated_at=?,row_version=row_version+1 WHERE id=? AND state='RUNNING' AND row_version=?",
        )
        .bind(serde_json::to_string(&job.progress).map_err(|e| db_error("serializando failover", &e))?)
        .bind(&now)
        .bind(job_id)
        .bind(job.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("reiniciando checkpoint para failover", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_mirror_failure_conflict").with_resource(job_id));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "download.mirror.failed",
                resource_type: "download_job",
                resource_id: job_id,
                resource_version: next_version,
                data: serde_json::json!({
                    "artifact_id": artifact_id,
                    "failed_source_id": failed_source_id,
                    "next_source_id": next_source_id,
                    "reason_code": reason_code,
                }),
            },
            &ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT failover de mirror", &e))
    }

    /// Reconciliación al arrancar: RUNNING/VERIFYING vuelven a QUEUED usando
    /// checkpoints fsync ya persistidos; PAUSING queda PAUSED y no se reinicia
    /// sin una orden posterior del operador.
    pub async fn recover_interrupted(&self) -> Result<u64, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN recovery descargas", &e))?;
        lock(&mut tx).await?;
        let rows = sqlx::query(
            "SELECT id FROM download_job WHERE state IN ('RUNNING','VERIFYING','PAUSING') OR (state='CANCELLING' AND COALESCE(json_extract(progress,'$.cancel_dispatch'),'')<>'QUEUED') ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("buscando descargas interrumpidas", &e))?;
        let mut recovered = 0_u64;
        for row in rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| db_error("id de descarga", &e))?;
            let Some(mut job) = load(&mut tx, &id).await? else {
                return Err(invalid("download_job_missing_during_recovery").with_resource(&id));
            };
            let next = match job.state {
                DownloadState::Pausing => "PAUSED",
                DownloadState::Cancelling => "CANCELLING",
                _ => "QUEUED",
            };
            let mut progress = serde_json::to_value(&job.progress)
                .map_err(|e| db_error("serializando progreso de descarga", &e))?;
            progress["phase"] = Value::String(next.into());
            progress["recovered_after_restart"] = Value::Bool(true);
            if job.state == DownloadState::Cancelling {
                progress["cancel_dispatch"] = Value::String("QUEUED".into());
            }
            let changed = sqlx::query(
                "UPDATE download_job SET state=?,progress=?,row_version=row_version+1,updated_at=? \
                 WHERE id=? AND state IN ('RUNNING','VERIFYING','PAUSING','CANCELLING')",
            )
            .bind(next)
            .bind(progress.to_string())
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("recuperando descarga interrumpida", &e))?;
            if changed.rows_affected() != 1 {
                return Err(invalid("download_job_recovery_conflict").with_resource(&id));
            }
            job.state = serde_json::from_value(Value::String(next.into()))
                .map_err(|e| db_error("estado recuperado de descarga", &e))?;
            job.row_version = job
                .row_version
                .checked_add(1)
                .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
            job.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
            job.progress = serde_json::from_value(progress)
                .map_err(|e| db_error("progreso recuperado de descarga", &e))?;
            let ctx = actor_context(&job)?;
            emit_state(&mut tx, &job, &ctx, "recovered_after_restart").await?;
            recovered += 1;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT recovery descargas", &e))?;
        Ok(recovered)
    }
}

#[async_trait]
impl DownloadQueueService for SqlDownloadJobs {
    async fn control(
        &self,
        job_id: &str,
        command: larm_domain::download::DownloadCommand,
        request: &larm_domain::download::DownloadStop,
        ctx: &MutationContext,
        idempotency: &larm_domain::download::DownloadControlIdempotency,
    ) -> Result<larm_domain::bridge::JobReceipt, ApiError> {
        self.request_control(job_id, command, request, ctx, idempotency)
            .await
    }

    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<DownloadJob>, ApiError> {
        self.list_owned(after, limit, &ctx.actor_id).await
    }

    async fn update_priority(
        &self,
        job_id: &str,
        expected_row_version: i64,
        priority: larm_domain::download::DownloadPriority,
        ctx: &MutationContext,
    ) -> Result<DownloadJob, ApiError> {
        SqlDownloadJobs::update_priority(self, job_id, expected_row_version, priority, ctx).await
    }
}

#[async_trait]
impl JobSource for SqlDownloadJobs {
    async fn job(&self, principal: &Principal, job_id: &str) -> Result<Option<JobState>, ApiError> {
        if !larm_domain::bridge::is_uuid_v7(job_id) {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "Identificador de job no válido",
                "job_id_invalid",
            ));
        }
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("conexión de job", &e))?;
        let Some(job) = load(&mut connection, job_id).await? else {
            return Ok(None);
        };
        let owner = job
            .metadata
            .get("actor_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if owner != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        let mut progress = job.progress.clone();
        progress.insert("download_state".into(), serde_json::json!(job.state));
        progress.insert("attempt".into(), serde_json::json!(job.attempt));
        let error = job
            .error
            .map(|fields| serde_json::from_value(Value::Object(fields)))
            .transpose()
            .map_err(|e| ApiError::from(db_error("error de job de descarga", &e)))?;
        Ok(Some(JobState {
            job_id: job.id,
            state: phase_for(job.state),
            resource_type: "download_job".into(),
            resource_id: Some(job.variant_id),
            progress,
            result: job
                .metadata
                .get("result")
                .filter(|value| !value.is_null())
                .cloned(),
            error,
            evidence_refs: job
                .metadata
                .get("evidence_refs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| ApiError::from(db_error("evidence_refs de descarga", &e)))?
                .unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use crate::testing::temp_migrated;
    use larm_domain::auth::{ApiScope, PrincipalKind};

    const VARIANT: &str = "0192f000-0000-7000-8000-00000000e101";
    const LOCATION: &str = "0192f000-0000-7000-8000-00000000e102";
    const OWNER: &str = "0192f000-0000-7000-8000-00000000e103";
    const OTHER: &str = "0192f000-0000-7000-8000-00000000e104";
    const SOURCE: &str = "0192f000-0000-7000-8000-00000000e106";
    const PLAN: &str = "0192f000-0000-7000-8000-00000000e107";
    const ARTIFACT: &str = "0192f000-0000-7000-8000-00000000e123";
    const EXPECTED_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn exact_estimate() -> Value {
        serde_json::json!({
            "confidence": "EXACT",
            "bytes": "4",
            "lower_bound_bytes": "4",
            "upper_bound_bytes": "4",
            "source": "MANIFEST",
            "evidence_refs": ["fixture:manifest"],
            "reason": null,
        })
    }

    async fn setup() -> (tempfile::TempDir, SqlDownloadJobs) {
        let (dir, db) = temp_migrated().await.unwrap();
        let pool = &db.pool;
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,'t','t',1,'{}','Source','msa','https://example.invalid',0,1,'NONE','{}')")
            .bind(SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm',?,'ns','model')")
            .bind(SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1','hash','CURRENT')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','Destination','D:\\models','D:\\models','NTFS','LOCAL',1000000,1000000,0,0,0,'ONLINE','volume-test','SSD')")
            .bind(LOCATION).execute(pool).await.unwrap();
        (dir, SqlDownloadJobs { pool: db.pool })
    }

    async fn insert_job(
        repo: &SqlDownloadJobs,
        id: &str,
        priority: &str,
        state: &str,
        created_at: &str,
        owner: &str,
    ) {
        let metadata = serde_json::json!({
            "actor_id": owner,
            "correlation_id": "0192f000-0000-7000-8000-00000000e105",
            "plan_id": PLAN,
            "source_id": SOURCE,
            "hard_budget_bytes": null,
            "artifacts": [{
                "artifact_id": ARTIFACT,
                "relative_path": "weights.bin",
                "required": true,
                "selected": true,
                "size_estimate": exact_estimate(),
                "sha256": EXPECTED_SHA,
            }],
        });
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,1,?,? ,?,?,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',?,0,'{}',NULL)")
            .bind(id).bind(created_at).bind(created_at).bind(metadata.to_string()).bind(priority).bind(VARIANT).bind(LOCATION).bind(state)
            .execute(&repo.pool).await.unwrap();
    }

    async fn prepare_completion(repo: &SqlDownloadJobs, id: &str) {
        insert_job(
            repo,
            id,
            "NORMAL",
            "VERIFYING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,expected_size,actual_size,sha256,source_url,required,size_estimate) VALUES(?,'t','t',1,'{}',?,'weights.bin','GGUF',NULL,4,NULL,?,NULL,1,?)")
            .bind(ARTIFACT)
            .bind(VARIANT)
            .bind(EXPECTED_SHA)
            .bind(exact_estimate().to_string())
            .execute(&repo.pool)
            .await
            .unwrap();
        let artifact_snapshot = serde_json::json!({
            "artifact_id": ARTIFACT,
            "relative_path": "weights.bin",
            "required": true,
            "selected": true,
            "size_estimate": exact_estimate(),
            "sha256": EXPECTED_SHA,
        });
        let record = serde_json::json!({
            "request": {
                "variant_id": VARIANT,
                "storage_location_id": LOCATION,
                "hard_budget_bytes": null,
                "source_id": SOURCE,
                "priority": "NORMAL",
                "optional_artifact_ids": [],
            },
            "response": {
                "preview_id": PLAN,
                "preview_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "variant_id": VARIANT,
                "optional_artifact_ids": [],
                "artifact_estimates": [artifact_snapshot],
                "destination_options": [{"storage_location_id": LOCATION, "selected": true, "eligible": true, "reason": "fits", "storage_estimate": null}],
                "selected_storage_location_id": LOCATION,
                "plan_id": PLAN,
                "plan_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "storage_estimate": null,
                "source_id": SOURCE,
                "source_candidates": [],
                "hard_budget_bytes": null,
                "priority": "NORMAL",
                "expires_at": "2030-01-01T00:05:00.000Z",
                "operator_confirmation_required": true,
            },
            "revision_id": "mr",
            "revision_key": "r1",
            "manifest_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "storage_observed_at": "2030-01-01T00:00:00.000Z",
        });
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',?,'t')")
            .bind(PLAN)
            .bind(serde_json::json!({"execution":{"job_id":id}, "record": record}).to_string())
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('completion-reservation','volume-test',?,100,100,NULL)")
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();
    }

    fn verified_artifact() -> DownloadedArtifact {
        DownloadedArtifact {
            artifact_id: ARTIFACT.into(),
            relative_path: format!("blobs/sha256/bb/{EXPECTED_SHA}"),
            size_bytes: 4,
            sha256: EXPECTED_SHA.into(),
            trust: Trust::Verified,
            source_id: SOURCE.into(),
            revision_key: "r1".into(),
        }
    }

    fn mirror_candidate() -> Value {
        serde_json::json!({
            "source_id": "0192f000-0000-7000-8000-00000000e108",
            "variant_id": "0192f000-0000-7000-8000-00000000e109",
            "revision_id": "0192f000-0000-7000-8000-00000000e10a",
            "revision_key": "mirror-r1",
            "manifest_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "source_priority": 5,
            "confidence": "EXACT",
            "authenticated": false,
            "https": true,
            "artifacts": [{
                "target_artifact_id": ARTIFACT,
                "source_artifact_id": "0192f000-0000-7000-8000-00000000e10b",
                "relative_path": "weights.bin",
                "sha256": EXPECTED_SHA,
                "size_bytes": "4",
            }],
        })
    }

    async fn set_plan_mirrors(repo: &SqlDownloadJobs, mirrors: Value) {
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=?")
                .bind(PLAN)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        let mut payload: Value = serde_json::from_str(&payload).unwrap();
        payload["record"]["response"]["source_candidates"] = mirrors;
        sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
            .bind(payload.to_string())
            .bind(PLAN)
            .execute(&repo.pool)
            .await
            .unwrap();
    }

    fn principal(id: &str, admin: bool) -> Principal {
        let scopes = if admin {
            vec![ApiScope::Read, ApiScope::Admin]
        } else {
            vec![ApiScope::Read]
        };
        Principal::new(id, "download-job-test", PrincipalKind::Operator, scopes, []).unwrap()
    }

    #[tokio::test]
    async fn reclama_por_prioridad_fifo_incrementa_attempt_y_expone_job_al_actor() {
        let (_dir, repo) = setup().await;
        let first_high = "0192f000-0000-7000-8000-00000000e111";
        let second_high = "0192f000-0000-7000-8000-00000000e112";
        let normal = "0192f000-0000-7000-8000-00000000e113";
        insert_job(
            &repo,
            normal,
            "NORMAL",
            "QUEUED",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            second_high,
            "HIGH",
            "QUEUED",
            "2026-10-23T00:00:02.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            first_high,
            "HIGH",
            "QUEUED",
            "2026-10-23T00:00:01.000Z",
            OWNER,
        )
        .await;

        for expected in [first_high, second_high, normal] {
            let claim = repo.claim_next().await.unwrap().unwrap();
            assert_eq!(claim.job.id, expected);
            assert_eq!(
                claim.job.state,
                larm_domain::download::DownloadState::Running
            );
            assert_eq!(claim.job.attempt, 1);
            let visible = repo
                .job(&principal(OWNER, false), expected)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(visible.state, JobPhase::Running);
            assert_eq!(
                visible.progress["download_state"],
                serde_json::json!("RUNNING")
            );
        }
        assert!(repo.claim_next().await.unwrap().is_none());
        assert!(repo
            .job(&principal(OTHER, false), first_high)
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .job(&principal(OTHER, true), first_high)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn claim_respects_active_download_limit_per_source() {
        let (_dir, repo) = setup().await;
        let active = "0192f000-0000-7000-8000-00000000e121";
        let blocked = "0192f000-0000-7000-8000-00000000e122";
        let other_source = "0192f000-0000-7000-8000-00000000e123";
        insert_job(
            &repo,
            active,
            "NORMAL",
            "RUNNING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            blocked,
            "HIGH",
            "QUEUED",
            "2026-10-23T00:00:01.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            other_source,
            "NORMAL",
            "QUEUED",
            "2026-10-23T00:00:02.000Z",
            OWNER,
        )
        .await;
        sqlx::query("UPDATE download_job SET metadata=json_set(metadata,'$.source_id','other-source') WHERE id=?")
            .bind(other_source)
            .execute(&repo.pool)
            .await
            .unwrap();

        let claimed = repo.claim_next_with_source_limit(1).await.unwrap().unwrap();
        assert_eq!(claimed.job.id, other_source);
        assert!(repo
            .claim_next_with_source_limit(1)
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .claim_next_with_source_limit(0)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn actualiza_prioridad_con_cas_emite_evento_y_conserva_plan_y_reserva() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e114";
        insert_job(
            &repo,
            id,
            "NORMAL",
            "QUEUED",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('priority-reservation','volume-test',?,100,100,NULL)")
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let ctx = MutationContext {
            actor_id: OWNER.into(),
            correlation_id: "0192f000-0000-7000-8000-00000000e105".into(),
        };
        let original_plan_hash: String =
            sqlx::query_scalar("SELECT plan_hash FROM download_job WHERE id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();

        let updated = repo
            .update_priority(id, 1, larm_domain::download::DownloadPriority::High, &ctx)
            .await
            .unwrap();
        assert_eq!(
            updated.priority,
            larm_domain::download::DownloadPriority::High
        );
        assert_eq!(updated.row_version, 2);

        let (priority, version, plan_hash): (String, i64, String) =
            sqlx::query_as("SELECT priority,row_version,plan_hash FROM download_job WHERE id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(priority, "HIGH");
        assert_eq!(version, 2);
        assert_eq!(plan_hash, original_plan_hash);
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 1);
        let event: String = sqlx::query_scalar(
            "SELECT e.payload FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.state_changed'",
        )
        .bind(id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        let event: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["action"], "priority_changed");
        assert_eq!(event["priority"], "HIGH");

        let stale = repo
            .update_priority(
                id,
                1,
                larm_domain::download::DownloadPriority::Background,
                &ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleRevision);
        assert_eq!(stale.details.reason, "stale_revision");

        let wrong_actor = MutationContext {
            actor_id: OTHER.into(),
            correlation_id: ctx.correlation_id.clone(),
        };
        let hidden = repo
            .update_priority(
                id,
                2,
                larm_domain::download::DownloadPriority::Background,
                &wrong_actor,
            )
            .await
            .unwrap_err();
        assert_eq!(hidden.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn prioridad_rechaza_verificacion_y_revierte_si_falla_el_evento() {
        let (_dir, repo) = setup().await;
        let verifying = "0192f000-0000-7000-8000-00000000e115";
        insert_job(
            &repo,
            verifying,
            "NORMAL",
            "VERIFYING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        let ctx = MutationContext {
            actor_id: OWNER.into(),
            correlation_id: "0192f000-0000-7000-8000-00000000e105".into(),
        };
        assert_eq!(
            repo.update_priority(
                verifying,
                1,
                larm_domain::download::DownloadPriority::High,
                &ctx,
            )
            .await
            .unwrap_err()
            .details
            .reason,
            "download_priority_state_not_allowed"
        );

        let queued = "0192f000-0000-7000-8000-00000000e116";
        insert_job(
            &repo,
            queued,
            "NORMAL",
            "QUEUED",
            "2026-10-23T00:00:01.000Z",
            OWNER,
        )
        .await;
        sqlx::query("CREATE TRIGGER reject_priority_event BEFORE INSERT ON event WHEN NEW.event_type='download.state_changed' BEGIN SELECT RAISE(ABORT,'outbox fault'); END")
            .execute(&repo.pool)
            .await
            .unwrap();
        assert!(repo
            .update_priority(
                queued,
                1,
                larm_domain::download::DownloadPriority::High,
                &ctx,
            )
            .await
            .is_err());
        let (priority, version): (String, i64) =
            sqlx::query_as("SELECT priority,row_version FROM download_job WHERE id=?")
                .bind(queued)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(priority, "NORMAL");
        assert_eq!(version, 1);
    }

    #[tokio::test]
    async fn recovery_reencola_trabajo_interrumpido_y_conserva_checkpoint() {
        let (_dir, repo) = setup().await;
        let running = "0192f000-0000-7000-8000-00000000e121";
        let pausing = "0192f000-0000-7000-8000-00000000e122";
        insert_job(
            &repo,
            running,
            "NORMAL",
            "RUNNING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            pausing,
            "NORMAL",
            "PAUSING",
            "2026-10-23T00:00:01.000Z",
            OWNER,
        )
        .await;
        let checkpoint = serde_json::json!({
            "artifacts": {
                "0192f000-0000-7000-8000-00000000e123": {
                    "durable_offset": "4",
                    "resume_validator": {"kind":"STRONG_ETAG","value":"\"v1\""}
                }
            }
        });
        sqlx::query("UPDATE download_job SET progress=? WHERE id=?")
            .bind(checkpoint.to_string())
            .bind(running)
            .execute(&repo.pool)
            .await
            .unwrap();

        assert_eq!(repo.recover_interrupted().await.unwrap(), 2);
        let claim = repo.claim_next().await.unwrap().unwrap();
        assert_eq!(claim.job.id, running);
        assert_eq!(claim.job.progress["recovered_after_restart"], true);
        let saved = crate::download_progress::SqlDownloadProgress {
            pool: repo.pool.clone(),
        }
        .load_checkpoint(running, "0192f000-0000-7000-8000-00000000e123")
        .await
        .unwrap();
        assert_eq!(saved.durable_offset.get(), 4);
        assert_eq!(
            repo.job(&principal(OWNER, false), pausing)
                .await
                .unwrap()
                .unwrap()
                .progress["download_state"],
            "PAUSED"
        );
    }

    #[tokio::test]
    async fn verifica_solo_un_job_en_ejecucion_y_emite_estado_durable() {
        let (_dir, repo) = setup().await;
        let running = "0192f000-0000-7000-8000-00000000e131";
        let queued = "0192f000-0000-7000-8000-00000000e132";
        insert_job(
            &repo,
            running,
            "NORMAL",
            "RUNNING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            queued,
            "NORMAL",
            "QUEUED",
            "2026-10-23T00:00:01.000Z",
            OWNER,
        )
        .await;

        let verifying = repo.begin_verification(running).await.unwrap();
        assert_eq!(verifying.state, DownloadState::Verifying);
        assert_eq!(verifying.progress["phase"], "VERIFYING");
        assert_eq!(verifying.row_version, 2);
        let visible = repo
            .job(&principal(OWNER, false), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.state, JobPhase::Running);
        assert_eq!(visible.progress["download_state"], "VERIFYING");
        assert!(repo.begin_verification(queued).await.is_err());

        let persisted: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(queued)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(persisted, "QUEUED");
    }

    #[tokio::test]
    async fn cierra_pausa_solo_despues_de_checkpoint_y_emite_estado() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e136";
        insert_job(
            &repo,
            id,
            "NORMAL",
            "PAUSING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        let checkpoint = serde_json::json!({
            "phase":"RUNNING",
            "artifacts": {
                ARTIFACT: {
                    "durable_offset":"4",
                    "resume_validator":{"kind":"STRONG_ETAG","value":"\"v1\""}
                }
            }
        });
        sqlx::query("UPDATE download_job SET progress=? WHERE id=?")
            .bind(checkpoint.to_string())
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();

        repo.finish_pause(id).await.unwrap();

        let job = repo
            .job(&principal(OWNER, false), id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.state, JobPhase::Running);
        assert_eq!(job.progress["download_state"], "PAUSED");
        assert_eq!(job.progress["artifacts"][ARTIFACT]["durable_offset"], "4");
        let row_version: i64 =
            sqlx::query_scalar("SELECT row_version FROM download_job WHERE id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(row_version, 2);
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.state_changed'",
        )
        .bind(id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
        assert!(repo.finish_pause(id).await.is_err());
    }

    #[tokio::test]
    async fn cierra_cancelacion_libera_reserva_y_cambia_journal_atomico() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e137";
        insert_job(
            &repo,
            id,
            "NORMAL",
            "CANCELLING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',?,'t')")
            .bind(PLAN)
            .bind(serde_json::json!({"execution":{"job_id":id}}).to_string())
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('cancel-reservation','volume-test',?,100,100,NULL)")
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();

        repo.finish_cancel(id).await.unwrap();

        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "CANCELLED");
        let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=?")
            .bind(PLAN)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(phase, "CANCELLED");
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
        assert!(repo.finish_cancel(id).await.is_err());
    }

    #[tokio::test]
    async fn fallo_sellado_cierra_journal_y_reserva_con_el_job() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e139";
        prepare_completion(&repo, id).await;
        let failure = LarmError::new(
            ErrorCode::IntegrityFailed,
            "Hash incorrecto",
            "download_artifact_integrity_failed",
        );
        repo.fail_download(id, &failure).await.unwrap();
        let (job_phase, journal_phase, execution_phase, reservations): (String,String,String,i64) = sqlx::query_as("SELECT d.state,j.phase,json_extract(j.payload,'$.execution.phase'),(SELECT count(*) FROM volume_reservation WHERE job_id=d.id) FROM download_job d JOIN operation_journal j ON j.id=json_extract(d.metadata,'$.plan_id') WHERE d.id=?")
            .bind(id).fetch_one(&repo.pool).await.unwrap();
        assert_eq!(
            (
                job_phase.as_str(),
                journal_phase.as_str(),
                execution_phase.as_str(),
                reservations
            ),
            ("FAILED", "FAILED", "FAILED", 0)
        );
    }

    #[tokio::test]
    async fn fallo_sellado_revierte_job_journal_y_reserva_si_falla_outbox_o_journal() {
        for trigger in [
            "CREATE TRIGGER reject_failure_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture fault'); END",
            "CREATE TRIGGER ignore_failure_journal BEFORE UPDATE ON operation_journal BEGIN SELECT RAISE(IGNORE); END",
            "UPDATE operation_journal SET payload=json_set(payload,'$.execution.job_id','another-job')",
        ] {
            let (_dir, repo) = setup().await;
            let id = "0192f000-0000-7000-8000-00000000e139";
            prepare_completion(&repo, id).await;
            sqlx::query(trigger).execute(&repo.pool).await.unwrap();
            let error = LarmError::new(ErrorCode::IntegrityFailed, "Hash incorrecto", "download_artifact_integrity_failed");
            assert!(repo.fail_download(id, &error).await.is_err());
            let (job_phase,journal_phase,reservations): (String,String,i64) = sqlx::query_as("SELECT d.state,j.phase,(SELECT count(*) FROM volume_reservation WHERE job_id=d.id) FROM download_job d JOIN operation_journal j ON j.id=json_extract(d.metadata,'$.plan_id') WHERE d.id=?")
                .bind(id).fetch_one(&repo.pool).await.unwrap();
            assert_eq!((job_phase.as_str(),journal_phase.as_str(),reservations), ("VERIFYING","QUEUED",1));
        }
    }

    #[tokio::test]
    async fn fallo_terminal_guarda_error_libera_reserva_y_emite_un_solo_cambio() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e133";
        insert_job(
            &repo,
            id,
            "NORMAL",
            "RUNNING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED',?,?, 't')")
            .bind(PLAN).bind("a".repeat(64)).bind(serde_json::json!({"execution":{"job_id":id,"phase":"QUEUED"}}).to_string())
            .execute(&repo.pool).await.unwrap();
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('reservation-test','volume-test',?,100,100,NULL)")
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();

        let failure = LarmError::new(
            ErrorCode::IntegrityFailed,
            "No se pudo verificar el archivo descargado",
            "artifact_checksum_mismatch",
        );
        repo.fail_download(id, &failure).await.unwrap();

        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "FAILED");
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
        let job = repo
            .job(&principal(OWNER, false), id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.state, JobPhase::Failed);
        assert_eq!(
            job.error.unwrap().details.reason,
            "artifact_checksum_mismatch"
        );
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.state_changed'",
        )
        .bind(id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
        assert!(repo.fail_download(id, &failure).await.is_err());
    }

    #[tokio::test]
    async fn fallo_de_outbox_revierte_estado_error_y_liberacion_de_reserva() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e134";
        insert_job(
            &repo,
            id,
            "NORMAL",
            "RUNNING",
            "2026-10-23T00:00:00.000Z",
            OWNER,
        )
        .await;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED',?,?, 't')")
            .bind(PLAN).bind("a".repeat(64)).bind(serde_json::json!({"execution":{"job_id":id,"phase":"QUEUED"}}).to_string())
            .execute(&repo.pool).await.unwrap();
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('reservation-rollback','volume-test',?,100,100,NULL)")
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER reject_download_event BEFORE INSERT ON event WHEN NEW.event_type='download.state_changed' BEGIN SELECT RAISE(ABORT,'outbox fault'); END")
            .execute(&repo.pool)
            .await
            .unwrap();

        let failure = LarmError::new(
            ErrorCode::IntegrityFailed,
            "No se pudo verificar el archivo descargado",
            "artifact_checksum_mismatch",
        );
        assert!(repo.fail_download(id, &failure).await.is_err());

        let (state, error): (String, Option<String>) =
            sqlx::query_as("SELECT state,error FROM download_job WHERE id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(state, "RUNNING");
        assert!(error.is_none());
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 1);
    }

    #[tokio::test]
    async fn completion_publica_catalogo_procedencia_journal_y_job_atomicos() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e135";
        prepare_completion(&repo, id).await;

        repo.complete_download(id, &[verified_artifact()])
            .await
            .unwrap();

        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "COMPLETED");
        let (phase, execution_phase): (String, String) = sqlx::query_as(
            "SELECT phase,json_extract(payload,'$.execution.phase') FROM operation_journal WHERE id=?",
        )
        .bind(PLAN)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(phase, "COMPLETED");
        assert_eq!(execution_phase, "COMPLETED");
        let actual_size: i64 =
            sqlx::query_scalar("SELECT actual_size FROM model_artifact WHERE id=?")
                .bind(ARTIFACT)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(actual_size, 4);
        let location: (String, String, String, i64) = sqlx::query_as(
            "SELECT relative_path,sha256,ownership,file_size FROM artifact_location WHERE artifact_id=?",
        )
        .bind(ARTIFACT)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(location.0, format!("blobs/sha256/bb/{EXPECTED_SHA}"));
        assert_eq!(location.1, EXPECTED_SHA);
        assert_eq!(location.2, "LARM_MANAGED");
        assert_eq!(location.3, 4);
        let provenance: (String, String, String, String) = sqlx::query_as(
            "SELECT source_id,revision_key,trust,actor_id FROM artifact_provenance WHERE artifact_id=?",
        )
        .bind(ARTIFACT)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(provenance.0, SOURCE);
        assert_eq!(provenance.1, "r1");
        assert_eq!(provenance.2, "VERIFIED");
        assert_eq!(provenance.3, OWNER);
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
        let visible = repo
            .job(&principal(OWNER, false), id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.state, JobPhase::Completed);
        assert_eq!(visible.progress["completed_bytes"], "4");
    }

    #[tokio::test]
    async fn completion_acepta_la_fuente_principal_ademas_de_mirrors_sellados() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e139";
        prepare_completion(&repo, id).await;
        set_plan_mirrors(&repo, serde_json::json!([mirror_candidate()])).await;

        repo.complete_download(id, &[verified_artifact()])
            .await
            .unwrap();

        let provenance: (String, String) = sqlx::query_as(
            "SELECT source_id,revision_key FROM artifact_provenance WHERE artifact_id=?",
        )
        .bind(ARTIFACT)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(provenance, (SOURCE.into(), "r1".into()));
    }

    #[tokio::test]
    async fn failover_reinicia_checkpoint_y_guarda_evento_con_outbox_atomicos() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e13a";
        prepare_completion(&repo, id).await;
        set_plan_mirrors(&repo, serde_json::json!([mirror_candidate()])).await;
        sqlx::query("UPDATE download_job SET state='RUNNING',progress=? WHERE id=?")
            .bind(
                serde_json::json!({"artifacts": {ARTIFACT: {
                    "durable_offset": "2",
                    "resume_validator": null,
                    "source_id": SOURCE,
                    "revision_key": "r1",
                }}})
                .to_string(),
            )
            .bind(id)
            .execute(&repo.pool)
            .await
            .unwrap();

        assert_eq!(
            repo.record_mirror_failure(
                id,
                ARTIFACT,
                SOURCE,
                "0192f000-0000-7000-8000-00000000e10c",
                "mirror-r1",
                "http_5xx",
            )
            .await
            .unwrap_err()
            .details
            .reason,
            "download_mirror_next_source_not_in_plan"
        );
        let events_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event WHERE resource_id=? AND event_type='download.mirror.failed'",
        )
        .bind(id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(events_before, 0);

        repo.record_mirror_failure(
            id,
            ARTIFACT,
            SOURCE,
            "0192f000-0000-7000-8000-00000000e108",
            "mirror-r1",
            "http_5xx",
        )
        .await
        .unwrap();

        let progress: String = sqlx::query_scalar("SELECT progress FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let progress: Value = serde_json::from_str(&progress).unwrap();
        assert_eq!(progress["artifacts"][ARTIFACT]["durable_offset"], "0");
        assert!(progress["artifacts"][ARTIFACT]["resume_validator"].is_null());
        assert_eq!(
            progress["artifacts"][ARTIFACT]["source_id"],
            "0192f000-0000-7000-8000-00000000e108"
        );
        let (payload, pending): (String, i64) = sqlx::query_as(
            "SELECT e.payload,CASE WHEN o.delivered_at IS NULL THEN 1 ELSE 0 END FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.mirror.failed'",
        )
        .bind(id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["reason_code"], "http_5xx");
        assert_eq!(pending, 1);
    }

    #[tokio::test]
    async fn fallo_de_outbox_al_completar_revierte_catalogo_procedencia_job_y_reserva() {
        let (_dir, repo) = setup().await;
        let id = "0192f000-0000-7000-8000-00000000e136";
        prepare_completion(&repo, id).await;
        sqlx::query("CREATE TRIGGER reject_download_completion BEFORE INSERT ON event WHEN NEW.event_type='download.state_changed' BEGIN SELECT RAISE(ABORT,'outbox fault'); END")
            .execute(&repo.pool)
            .await
            .unwrap();

        assert!(repo
            .complete_download(id, &[verified_artifact()])
            .await
            .is_err());

        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "VERIFYING");
        let actual_size: Option<i64> =
            sqlx::query_scalar("SELECT actual_size FROM model_artifact WHERE id=?")
                .bind(ARTIFACT)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(actual_size, None);
        for table in ["artifact_location", "artifact_provenance"] {
            let query = format!("SELECT COUNT(*) FROM {table} WHERE artifact_id=?");
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                .bind(ARTIFACT)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
            assert_eq!(count, 0);
        }
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 1);
        let plan_phase: String =
            sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=?")
                .bind(PLAN)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(plan_phase, "QUEUED");
    }

    #[tokio::test]
    async fn lista_solo_descargas_del_actor_y_pagina_por_fecha_e_id() {
        let (_dir, repo) = setup().await;
        let first = "0192f000-0000-7000-8000-00000000e137";
        let second = "0192f000-0000-7000-8000-00000000e138";
        let private = "0192f000-0000-7000-8000-00000000e139";
        insert_job(
            &repo,
            first,
            "NORMAL",
            "QUEUED",
            "2026-10-24T10:00:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            second,
            "HIGH",
            "QUEUED",
            "2026-10-24T10:01:00.000Z",
            OWNER,
        )
        .await;
        insert_job(
            &repo,
            private,
            "NORMAL",
            "QUEUED",
            "2026-10-24T09:00:00.000Z",
            OTHER,
        )
        .await;
        let ctx = MutationContext {
            actor_id: OWNER.into(),
            correlation_id: "0192f000-0000-7000-8000-00000000e105".into(),
        };

        let jobs = repo.list_jobs(None, 50, &ctx).await.unwrap();
        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            [first, second]
        );
        let page = repo
            .list_jobs(
                Some((jobs[0].created_at.clone(), jobs[0].id.clone())),
                50,
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            page.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            [second]
        );
    }

    fn control_context() -> MutationContext {
        MutationContext {
            actor_id: OWNER.into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn control_key(key: &str) -> larm_domain::download::DownloadControlIdempotency {
        larm_domain::download::DownloadControlIdempotency {
            key: key.into(),
            body_hash: "a".repeat(64),
        }
    }

    fn control_request() -> larm_domain::download::DownloadStop {
        larm_domain::download::DownloadStop {
            reason: "operator_request".into(),
            approval_id: None,
        }
    }

    #[tokio::test]
    async fn control_pausa_reanuda_con_checkpoint_reserva_y_replay_vinculado_al_job() {
        use larm_domain::download::DownloadCommand;
        let (_dir, repo) = setup().await;
        let id = uuid::Uuid::now_v7().to_string();
        prepare_completion(&repo, &id).await;
        sqlx::query("UPDATE download_job SET state='RUNNING' WHERE id=?")
            .bind(&id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let progress = crate::download_progress::SqlDownloadProgress {
            pool: repo.pool.clone(),
        };
        progress
            .checkpoint(
                &id,
                ARTIFACT,
                2,
                Some(serde_json::json!({"kind":"STRONG_ETAG","value":"\"v1\""})),
            )
            .await
            .unwrap();
        let ctx = control_context();
        let key = control_key("pause");
        let receipt = repo
            .request_control(&id, DownloadCommand::Pause, &control_request(), &ctx, &key)
            .await
            .unwrap();
        assert_eq!(
            repo.current_state(&id).await.unwrap(),
            Some(DownloadState::Pausing)
        );
        let replay = repo
            .request_control(&id, DownloadCommand::Pause, &control_request(), &ctx, &key)
            .await
            .unwrap();
        assert_eq!(receipt, replay);
        let mut different = key.clone();
        different.body_hash = "b".repeat(64);
        assert_eq!(
            repo.request_control(
                &id,
                DownloadCommand::Pause,
                &control_request(),
                &ctx,
                &different
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::IdempotencyConflict
        );
        assert_eq!(
            repo.request_control(
                &uuid::Uuid::now_v7().to_string(),
                DownloadCommand::Pause,
                &control_request(),
                &ctx,
                &key
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::IdempotencyConflict
        );
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event WHERE resource_id=? AND json_extract(payload,'$.action')='pause_requested'").bind(&id).fetch_one(&repo.pool).await.unwrap();
        assert_eq!(events, 1);
        assert!(repo.claim_next().await.unwrap().is_none());
        repo.finish_pause(&id).await.unwrap();
        repo.request_control(
            &id,
            DownloadCommand::Resume,
            &control_request(),
            &ctx,
            &control_key("resume"),
        )
        .await
        .unwrap();
        let checkpoint = progress.load_checkpoint(&id, ARTIFACT).await.unwrap();
        assert_eq!(checkpoint.durable_offset.get(), 2);
        assert!(checkpoint.resume_validator.is_some());
        let reserved: i64 =
            sqlx::query_scalar("SELECT remaining_bytes FROM volume_reservation WHERE job_id=?")
                .bind(&id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reserved, 100);
        assert_eq!(repo.claim_next().await.unwrap().unwrap().job.id, id);
    }

    #[tokio::test]
    async fn control_fallo_outbox_revierte_estado_journal_y_receipt() {
        use larm_domain::download::DownloadCommand;
        let (_dir, repo) = setup().await;
        let id = uuid::Uuid::now_v7().to_string();
        prepare_completion(&repo, &id).await;
        sqlx::query("UPDATE download_job SET state='QUEUED' WHERE id=?")
            .bind(&id)
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER ignore_control_write BEFORE UPDATE ON download_job WHEN NEW.state='CANCELLING' BEGIN SELECT RAISE(IGNORE); END").execute(&repo.pool).await.unwrap();
        assert!(repo
            .request_control(
                &id,
                DownloadCommand::Cancel,
                &control_request(),
                &control_context(),
                &control_key("cancel")
            )
            .await
            .is_err());
        let control: Option<String> = sqlx::query_scalar(
            "SELECT json_extract(payload,'$.execution.control') FROM operation_journal WHERE id=?",
        )
        .bind(PLAN)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert!(control.is_none());
        sqlx::query("DROP TRIGGER ignore_control_write")
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER reject_control_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture fault'); END").execute(&repo.pool).await.unwrap();
        assert!(repo
            .request_control(
                &id,
                DownloadCommand::Cancel,
                &control_request(),
                &control_context(),
                &control_key("cancel")
            )
            .await
            .is_err());
        assert_eq!(
            repo.current_state(&id).await.unwrap(),
            Some(DownloadState::Queued)
        );
        let control: Option<String> = sqlx::query_scalar(
            "SELECT json_extract(payload,'$.execution.control') FROM operation_journal WHERE id=?",
        )
        .bind(PLAN)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert!(control.is_none());
        let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM idempotency")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(receipts, 0);
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(&id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 1);
    }

    #[tokio::test]
    async fn control_rechaza_otros_actores_y_estados_y_recupera_cancelacion_activa() {
        use larm_domain::download::DownloadCommand;
        let (_dir, repo) = setup().await;
        let id = uuid::Uuid::now_v7().to_string();
        prepare_completion(&repo, &id).await;
        sqlx::query("UPDATE download_job SET state='RUNNING' WHERE id=?")
            .bind(&id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let mut other = control_context();
        other.actor_id = OTHER.into();
        assert_eq!(
            repo.request_control(
                &id,
                DownloadCommand::Cancel,
                &control_request(),
                &other,
                &control_key("cancel")
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            repo.request_control(
                &id,
                DownloadCommand::Resume,
                &control_request(),
                &control_context(),
                &control_key("resume")
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::InvalidStateTransition
        );
        repo.request_control(
            &id,
            DownloadCommand::Cancel,
            &control_request(),
            &control_context(),
            &control_key("cancel"),
        )
        .await
        .unwrap();
        assert!(repo.claim_pending_cancellation().await.unwrap().is_none());
        assert_eq!(repo.recover_interrupted().await.unwrap(), 1);
        assert_eq!(repo.recover_interrupted().await.unwrap(), 0);
        assert_eq!(
            repo.claim_pending_cancellation().await.unwrap().unwrap().id,
            id
        );
        assert!(repo.claim_pending_cancellation().await.unwrap().is_none());
        repo.finish_cancel(&id).await.unwrap();
        assert_eq!(
            repo.request_control(
                &id,
                DownloadCommand::Resume,
                &control_request(),
                &control_context(),
                &control_key("resume-new")
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::InvalidStateTransition
        );
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(&id)
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
    }
}
