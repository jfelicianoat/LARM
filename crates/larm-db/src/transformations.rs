//! Recuperación durable de jobs de transformación (P093/H07).

use larm_domain::{
    auth::{ApiScope, Principal},
    bridge::{ApiError, JobPhase, JobReceipt, JobState},
    error::{ErrorCode, LarmError},
    ports::{MutationContext, TransformationService},
    transformations::{
        ExecuteTransformationPlan, TransformationActivationIdempotency, TransformationJob,
        TransformationPlanResponse, TransformationRequest, TransformationState,
    },
};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::{events::NewEvent, pool::db_error};

const JOURNAL_KIND: &str = "transformation.job";

pub(crate) mod dispatch;
mod lifecycle;
pub use lifecycle::{CommittedTransformationPublication, PreparedTransformationPublication};

#[derive(Clone)]
pub struct SqlTransformationJobs {
    pub pool: SqlitePool,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo reconciliar el trabajo de transformación",
        reason,
    )
}

fn decode_job(row: &sqlx::sqlite::SqliteRow) -> Result<TransformationJob, LarmError> {
    let metadata: String = row
        .try_get("metadata")
        .map_err(|error| db_error("metadata de transformación", &error))?;
    let parameters: String = row
        .try_get("parameters")
        .map_err(|error| db_error("parámetros de transformación", &error))?;
    let error: Option<String> = row
        .try_get("error")
        .map_err(|error| db_error("error de transformación", &error))?;
    let job: TransformationJob = serde_json::from_value(json!({
        "id": row.try_get::<String, _>("id").map_err(|error| db_error("id de transformación", &error))?,
        "created_at": row.try_get::<String, _>("created_at").map_err(|error| db_error("creación de transformación", &error))?,
        "updated_at": row.try_get::<String, _>("updated_at").map_err(|error| db_error("actualización de transformación", &error))?,
        "row_version": row.try_get::<i64, _>("row_version").map_err(|error| db_error("revisión de transformación", &error))?,
        "metadata": serde_json::from_str::<Value>(&metadata).map_err(|error| db_error("metadata de transformación", &error))?,
        "input_variant_id": row.try_get::<String, _>("input_variant_id").map_err(|error| db_error("variante de entrada", &error))?,
        "output_variant_id": row.try_get::<Option<String>, _>("output_variant_id").map_err(|error| db_error("variante de salida", &error))?,
        "adapter_id": row.try_get::<String, _>("adapter_id").map_err(|error| db_error("adaptador de transformación", &error))?,
        "tool_version": row.try_get::<String, _>("tool_version").map_err(|error| db_error("versión de herramienta", &error))?,
        "parameters": serde_json::from_str::<Value>(&parameters).map_err(|error| db_error("parámetros de transformación", &error))?,
        "plan_hash": row.try_get::<String, _>("plan_hash").map_err(|error| db_error("hash de plan", &error))?,
        "state": row.try_get::<String, _>("state").map_err(|error| db_error("estado de transformación", &error))?,
        "attempt": u32::try_from(row.try_get::<i64, _>("attempt").map_err(|error| db_error("intento de transformación", &error))?).map_err(|error| db_error("intento de transformación", &error))?,
        "error": error.map(|text| serde_json::from_str::<Value>(&text)).transpose().map_err(|error| db_error("error de transformación", &error))?,
    }))
    .map_err(|error| db_error("registro de transformación", &error))?;
    job.validate()
        .map_err(|_| invalid("transformation_job_invalid"))?;
    Ok(job)
}

fn recovery_context(job: &TransformationJob) -> Result<MutationContext, LarmError> {
    let actor_id = job
        .metadata
        .get("actor_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("transformation_job_actor_missing"))?;
    let correlation_id = job
        .metadata
        .get("correlation_id")
        .and_then(Value::as_str)
        .filter(|value| larm_domain::bridge::is_uuid_v7(value))
        .ok_or_else(|| invalid("transformation_job_correlation_missing"))?;
    Ok(MutationContext {
        actor_id: actor_id.into(),
        correlation_id: correlation_id.into(),
    })
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE transformation_job SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|error| db_error("bloqueando recovery de transformaciones", &error))?;
    Ok(())
}

pub(crate) async fn load(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<TransformationJob>, LarmError> {
    let row = sqlx::query("SELECT * FROM transformation_job WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|error| db_error("leyendo job de transformación", &error))?;
    row.as_ref().map(decode_job).transpose()
}

async fn update_journal(
    conn: &mut SqliteConnection,
    job: &TransformationJob,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let payload: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM operation_journal WHERE id=? AND kind=? AND plan_hash=?",
    )
    .bind(&job.id)
    .bind(JOURNAL_KIND)
    .bind(&job.plan_hash)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|error| db_error("leyendo journal de transformación", &error))?;
    let mut payload: Value = serde_json::from_str(
        payload
            .as_deref()
            .ok_or_else(|| invalid("transformation_journal_missing"))?,
    )
    .map_err(|error| db_error("payload de journal de transformación", &error))?;
    if payload.get("actor_id").and_then(Value::as_str) != Some(&ctx.actor_id)
        || payload.get("correlation_id").and_then(Value::as_str) != Some(&ctx.correlation_id)
    {
        return Err(invalid("transformation_journal_context_mismatch"));
    }
    payload["state"] = json!(job.state);
    payload["error"] = job.error.clone().unwrap_or(Value::Null);
    payload["attempt"] = json!(job.attempt);
    let changed = sqlx::query(
        "UPDATE operation_journal SET phase='FAILED_RECOVERABLE',payload=?,updated_at=? \
         WHERE id=? AND kind=? AND plan_hash=? AND phase IN ('RUNNING','PAUSING','VERIFYING')",
    )
    .bind(payload.to_string())
    .bind(&job.updated_at)
    .bind(&job.id)
    .bind(JOURNAL_KIND)
    .bind(&job.plan_hash)
    .execute(&mut *conn)
    .await
    .map_err(|error| db_error("actualizando journal de transformación", &error))?;
    if changed.rows_affected() != 1 {
        return Err(invalid("transformation_journal_state_conflict"));
    }
    Ok(())
}

async fn emit_recovery(
    conn: &mut SqliteConnection,
    job: &TransformationJob,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    crate::events::append(
        conn,
        &NewEvent {
            event_type: "transformation.state_changed",
            resource_type: "transformation_job",
            resource_id: &job.id,
            resource_version: job.row_version,
            data: json!({
                "action": "interrupted_recovered",
                "state": job.state,
                "input_variant_id": job.input_variant_id,
                "attempt": job.attempt,
                "plan_hash": job.plan_hash,
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

impl SqlTransformationJobs {
    /// Persiste la recuperación de transformaciones cuya ejecución se cortó
    /// durante RUNNING/PAUSING/VERIFYING. Estado, journal y outbox se confirman
    /// en la misma transacción; un journal ausente aborta toda la reconciliación.
    /// Persiste la última observación de progreso del worker para el intento
    /// activo. Observación pura: no muta row_version, journal ni eventos, y
    /// sólo acepta trabajos RUNNING/VERIFYING del intento indicado; un intento
    /// terminado o una revisión antigua se rechazan sin escribir.
    pub async fn observe_progress(
        &self,
        job_id: &str,
        attempt: u32,
        progress: &Value,
    ) -> Result<(), LarmError> {
        if !larm_domain::bridge::is_uuid_v7(job_id) || !progress.is_object() {
            return Err(invalid("transformation_progress_invalid"));
        }
        let changed = sqlx::query(
            "UPDATE transformation_job SET metadata=json_set(metadata,'$.progress',json(?))              WHERE id=? AND state IN ('RUNNING','VERIFYING') AND attempt=?",
        )
        .bind(progress.to_string())
        .bind(job_id)
        .bind(i64::from(attempt))
        .execute(&self.pool)
        .await
        .map_err(|error| db_error("observando progreso de transformación", &error))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("transformation_progress_attempt_inactive"));
        }
        Ok(())
    }

    pub async fn recover_interrupted(&self) -> Result<u64, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN recovery de transformaciones", &error))?;
        lock(&mut tx).await?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM transformation_job WHERE state IN ('RUNNING','PAUSING','VERIFYING') ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| db_error("listando transformaciones interrumpidas", &error))?;
        let mut recovered = 0_u64;
        for id in ids {
            let mut job = load(&mut tx, &id)
                .await?
                .ok_or_else(|| invalid("transformation_job_missing_during_recovery"))?;
            let ctx = recovery_context(&job)?;
            let expected_version = job.row_version;
            let error = job.error.clone().unwrap_or_else(|| {
                json!({
                    "code": "WORKER_INTERRUPTED",
                    "message": "La transformación se interrumpió; requiere un nuevo preflight para reiniciarse",
                    "retryable": true,
                })
            });
            let now = larm_domain::bridge::timestamp(chrono::Utc::now());
            job.mark_interrupted_recoverable(error, now.clone())
                .map_err(|_| invalid("transformation_recovery_transition_invalid"))?;
            let changed = sqlx::query(
                "UPDATE transformation_job SET state='FAILED_RECOVERABLE',row_version=?,updated_at=?,error=? \
                 WHERE id=? AND row_version=? AND state IN ('RUNNING','PAUSING','VERIFYING')",
            )
            .bind(job.row_version)
            .bind(&job.updated_at)
            .bind(job.error.as_ref().map(Value::to_string))
            .bind(&job.id)
            .bind(expected_version)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("marcando transformación recuperable", &error))?;
            if changed.rows_affected() != 1 {
                return Err(invalid("transformation_recovery_row_conflict"));
            }
            update_journal(&mut tx, &job, &ctx).await?;
            emit_recovery(&mut tx, &job, &ctx).await?;
            recovered += 1;
        }
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT recovery de transformaciones", &error))?;
        Ok(recovered)
    }

    async fn list_owned(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        actor_id: &str,
    ) -> Result<Vec<TransformationJob>, LarmError> {
        let limit = i64::from(limit);
        let rows = if let Some((created_at, id)) = after {
            sqlx::query(
                "SELECT * FROM transformation_job \
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
                "SELECT * FROM transformation_job \
                 WHERE json_extract(metadata,'$.actor_id')=? \
                 ORDER BY created_at,id LIMIT ?",
            )
            .bind(actor_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| db_error("listando jobs de transformación", &error))?;
        rows.iter().map(decode_job).collect()
    }
}

fn worker_unavailable() -> ApiError {
    LarmError::new(
        ErrorCode::UpstreamUnavailable,
        "El motor de transformaciones no está disponible",
        "transformation_worker_unavailable",
    )
    .retryable(true)
}

#[async_trait::async_trait]
impl TransformationService for SqlTransformationJobs {
    async fn create_plan(
        &self,
        _request: &TransformationRequest,
        _ctx: &MutationContext,
    ) -> Result<TransformationPlanResponse, ApiError> {
        Err(worker_unavailable())
    }

    async fn execute_plan(
        &self,
        _request: &ExecuteTransformationPlan,
        _ctx: &MutationContext,
        _idempotency: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        Err(worker_unavailable())
    }

    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<TransformationJob>, ApiError> {
        self.list_owned(after, limit, &ctx.actor_id).await
    }

    async fn restart_plan(
        &self,
        _restart: &larm_domain::transformations::TransformationRestart,
        _request: &ExecuteTransformationPlan,
        _ctx: &MutationContext,
        _idempotency: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        Err(worker_unavailable())
    }

    async fn cancel_job(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        let job = self.request_cancellation(job_id, principal_id, ctx).await?;
        if job.state == TransformationState::Cancelling {
            // En cola cierra aquí; si el consumidor lo reclamó en la carrera,
            // la señal CANCELLING duradera lo cierra con su testigo.
            if let Ok(cancelled) = self.release_queued_cancellation(job_id).await {
                return Ok(JobReceipt {
                    status_url: format!("/api/v1/jobs/{}", cancelled.id),
                    job_id: cancelled.id,
                    state: job_phase(cancelled.state),
                });
            }
        }
        Ok(JobReceipt {
            status_url: format!("/api/v1/jobs/{}", job.id),
            job_id: job.id,
            state: job_phase(job.state),
        })
    }
}

fn job_phase(state: TransformationState) -> JobPhase {
    use TransformationState as S;
    match state {
        S::Planned | S::Queued => JobPhase::Queued,
        S::Running | S::Pausing | S::Paused | S::Verifying | S::Cancelling => JobPhase::Running,
        S::Completed => JobPhase::Completed,
        S::Cancelled => JobPhase::Cancelled,
        S::Failed | S::FailedRecoverable => JobPhase::Failed,
    }
}

#[async_trait::async_trait]
impl larm_domain::ports::JobSource for SqlTransformationJobs {
    async fn job(&self, principal: &Principal, job_id: &str) -> Result<Option<JobState>, ApiError> {
        if !larm_domain::bridge::is_uuid_v7(job_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Identificador de job no válido",
                "job_id_invalid",
            ));
        }
        let row = sqlx::query("SELECT * FROM transformation_job WHERE id=?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| db_error("leyendo job de transformación", &error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let job = decode_job(&row)?;
        let owner = job
            .metadata
            .get("actor_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if owner != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        let mut progress = serde_json::Map::new();
        progress.insert("transformation_state".into(), json!(job.state));
        progress.insert("attempt".into(), json!(job.attempt));
        if let Some(error) = &job.error {
            progress.insert("error".into(), error.clone());
        }
        // Última observación de progreso del worker: sólo vive con el intento
        // activo; los terminales responden con su resultado/publicación.
        if matches!(
            job.state,
            TransformationState::Running | TransformationState::Verifying
        ) {
            if let Some(worker) = job.metadata.get("progress") {
                progress.insert("worker".into(), worker.clone());
            }
        }
        Ok(Some(JobState {
            job_id: job.id,
            state: job_phase(job.state),
            resource_type: "transformation_job".into(),
            resource_id: Some(job.input_variant_id),
            progress,
            result: job
                .metadata
                .get("result")
                .filter(|value| !value.is_null())
                .cloned(),
            error: None,
            evidence_refs: job
                .metadata
                .get("evidence_refs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| db_error("evidence_refs de transformación", &error))?
                .unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use crate::testing::temp_migrated;
    use larm_domain::transformations::seal_plan;
    use larm_domain::{
        estimate::{Confidence, StorageEstimate, VolumeEstimate},
        transformations::{InputArtifact, PlanDraft, PlanStep, SealedTransformationPlan},
        units::DecimalU64,
    };

    const VARIANT: &str = "0192aaaa-0000-7000-8000-000000000001";
    const ARTIFACT: &str = "0192aaaa-0000-7000-8000-000000000002";
    const DESTINATION: &str = "0192aaaa-0000-7000-8000-000000000003";
    const ADAPTER: &str = "0192aaaa-0000-7000-8000-000000000004";
    const PLAN: &str = "0192aaaa-0000-7000-8000-000000000005";
    const JOB: &str = "0192aaaa-0000-7000-8000-000000000006";
    const OWNER: &str = "0192aaaa-0000-7000-8000-000000000011";
    const CORRELATION: &str = "0192f000-0000-7000-8000-00000000e105";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn sealed_plan() -> SealedTransformationPlan {
        seal_plan(
            PLAN,
            TransformationRequest {
                input_variant_id: VARIANT.into(),
                adapter_id: "transformer".into(),
                tool_version: "1.0".into(),
                target_format: "GGUF".into(),
                parameters: json!({}),
                storage_location_id: DESTINATION.into(),
                hard_budget_bytes: None,
            },
            vec![InputArtifact {
                artifact_id: ARTIFACT.into(),
                relative_path: "model.safetensors".into(),
                size_bytes: DecimalU64::new(50).unwrap(),
                sha256: HASH.into(),
            }],
            PlanDraft {
                operation: "transform".into(),
                resource_ids: vec![VARIANT.into(), DESTINATION.into()],
                storage_estimate: StorageEstimate {
                    download_bytes: None,
                    final_artifact_bytes: DecimalU64::new(45),
                    installed_bytes: None,
                    derived_cache_bytes: None,
                    temporary_peak_bytes: DecimalU64::new(50),
                    existing_bytes: DecimalU64::new(50),
                    peak_extra_bytes: DecimalU64::new(95),
                    required_free_bytes: DecimalU64::new(100),
                    upper_bound_bytes: DecimalU64::new(95),
                    confidence: Confidence::Estimated,
                    reason: "test fixture".into(),
                    evidence_refs: vec!["fixture:p093".into()],
                    per_volume: vec![VolumeEstimate {
                        volume_key: "test-volume".into(),
                        peak_extra_bytes: DecimalU64::new(95).unwrap(),
                        margin_bytes: DecimalU64::new(5).unwrap(),
                        other_reservations_bytes: DecimalU64::new(0).unwrap(),
                        usable_free_bytes: DecimalU64::new(1000).unwrap(),
                        eligible: true,
                        reason: "fits".into(),
                    }],
                },
                steps: vec![PlanStep {
                    phase: "convert".into(),
                    adapter_id: Some("transformer".into()),
                    request: json!({"tool_version":"1.0"}),
                }],
                expires_at: "2026-10-23T11:15:00.000Z".into(),
                approval_required: false,
            },
        )
        .unwrap()
    }

    async fn seed(db: &SqlitePool, journal: bool) {
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('adapter','t','t',1,'{}','transformer','1.0','transformation','ph','pub','VERIFIED','{}','VALIDATED')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-install','t','t',1,'{}','source-adapter','1.0','source','sh','pub','VERIFIED','{}','VALIDATED')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter','t','t',1,'{}','source-install','HTTP','[]')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source','t','t',1,'{}','Source','source-adapter','https://example.invalid',0,1,'NONE','{}')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','cm','source','ns','model')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('revision','t','t',1,'{}','distribution','r1','hash','CURRENT')")
            .execute(db).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','revision','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).execute(db).await.unwrap();
        let plan = sealed_plan();
        let plan_hash = plan.response.plan_hash.clone();
        let input_fingerprint = plan.input_fingerprint().unwrap();
        let metadata = json!({
            "actor_id": OWNER,
            "correlation_id": CORRELATION,
            "input_fingerprint_sha256": input_fingerprint,
            "input_artifacts": plan.input_artifacts,
            "request": plan.request,
            "plan_snapshot": plan.response,
            "attempt_history": [],
        });
        sqlx::query("INSERT INTO transformation_job(id,created_at,updated_at,row_version,metadata,input_variant_id,output_variant_id,adapter_id,tool_version,parameters,plan_hash,state,attempt,error) VALUES(?,'2026-10-23T11:00:00.000Z','2026-10-23T11:01:00.000Z',4,?,?,NULL,?,'1.0','{}',?,'RUNNING',2,NULL)")
            .bind(JOB).bind(metadata.to_string()).bind(VARIANT).bind(ADAPTER).bind(&plan_hash).execute(db).await.unwrap();
        if journal {
            sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'transformation.job',?,'RUNNING',?,?, '2026-10-23T11:01:00.000Z')")
                .bind(JOB).bind(json!([VARIANT]).to_string()).bind(&plan_hash)
                .bind(json!({"actor_id":OWNER,"correlation_id":CORRELATION,"state":"RUNNING","attempt":2,"plan_hash":plan_hash}).to_string())
                .execute(db).await.unwrap();
        }
    }

    #[tokio::test]
    async fn recovery_guarda_estado_causa_journal_y_outbox_atomicos_y_no_duplica() {
        let (_directory, db) = temp_migrated().await.unwrap();
        seed(&db.pool, true).await;
        let jobs = SqlTransformationJobs {
            pool: db.pool.clone(),
        };
        assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
        assert_eq!(jobs.recover_interrupted().await.unwrap(), 0);
        let row = sqlx::query(
            "SELECT state,row_version,output_variant_id,error FROM transformation_job WHERE id=?",
        )
        .bind(JOB)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let error: Value =
            serde_json::from_str(&row.try_get::<String, _>("error").unwrap()).unwrap();
        assert_eq!(
            row.try_get::<String, _>("state").unwrap(),
            "FAILED_RECOVERABLE"
        );
        assert_eq!(row.try_get::<i64, _>("row_version").unwrap(), 5);
        assert_eq!(
            row.try_get::<Option<String>, _>("output_variant_id")
                .unwrap(),
            None
        );
        assert_eq!(error["code"], "WORKER_INTERRUPTED");
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=? AND kind=?")
                .bind(JOB)
                .bind(JOURNAL_KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(phase, "FAILED_RECOVERABLE");
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=? AND event_type='transformation.state_changed'")
            .bind(JOB).fetch_one(&db.pool).await.unwrap();
        assert_eq!(events, 1);
        let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)")
            .bind(JOB).fetch_one(&db.pool).await.unwrap();
        assert_eq!(outbox, 1);
    }

    #[tokio::test]
    async fn recovery_sin_journal_revierte_el_estado_y_no_publica_evento() {
        let (_directory, db) = temp_migrated().await.unwrap();
        seed(&db.pool, false).await;
        let jobs = SqlTransformationJobs {
            pool: db.pool.clone(),
        };
        assert!(jobs.recover_interrupted().await.is_err());
        let state: String = sqlx::query_scalar("SELECT state FROM transformation_job WHERE id=?")
            .bind(JOB)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(state, "RUNNING");
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=?")
            .bind(JOB)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(events, 0);
    }

    #[tokio::test]
    async fn listado_y_job_status_solo_exponen_transformaciones_al_titular_o_admin() {
        use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
        use larm_domain::ports::JobSource;

        let (_directory, db) = temp_migrated().await.unwrap();
        seed(&db.pool, true).await;
        let jobs = SqlTransformationJobs {
            pool: db.pool.clone(),
        };
        let own_context = MutationContext {
            actor_id: OWNER.into(),
            correlation_id: CORRELATION.into(),
        };
        let own = jobs.list_jobs(None, 20, &own_context).await.unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].state, TransformationState::Running);

        let other_context = MutationContext {
            actor_id: "0192aaaa-0000-7000-8000-000000000012".into(),
            correlation_id: CORRELATION.into(),
        };
        assert!(jobs
            .list_jobs(None, 20, &other_context)
            .await
            .unwrap()
            .is_empty());

        let owner = Principal::new(
            OWNER,
            "owner",
            PrincipalKind::Operator,
            [ApiScope::Read],
            [],
        )
        .unwrap();
        let stranger = Principal::new(
            "0192aaaa-0000-7000-8000-000000000012",
            "stranger",
            PrincipalKind::Operator,
            [ApiScope::Read],
            [],
        )
        .unwrap();
        let visible = jobs.job(&owner, JOB).await.unwrap().unwrap();
        assert_eq!(visible.resource_type, "transformation_job");
        assert_eq!(visible.progress["transformation_state"], "RUNNING");
        assert!(jobs.job(&stranger, JOB).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn api_de_escritura_falla_cerrado_sin_worker_y_no_crea_filas() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let jobs = SqlTransformationJobs {
            pool: db.pool.clone(),
        };
        let ctx = MutationContext {
            actor_id: OWNER.into(),
            correlation_id: CORRELATION.into(),
        };
        let request = TransformationRequest {
            input_variant_id: VARIANT.into(),
            adapter_id: "transformer".into(),
            tool_version: "1.0".into(),
            target_format: "GGUF".into(),
            parameters: json!({}),
            storage_location_id: DESTINATION.into(),
            hard_budget_bytes: None,
        };
        let error = jobs.create_plan(&request, &ctx).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
        let execute = ExecuteTransformationPlan {
            plan_id: PLAN.into(),
            plan_hash: HASH.into(),
            approval_id: None,
        };
        let error = jobs
            .execute_plan(
                &execute,
                &ctx,
                &TransformationActivationIdempotency {
                    key: "activation-1".into(),
                    body_hash: HASH.into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM transformation_job")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }
}
