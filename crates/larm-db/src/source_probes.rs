//! Persistencia durable P045 para `POST /sources/{id}/probe`.

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::bridge::{timestamp, JobPhase};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, SourceProbeRepository};
use larm_domain::source_probes::{self, SourceAuthProbeResult, SourceProbeJob, KIND, OPERATION};
use serde_json::json;
use sqlx::{SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlSourceProbes {
    pub pool: SqlitePool,
}

fn phase_text(phase: JobPhase) -> &'static str {
    match phase {
        JobPhase::Queued => "QUEUED",
        JobPhase::Running => "RUNNING",
        JobPhase::Completed => "COMPLETED",
        JobPhase::Failed => "FAILED",
        JobPhase::Cancelled => "CANCELLED",
    }
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo confirmar el sondeo de la fuente",
        reason,
    )
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando sondeos source", &e))?;
    Ok(())
}

fn context_valid(ctx: &MutationContext) -> bool {
    !ctx.actor_id.is_empty()
        && ctx.actor_id.len() <= 256
        && !ctx.actor_id.chars().any(char::is_control)
        && larm_domain::bridge::is_uuid_v7(&ctx.correlation_id)
}

fn decode(
    id: &str,
    phase: &str,
    payload: &str,
    digest: &str,
    resources: &str,
) -> Result<SourceProbeJob, LarmError> {
    let record: SourceProbeJob =
        serde_json::from_str(payload).map_err(|_| invalid("source_probe_journal_invalid"))?;
    let refs: Vec<String> =
        serde_json::from_str(resources).map_err(|_| invalid("source_probe_journal_invalid"))?;
    let expected = match record.phase {
        JobPhase::Queued => 1,
        JobPhase::Running => 2,
        _ => 3,
    };
    let valid_result = record.result.as_ref().is_none_or(valid_result);
    if record.id != id
        || !larm_domain::bridge::is_uuid_v7(id)
        || phase_text(record.phase) != phase
        || record.version != expected
        || refs != vec![record.target.clone()]
        || larm_domain::conformance::hash(&record)? != digest
        || !context_valid(&record.context())
        || record.source_row_version < 1
        || source_probes::validate_request(&record.target, &record.request).is_err()
        || !valid_result
        || (record.phase == JobPhase::Completed && record.result.is_none())
        || (record.phase == JobPhase::Failed && record.error.is_none())
        || (matches!(record.phase, JobPhase::Queued | JobPhase::Running)
            && (record.result.is_some() || record.error.is_some()))
    {
        return Err(invalid("source_probe_journal_invalid"));
    }
    Ok(record)
}

fn valid_result(result: &SourceAuthProbeResult) -> bool {
    result.reason.len() <= 64
        && matches!(
            result.reason.as_str(),
            "authenticated"
                | "authentication_required"
                | "authentication_failed"
                | "license_required"
                | "probe_completed"
        )
}

async fn load(conn: &mut SqliteConnection, id: &str) -> Result<Option<SourceProbeJob>, LarmError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT phase,payload,plan_hash,resource_ids FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(id)
    .bind(KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo sondeo source", &e))?;
    row.map(|(phase, payload, digest, resources)| decode(id, &phase, &payload, &digest, &resources))
        .transpose()
}

async fn save(conn: &mut SqliteConnection, record: &SourceProbeJob) -> Result<(), LarmError> {
    let written = sqlx::query("UPDATE operation_journal SET phase=?,payload=?,plan_hash=?,updated_at=? WHERE id=? AND kind=?")
        .bind(phase_text(record.phase))
        .bind(serde_json::to_string(record).map_err(|e| db_error("serializando sondeo source", &e))?)
        .bind(larm_domain::conformance::hash(record)?)
        .bind(timestamp(chrono::Utc::now()))
        .bind(&record.id)
        .bind(KIND)
        .execute(conn)
        .await
        .map_err(|e| db_error("guardando sondeo source", &e))?;
    if written.rows_affected() != 1 {
        return Err(invalid("source_probe_journal_missing"));
    }
    Ok(())
}

async fn event(conn: &mut SqliteConnection, record: &SourceProbeJob) -> Result<(), LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "source.updated",
            resource_type: "job",
            resource_id: &record.id,
            resource_version: i64::from(record.version),
            data: json!({
                "action":"auth_probe_state_changed",
                "job_id":record.id,
                "source_id":record.target,
                "state":record.phase
            }),
        },
        &record.context(),
    )
    .await?;
    Ok(())
}

#[async_trait]
impl SourceProbeRepository for SqlSourceProbes {
    async fn enqueue_source_probe(
        &self,
        target: &str,
        request: &larm_domain::hardware::Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<SourceProbeJob, LarmError> {
        source_probes::validate_request(target, request)?;
        if !context_valid(ctx)
            || !larm_domain::adapter_updates::is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 256
            || key.chars().any(char::is_control)
        {
            return Err(invalid("source_probe_request_invalid"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let replay: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(OPERATION)
        .bind(key)
        .bind(timestamp(now))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("replay sondeo source", &e))?;
        if let Some((old_hash, payload)) = replay {
            let replay: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|_| invalid("source_probe_replay_invalid"))?;
            if old_hash != body_hash || replay["target"] != target {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otra solicitud",
                    "idempotency_body_mismatch",
                ));
            }
            let id = replay["job_id"]
                .as_str()
                .ok_or_else(|| invalid("source_probe_replay_invalid"))?;
            let record = load(&mut tx, id)
                .await?
                .ok_or_else(|| invalid("source_probe_replay_invalid"))?;
            if record.actor_id != ctx.actor_id || record.request != *request {
                return Err(invalid("source_probe_replay_invalid"));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(record);
        }
        let source: Option<(i64, bool)> =
            sqlx::query_as("SELECT row_version,enabled FROM model_source WHERE id=?")
                .bind(target)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("fuente de sondeo", &e))?;
        let Some((source_row_version, enabled)) = source else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Fuente desconocida",
                "source_not_found",
            ));
        };
        if !enabled {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La fuente está deshabilitada",
                "source_disabled",
            ));
        }
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.target')=?)",
        )
        .bind(KIND)
        .bind(target)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("sondeo source activo", &e))?;
        if active {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe un sondeo activo para esta fuente",
                "source_probe_active",
            ));
        }
        let record = SourceProbeJob {
            id: uuid::Uuid::now_v7().to_string(),
            target: target.into(),
            actor_id: ctx.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            request: request.clone(),
            source_row_version,
            phase: JobPhase::Queued,
            version: 1,
            result: None,
            error: None,
            authority_pending: Some(false),
        };
        let payload = serde_json::to_string(&record)
            .map_err(|e| db_error("serializando sondeo source", &e))?;
        let digest = larm_domain::conformance::hash(&record)?;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES (?,?,?,'QUEUED',?,?,?)")
            .bind(&record.id)
            .bind(KIND)
            .bind(json!([target]).to_string())
            .bind(digest)
            .bind(payload)
            .bind(timestamp(now))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("encolando sondeo source", &e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id)
            .bind(OPERATION)
            .bind(key)
            .bind(timestamp(now))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("limpiando replay source", &e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,202,?,?)")
            .bind(&ctx.actor_id)
            .bind(OPERATION)
            .bind(key)
            .bind(body_hash)
            .bind(json!({"target":target,"job_id":record.id}).to_string())
            .bind(timestamp(now + chrono::Duration::days(30)))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("persistiendo replay source", &e))?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record)
    }

    async fn claim_source_probe(&self) -> Result<Option<SourceProbeJob>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT q.id FROM operation_journal q WHERE q.kind=? AND q.phase='QUEUED' AND NOT EXISTS(SELECT 1 FROM operation_journal r WHERE r.kind=q.kind AND r.phase='RUNNING' AND json_extract(r.payload,'$.target')=json_extract(q.payload,'$.target')) ORDER BY q.id LIMIT 1",
        )
        .bind(KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("cola sondeo source", &e))?;
        let Some(id) = id else {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(None);
        };
        let mut record = load(&mut tx, &id)
            .await?
            .ok_or_else(|| invalid("source_probe_journal_invalid"))?;
        record.phase = JobPhase::Running;
        record.version = 2;
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(Some(record))
    }

    async fn source_probe_job(&self, id: &str) -> Result<Option<SourceProbeJob>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        load(&mut conn, id).await
    }

    async fn finish_source_probe(
        &self,
        id: &str,
        result: &SourceAuthProbeResult,
    ) -> Result<(), LarmError> {
        if !valid_result(result) {
            return Err(invalid("source_probe_result_invalid"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let mut record = load(&mut tx, id)
            .await?
            .ok_or_else(|| invalid("source_probe_journal_missing"))?;
        if record.phase == JobPhase::Completed {
            return if record.result.as_ref() == Some(result) {
                Ok(())
            } else {
                Err(invalid("source_probe_result_mismatch"))
            };
        }
        if record.phase != JobPhase::Running {
            return Err(invalid("source_probe_not_running"));
        }
        record.phase = JobPhase::Completed;
        record.version = 3;
        record.result = Some(result.clone());
        record.error = None;
        record.authority_pending = Some(false);
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }

    async fn fail_source_probe(
        &self,
        id: &str,
        error: &LarmError,
        authority_pending: bool,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let mut record = load(&mut tx, id)
            .await?
            .ok_or_else(|| invalid("source_probe_journal_missing"))?;
        if matches!(record.phase, JobPhase::Completed | JobPhase::Failed) {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if record.phase != JobPhase::Running {
            return Err(invalid("source_probe_not_running"));
        }
        record.phase = JobPhase::Failed;
        record.version = 3;
        record.result = None;
        record.error = Some(source_probes::public_failure(error, &record.correlation_id));
        record.authority_pending = Some(authority_pending);
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }
}
