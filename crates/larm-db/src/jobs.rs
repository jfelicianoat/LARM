//! Jobs largos sobre `operation_journal` (P011 refresco de hardware, P014
//! sondeo de ubicaciones).

use async_trait::async_trait;
use larm_domain::bridge::{JobPhase, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{JournalJob, JournalJobRepository, MutationContext};
use sqlx::SqlitePool;

use super::pool::db_error;

/// Jobs en `operation_journal` de un tipo (`kind`): fase = estado del job y
/// `payload` con el actor, la correlación, el resultado y el error. Un solo
/// job activo (QUEUED o RUNNING) por tipo.
#[derive(Clone)]
pub struct SqlJournalJobs {
    pub pool: SqlitePool,
    pub kind: &'static str,
    /// `resource_type` del `JobState` (`hardware_profile`, `storage_location`).
    pub resource_type: &'static str,
}

impl SqlJournalJobs {
    /// Refrescos de hardware (P011): `hardware.refresh`.
    #[must_use]
    pub fn hardware(pool: SqlitePool) -> Self {
        Self {
            pool,
            kind: larm_domain::hardware::REFRESH_JOB_KIND,
            resource_type: "hardware_profile",
        }
    }

    /// Sondeos de ubicaciones (P014): `storage.probe`.
    #[must_use]
    pub fn storage(pool: SqlitePool) -> Self {
        Self {
            pool,
            kind: larm_domain::locations::PROBE_JOB_KIND,
            resource_type: "storage_location",
        }
    }

    /// Admisión de una lease del puente Broker–LARM (P100/P107).
    #[must_use]
    pub fn broker_lease_admission(pool: SqlitePool) -> Self {
        Self {
            pool,
            kind: larm_domain::ports::BROKER_LEASE_ADMISSION_JOB_KIND,
            resource_type: "runtime_lease",
        }
    }
}

fn job_from_row(
    id: String,
    phase: &str,
    payload: &str,
    resource_type: &str,
) -> Result<JournalJob, LarmError> {
    let payload: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| db_error("payload de job", &e))?;
    let state: JobPhase = serde_json::from_value(serde_json::Value::String(phase.into()))
        .map_err(|e| db_error("fase de job desconocida", &e))?;
    let error = match payload.get("error") {
        Some(v) if !v.is_null() => {
            Some(serde_json::from_value(v.clone()).map_err(|e| db_error("error de job", &e))?)
        }
        _ => None,
    };
    Ok(JournalJob {
        actor_id: payload
            .get("actor_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        state: JobState {
            job_id: id,
            state,
            resource_type: resource_type.into(),
            resource_id: payload
                .get("resource_id")
                .and_then(serde_json::Value::as_str)
                .map(String::from),
            progress: serde_json::Map::new(),
            result: payload.get("result").filter(|v| !v.is_null()).cloned(),
            error,
            evidence_refs: payload
                .get("evidence_refs")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
        },
    })
}

impl SqlJournalJobs {
    /// Cambia de fase sólo desde las fases `from`; si no, `INVALID_STATE_TRANSITION`.
    async fn transition(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        patch: serde_json::Value,
    ) -> Result<(), LarmError> {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let placeholders = vec!["?"; from.len()].join(",");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE operation_journal SET phase = ?, updated_at = ?, payload = json_patch(payload, ?) \
             WHERE id = ? AND kind = ? AND phase IN ({placeholders})"
        )))
        .bind(to)
        .bind(now)
        .bind(patch.to_string())
        .bind(id)
        .bind(self.kind);
        for phase in from {
            query = query.bind(*phase);
        }
        let done = query
            .execute(&self.pool)
            .await
            .map_err(|e| db_error("actualizando job de hardware", &e))?;
        if done.rows_affected() == 0 {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El job no está en un estado que permita ese cambio",
                "job_state_conflict",
            )
            .with_resource(id));
        }
        Ok(())
    }
}

#[async_trait]
impl JournalJobRepository for SqlJournalJobs {
    async fn enqueue(
        &self,
        ctx: &MutationContext,
        resource_id: Option<&str>,
    ) -> Result<(JournalJob, bool), LarmError> {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // El UPDATE inocuo toma el bloqueo de escritura antes de mirar: dos
        // peticiones simultáneas no encolan dos sondeos.
        let active: Option<(String, String, String)> = sqlx::query_as(
            "UPDATE operation_journal SET updated_at = updated_at \
             WHERE kind = ? AND phase IN ('QUEUED','RUNNING') RETURNING id, phase, payload",
        )
        .bind(self.kind)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("buscando job de hardware activo", &e))?;
        if let Some((id, phase, payload)) = active {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok((
                job_from_row(id, &phase, &payload, self.resource_type)?,
                false,
            ));
        }
        let id = uuid::Uuid::now_v7().to_string();
        let payload = serde_json::json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "resource_id": resource_id,
            "result": null,
            "error": null,
            "evidence_refs": [],
        })
        .to_string();
        let resources: Vec<&str> = resource_id.into_iter().collect();
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES (?, ?, ?, 'QUEUED', ?, ?, ?)",
        )
        .bind(&id)
        .bind(self.kind)
        .bind(serde_json::to_string(&resources).unwrap_or_else(|_| "[]".into()))
        .bind(self.kind)
        .bind(&payload)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("encolando job de hardware", &e))?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok((
            job_from_row(id, "QUEUED", &payload, self.resource_type)?,
            true,
        ))
    }

    async fn running(&self, id: &str) -> Result<(), LarmError> {
        self.transition(id, &["QUEUED"], "RUNNING", serde_json::json!({}))
            .await
    }

    async fn complete(
        &self,
        id: &str,
        resource_id: &str,
        result: serde_json::Value,
        evidence_refs: &[String],
    ) -> Result<(), LarmError> {
        self.transition(
            id,
            &["RUNNING"],
            "COMPLETED",
            serde_json::json!({
                "resource_id": resource_id,
                "result": result,
                "evidence_refs": evidence_refs,
            }),
        )
        .await
    }

    async fn fail(&self, id: &str, error: &LarmError) -> Result<(), LarmError> {
        self.transition(
            id,
            &["QUEUED", "RUNNING"],
            "FAILED",
            serde_json::json!({ "error": error }),
        )
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<JournalJob>, LarmError> {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, phase, payload FROM operation_journal WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(self.kind)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo job de hardware", &e))?;
        row.map(|(id, phase, payload)| job_from_row(id, &phase, &payload, self.resource_type))
            .transpose()
    }

    async fn recover(&self) -> Result<u64, LarmError> {
        let error = LarmError::new(
            ErrorCode::Internal,
            "El daemon se detuvo antes de terminar el sondeo",
            "job_interrupted",
        )
        .retryable(true);
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let done = sqlx::query(
            "UPDATE operation_journal SET phase = 'FAILED', updated_at = ?, payload = json_patch(payload, ?) \
             WHERE kind = ? AND phase IN ('QUEUED','RUNNING')",
        )
        .bind(now)
        .bind(serde_json::json!({ "error": error }).to_string())
        .bind(self.kind)
        .execute(&self.pool)
        .await
        .map_err(|e| db_error("recuperando jobs de hardware", &e))?;
        Ok(done.rows_affected())
    }
}
