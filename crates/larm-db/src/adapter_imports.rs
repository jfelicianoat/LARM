//! P028: journal durable e idempotente para la importación inicial de paquetes.
use async_trait::async_trait;
use larm_domain::adapter_updates::{
    is_sha256, AdapterImport, AdapterImportJournal, AdapterImportPlan, UpdateStep,
};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{is_uuid_v7, timestamp, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::LockMode;
use larm_domain::ports::{AdapterImportRepository, JobSource, MutationContext, VolumeDemand};
use sqlx::{SqliteConnection, SqlitePool};

use crate::adapters::insert_row_in;
use crate::events::{append, NewEvent};
use crate::pool::db_error;

const KIND: &str = "adapter.import";
const OPERATION: &str = "P028_post__adapters_import";
const GENERATION: i64 = 1;

#[derive(Clone)]
pub struct SqlAdapterImports {
    pub pool: SqlitePool,
}

fn missing(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::NotFound,
        "Trabajo o instalación desconocidos",
        "not_found",
    )
    .with_resource(id)
}
fn invalid(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "El trabajo cambió de estado",
        "adapter_import_state_conflict",
    )
    .with_resource(id)
}
fn phase(step: UpdateStep) -> &'static str {
    match step {
        UpdateStep::Queued => "QUEUED",
        UpdateStep::Completed => "COMPLETED",
        UpdateStep::Failed => "FAILED",
        _ => "RUNNING",
    }
}
async fn write_lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando importación", &e))?;
    Ok(())
}
async fn load(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<AdapterImportJournal>, LarmError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT phase,payload,plan_hash FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(id)
    .bind(KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo importación", &e))?;
    row.map(|(stored_phase, payload, plan_hash)| {
        let record: AdapterImportJournal = serde_json::from_str(&payload)
            .map_err(|e| db_error("journal de importación ilegible", &e))?;
        if record.job_id != id
            || stored_phase != phase(record.step)
            || digest(&record)? != plan_hash
        {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "Journal de importación incoherente",
                "adapter_import_journal_invalid",
            )
            .with_resource(id));
        }
        Ok(record)
    })
    .transpose()
}
fn digest(record: &AdapterImportJournal) -> Result<String, LarmError> {
    larm_domain::canonical::sha256_hex(&serde_json::json!({
        "job_id":record.job_id,
        "actor_id":record.actor_id,
        "correlation_id":record.correlation_id,
        "request":record.request,
        "plan":record.plan,
    }))
    .map_err(|e| db_error("digest de importación", &e))
}
fn ctx(record: &AdapterImportJournal) -> MutationContext {
    MutationContext {
        actor_id: record.actor_id.clone(),
        correlation_id: record.correlation_id.clone(),
    }
}
async fn save(conn: &mut SqliteConnection, record: &AdapterImportJournal) -> Result<(), LarmError> {
    let resources = record.plan.as_ref().map_or_else(
        || vec![record.job_id.clone()],
        |p| vec![record.job_id.clone(), p.candidate.id.clone()],
    );
    let changed = sqlx::query("UPDATE operation_journal SET phase=?,payload=?,updated_at=?,resource_ids=?,plan_hash=? WHERE id=? AND kind=?")
        .bind(phase(record.step))
        .bind(serde_json::to_string(record).map_err(|e| db_error("serializando importación", &e))?)
        .bind(timestamp(chrono::Utc::now()))
        .bind(serde_json::to_string(&resources).map_err(|e| db_error("resources de importación", &e))?)
        .bind(digest(record)?)
        .bind(&record.job_id)
        .bind(KIND)
        .execute(conn)
        .await
        .map_err(|e| db_error("guardando importación", &e))?;
    if changed.rows_affected() != 1 {
        return Err(missing(&record.job_id));
    }
    Ok(())
}
async fn event(
    conn: &mut SqliteConnection,
    record: &AdapterImportJournal,
    event_type: &'static str,
) -> Result<(), LarmError> {
    let (resource_type, resource_id, version) = if event_type == "adapter.imported" {
        let candidate = record
            .plan
            .as_ref()
            .map(|p| p.candidate.id.as_str())
            .ok_or_else(|| invalid(&record.job_id))?;
        ("adapter_installation", candidate, 1)
    } else {
        ("adapter_import", record.job_id.as_str(), 1)
    };
    append(
        conn,
        &NewEvent {
            event_type,
            resource_type,
            resource_id,
            resource_version: version,
            data: serde_json::json!({
                "job_id":record.job_id,
                "step":record.step,
                "adapter_installation_id":record.plan.as_ref().map(|p| &p.candidate.id),
            }),
        },
        &ctx(record),
    )
    .await?;
    Ok(())
}
async fn fences(
    conn: &mut SqliteConnection,
    record: &AdapterImportJournal,
    plan: &AdapterImportPlan,
) -> Result<(), LarmError> {
    for (resource, wanted) in plan.locks() {
        let held: Option<String> = sqlx::query_scalar(
            "SELECT mode FROM resource_lock WHERE resource_id=? AND owner_id=? AND generation=?",
        )
        .bind(resource.key())
        .bind(&record.job_id)
        .bind(GENERATION)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("fencing de importación", &e))?;
        if !matches!(
            (held.as_deref(), wanted),
            (Some("EXCLUSIVE"), _) | (Some("SHARED"), LockMode::Shared)
        ) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El trabajo ya no tiene el bloqueo de escritura",
                "fence_lost",
            )
            .with_resource(&record.job_id));
        }
    }
    Ok(())
}
async fn release(conn: &mut SqliteConnection, id: &str) -> Result<(), LarmError> {
    sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("liberando reserva de importación", &e))?;
    sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=?")
        .bind(id)
        .bind(GENERATION)
        .execute(conn)
        .await
        .map_err(|e| db_error("liberando locks de importación", &e))?;
    Ok(())
}

#[async_trait]
impl AdapterImportRepository for SqlAdapterImports {
    async fn enqueue_import(
        &self,
        request: &AdapterImport,
        body_hash: &str,
        key: &str,
        mutation: &MutationContext,
    ) -> Result<(JobReceipt, bool), LarmError> {
        request.validate()?;
        if !is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 240
            || mutation.actor_id.is_empty()
            || !is_uuid_v7(&mutation.correlation_id)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de importación no válida",
                "adapter_import_request_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let stored: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&mutation.actor_id)
        .bind(OPERATION)
        .bind(key)
        .bind(timestamp(now))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo replay de importación", &e))?;
        if let Some((hash, response)) = stored {
            let receipt: JobReceipt = serde_json::from_str(&response)
                .map_err(|e| db_error("receipt de importación", &e))?;
            let record = load(&mut tx, &receipt.job_id)
                .await?
                .ok_or_else(|| missing(&receipt.job_id))?;
            if hash != body_hash
                || record.request != *request
                || record.actor_id != mutation.actor_id
            {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada para otra importación",
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok((receipt, false));
        }
        let record = AdapterImportJournal {
            job_id: uuid::Uuid::now_v7().to_string(),
            actor_id: mutation.actor_id.clone(),
            correlation_id: mutation.correlation_id.clone(),
            request: request.clone(),
            step: UpdateStep::Queued,
            plan: None,
            error: None,
        };
        let receipt = record.receipt();
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES (?,?,?,'QUEUED',?,?,?)")
            .bind(&record.job_id)
            .bind(KIND)
            .bind(serde_json::json!([record.job_id]).to_string())
            .bind(digest(&record)?)
            .bind(serde_json::to_string(&record).map_err(|e| db_error("journal de importación", &e))?)
            .bind(timestamp(now))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("encolando importación", &e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&mutation.actor_id)
            .bind(OPERATION)
            .bind(key)
            .bind(timestamp(now))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("retirando replay caducado", &e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,202,?,?)")
            .bind(&mutation.actor_id)
            .bind(OPERATION)
            .bind(key)
            .bind(body_hash)
            .bind(serde_json::to_string(&receipt).map_err(|e| db_error("receipt", &e))?)
            .bind(timestamp(now + chrono::Duration::days(30)))
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("guardando replay de importación", &e))?;
        event(&mut tx, &record, "adapter.import.queued").await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok((receipt, true))
    }

    async fn import_job(&self, id: &str) -> Result<Option<AdapterImportJournal>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        load(&mut conn, id).await
    }

    async fn pending_imports(&self) -> Result<Vec<AdapterImportJournal>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') ORDER BY id",
        )
        .bind(KIND)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("importaciones pendientes", &e))?;
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            result.push(load(&mut tx, &id).await?.ok_or_else(|| missing(&id))?);
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(result)
    }

    async fn queued_import(&self) -> Result<Option<AdapterImportJournal>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind=? AND phase='QUEUED' ORDER BY id LIMIT 1",
        )
        .bind(KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("siguiente importación", &e))?;
        let result = match id {
            Some(id) => load(&mut tx, &id).await?,
            None => None,
        };
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(result)
    }

    async fn advance_import(
        &self,
        id: &str,
        from: UpdateStep,
        to: UpdateStep,
    ) -> Result<(), LarmError> {
        if !from.next(to) {
            return Err(invalid(id));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if record.step != from {
            return Err(invalid(id));
        }
        if let Some(plan) = &record.plan {
            fences(&mut tx, &record, plan).await?;
        }
        record.step = to;
        save(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn prepare_import(
        &self,
        id: &str,
        plan: &AdapterImportPlan,
        demand: &VolumeDemand,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if record.step != UpdateStep::Inspecting {
            return Err(invalid(id));
        }
        plan.validate()?;
        if demand.required_bytes != plan.required_bytes.get()
            || demand.volume_key != plan.volume_key
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La reserva no cubre el plan",
                "adapter_import_reservation_mismatch",
            ));
        }
        let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM adapter_installation WHERE adapter_id=? COLLATE NOCASE AND version=? COLLATE NOCASE")
            .bind(&plan.candidate.adapter_id)
            .bind(&plan.candidate.version)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_error("identidad del candidato", &e))?;
        if existing != 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La versión ya está registrada",
                "adapter_version_exists",
            ));
        }
        crate::guard::acquire_in(&mut tx, id, GENERATION, &plan.locks(), None).await?;
        crate::reservations::admit_in(&mut tx, id, std::slice::from_ref(demand), observed_at, None)
            .await?;
        record.plan = Some(plan.clone());
        record.step = UpdateStep::Reserved;
        save(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn complete_import(&self, id: &str) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if record.step == UpdateStep::Completed {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if record.step != UpdateStep::Publishing {
            return Err(invalid(id));
        }
        let plan = record.plan.as_ref().ok_or_else(|| invalid(id))?;
        plan.validate()?;
        fences(&mut tx, &record, plan).await?;
        let reserved: Option<i64> = sqlx::query_scalar("SELECT total_bytes FROM volume_reservation WHERE job_id=? AND volume_key=? AND expires_at IS NULL")
            .bind(id)
            .bind(&plan.volume_key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("reserva de importación", &e))?;
        if reserved.is_none_or(|b| b < 0 || (b as u64) < plan.required_bytes.get()) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El trabajo perdió su reserva",
                "adapter_import_reservation_missing",
            ));
        }
        insert_row_in(&mut tx, &plan.candidate).await?;
        record.step = UpdateStep::Completed;
        save(&mut tx, &record).await?;
        release(&mut tx, id).await?;
        event(&mut tx, &record, "adapter.imported").await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn fail_import(&self, id: &str, error: &LarmError) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if record.step == UpdateStep::Failed {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if record.step == UpdateStep::Completed {
            return Err(invalid(id));
        }
        record.error = Some(error.clone());
        record.step = UpdateStep::Failed;
        save(&mut tx, &record).await?;
        release(&mut tx, id).await?;
        event(&mut tx, &record, "adapter.import.failed").await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }
}

#[async_trait]
impl JobSource for SqlAdapterImports {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        Ok(self
            .import_job(id)
            .await?
            .filter(|r| r.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
            .map(|r| r.public_state()))
    }
}
