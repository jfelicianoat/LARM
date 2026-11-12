//! Órdenes durables e idempotentes sobre la cola P051.

use super::*;
use larm_domain::bridge::JobReceipt;
use larm_domain::download::{DownloadCommand, DownloadControlIdempotency, DownloadStop};

impl SqlDownloadJobs {
    pub async fn request_control(
        &self,
        job_id: &str,
        command: DownloadCommand,
        request: &DownloadStop,
        ctx: &MutationContext,
        idempotency: &DownloadControlIdempotency,
    ) -> Result<JobReceipt, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(job_id)
            || request.reason.is_empty()
            || request
                .approval_id
                .as_deref()
                .is_some_and(|id| !larm_domain::bridge::is_uuid_v7(id))
            || idempotency.key.trim().is_empty()
            || !larm_domain::adapter_updates::is_sha256(&idempotency.body_hash)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La orden de descarga no es válida",
                "download_control_invalid",
            ));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN control descarga", &e))?;
        lock(&mut tx).await?;
        let operation = command.operation_id();
        let stored: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        ).bind(&ctx.actor_id).bind(operation).bind(&idempotency.key).bind(&now)
            .fetch_optional(&mut *tx).await.map_err(|e| db_error("replay control descarga", &e))?;
        if let Some((hash, response)) = stored {
            let receipt: JobReceipt = serde_json::from_str(&response)
                .map_err(|e| db_error("receipt control descarga", &e))?;
            // La operación contractual usa una plantilla de ruta: vincular el
            // recibo al ID impide reutilizar la clave para otro recurso.
            if hash != idempotency.body_hash || receipt.job_id != job_id {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "La clave ya corresponde a otra solicitud",
                    "idempotency_body_or_resource_mismatch",
                ));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay control", &e))?;
            return Ok(receipt);
        }
        let mut job = load(&mut tx, job_id)
            .await?
            .filter(|job| {
                job.metadata.get("actor_id").and_then(Value::as_str) == Some(ctx.actor_id.as_str())
            })
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Descarga desconocida",
                    "download_job_not_found",
                )
            })?;
        let (next, action) = match (command, job.state) {
            (DownloadCommand::Pause, DownloadState::Running) => {
                (DownloadState::Pausing, "pause_requested")
            }
            (DownloadCommand::Resume, DownloadState::Paused) => {
                (DownloadState::Queued, "resume_requested")
            }
            (DownloadCommand::Cancel, state)
                if state.can_transition_to(DownloadState::Cancelling) =>
            {
                (DownloadState::Cancelling, "cancel_requested")
            }
            _ => return Err(invalid("download_control_state_not_allowed").with_resource(job_id)),
        };
        if command == DownloadCommand::Cancel {
            // Sólo QUEUED/PAUSED/FAILED carecen ya de un consumer activo.
            // Para los demás, ese consumer debe cerrar primero el worker.
            let dispatch = if matches!(
                job.state,
                DownloadState::Queued
                    | DownloadState::Paused
                    | DownloadState::Planned
                    | DownloadState::Failed
            ) {
                "QUEUED"
            } else {
                "ACTIVE"
            };
            job.progress
                .insert("cancel_dispatch".into(), Value::String(dispatch.into()));
        }
        let plan_id = job
            .metadata
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("download_job_plan_missing"))?;
        let control = serde_json::json!({"action":action,"reason":request.reason,"approval_id":request.approval_id,"requested_at":now});
        // approval_id es auditado; no concede permisos sobre archivos ajenos.
        let journal = sqlx::query("UPDATE operation_journal SET updated_at=?,payload=json_set(payload,'$.execution.control',json(?)) WHERE id=? AND kind='download.plan' AND phase IN ('QUEUED','FAILED') AND plan_hash=? AND json_extract(payload,'$.execution.job_id')=?")
            .bind(&now).bind(control.to_string()).bind(plan_id).bind(&job.plan_hash).bind(job_id)
            .execute(&mut *tx).await.map_err(|e| db_error("journal control descarga", &e))?;
        if journal.rows_affected() != 1 {
            return Err(invalid("download_control_journal_conflict"));
        }
        job.progress.insert("phase".into(), serde_json::json!(next));
        let changed = sqlx::query("UPDATE download_job SET state=?,progress=?,row_version=row_version+1,updated_at=? WHERE id=? AND row_version=?")
            .bind(state_text(next)).bind(serde_json::to_string(&job.progress).map_err(|e| db_error("progreso control", &e))?)
            .bind(&now).bind(job_id).bind(job.row_version).execute(&mut *tx).await.map_err(|e| db_error("estado control descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_control_conflict"));
        }
        job.state = next;
        job.row_version = job
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("download_job_row_version_overflow"))?;
        job.updated_at = now.clone();
        emit_state(&mut tx, &job, ctx, action).await?;
        let receipt = JobReceipt {
            job_id: job_id.into(),
            status_url: format!("/api/v1/jobs/{job_id}"),
            state: phase_for(next),
        };
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id).bind(operation).bind(&idempotency.key).bind(&now).execute(&mut *tx).await.map_err(|e| db_error("idempotencia expirada control", &e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES(?,?,?,?,202,?,?)")
            .bind(&ctx.actor_id).bind(operation).bind(&idempotency.key).bind(&idempotency.body_hash)
            .bind(serde_json::to_string(&receipt).map_err(|e| db_error("receipt control", &e))?)
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now()+chrono::Duration::days(30)))
            .execute(&mut *tx).await.map_err(|e| db_error("guardar receipt control", &e))?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT control descarga", &e))?;
        Ok(receipt)
    }

    /// Reclama limpieza sin competir con el consumer que todavía cierra IO.
    pub async fn claim_pending_cancellation(&self) -> Result<Option<DownloadJob>, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN limpieza descarga", &e))?;
        lock(&mut tx).await?;
        let id: Option<String> = sqlx::query_scalar("SELECT id FROM download_job WHERE state='CANCELLING' AND json_extract(progress,'$.cancel_dispatch')='QUEUED' AND COALESCE(json_extract(progress,'$.cancel_retry_at'),'')<=? ORDER BY updated_at,id LIMIT 1")
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now())).fetch_optional(&mut *tx).await.map_err(|e| db_error("cola limpieza descarga", &e))?;
        let job = if let Some(id) = id {
            let changed = sqlx::query("UPDATE download_job SET progress=json_set(progress,'$.cancel_dispatch','RUNNING') WHERE id=?")
                .bind(&id).execute(&mut *tx).await.map_err(|e| db_error("claim limpieza descarga", &e))?;
            if changed.rows_affected() != 1 {
                return Err(invalid("download_cleanup_claim_conflict"));
            }
            load(&mut tx, &id).await?
        } else {
            None
        };
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT claim limpieza", &e))?;
        Ok(job)
    }

    /// Sólo después de cerrar el worker. Un fallo de disco conserva
    /// CANCELLING y la reserva para reintentar sin IO concurrente.
    pub async fn retry_cancellation_cleanup(&self, job_id: &str) -> Result<(), LarmError> {
        sqlx::query("UPDATE download_job SET progress=json_set(progress,'$.cancel_dispatch','QUEUED','$.cancel_retry_at',?),updated_at=? WHERE id=? AND state='CANCELLING' AND json_extract(progress,'$.cancel_dispatch') IN ('RUNNING','ACTIVE')")
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now()+chrono::Duration::seconds(5)))
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now())).bind(job_id).execute(&self.pool).await.map_err(|e| db_error("reintentar limpieza descarga", &e))?;
        Ok(())
    }

    pub async fn cancellation_job(&self, job_id: &str) -> Result<DownloadJob, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("leer cancelación", &e))?;
        load(&mut conn, job_id)
            .await?
            .filter(|job| job.state == DownloadState::Cancelling)
            .ok_or_else(|| invalid("download_job_not_cancelling"))
    }
}
