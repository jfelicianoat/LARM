//! Journal durable e idempotente de `POST /instances/{id}/stop` (P033).

use crate::{
    events::{append, NewEvent},
    instances::{record as instance_record, state_name},
    pool::db_error,
};
use async_trait::async_trait;
use larm_domain::{
    auth::{ApiScope, Principal},
    bridge::{is_uuid_v7, timestamp, JobPhase, JobReceipt, JobState},
    error::{ErrorCode, LarmError},
    instances::{InstanceState, RuntimeInstance, RuntimeInstanceStopJob, StopRequest, StopResult},
    ports::{JobSource, MutationContext},
    runtime::RuntimeOwnership,
    runtime_ports::RuntimeInstanceStopRepository,
};
use serde_json::json;
use sqlx::{sqlite::SqliteRow, SqliteConnection, SqlitePool};

const KIND: &str = "runtime.instance.stop";
const OPERATION: &str = "P033_post__instances_id_stop";

#[derive(Clone)]
pub struct SqlRuntimeInstanceStopJobs {
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

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El journal de parada no es coherente",
        reason,
    )
}

fn validate_submit(
    instance_id: &str,
    request: &StopRequest,
    body_hash: &str,
    key: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    request.validate()?;
    if !is_uuid_v7(instance_id)
        || !is_uuid_v7(&ctx.actor_id)
        || !is_uuid_v7(&ctx.correlation_id)
        || body_hash.len() != 64
        || !body_hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || key.is_empty()
        || key.len() > 240
    {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Solicitud de parada no válida",
            "runtime_instance_stop_request_invalid",
        ));
    }
    Ok(())
}

async fn write_lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando cola de parada", &e))?;
    Ok(())
}

fn decode(
    id: &str,
    phase: &str,
    payload: &str,
    plan_hash: &str,
    resources: &str,
) -> Result<RuntimeInstanceStopJob, LarmError> {
    let record: RuntimeInstanceStopJob = serde_json::from_str(payload)
        .map_err(|_| invalid("runtime_instance_stop_journal_invalid"))?;
    let refs: Vec<String> = serde_json::from_str(resources)
        .map_err(|_| invalid("runtime_instance_stop_journal_invalid"))?;
    if record.validate().is_err()
        || record.job_id != id
        || phase_text(record.state) != phase
        || refs != vec![record.instance_id.clone()]
        || plan_hash.len() != 64
        || !plan_hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || (record.state == JobPhase::Completed
            && !record.result.is_some_and(|result| result.stopped))
        || (record.state == JobPhase::Failed && record.error.is_none())
        || (matches!(record.state, JobPhase::Queued | JobPhase::Running)
            && (record.result.is_some() || record.error.is_some()))
    {
        return Err(invalid("runtime_instance_stop_journal_invalid").with_resource(id));
    }
    Ok(record)
}

async fn load(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<RuntimeInstanceStopJob>, LarmError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT phase,payload,plan_hash,resource_ids FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(id)
    .bind(KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo trabajo de parada", &e))?;
    row.map(|(phase, payload, plan_hash, resources)| {
        decode(id, &phase, &payload, &plan_hash, &resources)
    })
    .transpose()
}

fn context(job: &RuntimeInstanceStopJob) -> MutationContext {
    MutationContext {
        actor_id: job.actor_id.clone(),
        correlation_id: job.correlation_id.clone(),
    }
}

async fn save(conn: &mut SqliteConnection, job: &RuntimeInstanceStopJob) -> Result<(), LarmError> {
    let updated = sqlx::query(
        "UPDATE operation_journal SET phase=?,payload=?,updated_at=? WHERE id=? AND kind=?",
    )
    .bind(phase_text(job.state))
    .bind(serde_json::to_string(job).map_err(|e| db_error("serializando trabajo de parada", &e))?)
    .bind(timestamp(chrono::Utc::now()))
    .bind(&job.job_id)
    .bind(KIND)
    .execute(conn)
    .await
    .map_err(|e| db_error("guardando trabajo de parada", &e))?;
    if updated.rows_affected() != 1 {
        return Err(invalid("runtime_instance_stop_job_missing").with_resource(&job.job_id));
    }
    Ok(())
}

async fn current_instance(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<RuntimeInstance, LarmError> {
    let row: Option<SqliteRow> = sqlx::query("SELECT * FROM runtime_instance WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo instancia para parada", &e))?;
    row.as_ref()
        .map(instance_record)
        .transpose()?
        .ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Instancia de runtime desconocida",
                "runtime_instance_not_found",
            )
            .with_resource(id)
        })
}

async fn transition_in_tx(
    conn: &mut SqliteConnection,
    current: &RuntimeInstance,
    next: InstanceState,
    ctx: &MutationContext,
) -> Result<RuntimeInstance, LarmError> {
    let updated = current.transition_to(next)?;
    let generation = i64::try_from(updated.generation).map_err(|_| {
        LarmError::new(
            ErrorCode::ValidationFailed,
            "Instancia de runtime no válida",
            "runtime_instance_generation_invalid",
        )
    })?;
    let result = sqlx::query(
        "UPDATE runtime_instance SET updated_at=?,row_version=?,metadata=?,state=?,pid=?,\
         process_started_at=?,executable_hash=?,generation=?,execution_fingerprint=?,\
         last_used_at=? WHERE id=? AND row_version=?",
    )
    .bind(&updated.updated_at)
    .bind(updated.row_version)
    .bind(serde_json::Value::Object(updated.metadata.clone()).to_string())
    .bind(state_name(updated.state))
    .bind(updated.pid.map(i64::from))
    .bind(&updated.process_started_at)
    .bind(&updated.executable_hash)
    .bind(generation)
    .bind(&updated.execution_fingerprint)
    .bind(&updated.last_used_at)
    .bind(&updated.id)
    .bind(current.row_version)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("transicionando instancia para parada", &e))?;
    if result.rows_affected() != 1 {
        return Err(LarmError::new(
            ErrorCode::StaleRevision,
            "La instancia cambió desde la última lectura",
            "runtime_instance_stale",
        )
        .with_resource(&updated.id));
    }
    append(
        conn,
        &NewEvent {
            event_type: "instance.state_changed",
            resource_type: "runtime_instance",
            resource_id: &updated.id,
            resource_version: updated.row_version,
            data: json!({"from": current.state, "to": updated.state}),
        },
        ctx,
    )
    .await?;
    Ok(updated)
}

#[async_trait]
impl RuntimeInstanceStopRepository for SqlRuntimeInstanceStopJobs {
    async fn submit_stop(
        &self,
        instance_id: &str,
        request: &StopRequest,
        body_hash: &str,
        idempotency_key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        validate_submit(instance_id, request, body_hash, idempotency_key, ctx)?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let now_text = timestamp(now);
        let stored: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(OPERATION)
        .bind(idempotency_key)
        .bind(&now_text)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo replay de parada", &e))?;
        if let Some((prior_hash, response)) = stored {
            let receipt: JobReceipt =
                serde_json::from_str(&response).map_err(|e| db_error("receipt de parada", &e))?;
            let job = load(&mut tx, &receipt.job_id)
                .await?
                .ok_or_else(|| invalid("runtime_instance_stop_replay_missing"))?;
            if prior_hash != body_hash
                || job.instance_id != instance_id
                || job.request != *request
                || job.actor_id != ctx.actor_id
            {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada para otra parada",
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            if receipt.job_id != job.job_id
                || receipt.status_url != format!("/api/v1/jobs/{}", job.job_id)
                || receipt.state != JobPhase::Queued
            {
                return Err(invalid("runtime_instance_stop_replay_invalid"));
            }
            return Ok(receipt);
        }

        let current = current_instance(&mut tx, instance_id).await?;
        if current.ownership != RuntimeOwnership::LarmManaged
            || !matches!(
                current.state,
                InstanceState::Ready
                    | InstanceState::Busy
                    | InstanceState::Degraded
                    | InstanceState::Failed
                    | InstanceState::Crashed
            )
        {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instancia no admite una parada administrada",
                "runtime_instance_stop_state_invalid",
            )
            .with_resource(instance_id));
        }
        let active: Option<String> = sqlx::query_scalar(
            "SELECT id FROM runtime_lease WHERE instance_id=? AND state NOT IN ('EXPIRED','RELEASED','FAILED') LIMIT 1",
        )
        .bind(instance_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("comprobando leases antes de parar", &e))?;
        if active.is_some() {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "No se puede detener una instancia con leases no terminales",
                "runtime_instance_has_active_leases",
            )
            .with_resource(instance_id));
        }

        let job = RuntimeInstanceStopJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            actor_id: ctx.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            instance_id: instance_id.to_owned(),
            request: request.clone(),
            state: JobPhase::Queued,
            result: None,
            error: None,
            evidence_refs: Vec::new(),
        };
        job.validate()?;
        transition_in_tx(&mut tx, &current, InstanceState::Stopping, ctx).await?;
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES (?,?,?,'QUEUED',?,?,?)",
        )
        .bind(&job.job_id)
        .bind(KIND)
        .bind(json!([instance_id]).to_string())
        .bind(body_hash)
        .bind(serde_json::to_string(&job).map_err(|e| db_error("serializando job de parada", &e))?)
        .bind(&now_text)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("encolando parada de instancia", &e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id)
            .bind(OPERATION)
            .bind(idempotency_key)
            .bind(&now_text)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("limpiando replay caducado", &e))?;
        let receipt = job.receipt();
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES (?,?,?, ?,202,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(OPERATION)
        .bind(idempotency_key)
        .bind(body_hash)
        .bind(serde_json::to_string(&receipt).map_err(|e| db_error("receipt de parada", &e))?)
        .bind(timestamp(now + chrono::Duration::days(30)))
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando replay de parada", &e))?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT de parada", &e))?;
        Ok(receipt)
    }

    async fn claim_stop_job(&self) -> Result<Option<RuntimeInstanceStopJob>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind=? AND phase='QUEUED' ORDER BY updated_at,id LIMIT 1",
        )
        .bind(KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("seleccionando parada pendiente", &e))?;
        let Some(id) = id else {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(None);
        };
        let mut job = load(&mut tx, &id)
            .await?
            .ok_or_else(|| invalid("runtime_instance_stop_job_missing"))?;
        job.state = JobPhase::Running;
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='RUNNING',payload=?,updated_at=? WHERE id=? AND kind=? AND phase='QUEUED'",
        )
        .bind(serde_json::to_string(&job).map_err(|e| db_error("serializando job de parada", &e))?)
        .bind(timestamp(chrono::Utc::now()))
        .bind(&id)
        .bind(KIND)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("reclamando job de parada", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("runtime_instance_stop_claim_conflict").with_resource(id));
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(Some(job))
    }

    async fn complete_stop_job(&self, job_id: &str, result: StopResult) -> Result<(), LarmError> {
        if !result.stopped {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El proceso no confirmó la parada",
                "runtime_instance_stop_result_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Trabajo desconocido", "not_found")
        })?;
        if job.state == JobPhase::Completed {
            if job.result == Some(result) {
                tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
                return Ok(());
            }
            return Err(invalid("runtime_instance_stop_terminal_conflict").with_resource(job_id));
        }
        if job.state != JobPhase::Running {
            return Err(invalid("runtime_instance_stop_job_not_running").with_resource(job_id));
        }
        let current = current_instance(&mut tx, &job.instance_id).await?;
        if current.state != InstanceState::Stopping {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia ya no está en parada",
                "runtime_instance_stop_state_stale",
            )
            .with_resource(&current.id));
        }
        transition_in_tx(&mut tx, &current, InstanceState::Stopped, &context(&job)).await?;
        job.state = JobPhase::Completed;
        job.result = Some(result);
        save(&mut tx, &job).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn fail_stop_job(&self, job_id: &str, error: &LarmError) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut job = load(&mut tx, job_id).await?.ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Trabajo desconocido", "not_found")
        })?;
        if job.state == JobPhase::Failed {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if matches!(job.state, JobPhase::Completed | JobPhase::Cancelled) {
            return Err(invalid("runtime_instance_stop_terminal_conflict").with_resource(job_id));
        }
        job.state = JobPhase::Failed;
        job.error = Some(error.clone());
        save(&mut tx, &job).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn recover_stop_jobs(&self) -> Result<u64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind=? AND phase='RUNNING' ORDER BY id",
        )
        .bind(KIND)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("recuperando paradas", &e))?;
        for id in &ids {
            let mut job = load(&mut tx, id)
                .await?
                .ok_or_else(|| invalid("runtime_instance_stop_job_missing"))?;
            job.state = JobPhase::Queued;
            let updated = sqlx::query(
                "UPDATE operation_journal SET phase='QUEUED',payload=?,updated_at=? WHERE id=? AND kind=? AND phase='RUNNING'",
            )
            .bind(serde_json::to_string(&job).map_err(|e| db_error("serializando recuperación de parada", &e))?)
            .bind(timestamp(chrono::Utc::now()))
            .bind(id)
            .bind(KIND)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("reencolando parada interrumpida", &e))?;
            if updated.rows_affected() != 1 {
                return Err(invalid("runtime_instance_stop_recovery_conflict").with_resource(id));
            }
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        u64::try_from(ids.len()).map_err(|_| db_error("contando paradas recuperadas", &"overflow"))
    }
}

#[async_trait]
impl JobSource for SqlRuntimeInstanceStopJobs {
    async fn job(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<Option<JobState>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool de trabajos de parada", &e))?;
        let Some(job) = load(&mut conn, job_id).await? else {
            return Ok(None);
        };
        if job.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin) {
            Ok(Some(job.job_state()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use larm_domain::auth::{Principal, PrincipalKind};
    use serde_json::Map;
    use sqlx::sqlite::SqlitePoolOptions;

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: id(90),
            correlation_id: id(91),
        }
    }

    fn request() -> StopRequest {
        StopRequest {
            reason: "operator_requested".into(),
            approval_id: None,
        }
    }

    fn sample() -> RuntimeInstance {
        RuntimeInstance {
            id: id(1),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: id(2),
            variant_id: id(3),
            backend_id: id(4),
            launch_profile_id: id(5),
            state: InstanceState::Ready,
            pid: Some(1234),
            process_started_at: Some("2026-10-22T00:00:00.000Z".into()),
            executable_hash: Some("a".repeat(64)),
            generation: 0,
            execution_fingerprint: Some("b".repeat(64)),
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        }
    }

    async fn repo() -> SqlRuntimeInstanceStopJobs {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for schema in [
            "CREATE TABLE runtime_instance(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,runtime_installation_id TEXT NOT NULL,variant_id TEXT NOT NULL,backend_id TEXT NOT NULL,launch_profile_id TEXT NOT NULL,state TEXT NOT NULL,pid INTEGER,process_started_at TEXT,executable_hash TEXT,generation INTEGER NOT NULL,execution_fingerprint TEXT,last_used_at TEXT,ownership TEXT NOT NULL)",
            "CREATE TABLE runtime_lease(id TEXT PRIMARY KEY,instance_id TEXT NOT NULL,state TEXT NOT NULL)",
            "CREATE TABLE operation_journal(id TEXT PRIMARY KEY,kind TEXT NOT NULL,resource_ids TEXT NOT NULL,phase TEXT NOT NULL,plan_hash TEXT NOT NULL,payload TEXT NOT NULL,updated_at TEXT NOT NULL)",
            "CREATE TABLE idempotency(principal_id TEXT NOT NULL,operation TEXT NOT NULL,key TEXT NOT NULL,body_hash TEXT NOT NULL,status_code INTEGER NOT NULL,response_json TEXT NOT NULL,expires_at TEXT NOT NULL,PRIMARY KEY(principal_id,operation,key))",
            "CREATE TABLE event_sequence_alloc(id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "CREATE TABLE event(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,event_id INTEGER NOT NULL,event_type TEXT NOT NULL,actor_id TEXT NOT NULL,resource_type TEXT NOT NULL,resource_id TEXT NOT NULL,resource_version INTEGER NOT NULL,correlation_id TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL)",
            "CREATE TABLE event_outbox(event_id INTEGER PRIMARY KEY,event_record_id TEXT NOT NULL,delivered_at TEXT)",
        ] {
            sqlx::query(schema).execute(&pool).await.unwrap();
        }
        let instance = sample();
        sqlx::query("INSERT INTO runtime_instance VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&instance.id)
            .bind(&instance.created_at)
            .bind(&instance.updated_at)
            .bind(instance.row_version)
            .bind("{}")
            .bind(&instance.runtime_installation_id)
            .bind(&instance.variant_id)
            .bind(&instance.backend_id)
            .bind(&instance.launch_profile_id)
            .bind("READY")
            .bind(instance.pid.map(i64::from))
            .bind(&instance.process_started_at)
            .bind(&instance.executable_hash)
            .bind(0_i64)
            .bind(&instance.execution_fingerprint)
            .bind(&instance.last_used_at)
            .bind("LARM_MANAGED")
            .execute(&pool)
            .await
            .unwrap();
        SqlRuntimeInstanceStopJobs { pool }
    }

    async fn counts(repo: &SqlRuntimeInstanceStopJobs) -> (i64, i64, i64, i64) {
        let instance: i64 =
            sqlx::query_scalar("SELECT count(*) FROM runtime_instance WHERE state='STOPPING'")
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM operation_journal WHERE kind=?")
            .bind(KIND)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency WHERE operation=?")
            .bind(OPERATION)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        (instance, jobs, keys, events)
    }

    #[tokio::test]
    async fn submit_stop_commits_state_job_event_and_idempotency_together() {
        let repo = repo().await;
        let ctx = ctx();
        let request = request();
        let hash = "c".repeat(64);
        let first = repo
            .submit_stop(&id(1), &request, &hash, "stop-key", &ctx)
            .await
            .unwrap();
        assert_eq!(first.state, JobPhase::Queued);
        assert_eq!(counts(&repo).await, (1, 1, 1, 1));
        let replay = repo
            .submit_stop(&id(1), &request, &hash, "stop-key", &ctx)
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(counts(&repo).await, (1, 1, 1, 1));

        let mut changed = request.clone();
        changed.reason = "changed".into();
        let error = repo
            .submit_stop(&id(1), &changed, &"d".repeat(64), "stop-key", &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IdempotencyConflict);
        assert_eq!(counts(&repo).await, (1, 1, 1, 1));
    }

    #[tokio::test]
    async fn active_lease_and_outbox_failure_leave_no_partial_stop() {
        let repo = repo().await;
        sqlx::query("INSERT INTO runtime_lease VALUES(?,?,?)")
            .bind(id(20))
            .bind(id(1))
            .bind("ACTIVE")
            .execute(&repo.pool)
            .await
            .unwrap();
        let error = repo
            .submit_stop(&id(1), &request(), &"c".repeat(64), "lease-key", &ctx())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "runtime_instance_has_active_leases");
        assert_eq!(counts(&repo).await, (0, 0, 0, 0));

        sqlx::query("UPDATE runtime_lease SET state='RELEASED'")
            .execute(&repo.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER fail_stop_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected'); END")
            .execute(&repo.pool).await.unwrap();
        assert!(repo
            .submit_stop(&id(1), &request(), &"c".repeat(64), "failed-key", &ctx())
            .await
            .is_err());
        let state: String = sqlx::query_scalar("SELECT state FROM runtime_instance WHERE id=?")
            .bind(id(1))
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "READY");
        assert_eq!(counts(&repo).await, (0, 0, 0, 0));
    }

    #[tokio::test]
    async fn running_job_recovers_reclaims_completes_and_is_actor_scoped() {
        let repo = repo().await;
        let ctx = ctx();
        let receipt = repo
            .submit_stop(&id(1), &request(), &"c".repeat(64), "run-key", &ctx)
            .await
            .unwrap();
        let claimed = repo.claim_stop_job().await.unwrap().unwrap();
        assert_eq!(claimed.job_id, receipt.job_id);
        assert_eq!(claimed.state, JobPhase::Running);
        assert_eq!(repo.recover_stop_jobs().await.unwrap(), 1);
        assert_eq!(
            repo.claim_stop_job().await.unwrap().unwrap().state,
            JobPhase::Running
        );

        let actor = Principal::new(
            id(90),
            "stopper",
            PrincipalKind::Operator,
            [ApiScope::Read],
            [],
        )
        .unwrap();
        assert_eq!(
            repo.job(&actor, &receipt.job_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            JobPhase::Running
        );
        let other = Principal::new(
            id(92),
            "other",
            PrincipalKind::Operator,
            [ApiScope::Read],
            [],
        )
        .unwrap();
        assert!(repo.job(&other, &receipt.job_id).await.unwrap().is_none());

        repo.complete_stop_job(&receipt.job_id, StopResult { stopped: true })
            .await
            .unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM runtime_instance WHERE id=?")
            .bind(id(1))
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "STOPPED");
        assert_eq!(
            repo.job(&actor, &receipt.job_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            JobPhase::Completed
        );
        repo.complete_stop_job(&receipt.job_id, StopResult { stopped: true })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failed_stop_job_does_not_claim_the_instance_stopped() {
        let repo = repo().await;
        let receipt = repo
            .submit_stop(&id(1), &request(), &"c".repeat(64), "fail-key", &ctx())
            .await
            .unwrap();
        repo.claim_stop_job().await.unwrap().unwrap();
        let error = LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo comprobar el proceso",
            "runtime_process_probe_unavailable",
        );
        repo.fail_stop_job(&receipt.job_id, &error).await.unwrap();

        let state: String = sqlx::query_scalar("SELECT state FROM runtime_instance WHERE id=?")
            .bind(id(1))
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(state, "STOPPING");
        assert_eq!(
            repo.job(
                &Principal::new(
                    id(90),
                    "stopper",
                    PrincipalKind::Operator,
                    [ApiScope::Read],
                    [],
                )
                .unwrap(),
                &receipt.job_id,
            )
            .await
            .unwrap()
            .unwrap()
            .state,
            JobPhase::Failed
        );
        assert!(repo.claim_stop_job().await.unwrap().is_none());
    }
}
