//! Revalidación de catálogo y espacio antes de consumir un plan P051.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use larm_domain::bridge::{is_uuid_v7, ApiError, JobReceipt};
use larm_domain::download::{DownloadPlanRecord, ExecutePlan};
use larm_domain::estimate::HardBudget;
use larm_domain::ports::{
    AcquisitionManifestCatalog, DownloadActivationIdempotency, DownloadMirrorCatalog,
    DownloadPlanActivationRepository, DownloadPlanActivationService, DownloadPlanExecutionGate,
    DownloadPlanRepository, ModelRepository, MutationContext, StorageLocationRepository,
    StorageObservationRefresher, VariantRepository, VolumeDemand, VolumeReservationRepository,
};
use larm_storage::estimate::{snapshots, SpacePolicy};
use sha2::{Digest, Sha256};

use crate::planner::order_source_candidates;
use crate::preview_service::{plan_hash, PlanHashInput};

pub struct DownloadPlanActivator {
    pub acquisitions: Arc<dyn AcquisitionManifestCatalog>,
    pub plans: Arc<dyn DownloadPlanRepository>,
    pub activation: Arc<dyn DownloadPlanActivationRepository>,
    pub variants: Arc<dyn VariantRepository>,
    pub models: Arc<dyn ModelRepository>,
    pub locations: Arc<dyn StorageLocationRepository>,
    pub storage: Arc<dyn StorageObservationRefresher>,
    pub reservations: Arc<dyn VolumeReservationRepository>,
    pub mirrors: Arc<dyn DownloadMirrorCatalog>,
    pub gate: Arc<dyn DownloadPlanExecutionGate>,
    pub policy: SpacePolicy,
}

/// Se instala hasta que P043/P051 proporcionen la validación de cuenta,
/// licencia y worker real. Falla cerrado para no dejar jobs QUEUED sin dueño.
pub struct UnavailableDownloadExecutionGate;

#[async_trait]
impl DownloadPlanExecutionGate for UnavailableDownloadExecutionGate {
    async fn authorize(
        &self,
        _record: &DownloadPlanRecord,
        _ctx: &MutationContext,
    ) -> Result<(), ApiError> {
        Err(ApiError::new(
            "UPSTREAM_UNAVAILABLE",
            "La ejecución de descargas requiere una fuente y un worker cualificados",
            "download_execution_unavailable",
        )
        .retryable(true))
    }
}

fn invalid(reason: &str, message: &str) -> ApiError {
    ApiError::new("VALIDATION_FAILED", message, reason)
}

fn changed() -> ApiError {
    ApiError::new(
        "CONFLICT",
        "El catálogo o el espacio cambió desde la preparación; vuelve a preparar el plan",
        "download_plan_changed",
    )
}

fn verify_seal(request: &ExecutePlan, record: &DownloadPlanRecord) -> Result<(), ApiError> {
    if !request.operator_confirmed
        || !is_uuid_v7(&request.plan_id)
        || !larm_domain::adapter_updates::is_sha256(&request.plan_hash)
    {
        return Err(invalid(
            "download_activation_invalid",
            "La confirmación del plan de descarga no es válida",
        ));
    }
    let response = &record.response;
    let Some(source_id) = response.source_id.as_deref() else {
        return Err(changed());
    };
    let mut optional_ids = record.request.optional_artifact_ids.clone();
    optional_ids.sort();
    let sealed = plan_hash(PlanHashInput {
        acquisition: response.acquisition.as_ref(),
        request: &record.request,
        variant_id: &response.variant_id,
        revision_id: &record.revision_id,
        revision_key: &record.revision_key,
        manifest_hash: &record.manifest_hash,
        artifact_estimates: &response.artifact_estimates,
        destination_options: &response.destination_options,
        source_candidates: &response.source_candidates,
        include_source_candidates: true,
        observed_at: &record.storage_observed_at,
        expires_at: &response.expires_at,
        source_id,
    })?;
    let expires = DateTime::parse_from_rfc3339(&response.expires_at)
        .ok()
        .map(|value| value.with_timezone(&Utc));
    if response.plan_id.as_deref() != Some(request.plan_id.as_str())
        || response.plan_hash.as_deref() != Some(request.plan_hash.as_str())
        || response.preview_hash != request.plan_hash
        || response.variant_id != record.request.variant_id
        || record.request.storage_location_id.as_deref()
            != response.selected_storage_location_id.as_deref()
        || response.selected_storage_location_id.is_none()
        || optional_ids != response.optional_artifact_ids
        || record.request.priority != response.priority
        || record.request.hard_budget_bytes != response.hard_budget_bytes
        || record
            .request
            .source_id
            .as_deref()
            .is_some_and(|requested| requested != source_id)
        || !response.operator_confirmation_required
        || !response.destination_options.iter().any(|option| {
            Some(option.storage_location_id.as_str())
                == response.selected_storage_location_id.as_deref()
                && option.selected
                && option.eligible
        })
        || expires.is_none_or(|value| value <= Utc::now())
        || (sealed != request.plan_hash
            && !(response.source_candidates.is_empty()
                && response.acquisition.is_none()
                && plan_hash(PlanHashInput {
                    acquisition: response.acquisition.as_ref(),
                    request: &record.request,
                    variant_id: &response.variant_id,
                    revision_id: &record.revision_id,
                    revision_key: &record.revision_key,
                    manifest_hash: &record.manifest_hash,
                    artifact_estimates: &response.artifact_estimates,
                    destination_options: &response.destination_options,
                    source_candidates: &response.source_candidates,
                    include_source_candidates: false,
                    observed_at: &record.storage_observed_at,
                    expires_at: &response.expires_at,
                    source_id,
                })? == request.plan_hash))
    {
        return Err(changed());
    }
    Ok(())
}

fn same_reservation_basis(
    stored: &larm_domain::estimate::StorageEstimate,
    current: &larm_domain::estimate::StorageEstimate,
) -> bool {
    if stored.required_free_bytes != current.required_free_bytes
        || stored.peak_extra_bytes != current.peak_extra_bytes
        || stored.confidence != current.confidence
        || stored.per_volume.len() != current.per_volume.len()
    {
        return false;
    }
    stored.per_volume.iter().all(|old| {
        current.per_volume.iter().any(|new| {
            old.volume_key == new.volume_key
                && old.peak_extra_bytes == new.peak_extra_bytes
                && old.margin_bytes == new.margin_bytes
        })
    })
}

fn observation_time(value: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| {
            ApiError::new(
                "RESOURCE_UNAVAILABLE",
                "El sondeo de almacenamiento no devolvió una fecha válida",
                "storage_observation_invalid",
            )
        })
}

#[async_trait]
impl DownloadPlanActivationService for DownloadPlanActivator {
    async fn activate(
        &self,
        request: &ExecutePlan,
        ctx: &MutationContext,
        idempotency: &DownloadActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        if !request.operator_confirmed {
            return Err(invalid(
                "operator_confirmation_required",
                "Confirma explícitamente la descarga para continuar",
            ));
        }
        let record = self
            .plans
            .get_plan(&request.plan_id)
            .await?
            .ok_or_else(|| {
                ApiError::new(
                    "NOT_FOUND",
                    "Plan de descarga desconocido",
                    "download_plan_not_found",
                )
                .with_resource(request.plan_id.clone())
            })?;
        verify_seal(request, &record)?;

        let response = &record.response;
        let variant = self
            .variants
            .get_variant(&response.variant_id)
            .await?
            .ok_or_else(changed)?;
        if variant.revision_id != record.revision_id {
            return Err(changed());
        }
        let basis = crate::catalog_plan::CatalogPlan::for_record(
            self.variants.as_ref(),
            self.acquisitions.as_ref(),
            &record,
            ctx,
        )
        .await?;
        let revision = self
            .models
            .get_revision(&variant.revision_id)
            .await?
            .filter(|revision| {
                revision.id == record.revision_id && revision.revision_key == record.revision_key
            })
            .ok_or_else(changed)?;
        let distribution = self
            .models
            .get_distribution(&revision.distribution_id)
            .await?
            .ok_or_else(changed)?;

        let current_artifact_estimates = basis.estimates(&record.request.optional_artifact_ids)?;
        if current_artifact_estimates != response.artifact_estimates {
            return Err(changed());
        }
        let current_candidates = order_source_candidates(
            self.mirrors
                .eligible_candidates(
                    &distribution.canonical_model_id,
                    &current_artifact_estimates,
                )
                .await?
                .into_iter()
                .filter(|candidate| candidate.source_id != distribution.source_id)
                .collect(),
            &current_artifact_estimates,
            None,
        )
        .map_err(|_| changed())?;
        if current_candidates != response.source_candidates
            || response.source_id.as_deref() != Some(distribution.source_id.as_str())
        {
            return Err(changed());
        }

        // La cualificación física y el RPC pueden tardar más que los cinco
        // segundos de validez del espacio libre. Medir después de cerrarlos
        // mantiene fresca la instantánea usada para admitir la reserva.
        self.gate.authorize(&record, ctx).await?;
        let observation = self.storage.refresh(ctx).await?;
        let snapshot_at = observation_time(&observation.observed_at)?;
        let locations = self.locations.all().await?;
        let volumes = snapshots(
            &observation,
            &locations,
            self.reservations.as_ref(),
            self.policy,
            None,
        )
        .await?;
        let selected_optional = &record.request.optional_artifact_ids;
        let budget = record
            .request
            .hard_budget_bytes
            .map(|bytes| HardBudget(bytes.get()));
        let preview = basis
            .preview(selected_optional, &locations, &volumes, self.policy, budget)
            .map_err(|error| {
                ApiError::new(
                    "CONFLICT",
                    "El bundle o el preflight cambió; prepara un plan nuevo",
                    "download_plan_changed",
                )
                .with_technical(format!("{error:?}"))
            })?;
        let selected_id = response
            .selected_storage_location_id
            .as_deref()
            .ok_or_else(changed)?;
        let current_destination = preview
            .destinations
            .iter()
            .find(|destination| destination.location_id == selected_id && destination.eligible)
            .ok_or_else(|| {
                ApiError::new(
                    "STORAGE_INSUFFICIENT",
                    "El destino ya no tiene espacio suficiente o no está disponible",
                    "download_destination_no_longer_eligible",
                )
            })?;
        let estimate = current_destination
            .estimate
            .as_ref()
            .filter(|estimate| estimate.required_free_bytes.is_some())
            .ok_or_else(changed)?;
        let stored_estimate = response.storage_estimate.as_ref().ok_or_else(changed)?;
        if !same_reservation_basis(stored_estimate, estimate) {
            return Err(changed());
        }

        let demands = estimate
            .per_volume
            .iter()
            .map(|volume| {
                let snapshot = volumes
                    .iter()
                    .find(|snapshot| snapshot.volume_key == volume.volume_key)
                    .ok_or_else(changed)?;
                let required_bytes = volume
                    .peak_extra_bytes
                    .get()
                    .checked_add(volume.margin_bytes.get())
                    .filter(|bytes| *bytes <= i64::MAX as u64)
                    .ok_or_else(|| {
                        invalid(
                            "size_overflow",
                            "La reserva del destino excede el límite admitido",
                        )
                    })?;
                Ok(VolumeDemand {
                    volume_key: volume.volume_key.clone(),
                    required_bytes,
                    os_free: snapshot.os_free,
                    policy_minimum: snapshot.policy_minimum,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        if demands.is_empty() {
            return Err(changed());
        }

        self.activation
            .activate(request, ctx, idempotency, snapshot_at, &demands)
            .await
    }
}

/// Hash del cuerpo recibido por la API para ligar el replay al mismo JSON.
#[must_use]
pub fn idempotency_body_hash(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use larm_domain::download::{
        DownloadDestinationOption, DownloadPlanRequest, DownloadPlanResponse,
    };

    const PLAN_ID: &str = "0192f000-0000-7000-8000-000000000101";
    const VARIANT_ID: &str = "0192f000-0000-7000-8000-000000000102";
    const LOCATION_ID: &str = "0192f000-0000-7000-8000-000000000103";
    const SOURCE_ID: &str = "0192f000-0000-7000-8000-000000000104";

    fn sealed_plan() -> (ExecutePlan, DownloadPlanRecord) {
        let request = DownloadPlanRequest {
            variant_id: VARIANT_ID.into(),
            storage_location_id: Some(LOCATION_ID.into()),
            hard_budget_bytes: None,
            source_id: Some(SOURCE_ID.into()),
            priority: larm_domain::download::DownloadPriority::Normal,
            optional_artifact_ids: vec![],
        };
        let expires_at = (Utc::now() + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut record = DownloadPlanRecord {
            request,
            response: DownloadPlanResponse {
                acquisition: None,
                preview_id: "0192f000-0000-7000-8000-000000000105".into(),
                preview_hash: String::new(),
                variant_id: VARIANT_ID.into(),
                optional_artifact_ids: vec![],
                artifact_estimates: vec![],
                destination_options: vec![DownloadDestinationOption {
                    storage_location_id: LOCATION_ID.into(),
                    selected: true,
                    eligible: true,
                    reason: "eligible".into(),
                    storage_estimate: None,
                }],
                selected_storage_location_id: Some(LOCATION_ID.into()),
                plan_id: Some(PLAN_ID.into()),
                plan_hash: Some(String::new()),
                storage_estimate: None,
                source_id: Some(SOURCE_ID.into()),
                source_candidates: vec![],
                hard_budget_bytes: None,
                priority: larm_domain::download::DownloadPriority::Normal,
                expires_at,
                operator_confirmation_required: true,
            },
            revision_id: "0192f000-0000-7000-8000-000000000106".into(),
            revision_key: "main".into(),
            manifest_hash: "a".repeat(64),
            storage_observed_at: "2026-10-23T12:00:00.000Z".into(),
        };
        let sealed_hash = plan_hash(PlanHashInput {
            acquisition: record.response.acquisition.as_ref(),
            request: &record.request,
            variant_id: &record.response.variant_id,
            revision_id: &record.revision_id,
            revision_key: &record.revision_key,
            manifest_hash: &record.manifest_hash,
            artifact_estimates: &record.response.artifact_estimates,
            destination_options: &record.response.destination_options,
            source_candidates: &record.response.source_candidates,
            include_source_candidates: true,
            observed_at: &record.storage_observed_at,
            expires_at: &record.response.expires_at,
            source_id: record.response.source_id.as_deref().expect("source id"),
        })
        .expect("plan hash");
        record.response.plan_hash = Some(sealed_hash.clone());
        record.response.preview_hash = sealed_hash.clone();
        (
            ExecutePlan {
                plan_id: PLAN_ID.into(),
                plan_hash: sealed_hash,
                operator_confirmed: true,
            },
            record,
        )
    }

    #[test]
    fn sealed_plan_requires_confirmation_and_exact_hash() {
        let (request, record) = sealed_plan();
        assert!(verify_seal(&request, &record).is_ok());

        let mut unconfirmed = request.clone();
        unconfirmed.operator_confirmed = false;
        assert_eq!(
            verify_seal(&unconfirmed, &record)
                .expect_err("unconfirmed plan must fail")
                .details
                .reason,
            "download_activation_invalid"
        );

        let mut changed_hash = request;
        changed_hash.plan_hash = "b".repeat(64);
        assert_eq!(
            verify_seal(&changed_hash, &record)
                .expect_err("changed plan hash must fail")
                .details
                .reason,
            "download_plan_changed"
        );
    }

    #[test]
    fn acquisition_binding_is_sealed_separately_from_the_upstream_hash() {
        let (mut request, mut record) = sealed_plan();
        record.response.acquisition = Some(larm_domain::acquisition::AcquisitionPlanBinding {
            manifest_id: uuid::Uuid::now_v7().to_string(),
            snapshot_hash: "d".repeat(64),
            upstream_manifest_hash: record.manifest_hash.clone(),
        });
        assert!(
            verify_seal(&request, &record).is_err(),
            "an old seal cannot acquire a snapshot binding"
        );
        let hash = plan_hash(PlanHashInput {
            acquisition: record.response.acquisition.as_ref(),
            request: &record.request,
            variant_id: &record.response.variant_id,
            revision_id: &record.revision_id,
            revision_key: &record.revision_key,
            manifest_hash: &record.manifest_hash,
            artifact_estimates: &record.response.artifact_estimates,
            destination_options: &record.response.destination_options,
            source_candidates: &record.response.source_candidates,
            include_source_candidates: true,
            observed_at: &record.storage_observed_at,
            expires_at: &record.response.expires_at,
            source_id: record.response.source_id.as_deref().expect("source id"),
        })
        .expect("acquisition plan hash");
        request.plan_hash = hash.clone();
        record.response.plan_hash = Some(hash.clone());
        record.response.preview_hash = hash;
        verify_seal(&request, &record).expect("valid bound plan");
        for field in ["manifest_id", "snapshot_hash", "upstream_manifest_hash"] {
            let mut changed = serde_json::to_value(&record).expect("record JSON");
            changed["response"]["acquisition"][field] =
                serde_json::json!(if field == "manifest_id" {
                    uuid::Uuid::now_v7().to_string()
                } else {
                    "e".repeat(64)
                });
            let changed: DownloadPlanRecord = serde_json::from_value(changed).expect("record DTO");
            assert!(verify_seal(&request, &changed).is_err(), "{field}");
        }
    }

    #[tokio::test]
    async fn execution_gate_fails_closed_while_no_worker_is_qualified() {
        let (_, record) = sealed_plan();
        let context = MutationContext {
            actor_id: "operator-test".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000107".into(),
        };
        let error = UnavailableDownloadExecutionGate
            .authorize(&record, &context)
            .await
            .expect_err("no worker must never admit a durable queued job");
        assert_eq!(error.code, "UPSTREAM_UNAVAILABLE");
        assert_eq!(error.details.reason, "download_execution_unavailable");
        assert!(error.retryable);
    }
}
