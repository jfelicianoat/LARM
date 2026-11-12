//! Preflight con catálogo y observación local de espacio para P052.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use larm_domain::bridge::{is_uuid_v7, timestamp, ApiError};
use larm_domain::download::{
    DownloadArtifactEstimate, DownloadDestinationOption, DownloadPlanRequest, DownloadPlanResponse,
};
use larm_domain::estimate::HardBudget;
use larm_domain::model::{
    ArtifactSizeConfidence, ArtifactSizeEstimate, ArtifactSizeSource, ModelArtifact,
};
use larm_domain::ports::{
    AcquisitionManifestCatalog, DownloadMirrorCatalog, DownloadPlanExecutionGate,
    DownloadPlanPreviewService, DownloadPlanRepository, ModelRepository, MutationContext,
    StorageLocationRepository, StorageObservationRefresher, VariantRepository,
    VolumeReservationRepository,
};
use larm_domain::units::DecimalU64;
use larm_storage::estimate::{snapshots, SpacePolicy};

use crate::planner::{order_source_candidates, ArtifactSize, PlannerArtifact};

const PREVIEW_TTL_MINUTES: i64 = 5;

pub struct DownloadPlanPreview {
    pub acquisitions: Arc<dyn AcquisitionManifestCatalog>,
    pub gate: Arc<dyn DownloadPlanExecutionGate>,
    pub variants: Arc<dyn VariantRepository>,
    pub models: Arc<dyn ModelRepository>,
    pub locations: Arc<dyn StorageLocationRepository>,
    pub storage: Arc<dyn StorageObservationRefresher>,
    pub reservations: Arc<dyn VolumeReservationRepository>,
    pub plans: Arc<dyn DownloadPlanRepository>,
    pub mirrors: Arc<dyn DownloadMirrorCatalog>,
    pub policy: SpacePolicy,
}

fn not_found(what: &str, id: &str) -> ApiError {
    ApiError::new("NOT_FOUND", format!("{what} desconocido"), "not_found")
        .with_resource(id.to_string())
}

fn invalid(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new("VALIDATION_FAILED", message, reason)
}

pub(crate) fn planner_error(error: crate::planner::PlannerError) -> ApiError {
    use crate::planner::PlannerError;
    match error {
        PlannerError::InvalidAcquisition => invalid(
            "acquisition_manifest_invalid",
            "El manifiesto de adquisición no es válido",
        ),
        PlannerError::BundleNotVerified => ApiError::new(
            "CONFLICT",
            "El bundle aún no está verificado",
            "bundle_not_verified",
        ),
        PlannerError::InvalidOptionalSelection => invalid(
            "optional_artifact_selection_invalid",
            "La selección contiene artefactos opcionales ajenos o repetidos",
        ),
        PlannerError::SizeOverflow => invalid("size_overflow", "La suma de tamaños desborda"),
        PlannerError::InvalidSize => {
            invalid("artifact_size_invalid", "Tamaño de artefacto no válido")
        }
        PlannerError::InvalidMirrorCandidate => invalid(
            "download_mirror_candidates_invalid",
            "Las fuentes alternativas no coinciden con los artefactos exactos del plan",
        ),
        PlannerError::InvalidMirrorPreference => invalid(
            "download_source_not_eligible",
            "La fuente solicitada no está entre las alternativas elegibles",
        ),
        PlannerError::InvalidBundle
        | PlannerError::InvalidArtifact
        | PlannerError::MissingArtifact => invalid(
            "bundle_manifest_invalid",
            "El manifiesto del bundle no es válido",
        ),
        PlannerError::DestinationPlanning => ApiError::new(
            "RESOURCE_UNAVAILABLE",
            "No se pudo calcular el espacio disponible",
            "destination_preflight_unavailable",
        ),
    }
}

pub(crate) use larm_domain::download::{plan_hash, PlanHashInput};

pub(crate) fn planner_size(size: &ArtifactSizeEstimate) -> Result<ArtifactSize, ApiError> {
    size.validate().map_err(|_| {
        invalid(
            "artifact_size_estimate_invalid",
            "Estimación de tamaño no válida",
        )
    })?;
    let bytes = |v: Option<DecimalU64>| v.map(DecimalU64::get);
    match size.confidence {
        ArtifactSizeConfidence::Exact => Ok(ArtifactSize::Exact {
            bytes: bytes(size.bytes).ok_or_else(|| {
                invalid("artifact_size_estimate_invalid", "Faltan los bytes exactos")
            })?,
        }),
        ArtifactSizeConfidence::High => Ok(ArtifactSize::High {
            bytes: bytes(size.bytes).ok_or_else(|| {
                invalid(
                    "artifact_size_estimate_invalid",
                    "Faltan los bytes estimados",
                )
            })?,
            upper_bound_bytes: bytes(size.upper_bound_bytes).ok_or_else(|| {
                invalid("artifact_size_estimate_invalid", "Falta la cota superior")
            })?,
        }),
        ArtifactSizeConfidence::Estimated => match (
            bytes(size.bytes),
            bytes(size.lower_bound_bytes),
            bytes(size.upper_bound_bytes),
        ) {
            (Some(bytes), None, Some(upper_bound_bytes)) => Ok(ArtifactSize::Estimated {
                bytes,
                upper_bound_bytes,
            }),
            (None, Some(minimum_bytes), Some(maximum_bytes)) => Ok(ArtifactSize::Range {
                minimum_bytes,
                maximum_bytes,
            }),
            _ => Err(invalid(
                "artifact_size_estimate_invalid",
                "Los límites de tamaño estimado no son válidos",
            )),
        },
        ArtifactSizeConfidence::Unknown => {
            if size.source != ArtifactSizeSource::Unknown {
                return Err(invalid(
                    "artifact_size_estimate_invalid",
                    "Un tamaño UNKNOWN debe indicar origen UNKNOWN",
                ));
            }
            Ok(ArtifactSize::Unknown)
        }
    }
}

pub(crate) fn planner_artifact(artifact: &ModelArtifact) -> Result<PlannerArtifact, ApiError> {
    artifact
        .validate()
        .map_err(|_| invalid("artifact_invalid", "El artefacto del bundle no es válido"))?;
    Ok(PlannerArtifact {
        artifact_id: artifact.id.clone(),
        relative_path: artifact.filename.clone(),
        required: artifact.required,
        size: planner_size(&artifact.size_estimate)?,
        sha256: artifact.sha256.clone(),
    })
}

pub(crate) fn artifact_estimates(
    artifacts: &[ModelArtifact],
    optional_ids: &[String],
) -> Result<Vec<DownloadArtifactEstimate>, ApiError> {
    let selected_optional: BTreeSet<&str> = optional_ids.iter().map(String::as_str).collect();
    let mut estimates = artifacts
        .iter()
        .map(|artifact| {
            artifact
                .validate()
                .map_err(|_| invalid("artifact_invalid", "El artefacto del bundle no es válido"))?;
            Ok(DownloadArtifactEstimate {
                artifact_id: artifact.id.clone(),
                relative_path: artifact.filename.clone(),
                required: artifact.required,
                selected: artifact.required || selected_optional.contains(artifact.id.as_str()),
                size_estimate: artifact.size_estimate.clone(),
                sha256: artifact.sha256.clone(),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    estimates.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    Ok(estimates)
}

#[async_trait]
impl DownloadPlanPreviewService for DownloadPlanPreview {
    async fn preview(
        &self,
        request: &DownloadPlanRequest,
        ctx: &MutationContext,
    ) -> Result<DownloadPlanResponse, ApiError> {
        if !is_uuid_v7(&request.variant_id) {
            return Err(invalid("path_id_invalid", "variant_id no es UUIDv7"));
        }
        let variant = self
            .variants
            .get_variant(&request.variant_id)
            .await?
            .ok_or_else(|| not_found("Variante", &request.variant_id))?;
        let basis = crate::catalog_plan::CatalogPlan::latest(
            self.variants.as_ref(),
            self.acquisitions.as_ref(),
            &variant.id,
            ctx,
        )
        .await?;
        let acquisition = basis.binding();
        let revision = self
            .models
            .get_revision(&variant.revision_id)
            .await?
            .ok_or_else(|| not_found("Revisión", &variant.revision_id))?;
        let distribution = self
            .models
            .get_distribution(&revision.distribution_id)
            .await?
            .ok_or_else(|| not_found("Distribución", &revision.distribution_id))?;
        let observation = self.storage.refresh(ctx).await?;
        let locations = self.locations.all().await?;
        let snapshot_at = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| {
                ApiError::new(
                    "RESOURCE_UNAVAILABLE",
                    "El sondeo no devolvió una fecha válida",
                    "storage_observation_invalid",
                )
            })?;
        let volumes = snapshots(
            &observation,
            &locations,
            self.reservations.as_ref(),
            self.policy,
            None,
        )
        .await?;
        let mut selected_ids = request.optional_artifact_ids.clone();
        selected_ids.sort();
        let budget = request
            .hard_budget_bytes
            .map(|bytes| HardBudget(bytes.get()));
        let preview = basis.preview(&selected_ids, &locations, &volumes, self.policy, budget)?;
        let artifact_estimates = basis.estimates(&request.optional_artifact_ids)?;
        let source_id = distribution.source_id.clone();
        if request
            .source_id
            .as_deref()
            .is_some_and(|requested| requested != source_id)
        {
            return Err(invalid(
                "download_source_preference_invalid",
                "La fuente seleccionada no pertenece a esta variante",
            ));
        }
        let candidates = self
            .mirrors
            .eligible_candidates(&distribution.canonical_model_id, &artifact_estimates)
            .await?;
        let source_candidates = order_source_candidates(
            candidates
                .into_iter()
                .filter(|candidate| candidate.source_id != source_id)
                .collect(),
            &artifact_estimates,
            None,
        )
        .map_err(planner_error)?;
        let destination_options: Vec<DownloadDestinationOption> = preview
            .destinations
            .iter()
            .map(|option| DownloadDestinationOption {
                storage_location_id: option.location_id.clone(),
                selected: request.storage_location_id.as_deref() == Some(&option.location_id),
                eligible: option.eligible,
                reason: option.reason.clone(),
                storage_estimate: option.estimate.clone(),
            })
            .collect();
        if let Some(selected_id) = request.storage_location_id.as_deref() {
            if !destination_options
                .iter()
                .any(|option| option.storage_location_id == selected_id)
            {
                return Err(not_found("Ubicación", selected_id));
            }
        }

        let expires_at = timestamp(Utc::now() + chrono::Duration::minutes(PREVIEW_TTL_MINUTES));
        let preview_id = uuid::Uuid::now_v7().to_string();
        let observed_at = snapshot_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let preview_hash = plan_hash(PlanHashInput {
            acquisition: acquisition.as_ref(),
            request,
            variant_id: &variant.id,
            revision_id: &revision.id,
            revision_key: &revision.revision_key,
            manifest_hash: basis.manifest_hash(),
            artifact_estimates: &artifact_estimates,
            destination_options: &destination_options,
            source_candidates: &source_candidates,
            include_source_candidates: true,
            observed_at: &observed_at,
            expires_at: &expires_at,
            source_id: &source_id,
        })?;

        let selected_storage_location_id = request.storage_location_id.clone();
        let storage_estimate = selected_storage_location_id.as_ref().and_then(|selected| {
            destination_options
                .iter()
                .find(|option| &option.storage_location_id == selected && option.eligible)
                .and_then(|option| option.storage_estimate.clone())
        });
        let plan_id = storage_estimate
            .as_ref()
            .map(|_| uuid::Uuid::now_v7().to_string());
        let plan_hash = plan_id.as_ref().map(|_| preview_hash.clone());
        let response = DownloadPlanResponse {
            acquisition,
            preview_id,
            preview_hash,
            variant_id: variant.id,
            optional_artifact_ids: selected_ids,
            artifact_estimates,
            destination_options,
            selected_storage_location_id,
            plan_id,
            plan_hash,
            storage_estimate,
            source_id: Some(source_id),
            source_candidates,
            hard_budget_bytes: request.hard_budget_bytes,
            priority: request.priority,
            expires_at,
            operator_confirmation_required: true,
        };
        let record = larm_domain::download::DownloadPlanRecord {
            request: request.clone(),
            response: response.clone(),
            revision_id: revision.id.clone(),
            revision_key: revision.revision_key.clone(),
            manifest_hash: basis.manifest_hash().to_owned(),
            storage_observed_at: observed_at,
        };
        basis.validate_record(&record)?;
        if response.acquisition.is_some() && response.plan_id.is_some() {
            self.gate.authorize(&record, ctx).await?;
        }
        if response.plan_id.is_some() {
            self.plans.store_plan(&record, ctx).await?;
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn estimate(
        confidence: ArtifactSizeConfidence,
        bytes: Option<u64>,
        lower: Option<u64>,
        upper: Option<u64>,
        source: ArtifactSizeSource,
        reason: Option<&str>,
    ) -> ArtifactSizeEstimate {
        ArtifactSizeEstimate {
            confidence,
            bytes: bytes.and_then(DecimalU64::new),
            lower_bound_bytes: lower.and_then(DecimalU64::new),
            upper_bound_bytes: upper.and_then(DecimalU64::new),
            source,
            evidence_refs: vec!["test:manifest".into()],
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn exact_range_and_unknown_keep_distinct_planning_semantics() {
        assert_eq!(
            planner_size(&estimate(
                ArtifactSizeConfidence::Exact,
                Some(0),
                Some(0),
                Some(0),
                ArtifactSizeSource::Manifest,
                None,
            ))
            .unwrap(),
            ArtifactSize::Exact { bytes: 0 }
        );
        assert_eq!(
            planner_size(&estimate(
                ArtifactSizeConfidence::Estimated,
                None,
                Some(10),
                Some(25),
                ArtifactSizeSource::UserDeclared,
                None,
            ))
            .unwrap(),
            ArtifactSize::Range {
                minimum_bytes: 10,
                maximum_bytes: 25,
            }
        );
        assert_eq!(
            planner_size(&ArtifactSizeEstimate::unknown(
                "sin evidencia",
                "test:legacy",
            ))
            .unwrap(),
            ArtifactSize::Unknown
        );
    }

    #[test]
    fn confidence_incoherent_with_evidence_fails_closed() {
        let invalid = estimate(
            ArtifactSizeConfidence::Exact,
            Some(8),
            Some(8),
            Some(8),
            ArtifactSizeSource::UserDeclared,
            None,
        );
        assert_eq!(
            planner_size(&invalid).unwrap_err().details.reason,
            "artifact_size_estimate_invalid"
        );
    }
}
