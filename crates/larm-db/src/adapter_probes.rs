//! Cola P026, replay por actor/cuerpo/destino y resultado unido a conformidad.
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::adapter_probes::{self, ProbeJob, KIND, OPERATION};
use larm_domain::adapters::{AdapterState, AdapterTrust};
use larm_domain::bridge::{timestamp, JobPhase};
use larm_domain::conformance::{hash, Outcome, Receipt, Run};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::Probe;
use larm_domain::ports::{AdapterProbeRepository, MutationContext};
use serde_json::json;
use sqlx::{SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlAdapterProbes {
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
        "No se pudo confirmar el sondeo",
        reason,
    )
}
async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando sondeos", &e))?;
    Ok(())
}
fn decode(
    id: &str,
    phase: &str,
    payload: &str,
    digest: &str,
    resources: &str,
) -> Result<ProbeJob, LarmError> {
    let record: ProbeJob =
        serde_json::from_str(payload).map_err(|_| invalid("probe_journal_invalid"))?;
    let refs: Vec<String> =
        serde_json::from_str(resources).map_err(|_| invalid("probe_journal_invalid"))?;
    let expected = match record.phase {
        JobPhase::Queued => 1,
        JobPhase::Running => 2,
        _ => 3,
    };
    if record.id != id
        || !larm_domain::bridge::is_uuid_v7(id)
        || phase_text(record.phase) != phase
        || record.version != expected
        || refs != vec![record.target.clone()]
        || hash(&record)? != digest
        || !crate::adapter_probes::context_valid(&record.context())
        || !larm_domain::adapter_updates::is_sha256(&record.installation_sha256)
        || adapter_probes::validate_request(&record.target, &record.request).is_err()
        || (record.phase == JobPhase::Completed
            && !record
                .receipt
                .as_ref()
                .is_some_and(|r| r.passed && r.installation_id == record.target))
        || (record.phase == JobPhase::Failed && record.error.is_none())
        || (matches!(record.phase, JobPhase::Queued | JobPhase::Running)
            && (record.receipt.is_some() || record.error.is_some()))
    {
        return Err(invalid("probe_journal_invalid"));
    }
    Ok(record)
}
pub(crate) async fn load(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<ProbeJob>, LarmError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT phase,payload,plan_hash,resource_ids FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(id)
    .bind(KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo sondeo", &e))?;
    row.map(|(phase, payload, digest, resources)| decode(id, &phase, &payload, &digest, &resources))
        .transpose()
}
async fn save(conn: &mut SqliteConnection, record: &ProbeJob) -> Result<(), LarmError> {
    let written=sqlx::query("UPDATE operation_journal SET phase=?,payload=?,plan_hash=?,updated_at=? WHERE id=? AND kind=?")
        .bind(phase_text(record.phase)).bind(serde_json::to_string(record).map_err(|e|db_error("serializando sondeo",&e))?).bind(hash(record)?).bind(timestamp(chrono::Utc::now())).bind(&record.id).bind(KIND).execute(conn).await.map_err(|e|db_error("guardando sondeo",&e))?;
    if written.rows_affected() != 1 {
        return Err(invalid("probe_journal_missing"));
    }
    Ok(())
}
async fn event(conn: &mut SqliteConnection, record: &ProbeJob) -> Result<(), LarmError> {
    append(conn,&NewEvent{event_type:"adapter.updated",resource_type:"job",resource_id:&record.id,resource_version:i64::from(record.version),data:json!({"action":"probe_state_changed","job_id":record.id,"installation_id":record.target,"state":record.phase})},&record.context()).await?;
    Ok(())
}
pub(crate) fn context_valid(ctx: &MutationContext) -> bool {
    !ctx.actor_id.is_empty()
        && ctx.actor_id.len() <= 256
        && !ctx.actor_id.chars().any(char::is_control)
        && larm_domain::bridge::is_uuid_v7(&ctx.correlation_id)
}
#[async_trait]
impl AdapterProbeRepository for SqlAdapterProbes {
    async fn enqueue_probe(
        &self,
        target: &str,
        request: &Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<ProbeJob, LarmError> {
        adapter_probes::validate_request(target, request)?;
        if !context_valid(ctx)
            || !larm_domain::adapter_updates::is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 256
            || key.chars().any(char::is_control)
        {
            return Err(invalid("probe_request_invalid"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let replay:Option<(String,String)>=sqlx::query_as("SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?").bind(&ctx.actor_id).bind(OPERATION).bind(key).bind(timestamp(now)).fetch_optional(&mut *tx).await.map_err(|e|db_error("replay de sondeo",&e))?;
        if let Some((old_hash, payload)) = replay {
            let replay: serde_json::Value =
                serde_json::from_str(&payload).map_err(|_| invalid("probe_replay_invalid"))?;
            if old_hash != body_hash || replay["target"] != target {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otra solicitud",
                    "idempotency_body_mismatch",
                ));
            }
            let id = replay["job_id"]
                .as_str()
                .ok_or_else(|| invalid("probe_replay_invalid"))?;
            let record = load(&mut tx, id)
                .await?
                .ok_or_else(|| invalid("probe_replay_invalid"))?;
            if record.actor_id != ctx.actor_id || record.request != *request {
                return Err(invalid("probe_replay_invalid"));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(record);
        }
        let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
            .bind(target)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("instalación de sondeo", &e))?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instalación desconocida",
                    "adapter_installation_not_found",
                )
            })?;
        let a = crate::adapters::to_installation(&row)?;
        if a.trust == AdapterTrust::Revoked {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Adaptador revocado",
                "adapter_revoked",
            ));
        }
        if !matches!(a.state, AdapterState::Installed | AdapterState::Validated) {
            return Err(invalid("adapter_not_installed"));
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.target')=?)").bind(KIND).bind(target).fetch_one(&mut *tx).await.map_err(|e|db_error("sondeo activo",&e))?;
        if active {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe un sondeo activo",
                "adapter_probe_active",
            ));
        }
        let record = ProbeJob {
            id: uuid::Uuid::now_v7().to_string(),
            target: target.into(),
            actor_id: ctx.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            request: request.clone(),
            installation_sha256: adapter_probes::snapshot(&a)?,
            phase: JobPhase::Queued,
            version: 1,
            receipt: None,
            error: None,
            authority_pending: Some(false),
        };
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES (?,?,?,'QUEUED',?,?,?)").bind(&record.id).bind(KIND).bind(json!([target]).to_string()).bind(hash(&record)?).bind(serde_json::to_string(&record).map_err(|e|db_error("sondeo",&e))?).bind(timestamp(now)).execute(&mut *tx).await.map_err(|e|db_error("encolando sondeo",&e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?").bind(&ctx.actor_id).bind(OPERATION).bind(key).bind(timestamp(now)).execute(&mut *tx).await.map_err(|e|db_error("replay caducado",&e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,202,?,?)").bind(&ctx.actor_id).bind(OPERATION).bind(key).bind(body_hash).bind(json!({"target":target,"job_id":record.id}).to_string()).bind(timestamp(now+chrono::Duration::days(30))).execute(&mut *tx).await.map_err(|e|db_error("replay de sondeo",&e))?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record)
    }
    async fn claim_probe(&self) -> Result<Option<ProbeJob>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let id:Option<String>=sqlx::query_scalar("SELECT q.id FROM operation_journal q WHERE q.kind=? AND q.phase='QUEUED' AND NOT EXISTS(SELECT 1 FROM operation_journal r WHERE r.kind=q.kind AND r.phase='RUNNING' AND json_extract(r.payload,'$.target')=json_extract(q.payload,'$.target')) ORDER BY q.id LIMIT 1").bind(KIND).fetch_optional(&mut *tx).await.map_err(|e|db_error("cola de sondeos",&e))?;
        let Some(id) = id else {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(None);
        };
        let mut record = load(&mut tx, &id)
            .await?
            .ok_or_else(|| invalid("probe_journal_invalid"))?;
        record.phase = JobPhase::Running;
        record.version = 2;
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(Some(record))
    }
    async fn probe_job(&self, id: &str) -> Result<Option<ProbeJob>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        load(&mut conn, id).await
    }
    async fn fail_probe(
        &self,
        id: &str,
        error: &LarmError,
        authority_pending: bool,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let mut record = load(&mut tx, id)
            .await?
            .ok_or_else(|| invalid("probe_journal_missing"))?;
        if matches!(record.phase, JobPhase::Completed | JobPhase::Failed) {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if record.phase != JobPhase::Running {
            return Err(invalid("probe_not_running"));
        }
        record.authority_pending = Some(authority_pending);
        record.phase = JobPhase::Failed;
        record.version = 3;
        record.error = Some(adapter_probes::public_failure(
            error,
            &record.correlation_id,
        ));
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }
}
/// Invocado dentro del MISMO COMMIT que instalación/recibo de conformidad.
pub(crate) async fn finish_with_receipt(
    conn: &mut SqliteConnection,
    id: &str,
    run: &Run,
    outcome: &Outcome,
    receipt: &Receipt,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let mut record = load(conn, id)
        .await?
        .ok_or_else(|| invalid("probe_journal_missing"))?;
    if record.target != run.installation_id
        || record.installation_sha256 != run.installation_sha256
        || record.actor_id != ctx.actor_id
        || record.correlation_id != ctx.correlation_id
    {
        return Err(invalid("probe_binding_mismatch"));
    }
    if matches!(record.phase, JobPhase::Completed | JobPhase::Failed) {
        return if record.receipt.as_ref() == Some(receipt) {
            Ok(())
        } else {
            Err(invalid("probe_outcome_mismatch"))
        };
    }
    if record.phase != JobPhase::Running {
        return Err(invalid("probe_not_running"));
    }
    record.phase = if receipt.passed {
        JobPhase::Completed
    } else {
        JobPhase::Failed
    };
    record.version = 3;
    record.authority_pending = Some(false);
    record.receipt = Some(receipt.clone());
    if let Outcome::Failed { error_code } = outcome {
        record.error = Some(adapter_probes::public_failure(
            &LarmError::new(
                ErrorCode::from(error_code.as_str()),
                "Fallo de conformidad",
                "conformance_failed",
            ),
            &ctx.correlation_id,
        ));
    }
    save(conn, &record).await?;
    event(conn, &record).await
}
