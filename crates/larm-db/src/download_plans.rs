//! Persistencia auditable de planes elegibles de descarga (P052, revisión 3.1.0).

use async_trait::async_trait;
use larm_domain::bridge::{is_uuid_v7, ApiError, JobPhase, JobReceipt};
use larm_domain::download::{DownloadPlanRecord, ExecutePlan};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    DownloadActivationIdempotency, DownloadPlanActivationRepository, DownloadPlanRepository,
    MutationContext, VolumeDemand,
};
use sqlx::SqlitePool;

use crate::pool::db_error;

const KIND: &str = "download.plan";
const ACTIVATE_OPERATION: &str = "P051_post__downloads";
const IDEMPOTENCY_RETENTION_DAYS: i64 = 30;

#[derive(Clone)]
pub struct SqlDownloadPlans {
    pub pool: SqlitePool,
    pub qualification: Option<std::sync::Arc<dyn larm_domain::ports::AdapterQualification>>,
}

#[async_trait]
impl DownloadPlanRepository for SqlDownloadPlans {
    async fn get_plan(&self, plan_id: &str) -> Result<Option<DownloadPlanRecord>, ApiError> {
        if !is_uuid_v7(plan_id) {
            return Err(invalid_plan());
        }
        let payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=? AND kind=?")
                .bind(plan_id)
                .bind(KIND)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo plan de descarga", &e))?;
        payload
            .map(|payload| {
                let value: serde_json::Value = serde_json::from_str(&payload)
                    .map_err(|e| db_error("payload de plan de descarga", &e))?;
                serde_json::from_value(value.get("record").cloned().ok_or_else(invalid_plan)?)
                    .map_err(|e| db_error("registro de plan de descarga", &e))
            })
            .transpose()
    }

    async fn store_plan(
        &self,
        record: &larm_domain::download::DownloadPlanRecord,
        ctx: &MutationContext,
    ) -> Result<(), ApiError> {
        let response = &record.response;
        let Some(plan_id) = response.plan_id.as_deref() else {
            return Err(invalid_plan());
        };
        let Some(plan_hash) = response.plan_hash.as_deref() else {
            return Err(invalid_plan());
        };
        let Some(destination_id) = response.selected_storage_location_id.as_deref() else {
            return Err(invalid_plan());
        };
        let selected = response.destination_options.iter().find(|option| {
            option.storage_location_id == destination_id && option.selected && option.eligible
        });
        if !is_uuid_v7(plan_id)
            || !is_uuid_v7(&response.variant_id)
            || !is_uuid_v7(destination_id)
            || !larm_domain::adapter_updates::is_sha256(plan_hash)
            || response.preview_hash != plan_hash
            || record.request.storage_location_id.as_deref() != Some(destination_id)
            || selected.is_none()
            || chrono::DateTime::parse_from_rfc3339(&response.expires_at).is_err()
            || chrono::DateTime::parse_from_rfc3339(&record.storage_observed_at).is_err()
        {
            return Err(invalid_plan());
        }

        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let payload = serde_json::json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "record": record,
        });
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN plan de descarga", &e))?;
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando journal de descarga", &e))?;
        let acquisition_guard = crate::acquisition::validate_download_in(
            &mut tx,
            record,
            ctx,
            self.qualification.as_ref(),
        )
        .await?;
        sqlx::query(
            "UPDATE operation_journal SET phase='EXPIRED',updated_at=? \
             WHERE kind=? AND phase='READY' AND json_extract(payload,'$.record.response.expires_at') <= ?",
        )
        .bind(&now)
        .bind(KIND)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("expirando planes antiguos", &e))?;
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES(?,?,?,'READY',?,?,?)",
        )
        .bind(plan_id)
        .bind(KIND)
        .bind(serde_json::json!([response.variant_id, destination_id]).to_string())
        .bind(plan_hash)
        .bind(payload.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando plan de descarga", &e))?;
        crate::events::append(
            &mut tx,
            &crate::events::NewEvent {
                event_type: "download.plan.created",
                resource_type: "download_plan",
                resource_id: plan_id,
                resource_version: 1,
                data: serde_json::json!({
                    "variant_id": response.variant_id,
                    "storage_location_id": destination_id,
                    "plan_hash": plan_hash,
                    "expires_at": response.expires_at,
                }),
            },
            ctx,
        )
        .await?;
        if let Some(guard) = &acquisition_guard {
            guard.recheck().await?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT plan de descarga", &e))
    }
}

#[async_trait]
impl DownloadPlanActivationRepository for SqlDownloadPlans {
    async fn activate(
        &self,
        request: &ExecutePlan,
        ctx: &MutationContext,
        idempotency: &DownloadActivationIdempotency,
        snapshot_at: chrono::DateTime<chrono::Utc>,
        demands: &[VolumeDemand],
    ) -> Result<JobReceipt, ApiError> {
        if !request.operator_confirmed
            || !is_uuid_v7(&request.plan_id)
            || !larm_domain::adapter_updates::is_sha256(&request.plan_hash)
            || idempotency.key.trim().is_empty()
            || !larm_domain::adapter_updates::is_sha256(&idempotency.body_hash)
        {
            return Err(invalid_activation());
        }

        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN activación de descarga", &e))?;
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando journal de descarga", &e))?;

        let stored: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash,status_code,response_json FROM idempotency \
             WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(ACTIVATE_OPERATION)
        .bind(&idempotency.key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo idempotencia de descarga", &e))?;
        if let Some((body_hash, status, response)) = stored {
            if body_hash != idempotency.body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            if status != 202 {
                return Err(db_error(
                    "respuesta idempotente de descarga",
                    &"status inesperado",
                ));
            }
            let receipt = serde_json::from_str(&response)
                .map_err(|e| db_error("respuesta idempotente de descarga", &e))?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay descarga", &e))?;
            return Ok(receipt);
        }
        sqlx::query(
            "DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?",
        )
        .bind(&ctx.actor_id)
        .bind(ACTIVATE_OPERATION)
        .bind(&idempotency.key)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("retirando idempotencia expirada", &e))?;

        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT phase,plan_hash,payload FROM operation_journal WHERE id=? AND kind=?",
        )
        .bind(&request.plan_id)
        .bind(KIND)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo plan de descarga", &e))?;
        let Some((phase, journal_hash, payload)) = row else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Plan de descarga desconocido",
                "download_plan_not_found",
            )
            .with_resource(request.plan_id.clone()));
        };
        if phase != "READY" {
            return Err(plan_not_ready());
        }
        if journal_hash != request.plan_hash {
            return Err(plan_changed());
        }
        let mut payload: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| db_error("payload de plan de descarga", &e))?;
        if payload.get("actor_id").and_then(serde_json::Value::as_str) != Some(&ctx.actor_id) {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El plan pertenece a otro operador",
                "download_plan_actor_mismatch",
            ));
        }
        let record: DownloadPlanRecord = serde_json::from_value(
            payload
                .get("record")
                .cloned()
                .ok_or_else(invalid_activation)?,
        )
        .map_err(|e| db_error("registro de plan de descarga", &e))?;
        let response = &record.response;
        let mut request_optional = record.request.optional_artifact_ids.clone();
        request_optional.sort();
        let selected_destination = response.selected_storage_location_id.as_deref();
        let expires_at = chrono::DateTime::parse_from_rfc3339(&response.expires_at)
            .ok()
            .map(|expires| expires.with_timezone(&chrono::Utc));
        if expires_at.is_none_or(|expires| expires <= chrono::Utc::now()) {
            sqlx::query(
                "UPDATE operation_journal SET phase='EXPIRED',updated_at=? \
                 WHERE id=? AND kind=? AND phase='READY' AND plan_hash=?",
            )
            .bind(&now)
            .bind(&request.plan_id)
            .bind(KIND)
            .bind(&request.plan_hash)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("expirando plan de descarga", &e))?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT plan expirado", &e))?;
            return Err(plan_changed());
        }
        if response.plan_id.as_deref() != Some(request.plan_id.as_str())
            || response.plan_hash.as_deref() != Some(request.plan_hash.as_str())
            || response.preview_hash != request.plan_hash
            || selected_destination.is_none()
            || record.request.storage_location_id.as_deref() != selected_destination
            || record.request.variant_id != response.variant_id
            || request_optional != response.optional_artifact_ids
            || record.request.priority != response.priority
            || record.request.hard_budget_bytes != response.hard_budget_bytes
            || record
                .request
                .source_id
                .as_deref()
                .is_some_and(|source| Some(source) != response.source_id.as_deref())
            || !response.operator_confirmation_required
            || !response.destination_options.iter().any(|option| {
                Some(option.storage_location_id.as_str()) == selected_destination
                    && option.selected
                    && option.eligible
            })
        {
            return Err(plan_changed());
        }
        let estimate = response
            .storage_estimate
            .as_ref()
            .filter(|estimate| estimate.required_free_bytes.is_some())
            .ok_or_else(invalid_activation)?;
        let expected: Vec<VolumeDemand> = estimate
            .per_volume
            .iter()
            .map(|volume| {
                let required_bytes = volume
                    .peak_extra_bytes
                    .get()
                    .checked_add(volume.margin_bytes.get())
                    .ok_or_else(invalid_activation)?;
                let demand = demands
                    .iter()
                    .find(|demand| demand.volume_key == volume.volume_key)
                    .ok_or_else(invalid_activation)?;
                if !volume.eligible || demand.required_bytes != required_bytes {
                    return Err(invalid_activation());
                }
                Ok(demand.clone())
            })
            .collect::<Result<_, ApiError>>()?;
        if expected.len() != demands.len() || expected.is_empty() {
            return Err(invalid_activation());
        }
        let unique_volumes: std::collections::BTreeSet<&str> = demands
            .iter()
            .map(|demand| demand.volume_key.as_str())
            .collect();
        if unique_volumes.len() != demands.len() {
            return Err(invalid_activation());
        }

        let acquisition_guard = crate::acquisition::validate_download_in(
            &mut tx,
            &record,
            ctx,
            self.qualification.as_ref(),
        )
        .await?;

        crate::download_bundle::admit(&mut tx, &record, ctx).await?;
        let job_id = uuid::Uuid::now_v7().to_string();
        let receipt = JobReceipt {
            job_id: job_id.clone(),
            status_url: format!("/api/v1/jobs/{job_id}"),
            state: JobPhase::Queued,
        };
        let metadata = serde_json::json!({
            "plan_id": request.plan_id,
            "source_id": response.source_id,
            "hard_budget_bytes": response.hard_budget_bytes,
            "optional_artifact_ids": response.optional_artifact_ids,
            "artifacts": response.artifact_estimates,
            "acquisition": response.acquisition,
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "confirmed_at": now,
        });
        sqlx::query(
            "INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,\
             storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,?,?,?,?,?,?,'QUEUED',0,'{}',NULL)",
        )
        .bind(&job_id)
        .bind(&now)
        .bind(&now)
        .bind(1_i64)
        .bind(metadata.to_string())
        .bind(serde_json::to_value(response.priority).unwrap_or_default().as_str().unwrap_or("NORMAL"))
        .bind(&response.variant_id)
        .bind(response.selected_storage_location_id.as_deref().unwrap_or_default())
        .bind(&request.plan_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("creando job de descarga", &e))?;
        crate::reservations::admit_in(&mut tx, &job_id, &expected, snapshot_at, None).await?;

        let execution = serde_json::json!({
            "job_id": job_id,
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "confirmed_at": now,
        });
        if let Some(object) = payload.as_object_mut() {
            object.insert("execution".into(), execution);
        } else {
            return Err(invalid_activation());
        }
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='QUEUED',updated_at=?,payload=? \
             WHERE id=? AND kind=? AND phase='READY' AND plan_hash=?",
        )
        .bind(&now)
        .bind(payload.to_string())
        .bind(&request.plan_id)
        .bind(KIND)
        .bind(&request.plan_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("consumiendo plan de descarga", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(plan_not_ready());
        }
        crate::events::append(
            &mut tx,
            &crate::events::NewEvent {
                event_type: "download.queued",
                resource_type: "download_job",
                resource_id: &job_id,
                resource_version: 1,
                data: serde_json::json!({
                    "variant_id": response.variant_id,
                    "storage_location_id": response.selected_storage_location_id,
                    "plan_id": request.plan_id,
                    "plan_hash": request.plan_hash,
                }),
            },
            ctx,
        )
        .await?;

        let expires_at = larm_domain::bridge::timestamp(
            chrono::Utc::now() + chrono::Duration::days(IDEMPOTENCY_RETENTION_DAYS),
        );
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES(?,?,?,?,202,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(ACTIVATE_OPERATION)
        .bind(&idempotency.key)
        .bind(&idempotency.body_hash)
        .bind(serde_json::to_string(&receipt).map_err(|e| db_error("serializando receipt", &e))?)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando idempotencia de descarga", &e))?;
        if let Some(guard) = &acquisition_guard {
            guard.recheck().await?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT activación de descarga", &e))?;
        Ok(receipt)
    }
}

fn invalid_activation() -> ApiError {
    ApiError::new(
        "VALIDATION_FAILED",
        "La confirmación del plan de descarga no es válida",
        "download_activation_invalid",
    )
}

fn plan_not_ready() -> ApiError {
    ApiError::new(
        "CONFLICT",
        "El plan ya se consumió o no está disponible",
        "download_plan_not_ready",
    )
}

fn plan_changed() -> ApiError {
    ApiError::new(
        "CONFLICT",
        "El plan caducó o cambió; prepara uno nuevo",
        "download_plan_changed",
    )
}

fn invalid_plan() -> ApiError {
    larm_domain::bridge::ApiError::new(
        "VALIDATION_FAILED",
        "El plan de descarga no contiene una selección elegible",
        "download_plan_invalid",
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use larm_domain::download::{
        DownloadDestinationOption, DownloadPlanRecord, DownloadPlanRequest, DownloadPlanResponse,
        DownloadPriority, ExecutePlan,
    };
    use larm_domain::estimate::{Confidence, StorageEstimate, VolumeEstimate};
    use larm_domain::ports::{
        DownloadActivationIdempotency, DownloadPlanActivationRepository, VolumeDemand,
    };
    use larm_domain::units::DecimalU64;

    const VARIANT: &str = "0192f000-0000-7000-8000-000000000031";
    const LOCATION: &str = "0192f000-0000-7000-8000-000000000032";
    const PLAN: &str = "0192f000-0000-7000-8000-000000000033";

    fn record(plan_id: &str, selected: bool) -> DownloadPlanRecord {
        let hash = "a".repeat(64);
        DownloadPlanRecord {
            request: DownloadPlanRequest {
                variant_id: VARIANT.into(),
                storage_location_id: Some(LOCATION.into()),
                hard_budget_bytes: None,
                source_id: None,
                priority: DownloadPriority::Normal,
                optional_artifact_ids: vec![],
            },
            response: DownloadPlanResponse {
                acquisition: None,
                preview_id: "0192f000-0000-7000-8000-000000000034".into(),
                preview_hash: hash.clone(),
                variant_id: VARIANT.into(),
                optional_artifact_ids: vec![],
                artifact_estimates: vec![],
                destination_options: vec![DownloadDestinationOption {
                    storage_location_id: LOCATION.into(),
                    selected,
                    eligible: true,
                    reason: "fits".into(),
                    storage_estimate: None,
                }],
                selected_storage_location_id: Some(LOCATION.into()),
                plan_id: Some(plan_id.into()),
                plan_hash: Some(hash),
                storage_estimate: None,
                source_id: None,
                source_candidates: vec![],
                hard_budget_bytes: None,
                priority: DownloadPriority::Normal,
                expires_at: "2030-01-01T00:05:00.000Z".into(),
                operator_confirmation_required: true,
            },
            revision_id: "0192f000-0000-7000-8000-000000000035".into(),
            revision_key: "fixture-revision".into(),
            manifest_hash: "c".repeat(64),
            storage_observed_at: "2030-01-01T00:00:00.000Z".into(),
        }
    }

    fn executable_record() -> DownloadPlanRecord {
        let mut value = record(PLAN, true);
        value.response.storage_estimate = Some(StorageEstimate {
            download_bytes: Some(DecimalU64::new(100).unwrap()),
            final_artifact_bytes: Some(DecimalU64::new(100).unwrap()),
            installed_bytes: None,
            derived_cache_bytes: None,
            temporary_peak_bytes: None,
            existing_bytes: Some(DecimalU64::new(0).unwrap()),
            peak_extra_bytes: Some(DecimalU64::new(100).unwrap()),
            required_free_bytes: Some(DecimalU64::new(110).unwrap()),
            upper_bound_bytes: Some(DecimalU64::new(100).unwrap()),
            confidence: Confidence::Exact,
            reason: "fits".into(),
            evidence_refs: vec!["fixture:manifest".into()],
            per_volume: vec![VolumeEstimate {
                volume_key: "volume-test".into(),
                peak_extra_bytes: DecimalU64::new(100).unwrap(),
                margin_bytes: DecimalU64::new(10).unwrap(),
                other_reservations_bytes: DecimalU64::new(0).unwrap(),
                usable_free_bytes: DecimalU64::new(1_000_000).unwrap(),
                eligible: true,
                reason: "fits".into(),
            }],
        });
        value.response.expires_at =
            larm_domain::bridge::timestamp(chrono::Utc::now() + chrono::Duration::minutes(5));
        value.storage_observed_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        value
    }

    async fn seed_job_foreign_keys(pool: &SqlitePool) {
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','{}')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','Source','msa','https://example.invalid',0,1,'NONE','{}')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm','ms','ns','model')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1','hash','CURRENT')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','Destination','D:\\models','D:\\models','NTFS','LOCAL',1000000,1000000,0,0,0,'ONLINE','volume-test','SSD')")
            .bind(LOCATION).execute(pool).await.unwrap();
    }

    fn activation_request() -> ExecutePlan {
        ExecutePlan {
            plan_id: PLAN.into(),
            plan_hash: "a".repeat(64),
            operator_confirmed: true,
        }
    }

    fn activation_demands() -> Vec<VolumeDemand> {
        vec![VolumeDemand {
            volume_key: "volume-test".into(),
            required_bytes: 110,
            os_free: 1_000_000,
            policy_minimum: 0,
        }]
    }

    fn activation_idempotency(key: &str, body_hash: &str) -> DownloadActivationIdempotency {
        DownloadActivationIdempotency {
            key: key.into(),
            body_hash: body_hash.into(),
        }
    }

    fn mutation(actor_id: &str) -> MutationContext {
        MutationContext {
            actor_id: actor_id.into(),
            correlation_id: "0192f000-0000-7000-8000-000000000041".into(),
        }
    }

    #[tokio::test]
    async fn stores_an_eligible_plan_and_event_atomically_and_rejects_duplicate_id() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        let ctx = MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000041".into(),
        };
        let eligible = record(PLAN, true);
        plans.store_plan(&eligible, &ctx).await.unwrap();
        assert_eq!(plans.get_plan(PLAN).await.unwrap(), Some(eligible.clone()));
        let (phase, hash, payload): (String, String, String) = sqlx::query_as(
            "SELECT phase,plan_hash,payload FROM operation_journal WHERE id=? AND kind=?",
        )
        .bind(PLAN)
        .bind(KIND)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(phase, "READY");
        assert_eq!(hash, "a".repeat(64));
        let decoded: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(decoded["actor_id"], "operator-1");
        assert_eq!(
            decoded["record"]["request"]["storage_location_id"],
            LOCATION
        );
        let outbox: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox o JOIN event e ON e.id=o.event_record_id WHERE e.resource_id=?",
        )
        .bind(PLAN)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(outbox, 1);

        assert!(plans.store_plan(&eligible, &ctx).await.is_err());
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM operation_journal WHERE id=? AND kind=?")
                .bind(PLAN)
                .bind(KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn refuses_a_plan_that_did_not_select_an_eligible_destination() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        let ctx = MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000041".into(),
        };
        assert_eq!(
            plans
                .store_plan(&record(PLAN, false), &ctx)
                .await
                .unwrap_err()
                .details
                .reason,
            "download_plan_invalid"
        );
    }

    #[tokio::test]
    async fn outbox_failure_rolls_back_the_plan_and_event() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        let ctx = MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000041".into(),
        };
        sqlx::query(
            "CREATE TRIGGER reject_download_plan_outbox BEFORE INSERT ON event_outbox \
             BEGIN SELECT RAISE(ABORT,'injected outbox fault'); END",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        assert!(plans.store_plan(&record(PLAN, true), &ctx).await.is_err());
        let plans_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM operation_journal WHERE id=? AND kind=?")
                .bind(PLAN)
                .bind(KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let events_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM event WHERE resource_id=?")
                .bind(PLAN)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(plans_count, 0);
        assert_eq!(events_count, 0);
    }

    #[tokio::test]
    async fn confirmed_plan_creates_job_reservation_event_and_idempotent_receipt_atomically() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        seed_job_foreign_keys(&db.pool).await;
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        plans
            .store_plan(&executable_record(), &mutation("operator-1"))
            .await
            .unwrap();
        let request = activation_request();
        let idempotency = activation_idempotency("activate-1", &"b".repeat(64));
        let snapshot_at = chrono::Utc::now();
        let receipt = plans
            .activate(
                &request,
                &mutation("operator-1"),
                &idempotency,
                snapshot_at,
                &activation_demands(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.state, JobPhase::Queued);
        assert_eq!(
            receipt.status_url,
            format!("/api/v1/jobs/{}", receipt.job_id)
        );

        let job: (String, String, String, String) = sqlx::query_as(
            "SELECT state,priority,variant_id,storage_location_id FROM download_job WHERE id=?",
        )
        .bind(&receipt.job_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            job,
            (
                "QUEUED".into(),
                "NORMAL".into(),
                VARIANT.into(),
                LOCATION.into()
            )
        );
        let reserved: i64 = sqlx::query_scalar("SELECT remaining_bytes FROM volume_reservation WHERE job_id=? AND volume_key='volume-test'")
            .bind(&receipt.job_id).fetch_one(&db.pool).await.unwrap();
        assert_eq!(reserved, 110);
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=? AND kind=?")
                .bind(PLAN)
                .bind(KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(phase, "QUEUED");
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.queued'")
            .bind(&receipt.job_id).fetch_one(&db.pool).await.unwrap();
        assert_eq!(outbox, 1);

        let replay = plans
            .activate(
                &request,
                &mutation("operator-1"),
                &idempotency,
                snapshot_at,
                &activation_demands(),
            )
            .await
            .unwrap();
        assert_eq!(replay, receipt);
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM download_job")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(jobs, 1);
        assert_eq!(
            plans
                .activate(
                    &request,
                    &mutation("operator-1"),
                    &activation_idempotency("activate-1", &"c".repeat(64)),
                    snapshot_at,
                    &activation_demands()
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "idempotency_body_mismatch"
        );
    }

    #[tokio::test]
    async fn outbox_failure_rolls_back_job_reservation_plan_and_idempotency() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        seed_job_foreign_keys(&db.pool).await;
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        plans
            .store_plan(&executable_record(), &mutation("operator-1"))
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER reject_download_activation_outbox BEFORE INSERT ON event_outbox WHEN (SELECT event_type FROM event WHERE event.id=NEW.event_record_id)='download.queued' BEGIN SELECT RAISE(ABORT,'injected outbox fault'); END")
            .execute(&db.pool).await.unwrap();

        assert!(plans
            .activate(
                &activation_request(),
                &mutation("operator-1"),
                &activation_idempotency("activate-1", &"b".repeat(64)),
                chrono::Utc::now(),
                &activation_demands()
            )
            .await
            .is_err());
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM download_job")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let reservations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let idem: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM idempotency WHERE operation=?")
            .bind(ACTIVATE_OPERATION)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=? AND kind=?")
                .bind(PLAN)
                .bind(KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            (jobs, reservations, idem, phase.as_str()),
            (0, 0, 0, "READY")
        );
    }

    #[tokio::test]
    async fn rejects_missing_confirmation_wrong_actor_changed_hash_expiry_and_stale_space() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        seed_job_foreign_keys(&db.pool).await;
        let plans = SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: None,
        };
        let ctx = mutation("operator-1");
        plans.store_plan(&executable_record(), &ctx).await.unwrap();
        let mut request = activation_request();
        request.operator_confirmed = false;
        assert_eq!(
            plans
                .activate(
                    &request,
                    &ctx,
                    &activation_idempotency("a", &"b".repeat(64)),
                    chrono::Utc::now(),
                    &activation_demands()
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "download_activation_invalid"
        );
        request = activation_request();
        assert_eq!(
            plans
                .activate(
                    &request,
                    &mutation("operator-2"),
                    &activation_idempotency("b", &"b".repeat(64)),
                    chrono::Utc::now(),
                    &activation_demands()
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "download_plan_actor_mismatch"
        );
        request.plan_hash = "c".repeat(64);
        assert_eq!(
            plans
                .activate(
                    &request,
                    &ctx,
                    &activation_idempotency("c", &"b".repeat(64)),
                    chrono::Utc::now(),
                    &activation_demands()
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "download_plan_changed"
        );
        request = activation_request();
        let mut wrong_demand = activation_demands();
        wrong_demand[0].required_bytes = 100;
        assert_eq!(
            plans
                .activate(
                    &request,
                    &ctx,
                    &activation_idempotency("demand", &"b".repeat(64)),
                    chrono::Utc::now(),
                    &wrong_demand,
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "download_activation_invalid"
        );
        assert_eq!(
            plans
                .activate(
                    &request,
                    &ctx,
                    &activation_idempotency("stale", &"b".repeat(64)),
                    chrono::Utc::now() - chrono::Duration::seconds(6),
                    &activation_demands()
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "free_snapshot_stale"
        );
        let expired =
            larm_domain::bridge::timestamp(chrono::Utc::now() - chrono::Duration::seconds(1));
        sqlx::query("UPDATE operation_journal SET payload=json_set(payload,'$.record.response.expires_at',?) WHERE id=? AND kind=?")
            .bind(expired).bind(PLAN).bind(KIND).execute(&db.pool).await.unwrap();
        assert_eq!(
            plans
                .activate(
                    &request,
                    &ctx,
                    &activation_idempotency("expired", &"b".repeat(64)),
                    chrono::Utc::now(),
                    &activation_demands(),
                )
                .await
                .unwrap_err()
                .details
                .reason,
            "download_plan_changed"
        );
        let expired_phase: String =
            sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=? AND kind=?")
                .bind(PLAN)
                .bind(KIND)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(expired_phase, "EXPIRED");
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM download_job")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(jobs, 0);
    }
}
