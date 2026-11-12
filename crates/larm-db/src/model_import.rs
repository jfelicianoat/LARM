//! Journal durable e idempotencia para importaciones locales P098.

use async_trait::async_trait;
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model_import::{
    ModelImportClaim, ModelImportJobs, ModelImportRequest, ModelImportSourceResolver,
    ModelImportSubmitter, IMPORT_JOB_KIND, IMPORT_OPERATION_ID,
};
use larm_domain::ports::{JobSource, MutationContext};
use sqlx::{SqliteConnection, SqlitePool};

use crate::events::{append, NewEvent};
use crate::pool::db_error;

const RESOURCE_TYPE: &str = "model_import";
const IDEMPOTENCY_TTL_HOURS: i64 = 24;
pub const LOCAL_IMPORT_SOURCE_ID: &str = "0192f000-0000-7000-8000-00000000f103";
const LOCAL_IMPORT_ADAPTER_BINDING_ID: &str = "0192f000-0000-7000-8000-00000000f102";
const LOCAL_IMPORT_ADAPTER_ID: &str = "larm.builtin.local-folder";

#[derive(Clone)]
pub struct SqlModelImportJobs {
    pub pool: SqlitePool,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo actualizar el trabajo de importación",
        reason,
    )
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

fn phase_value(phase: &str) -> Result<JobPhase, LarmError> {
    serde_json::from_value(serde_json::Value::String(phase.into()))
        .map_err(|e| db_error("fase de importación", &e))
}

fn decode_job(id: String, phase: &str, payload: &str) -> Result<(JobState, String), LarmError> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| db_error("payload de importación", &e))?;
    let state = phase_value(phase)?;
    let actor_id = value
        .get("actor_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("import_journal_actor_missing"))?
        .to_string();
    let job = JobState {
        job_id: id,
        state,
        resource_type: RESOURCE_TYPE.into(),
        resource_id: value
            .get("resource_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        progress: value
            .get("progress")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default(),
        result: value.get("result").filter(|v| !v.is_null()).cloned(),
        error: value
            .get("error")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| db_error("error de importación", &e))?,
        evidence_refs: value
            .get("evidence_refs")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| db_error("evidence_refs de importación", &e))?
            .unwrap_or_default(),
    };
    Ok((job, actor_id))
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando importaciones", &e))?;
    Ok(())
}

async fn emit_job(
    conn: &mut SqliteConnection,
    id: &str,
    phase: JobPhase,
    resource_id: &str,
    version: i64,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "model.state_changed",
            resource_type: "job",
            resource_id: id,
            resource_version: version,
            data: serde_json::json!({
                "action": "model_import_state_changed",
                "storage_location_id": resource_id,
                "state": phase_text(phase),
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

fn ctx_from_claim(claim: &ModelImportClaim) -> MutationContext {
    MutationContext {
        actor_id: claim.actor_id.clone(),
        correlation_id: claim.correlation_id.clone(),
    }
}

#[async_trait]
impl ModelImportSubmitter for SqlModelImportJobs {
    async fn enqueue_import(
        &self,
        request: &ModelImportRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        request.validate()?;
        if ctx.actor_id.is_empty()
            || idempotency_key.is_empty()
            || idempotency_key.len() > 240
            || !idempotency_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
            || body_hash.len() != 64
            || !body_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de importación no válida",
                "import_request_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let now_dt = chrono::Utc::now();
        let now = larm_domain::bridge::timestamp(now_dt);
        let expires_at =
            larm_domain::bridge::timestamp(now_dt + chrono::Duration::hours(IDEMPOTENCY_TTL_HOURS));
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(IMPORT_OPERATION_ID)
        .bind(idempotency_key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("consultando idempotencia de importación", &e))?;
        if let Some((stored_hash, response_json)) = existing {
            if stored_hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otra solicitud",
                    "idempotency_body_mismatch",
                ));
            }
            let receipt = serde_json::from_str(&response_json)
                .map_err(|e| db_error("leyendo respuesta idempotente de importación", &e))?;
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(receipt);
        }

        let job_id = uuid::Uuid::now_v7().to_string();
        let receipt = JobReceipt {
            job_id: job_id.clone(),
            status_url: format!("/api/v1/jobs/{job_id}"),
            state: JobPhase::Queued,
        };
        let payload = serde_json::json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "storage_location_id": request.storage_location_id,
            "request": request,
            "resource_id": null,
            "progress": { "phase": "QUEUED" },
            "result": null,
            "error": null,
            "evidence_refs": [],
        });
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,?,?,'QUEUED',?,?,?)",
        )
        .bind(&job_id)
        .bind(IMPORT_JOB_KIND)
        .bind(serde_json::json!([request.storage_location_id]).to_string())
        .bind(body_hash)
        .bind(payload.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("encolando importación", &e))?;
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES(?,?,?,?,202,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(IMPORT_OPERATION_ID)
        .bind(idempotency_key)
        .bind(body_hash)
        .bind(serde_json::to_string(&receipt).map_err(|e| db_error("serializando recibo de importación", &e))?)
        .bind(&expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando idempotencia de importación", &e))?;
        emit_job(
            &mut tx,
            &job_id,
            JobPhase::Queued,
            &request.storage_location_id,
            1,
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(receipt)
    }
}

#[async_trait]
impl ModelImportSourceResolver for SqlModelImportJobs {
    async fn resolve_source_id(
        &self,
        request: &ModelImportRequest,
        ctx: &MutationContext,
    ) -> Result<String, ApiError> {
        let folder = std::path::Path::new(&request.source_path);
        let base_url = url::Url::from_file_path(folder)
            .map_err(|_| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "La ruta local de importación no es válida",
                    "import_source_path_invalid",
                )
            })?
            .to_string();
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let available: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM model_source_adapter a \
             JOIN adapter_installation i ON i.id=a.adapter_installation_id \
             WHERE a.id=? AND a.source_type='LOCAL' AND i.adapter_id=? \
             AND i.family='source' AND i.state='VALIDATED' AND i.trust<>'REVOKED')",
        )
        .bind(LOCAL_IMPORT_ADAPTER_BINDING_ID)
        .bind(LOCAL_IMPORT_ADAPTER_ID)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("validando el adaptador local de importación", &e))?;
        if !available {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La fuente interna de carpetas locales no está disponible",
                "local_import_source_unavailable",
            ));
        }
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM model_source WHERE adapter_id=? AND base_url=? \
             AND json_extract(policy,'$.local_import')=1 \
             AND json_extract(policy,'$.automatic')=1 ORDER BY created_at,id LIMIT 1",
        )
        .bind(LOCAL_IMPORT_ADAPTER_BINDING_ID)
        .bind(&base_url)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("buscando la fuente de la carpeta local", &e))?;
        if let Some(id) = existing {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(id);
        }

        let id = uuid::Uuid::now_v7().to_string();
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let metadata = serde_json::json!({"builtin": true, "local_import": true});
        let policy = serde_json::json!({"local_import": true, "automatic": true});
        sqlx::query(
            "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) \
             VALUES(?,?,?,1,?,'Importación local',?,?,0,1,'NONE',NULL,?)",
        )
        .bind(&id)
        .bind(&now)
        .bind(&now)
        .bind(metadata.to_string())
        .bind(LOCAL_IMPORT_ADAPTER_BINDING_ID)
        .bind(&base_url)
        .bind(policy.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("creando fuente local de importación", &e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "source.updated",
                resource_type: "model_source",
                resource_id: &id,
                resource_version: 1,
                data: serde_json::json!({
                    "action": "created",
                    "adapter_id": LOCAL_IMPORT_ADAPTER_BINDING_ID,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(id)
    }
}

#[async_trait]
impl ModelImportJobs for SqlModelImportJobs {
    async fn claim_import(&self) -> Result<Option<ModelImportClaim>, ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT id,payload FROM operation_journal WHERE kind=? AND phase='QUEUED' ORDER BY updated_at,id LIMIT 1",
        )
        .bind(IMPORT_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("buscando importación pendiente", &e))?;
        let Some((job_id, payload)) = row else {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(&payload).map_err(|e| db_error("payload de importación", &e))?;
        let request: ModelImportRequest = serde_json::from_value(
            value
                .get("request")
                .cloned()
                .ok_or_else(|| invalid("import_journal_request_missing"))?,
        )
        .map_err(|e| db_error("solicitud del journal de importación", &e))?;
        request.validate()?;
        let claim = ModelImportClaim {
            job_id: job_id.clone(),
            request,
            actor_id: value
                .get("actor_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("import_journal_actor_missing"))?
                .to_string(),
            correlation_id: value
                .get("correlation_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='RUNNING',updated_at=?,payload=json_set(payload,'$.progress.phase','RUNNING') WHERE id=? AND kind=? AND phase='QUEUED'",
        )
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(&job_id)
        .bind(IMPORT_JOB_KIND)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("iniciando importación", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("import_job_claim_conflict"));
        }
        emit_job(
            &mut tx,
            &job_id,
            JobPhase::Running,
            &claim.request.storage_location_id,
            2,
            &ctx_from_claim(&claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(Some(claim))
    }

    async fn complete_import(
        &self,
        claim: &ModelImportClaim,
        result: serde_json::Value,
        first_variant_id: Option<&str>,
    ) -> Result<(), ApiError> {
        if first_variant_id.is_some_and(|id| !larm_domain::bridge::is_uuid_v7(id)) {
            return Err(invalid("import_result_variant_invalid"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let patch = serde_json::json!({
            "resource_id": first_variant_id,
            "result": result,
            "error": null,
            "progress": { "phase": "COMPLETED" },
        });
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='COMPLETED',updated_at=?,payload=json_patch(payload,?) WHERE id=? AND kind=? AND phase='RUNNING' AND json_extract(payload,'$.actor_id')=?",
        )
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(patch.to_string())
        .bind(&claim.job_id)
        .bind(IMPORT_JOB_KIND)
        .bind(&claim.actor_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("completando importación", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("import_job_not_running"));
        }
        emit_job(
            &mut tx,
            &claim.job_id,
            JobPhase::Completed,
            &claim.request.storage_location_id,
            3,
            &ctx_from_claim(claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }

    async fn fail_import(
        &self,
        claim: &ModelImportClaim,
        error: &ApiError,
    ) -> Result<(), ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let patch = serde_json::json!({
            "error": error,
            "result": null,
            "progress": { "phase": "FAILED" },
        });
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='FAILED',updated_at=?,payload=json_patch(payload,?) WHERE id=? AND kind=? AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.actor_id')=?",
        )
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(patch.to_string())
        .bind(&claim.job_id)
        .bind(IMPORT_JOB_KIND)
        .bind(&claim.actor_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("fallando importación", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("import_job_not_active"));
        }
        emit_job(
            &mut tx,
            &claim.job_id,
            JobPhase::Failed,
            &claim.request.storage_location_id,
            3,
            &ctx_from_claim(claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }

    async fn recover_imports(&self) -> Result<u64, ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,payload FROM operation_journal WHERE kind=? AND phase='RUNNING' ORDER BY id",
        )
        .bind(IMPORT_JOB_KIND)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("buscando importaciones interrumpidas", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let error = LarmError::new(
            ErrorCode::Internal,
            "El daemon se detuvo durante la importación; comprueba los archivos registrados antes de repetirla",
            "job_interrupted",
        )
        .retryable(true);
        for (id, payload) in &rows {
            let value: serde_json::Value = serde_json::from_str(payload)
                .map_err(|e| db_error("payload de importación", &e))?;
            let location_id = value
                .get("storage_location_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let actor_id = value
                .get("actor_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let claim = ModelImportClaim {
                job_id: id.clone(),
                request: serde_json::from_value(
                    value
                        .get("request")
                        .cloned()
                        .ok_or_else(|| invalid("import_journal_request_missing"))?,
                )
                .map_err(|e| db_error("solicitud del journal de importación", &e))?,
                actor_id: actor_id.to_string(),
                correlation_id: value
                    .get("correlation_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            };
            let patch = serde_json::json!({
                "error": error,
                "result": null,
                "progress": { "phase": "FAILED" },
            });
            let changed = sqlx::query(
                "UPDATE operation_journal SET phase='FAILED',updated_at=?,payload=json_patch(payload,?) WHERE id=? AND kind=? AND phase='RUNNING'",
            )
            .bind(&now)
            .bind(patch.to_string())
            .bind(id)
            .bind(IMPORT_JOB_KIND)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("recuperando importación interrumpida", &e))?;
            if changed.rows_affected() == 1 {
                emit_job(
                    &mut tx,
                    id,
                    JobPhase::Failed,
                    location_id,
                    3,
                    &ctx_from_claim(&claim),
                )
                .await?;
            }
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(rows.len() as u64)
    }
}

#[async_trait]
impl JobSource for SqlModelImportJobs {
    async fn job(
        &self,
        principal: &larm_domain::auth::Principal,
        job_id: &str,
    ) -> Result<Option<JobState>, ApiError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT phase,payload FROM operation_journal WHERE id=? AND kind=?")
                .bind(job_id)
                .bind(IMPORT_JOB_KIND)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo estado de importación", &e))?;
        let Some((phase, payload)) = row else {
            return Ok(None);
        };
        let (state, actor_id) = decode_job(job_id.to_string(), &phase, &payload)?;
        Ok((actor_id == principal.id
            || principal.has_api_scope(larm_domain::auth::ApiScope::Admin))
        .then_some(state))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
    use larm_domain::model_import::ModelImportMode;

    const ACTOR: &str = "0192f000-0000-7000-8000-000000000001";
    const LOCATION: &str = "0192f000-0000-7000-8000-00000000c001";

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: ACTOR.into(),
            correlation_id: "0192f000-0000-7000-8000-000000000002".into(),
        }
    }

    fn request() -> ModelImportRequest {
        ModelImportRequest {
            source_path: "C:\\Modelos\\qwen".into(),
            storage_location_id: LOCATION.into(),
            mode: ModelImportMode::Adopt,
            format: None,
            license_id: Some("apache-2.0".into()),
        }
    }

    fn principal(id: &str) -> Principal {
        Principal::new(
            id,
            "import-test",
            PrincipalKind::Operator,
            [ApiScope::Read],
            [],
        )
        .unwrap()
    }

    async fn jobs() -> (tempfile::TempDir, SqlModelImportJobs) {
        let (dir, db) = crate::testing::temp_migrated().await.unwrap();
        (dir, SqlModelImportJobs { pool: db.pool })
    }

    #[tokio::test]
    async fn enqueue_es_atomico_idempotente_y_filtra_estado_por_principal() {
        let (_dir, jobs) = jobs().await;
        let request = request();
        let hash = "a".repeat(64);
        let first = jobs
            .enqueue_import(&request, "import-1", &hash, &ctx())
            .await
            .unwrap();
        let replay = jobs
            .enqueue_import(&request, "import-1", &hash, &ctx())
            .await
            .unwrap();
        assert_eq!(first, replay);
        let mismatch = jobs
            .enqueue_import(&request, "import-1", &"b".repeat(64), &ctx())
            .await
            .unwrap_err();
        assert_eq!(mismatch.details.reason, "idempotency_body_mismatch");

        let journal_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM operation_journal WHERE kind=?")
                .bind(IMPORT_JOB_KIND)
                .fetch_one(&jobs.pool)
                .await
                .unwrap();
        let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&jobs.pool)
            .await
            .unwrap();
        assert_eq!(journal_count, 1);
        assert_eq!(outbox_count, 1);
        assert!(jobs
            .job(&principal(ACTOR), &first.job_id)
            .await
            .unwrap()
            .is_some());
        assert!(jobs
            .job(
                &principal("0192f000-0000-7000-8000-000000000099"),
                &first.job_id,
            )
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn crea_una_fuente_por_carpeta_y_reutiliza_la_misma_en_reintentos() {
        let (_dir, jobs) = jobs().await;
        let folders = tempfile::tempdir().unwrap();
        let first_path = folders.path().join("qwen");
        let second_path = folders.path().join("llama");
        std::fs::create_dir_all(&first_path).unwrap();
        std::fs::create_dir_all(&second_path).unwrap();
        let mut first_request = request();
        first_request.source_path = std::fs::canonicalize(&first_path)
            .unwrap()
            .display()
            .to_string();
        let first_ctx = ctx();
        let replay_ctx = ctx();
        let (first_result, replay_result) = tokio::join!(
            jobs.resolve_source_id(&first_request, &first_ctx),
            jobs.resolve_source_id(&first_request, &replay_ctx),
        );
        let first_id = first_result.unwrap();
        let replay_id = replay_result.unwrap();
        assert_eq!(first_id, replay_id);

        let expected_url = url::Url::from_file_path(first_path.canonicalize().unwrap())
            .unwrap()
            .to_string();
        let stored_url: String = sqlx::query_scalar("SELECT base_url FROM model_source WHERE id=?")
            .bind(&first_id)
            .fetch_one(&jobs.pool)
            .await
            .unwrap();
        assert_eq!(stored_url, expected_url);

        let mut second_request = request();
        second_request.source_path = std::fs::canonicalize(second_path)
            .unwrap()
            .display()
            .to_string();
        let second_id = jobs
            .resolve_source_id(&second_request, &ctx())
            .await
            .unwrap();
        assert_ne!(first_id, second_id);
        let automatic_sources: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM model_source WHERE json_extract(policy,'$.automatic')=1",
        )
        .fetch_one(&jobs.pool)
        .await
        .unwrap();
        let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&jobs.pool)
            .await
            .unwrap();
        assert_eq!(automatic_sources, 2);
        assert_eq!(outbox_count, 2);

        sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE adapter_id=?")
            .bind(LOCAL_IMPORT_ADAPTER_ID)
            .execute(&jobs.pool)
            .await
            .unwrap();
        let unavailable = jobs
            .resolve_source_id(&first_request, &ctx())
            .await
            .unwrap_err();
        assert_eq!(
            unavailable.details.reason,
            "local_import_source_unavailable"
        );
    }

    #[tokio::test]
    async fn claim_complete_y_outbox_persisten_el_resultado_y_recovery_falla_running() {
        let (_dir, jobs) = jobs().await;
        let receipt = jobs
            .enqueue_import(&request(), "import-2", &"c".repeat(64), &ctx())
            .await
            .unwrap();
        let claim = jobs.claim_import().await.unwrap().unwrap();
        assert_eq!(claim.job_id, receipt.job_id);
        assert_eq!(claim.actor_id, ACTOR);
        assert!(jobs.claim_import().await.unwrap().is_none());
        let variant_id = "0192f000-0000-7000-8000-00000000c099";
        jobs.complete_import(
            &claim,
            serde_json::json!({"variants": [variant_id], "hashed": 2}),
            Some(variant_id),
        )
        .await
        .unwrap();
        let completed = jobs
            .job(&principal(ACTOR), &receipt.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, JobPhase::Completed);
        assert_eq!(completed.resource_id.as_deref(), Some(variant_id));
        assert_eq!(completed.result.unwrap()["hashed"], 2);
        let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&jobs.pool)
            .await
            .unwrap();
        assert_eq!(outbox_count, 3);

        let second = jobs
            .enqueue_import(&request(), "import-3", &"d".repeat(64), &ctx())
            .await
            .unwrap();
        let _claim = jobs.claim_import().await.unwrap().unwrap();
        assert_eq!(jobs.recover_imports().await.unwrap(), 1);
        let interrupted = jobs
            .job(&principal(ACTOR), &second.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(interrupted.state, JobPhase::Failed);
        assert_eq!(interrupted.error.unwrap().details.reason, "job_interrupted");
    }
}
