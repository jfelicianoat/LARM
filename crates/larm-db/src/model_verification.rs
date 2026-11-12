//! Cola y finalización atómica de verificaciones de variantes (P042).

use async_trait::async_trait;
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::BundleState;
use larm_domain::model_verification::{
    ModelVerificationJobs, ModelVerificationSubmitter, VerificationClaim, VerificationCommit,
    VERIFY_JOB_KIND,
};
use larm_domain::ports::{JobSource, MutationContext};
use sqlx::{SqliteConnection, SqlitePool};

use crate::events::{append, NewEvent};
use crate::pool::db_error;

const RESOURCE_TYPE: &str = "model_variant";

#[derive(Clone)]
pub struct SqlModelVerificationJobs {
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

fn context(claim: &VerificationClaim) -> MutationContext {
    MutationContext {
        actor_id: claim.actor_id.clone(),
        correlation_id: claim.correlation_id.clone(),
    }
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo cerrar la verificación de la variante",
        reason,
    )
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando verificaciones", &e))?;
    Ok(())
}

fn decode_job(id: String, phase: &str, payload: &str) -> Result<(JobState, String), LarmError> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| db_error("payload de verificación", &e))?;
    let state: JobPhase = serde_json::from_value(serde_json::Value::String(phase.into()))
        .map_err(|e| db_error("fase de verificación", &e))?;
    let actor_id = value
        .get("actor_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let job = JobState {
        job_id: id,
        state,
        resource_type: RESOURCE_TYPE.into(),
        resource_id: value
            .get("resource_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        progress: serde_json::Map::new(),
        result: value.get("result").filter(|v| !v.is_null()).cloned(),
        error: value
            .get("error")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| db_error("error de verificación", &e))?,
        evidence_refs: value
            .get("evidence_refs")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| db_error("evidence_refs de verificación", &e))?
            .unwrap_or_default(),
    };
    Ok((job, actor_id))
}

async fn emit_job(
    conn: &mut SqliteConnection,
    id: &str,
    phase: JobPhase,
    version: i64,
    target: &str,
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
                "action": "verification_state_changed",
                "variant_id": target,
                "state": phase_text(phase)
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

#[async_trait]
impl ModelVerificationSubmitter for SqlModelVerificationJobs {
    async fn enqueue_verification(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        if !larm_domain::bridge::is_uuid_v7(variant_id) || ctx.actor_id.is_empty() {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de verificación no válida",
                "verification_request_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM model_variant WHERE id=?")
            .bind(variant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("buscando variante para verificar", &e))?;
        if exists.is_none() {
            return Err(
                LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
                    .with_resource(variant_id.to_string()),
            );
        }
        let active: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id,phase,payload FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') \
             AND json_extract(payload,'$.resource_id')=? ORDER BY id LIMIT 1",
        )
        .bind(VERIFY_JOB_KIND)
        .bind(variant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("buscando verificación activa", &e))?;
        if let Some((id, phase, payload)) = active {
            let (_, actor_id) = decode_job(id.clone(), &phase, &payload)?;
            if actor_id != ctx.actor_id {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya existe una verificación activa para esta variante",
                    "verification_active",
                )
                .with_resource(variant_id.to_string()));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            let state = serde_json::from_value(serde_json::Value::String(phase))
                .map_err(|e| db_error("fase de verificación activa", &e))?;
            return Ok(JobReceipt {
                job_id: id.clone(),
                status_url: format!("/api/v1/jobs/{id}"),
                state,
            });
        }
        let id = uuid::Uuid::now_v7().to_string();
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let payload = serde_json::json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "resource_id": variant_id,
            "result": null,
            "error": null,
            "evidence_refs": []
        });
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES(?,?,?,'QUEUED',?,?,?)",
        )
        .bind(&id)
        .bind(VERIFY_JOB_KIND)
        .bind(serde_json::json!([variant_id]).to_string())
        .bind(VERIFY_JOB_KIND)
        .bind(payload.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("encolando verificación", &e))?;
        emit_job(&mut tx, &id, JobPhase::Queued, 1, variant_id, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(JobReceipt {
            job_id: id.clone(),
            status_url: format!("/api/v1/jobs/{id}"),
            state: JobPhase::Queued,
        })
    }
}

#[async_trait]
impl ModelVerificationJobs for SqlModelVerificationJobs {
    async fn claim_verification(&self) -> Result<Option<VerificationClaim>, ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id,phase,payload FROM operation_journal WHERE kind=? AND phase='QUEUED' \
             AND NOT EXISTS (SELECT 1 FROM operation_journal r WHERE r.kind=? AND r.phase='RUNNING' \
             AND json_extract(r.payload,'$.resource_id')=json_extract(operation_journal.payload,'$.resource_id')) \
             ORDER BY updated_at,id LIMIT 1",
        )
        .bind(VERIFY_JOB_KIND)
        .bind(VERIFY_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("buscando verificación pendiente", &e))?;
        let Some((id, phase, payload)) = row else {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(None);
        };
        if phase != "QUEUED" {
            return Err(invalid("verification_journal_invalid"));
        }
        let value: serde_json::Value =
            serde_json::from_str(&payload).map_err(|_| invalid("verification_journal_invalid"))?;
        let variant_id = value["resource_id"]
            .as_str()
            .ok_or_else(|| invalid("verification_journal_invalid"))?
            .to_string();
        let claim = VerificationClaim {
            job_id: id,
            variant_id,
            actor_id: value["actor_id"]
                .as_str()
                .ok_or_else(|| invalid("verification_journal_invalid"))?
                .to_string(),
            correlation_id: value["correlation_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        sqlx::query("UPDATE operation_journal SET phase='RUNNING',updated_at=? WHERE id=? AND kind=? AND phase='QUEUED'")
            .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
            .bind(&claim.job_id)
            .bind(VERIFY_JOB_KIND)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("iniciando verificación", &e))?;
        emit_job(
            &mut tx,
            &claim.job_id,
            JobPhase::Running,
            2,
            &claim.variant_id,
            &context(&claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(Some(claim))
    }

    async fn complete_verification(
        &self,
        claim: &VerificationClaim,
        commit: &VerificationCommit,
    ) -> Result<(), ApiError> {
        if commit.variant_id != claim.variant_id
            || commit.expected_variant_row_version < 1
            || commit.expected_bundle_row_version < 1
        {
            return Err(invalid("verification_commit_invalid"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for location in &commit.locations {
            if !ids.insert(location.id.as_str())
                || location.expected_row_version < 1
                || location.size_bytes.is_some_and(|v| v > i64::MAX as u64)
                || location.sha256.as_deref().is_some_and(|h| {
                    h.len() != 64
                        || !h
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Err(invalid("verification_location_invalid"));
            }
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let payload: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM operation_journal WHERE id=? AND kind=? AND phase='RUNNING'",
        )
        .bind(&claim.job_id)
        .bind(VERIFY_JOB_KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo job de verificación", &e))?;
        let payload = payload.ok_or_else(|| invalid("verification_job_not_running"))?;
        let value: serde_json::Value =
            serde_json::from_str(&payload).map_err(|_| invalid("verification_journal_invalid"))?;
        if value["resource_id"] != claim.variant_id || value["actor_id"] != claim.actor_id {
            return Err(invalid("verification_claim_mismatch"));
        }
        let current: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT lifecycle,availability,update_available FROM model_variant WHERE id=? AND row_version=?",
        )
        .bind(&claim.variant_id)
        .bind(commit.expected_variant_row_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("comprobando versión de variante", &e))?;
        let (lifecycle, availability, update_available) = current.ok_or_else(|| {
            LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió durante la verificación",
                "stale_revision",
            )
            .with_resource(claim.variant_id.clone())
        })?;
        let bundle: Option<i64> = sqlx::query_scalar(
            "SELECT row_version FROM model_bundle WHERE id=? AND variant_id=? AND row_version=?",
        )
        .bind(&commit.bundle_id)
        .bind(&claim.variant_id)
        .bind(commit.expected_bundle_row_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("comprobando versión de bundle", &e))?;
        if bundle.is_none() {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El bundle cambió durante la verificación",
                "stale_revision",
            )
            .with_resource(commit.bundle_id.clone()));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        for location in &commit.locations {
            let updated = sqlx::query(
                "UPDATE artifact_location SET state=?,sha256=?,file_size=?,verified_at=?,updated_at=?,row_version=row_version+1 \
                 WHERE id=? AND row_version=? AND artifact_id IN (SELECT id FROM model_artifact WHERE variant_id=?)",
            )
            .bind(location.state.as_str())
            .bind(&location.sha256)
            .bind(location.size_bytes.map(|size| size as i64))
            .bind(&location.verified_at)
            .bind(&now)
            .bind(&location.id)
            .bind(location.expected_row_version)
            .bind(&claim.variant_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("actualizando ubicación verificada", &e))?;
            if updated.rows_affected() != 1 {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "Una ubicación cambió durante la verificación",
                    "stale_revision",
                )
                .with_resource(location.id.clone()));
            }
            append(
                &mut tx,
                &NewEvent {
                    event_type: "model.state_changed",
                    resource_type: "artifact_location",
                    resource_id: &location.id,
                    resource_version: location.expected_row_version + 1,
                    data: serde_json::json!({"action":"verified","state":location.state.as_str()}),
                },
                &context(claim),
            )
            .await?;
        }
        let bundle_updated = sqlx::query(
            "UPDATE model_bundle SET state=?,metadata=json_set(metadata,'$.verification',json(?)),updated_at=?,row_version=row_version+1 \
             WHERE id=? AND row_version=? AND variant_id=?",
        )
        .bind(commit.bundle_state.as_str())
        .bind(commit.evidence.to_string())
        .bind(&now)
        .bind(&commit.bundle_id)
        .bind(commit.expected_bundle_row_version)
        .bind(&claim.variant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando bundle verificado", &e))?;
        if bundle_updated.rows_affected() != 1 {
            return Err(invalid("verification_bundle_update_failed"));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "model.state_changed",
                resource_type: "model_bundle",
                resource_id: &commit.bundle_id,
                resource_version: commit.expected_bundle_row_version + 1,
                data: serde_json::json!({"action":"verification_completed","state":commit.bundle_state.as_str()}),
            },
            &context(claim),
        )
        .await?;

        let variant_change = match commit.bundle_state {
            BundleState::Corrupt if lifecycle != "CORRUPT" || availability != "CORRUPT" => {
                Some(("CORRUPT", "CORRUPT", "CORRUPT", "corruption_confirmed"))
            }
            BundleState::Verified if lifecycle == "CORRUPT" => {
                Some(("VERIFIED", "UNKNOWN", "reverified", "reverification_passed"))
            }
            BundleState::Verified if lifecycle == "DOWNLOADED" => Some((
                "VERIFIED",
                "UNKNOWN",
                "verified",
                "hashes_and_bundle_verified",
            )),
            _ => None,
        };
        if let Some((next_lifecycle, next_availability, action, evidence_key)) = variant_change {
            if lifecycle != next_lifecycle {
                let from = larm_domain::model::ModelLifecycle::parse(&lifecycle)
                    .ok_or_else(|| invalid("verification_lifecycle_invalid"))?;
                let to = larm_domain::model::ModelLifecycle::parse(next_lifecycle)
                    .ok_or_else(|| invalid("verification_lifecycle_invalid"))?;
                let evidence = larm_domain::model::LifecycleEvidence {
                    hashes_and_bundle_verified: evidence_key == "hashes_and_bundle_verified",
                    corruption_confirmed: evidence_key == "corruption_confirmed",
                    reverification_passed: evidence_key == "reverification_passed",
                    ..Default::default()
                };
                larm_domain::model::validate_lifecycle_transition(from, to, &evidence)?;
            }
            let status = larm_domain::model::ModelVariant::status_for(
                next_lifecycle,
                next_availability,
                update_available != 0,
            );
            let updated = sqlx::query(
                "UPDATE model_variant SET lifecycle=?,availability=?,status=?,metadata=json_set(json_remove(metadata,'$.availability_before_offline'),'$.verification',json(?)),updated_at=?,row_version=row_version+1 \
                 WHERE id=? AND row_version=?",
            )
            .bind(next_lifecycle)
            .bind(next_availability)
            .bind(status.to_string())
            .bind(commit.evidence.to_string())
            .bind(&now)
            .bind(&claim.variant_id)
            .bind(commit.expected_variant_row_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("actualizando lifecycle verificado", &e))?;
            if updated.rows_affected() != 1 {
                return Err(invalid("verification_variant_update_failed"));
            }
            append(
                &mut tx,
                &NewEvent {
                    event_type: "model.state_changed",
                    resource_type: "model_variant",
                    resource_id: &claim.variant_id,
                    resource_version: commit.expected_variant_row_version + 1,
                    data: serde_json::json!({"action":action,"from":lifecycle,"to":next_lifecycle,"availability_from":availability,"availability_to":next_availability,"evidence":evidence_key}),
                },
                &context(claim),
            )
            .await?;
        }
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='COMPLETED',payload=json_patch(payload,?),updated_at=? WHERE id=? AND kind=? AND phase='RUNNING'",
        )
        .bind(serde_json::json!({"result":commit.result,"error":null,"evidence_refs":[]}).to_string())
        .bind(&now)
        .bind(&claim.job_id)
        .bind(VERIFY_JOB_KIND)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("completando job de verificación", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("verification_job_not_running"));
        }
        emit_job(
            &mut tx,
            &claim.job_id,
            JobPhase::Completed,
            3,
            &claim.variant_id,
            &context(claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }

    async fn fail_verification(
        &self,
        claim: &VerificationClaim,
        error: &ApiError,
    ) -> Result<(), ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='FAILED',payload=json_patch(payload,?),updated_at=? \
             WHERE id=? AND kind=? AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.actor_id')=? AND json_extract(payload,'$.resource_id')=?",
        )
        .bind(serde_json::json!({"error":error,"result":null}).to_string())
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(&claim.job_id)
        .bind(VERIFY_JOB_KIND)
        .bind(&claim.actor_id)
        .bind(&claim.variant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando verificación fallida", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("verification_job_not_active"));
        }
        emit_job(
            &mut tx,
            &claim.job_id,
            JobPhase::Failed,
            3,
            &claim.variant_id,
            &context(claim),
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }

    async fn recover_verifications(&self) -> Result<u64, ApiError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        lock(&mut tx).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,payload FROM operation_journal WHERE kind=? AND phase IN ('QUEUED','RUNNING') ORDER BY id",
        )
        .bind(VERIFY_JOB_KIND)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("buscando verificaciones interrumpidas", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let error = LarmError::new(
            ErrorCode::Internal,
            "El daemon se detuvo antes de terminar la verificación",
            "job_interrupted",
        )
        .retryable(true);
        for (id, payload) in &rows {
            let value: serde_json::Value = serde_json::from_str(payload)
                .map_err(|e| db_error("payload de verificación", &e))?;
            let claim = VerificationClaim {
                job_id: id.clone(),
                variant_id: value["resource_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                actor_id: value["actor_id"].as_str().unwrap_or_default().to_string(),
                correlation_id: value["correlation_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            };
            sqlx::query("UPDATE operation_journal SET phase='FAILED',payload=json_patch(payload,?),updated_at=? WHERE id=? AND kind=? AND phase IN ('QUEUED','RUNNING')")
                .bind(serde_json::json!({"error":error,"result":null}).to_string())
                .bind(&now)
                .bind(id)
                .bind(VERIFY_JOB_KIND)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("recuperando verificación", &e))?;
            emit_job(
                &mut tx,
                id,
                JobPhase::Failed,
                3,
                &claim.variant_id,
                &context(&claim),
            )
            .await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(rows.len() as u64)
    }
}

#[async_trait]
impl JobSource for SqlModelVerificationJobs {
    async fn job(
        &self,
        principal: &larm_domain::auth::Principal,
        job_id: &str,
    ) -> Result<Option<JobState>, ApiError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT phase,payload FROM operation_journal WHERE id=? AND kind=?")
                .bind(job_id)
                .bind(VERIFY_JOB_KIND)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo estado de verificación", &e))?;
        let Some((phase, payload)) = row else {
            return Ok(None);
        };
        let (state, actor_id) = decode_job(job_id.to_string(), &phase, &payload)?;
        Ok((actor_id == principal.id
            || principal.has_api_scope(larm_domain::auth::ApiScope::Admin))
        .then_some(state))
    }
}
