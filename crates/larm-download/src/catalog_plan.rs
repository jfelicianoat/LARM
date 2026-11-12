//! Explicit remote/legacy planning paths; neither path promotes physical state.
use larm_domain::{
    acquisition::{AcquisitionPlanBinding, ModelAcquisitionManifest},
    bridge::ApiError,
    download::{DownloadArtifactEstimate, DownloadPlanRecord},
    estimate::{HardBudget, VolumeSnapshot},
    locations::StorageLocation,
    model::{BundleState, ModelArtifact, ModelBundle},
    ports::{AcquisitionManifestCatalog, MutationContext, VariantRepository},
};
use larm_storage::estimate::SpacePolicy;

pub(crate) enum CatalogPlan {
    Remote(Box<ModelAcquisitionManifest>),
    Legacy {
        bundle: Box<ModelBundle>,
        artifacts: Vec<ModelArtifact>,
    },
}

impl CatalogPlan {
    async fn legacy(variants: &dyn VariantRepository, variant_id: &str) -> Result<Self, ApiError> {
        let bundle =
            variants
                .bundle_of(variant_id)
                .await?
                .filter(|bundle| {
                    bundle.variant_id == variant_id && bundle.state == BundleState::Verified
                })
                .ok_or_else(|| {
                    ApiError::new("CONFLICT",
                "Actualiza el catálogo de la fuente para obtener un manifiesto de adquisición",
                "download_manifest_unavailable")
                })?;
        Ok(Self::Legacy {
            bundle: Box::new(bundle),
            artifacts: variants.artifacts_of(variant_id).await?,
        })
    }

    pub(crate) async fn latest(
        variants: &dyn VariantRepository,
        acquisitions: &dyn AcquisitionManifestCatalog,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<Self, ApiError> {
        match acquisitions.latest_current(variant_id, ctx).await? {
            Some(manifest) => Ok(Self::Remote(Box::new(manifest))),
            None => Self::legacy(variants, variant_id).await,
        }
    }

    pub(crate) async fn for_record(
        variants: &dyn VariantRepository,
        acquisitions: &dyn AcquisitionManifestCatalog,
        record: &DownloadPlanRecord,
        ctx: &MutationContext,
    ) -> Result<Self, ApiError> {
        let basis = match &record.response.acquisition {
            Some(binding) => Self::Remote(Box::new(
                acquisitions
                    .bound_current(&record.response.variant_id, binding, ctx)
                    .await?,
            )),
            None => Self::legacy(variants, &record.response.variant_id).await?,
        };
        basis.validate_record(record)?;
        Ok(basis)
    }

    pub(crate) fn validate_record(&self, record: &DownloadPlanRecord) -> Result<(), ApiError> {
        match self {
            Self::Remote(manifest) => manifest.validate_download_record(record),
            Self::Legacy { bundle, .. } => {
                if record.response.acquisition.is_some()
                    || record.manifest_hash != bundle.manifest_hash
                {
                    return Err(ApiError::new(
                        "CONFLICT",
                        "El catálogo del plan cambió",
                        "download_plan_changed",
                    ));
                }
                Ok(())
            }
        }
    }

    pub(crate) fn binding(&self) -> Option<AcquisitionPlanBinding> {
        match self {
            Self::Remote(manifest) => Some(manifest.binding()),
            Self::Legacy { .. } => None,
        }
    }

    pub(crate) fn manifest_hash(&self) -> &str {
        match self {
            Self::Remote(manifest) => &manifest.upstream_manifest_hash,
            Self::Legacy { bundle, .. } => &bundle.manifest_hash,
        }
    }

    pub(crate) fn estimates(
        &self,
        optional: &[String],
    ) -> Result<Vec<DownloadArtifactEstimate>, ApiError> {
        match self {
            Self::Remote(manifest) => manifest.download_estimates(optional),
            Self::Legacy { artifacts, .. } => {
                crate::preview_service::artifact_estimates(artifacts, optional)
            }
        }
    }

    pub(crate) fn preview(
        &self,
        optional: &[String],
        locations: &[StorageLocation],
        volumes: &[VolumeSnapshot],
        policy: SpacePolicy,
        budget: Option<HardBudget>,
    ) -> Result<crate::planner::BundleDownloadPreview, ApiError> {
        match self {
            Self::Remote(manifest) => crate::planner::preview_acquisition_download(
                manifest, optional, locations, volumes, policy, budget,
            ),
            Self::Legacy { bundle, artifacts } => {
                let artifacts = artifacts
                    .iter()
                    .map(crate::preview_service::planner_artifact)
                    .collect::<Result<Vec<_>, _>>()?;
                crate::planner::preview_bundle_download(
                    bundle, &artifacts, optional, locations, volumes, policy, budget,
                )
            }
        }
        .map_err(crate::preview_service::planner_error)
    }
}
