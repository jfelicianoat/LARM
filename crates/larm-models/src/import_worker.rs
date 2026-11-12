//! Worker P098 para ejecutar importaciones persistidas y registrar su resultado.

use std::path::PathBuf;
use std::sync::Arc;

use larm_domain::artifacts::Ownership;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model_import::{ModelImportJobs, ModelImportMode, ModelImportSourceResolver};
use larm_domain::ports::{
    ArtifactLocationRepository, ModelRepository, MutationContext, StorageLocationRepository,
    VariantRepository,
};

pub struct ModelImportPorts {
    pub jobs: Arc<dyn ModelImportJobs>,
    pub source: Arc<dyn ModelImportSourceResolver>,
    pub models: Arc<dyn ModelRepository>,
    pub variants: Arc<dyn VariantRepository>,
    pub artifact_locations: Arc<dyn ArtifactLocationRepository>,
    pub storage_locations: Arc<dyn StorageLocationRepository>,
}

pub struct ModelImportWorker {
    ports: ModelImportPorts,
}

impl ModelImportWorker {
    #[must_use]
    pub fn new(ports: ModelImportPorts) -> Self {
        Self { ports }
    }

    /// Ejecuta como máximo una importación pendiente.
    pub async fn run_once(&self) -> Result<bool, LarmError> {
        let Some(claim) = self.ports.jobs.claim_import().await? else {
            return Ok(false);
        };
        match self.process(&claim).await {
            Ok(report) => {
                let result = match serde_json::to_value(&report) {
                    Ok(result) => result,
                    Err(error) => {
                        let failure = LarmError::new(
                            ErrorCode::Internal,
                            "No se pudo serializar el resultado de la importación",
                            "import_result_serialize_failed",
                        )
                        .with_technical(error.to_string());
                        self.ports.jobs.fail_import(&claim, &failure).await?;
                        return Err(failure);
                    }
                };
                let first_variant_id = report
                    .variants
                    .first()
                    .map(|variant| variant.variant_id.as_str());
                if let Err(error) = self
                    .ports
                    .jobs
                    .complete_import(&claim, result, first_variant_id)
                    .await
                {
                    self.ports.jobs.fail_import(&claim, &error).await?;
                    return Err(error);
                }
            }
            Err(error) => self.ports.jobs.fail_import(&claim, &error).await?,
        }
        Ok(true)
    }

    async fn process(
        &self,
        claim: &larm_domain::model_import::ModelImportClaim,
    ) -> Result<crate::import::DirImportReport, LarmError> {
        let location = self
            .ports
            .storage_locations
            .get(&claim.request.storage_location_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "La ubicación de importación no existe",
                    "import_location_not_found",
                )
                .with_resource(claim.request.storage_location_id.clone())
            })?;
        if location.state != larm_domain::locations::LocationState::Online {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La ubicación de importación no está disponible",
                "import_location_offline",
            )
            .with_resource(location.id));
        }
        let mode = match claim.request.mode {
            ModelImportMode::Adopt => crate::import::ImportMode::Adopt,
            ModelImportMode::Copy => crate::import::ImportMode::Copy,
        };
        let source_path = crate::import::canonical_source_dir(
            &PathBuf::from(&claim.request.source_path),
            &PathBuf::from(&location.canonical_path),
            mode,
        )?;
        let mut request = claim.request.clone();
        request.source_path = source_path.display().to_string();
        let ctx = MutationContext {
            actor_id: claim.actor_id.clone(),
            correlation_id: claim.correlation_id.clone(),
        };
        let source_id = self.ports.source.resolve_source_id(&request, &ctx).await?;
        if !larm_domain::bridge::is_uuid_v7(&source_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La fuente local de importación no es válida",
                "import_source_id_invalid",
            ));
        }
        let options = crate::import::ImportOptions {
            dir: source_path,
            storage_location_id: location.id.clone(),
            location_root: PathBuf::from(&location.canonical_path),
            source_id,
            mode,
            format: claim.request.format.clone(),
            license_id: claim.request.license_id.clone(),
            adopt_as: Ownership::External,
        };
        crate::import::import_dir(
            self.ports.models.as_ref(),
            self.ports.variants.as_ref(),
            self.ports.artifact_locations.as_ref(),
            &options,
            &ctx,
        )
        .await
    }
}
