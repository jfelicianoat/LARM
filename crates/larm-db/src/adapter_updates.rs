//! P024: aceptación idempotente y transacciones del journal de actualización.
//! No hace IO de paquetes: el host comprueba bytes y quiescencia antes de
//! completar o fallar. Los trabajos pendientes se devuelven, no se caducan.
use async_trait::async_trait;
use larm_domain::adapter_updates::{
    is_sha256, AdapterImport, AdapterUpdate, UpdatePlan, UpdateStep, UPDATE_GENERATION,
    UPDATE_KIND, UPDATE_OPERATION,
};
use larm_domain::adapters::AdapterInstallation;
use larm_domain::approval::{ApprovalRecord, ApprovalUse};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{is_uuid_v7, timestamp, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::LockMode;
use larm_domain::ports::{AdapterUpdateRepository, JobSource, MutationContext, VolumeDemand};
use sqlx::{SqliteConnection, SqlitePool};

use crate::adapters::{insert_in, to_installation, COLUMNS};
use crate::events::{append, NewEvent};
use crate::pool::db_error;

#[derive(Clone)]
pub struct SqlAdapterUpdates {
    pub pool: SqlitePool,
}

fn invalid(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "El trabajo cambió de estado",
        "adapter_update_state_conflict",
    )
    .with_resource(id)
}
fn missing(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::NotFound,
        "Trabajo o instalación desconocidos",
        "not_found",
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
        .map_err(|e| db_error("bloqueando actualización", &e))?;
    Ok(())
}
async fn installation(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<AdapterInstallation, LarmError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM adapter_installation WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo instalación de actualización", &e))?
    .ok_or_else(|| missing(id))?;
    to_installation(&row)
}
async fn load(conn: &mut SqliteConnection, id: &str) -> Result<Option<AdapterUpdate>, LarmError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT phase,payload,plan_hash FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(id)
    .bind(UPDATE_KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo actualización", &e))?;
    row.map(|(stored_phase, payload, plan_hash)| {
        let record: AdapterUpdate = serde_json::from_str(&payload)
            .map_err(|e| db_error("journal de actualización ilegible", &e))?;
        if record.job_id != id
            || stored_phase != phase(record.step)
            || digest(&record)? != plan_hash
        {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "Journal de actualización incoherente",
                "adapter_update_journal_invalid",
            )
            .with_resource(id));
        }
        Ok(record)
    })
    .transpose()
}
fn digest(record: &AdapterUpdate) -> Result<String, LarmError> {
    larm_domain::canonical::sha256_hex(&serde_json::json!({
        "job_id":record.job_id,"actor_id":record.actor_id,"correlation_id":record.correlation_id,
        "target":record.target_installation_id,"target_row_version":record.target_row_version.to_string(),
        "request":record.request,"plan":record.plan,
    })).map_err(|e| db_error("digest de actualización", &e))
}
fn ctx(record: &AdapterUpdate) -> MutationContext {
    MutationContext {
        actor_id: record.actor_id.clone(),
        correlation_id: record.correlation_id.clone(),
    }
}
async fn save(conn: &mut SqliteConnection, record: &AdapterUpdate) -> Result<(), LarmError> {
    let resources = record.plan.as_ref().map_or_else(
        || vec![record.target_installation_id.clone()],
        |p| {
            vec![
                record.target_installation_id.clone(),
                p.candidate.id.clone(),
            ]
        },
    );
    let changed = sqlx::query("UPDATE operation_journal SET phase=?,payload=?,updated_at=?,resource_ids=?,plan_hash=? WHERE id=? AND kind=?")
        .bind(phase(record.step)).bind(serde_json::to_string(record).map_err(|e| db_error("serializando actualización", &e))?)
        .bind(timestamp(chrono::Utc::now())).bind(serde_json::to_string(&resources).map_err(|e| db_error("resources", &e))?)
        .bind(digest(record)?).bind(&record.job_id).bind(UPDATE_KIND).execute(conn).await.map_err(|e| db_error("guardando actualización", &e))?;
    if changed.rows_affected() != 1 {
        return Err(missing(&record.job_id));
    }
    Ok(())
}
async fn event(conn: &mut SqliteConnection, record: &AdapterUpdate) -> Result<(), LarmError> {
    // El origen puede haber cambiado mientras había IO. Un evento terminal
    // no retrocede resource_version respecto a la fila actual del recurso.
    let version: Option<i64> =
        sqlx::query_scalar("SELECT row_version FROM adapter_installation WHERE id=?")
            .bind(&record.target_installation_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| db_error("revisión del evento de actualización", &e))?;
    append(conn, &NewEvent { event_type:"adapter.updated", resource_type:"adapter_installation",
        resource_id:&record.target_installation_id, resource_version:version.unwrap_or(record.target_row_version),
        data:serde_json::json!({"action":"update_job_changed","job_id":record.job_id,"step":record.step})
    }, &ctx(record)).await?;
    Ok(())
}
fn target_unchanged(target: &AdapterInstallation, record: &AdapterUpdate) -> Result<(), LarmError> {
    if target.row_version != record.target_row_version {
        return Err(LarmError::new(
            ErrorCode::StaleRevision,
            "La instalación cambió durante la actualización",
            "adapter_update_target_stale",
        )
        .with_resource(&target.id));
    }
    Ok(())
}
async fn fences(
    conn: &mut SqliteConnection,
    record: &AdapterUpdate,
    plan: &UpdatePlan,
) -> Result<(), LarmError> {
    for (resource, wanted) in plan.locks() {
        let held: Option<String> = sqlx::query_scalar(
            "SELECT mode FROM resource_lock WHERE resource_id=? AND owner_id=? AND generation=?",
        )
        .bind(resource.key())
        .bind(&record.job_id)
        .bind(UPDATE_GENERATION)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("fencing de actualización", &e))?;
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
        .map_err(|e| db_error("liberando reserva de actualización", &e))?;
    sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=?")
        .bind(id)
        .bind(UPDATE_GENERATION)
        .execute(conn)
        .await
        .map_err(|e| db_error("liberando locks de actualización", &e))?;
    Ok(())
}

#[async_trait]
impl AdapterUpdateRepository for SqlAdapterUpdates {
    async fn enqueue_update(
        &self,
        target: &str,
        request: &AdapterImport,
        body_hash: &str,
        key: &str,
        mutation: &MutationContext,
    ) -> Result<(JobReceipt, bool), LarmError> {
        request.validate()?;
        if !is_uuid_v7(target)
            || !is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 240
            || mutation.actor_id.is_empty()
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de actualización no válida",
                "adapter_update_request_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let stored: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        ).bind(&mutation.actor_id).bind(UPDATE_OPERATION).bind(key).bind(timestamp(now)).fetch_optional(&mut *tx).await
            .map_err(|e| db_error("leyendo replay de actualización", &e))?;
        if let Some((hash, response)) = stored {
            let receipt: JobReceipt = serde_json::from_str(&response)
                .map_err(|e| db_error("receipt de actualización", &e))?;
            let record = load(&mut tx, &receipt.job_id)
                .await?
                .ok_or_else(|| missing(&receipt.job_id))?;
            if hash != body_hash
                || record.target_installation_id != target
                || record.request != *request
                || record.actor_id != mutation.actor_id
            {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada para otra actualización",
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok((receipt, false));
        }
        let target = installation(&mut tx, target).await?;
        if !matches!(
            target.state,
            larm_domain::adapters::AdapterState::Installed
                | larm_domain::adapters::AdapterState::Validated
                | larm_domain::adapters::AdapterState::Failed
        ) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación no admite actualización",
                "adapter_update_target_invalid",
            )
            .with_resource(&target.id));
        }
        let record = AdapterUpdate {
            job_id: uuid::Uuid::now_v7().to_string(),
            actor_id: mutation.actor_id.clone(),
            correlation_id: mutation.correlation_id.clone(),
            target_installation_id: target.id.clone(),
            target_row_version: target.row_version,
            request: request.clone(),
            step: UpdateStep::Queued,
            plan: None,
            error: None,
        };
        let receipt = record.receipt();
        let hash = digest(&record)?;
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES (?,?,?,'QUEUED',?,?,?)")
            .bind(&record.job_id).bind(UPDATE_KIND).bind(serde_json::json!([target.id]).to_string()).bind(hash)
            .bind(serde_json::to_string(&record).map_err(|e| db_error("journal de actualización", &e))?).bind(timestamp(now))
            .execute(&mut *tx).await.map_err(|e| db_error("encolando actualización", &e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&mutation.actor_id).bind(UPDATE_OPERATION).bind(key).bind(timestamp(now)).execute(&mut *tx).await
            .map_err(|e| db_error("retirando replay caducado", &e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,202,?,?)")
            .bind(&mutation.actor_id).bind(UPDATE_OPERATION).bind(key).bind(body_hash)
            .bind(serde_json::to_string(&receipt).map_err(|e| db_error("receipt", &e))?).bind(timestamp(now + chrono::Duration::days(30)))
            .execute(&mut *tx).await.map_err(|e| db_error("guardando replay de actualización", &e))?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok((receipt, true))
    }

    async fn update_job(&self, id: &str) -> Result<Option<AdapterUpdate>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        load(&mut conn, id).await
    }

    async fn pending_updates(&self) -> Result<Vec<AdapterUpdate>, LarmError> {
        // Una sola instantánea: no mezclar ids RUNNING con fases leídas después.
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') ORDER BY id")
            .bind(UPDATE_KIND).fetch_all(&mut *tx).await.map_err(|e| db_error("actualizaciones pendientes", &e))?;
        let mut result = Vec::new();
        for id in ids {
            result.push(load(&mut tx, &id).await?.ok_or_else(|| missing(&id))?);
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(result)
    }

    async fn queued_update(&self) -> Result<Option<AdapterUpdate>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM operation_journal WHERE kind=? AND phase='QUEUED' ORDER BY id LIMIT 1",
        )
        .bind(UPDATE_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("siguiente actualización", &e))?;
        let result = match id {
            Some(id) => load(&mut tx, &id).await?,
            None => None,
        };
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(result)
    }

    async fn advance_update(
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
        let target = installation(&mut tx, &record.target_installation_id).await?;
        target_unchanged(&target, &record)?;
        if let Some(plan) = &record.plan {
            fences(&mut tx, &record, plan).await?;
        }
        record.step = to;
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn prepare_update(
        &self,
        id: &str,
        plan: &UpdatePlan,
        demand: &VolumeDemand,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if record.step != UpdateStep::Inspecting {
            return Err(invalid(id));
        }
        let target = installation(&mut tx, &record.target_installation_id).await?;
        target_unchanged(&target, &record)?;
        plan.validate(&target)?;
        if demand.required_bytes != plan.required_bytes.get()
            || demand.volume_key != plan.volume_key
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La reserva no cubre el plan",
                "adapter_update_reservation_mismatch",
            ));
        }
        let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM adapter_installation WHERE adapter_id=? COLLATE NOCASE AND version=? COLLATE NOCASE")
            .bind(&plan.candidate.adapter_id).bind(&plan.candidate.version).fetch_one(&mut *tx).await
            .map_err(|e| db_error("identidad del candidato", &e))?;
        if existing != 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La versión ya está registrada",
                "adapter_version_exists",
            ));
        }
        crate::guard::acquire_in(&mut tx, id, UPDATE_GENERATION, &plan.locks(), None).await?;
        crate::reservations::admit_in(&mut tx, id, std::slice::from_ref(demand), observed_at, None)
            .await?;
        if let Some(proof) = &plan.approval {
            if record.request.approval_id.as_deref() != Some(proof.id.as_str()) {
                return Err(LarmError::new(
                    ErrorCode::ApprovalRequired,
                    "La aprobación no corresponde al trabajo",
                    "approval_id_mismatch",
                ));
            }
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='approval'",
            )
            .bind(&proof.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("aprobación de actualización", &e))?;
            let (approval_phase, payload) = row.ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ApprovalRequired,
                    "Falta la aprobación",
                    "approval_missing",
                )
            })?;
            if approval_phase != "APPROVED" {
                return Err(LarmError::new(
                    ErrorCode::ApprovalRequired,
                    "La aprobación ya se usó",
                    "approval_consumed",
                ));
            }
            let approval: ApprovalRecord =
                serde_json::from_str(&payload).map_err(|e| db_error("aprobación ilegible", &e))?;
            let now = chrono::Utc::now();
            approval.check(
                &ApprovalUse {
                    actor_id: &record.actor_id,
                    operation_digest: &proof.operation_digest,
                    resource_ids: &proof.resource_ids,
                    code_hash: Some(&proof.code_hash),
                },
                now,
            )?;
            sqlx::query("UPDATE operation_journal SET phase='CONSUMED',updated_at=?,payload=json_set(payload,'$.consumed_at',?) WHERE id=? AND kind='approval' AND phase='APPROVED'")
                .bind(timestamp(now)).bind(timestamp(now)).bind(&proof.id).execute(&mut *tx).await.map_err(|e| db_error("consumiendo aprobación de actualización", &e))?;
        }
        record.plan = Some(plan.clone());
        record.step = UpdateStep::Reserved;
        save(&mut tx, &record).await?;
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn complete_update(&self, id: &str) -> Result<(), LarmError> {
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
        let target = installation(&mut tx, &record.target_installation_id).await?;
        target_unchanged(&target, &record)?;
        plan.validate(&target)?;
        fences(&mut tx, &record, plan).await?;
        let reserved: Option<i64> = sqlx::query_scalar("SELECT total_bytes FROM volume_reservation WHERE job_id=? AND volume_key=? AND expires_at IS NULL")
            .bind(id).bind(&plan.volume_key).fetch_optional(&mut *tx).await.map_err(|e| db_error("reserva de actualización", &e))?;
        if reserved.is_none_or(|b| b < 0 || (b as u64) < plan.required_bytes.get()) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El trabajo perdió su reserva",
                "adapter_update_reservation_missing",
            ));
        }
        insert_in(&mut tx, &plan.candidate, &ctx(&record)).await?;
        record.step = UpdateStep::Completed;
        save(&mut tx, &record).await?;
        release(&mut tx, id).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn fail_update(&self, id: &str, error: &LarmError) -> Result<(), LarmError> {
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
        event(&mut tx, &record).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }
}

#[async_trait]
impl JobSource for SqlAdapterUpdates {
    async fn job(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<Option<JobState>, LarmError> {
        Ok(self
            .update_job(job_id)
            .await?
            .filter(|r| r.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
            .map(|r| r.public_state()))
    }
}
