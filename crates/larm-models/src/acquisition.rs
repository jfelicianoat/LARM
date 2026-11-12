//! P041 identity resolution followed by one qualified, atomic remote ingestion.
//! The caller supplies RPC observations; this module never runs a worker or
//! infers physical verification from source metadata.

use crate::registry::{
    self, ArtifactImport, DistributionImport, EvidenceImport, ImportReport, ModelImport,
    RevisionImport, VariantImport,
};
use larm_adapter_sdk::source::{ArtifactManifest, ModelInspection, SourceMetadata};
use larm_db::acquisition::SqlAcquisitionManifests;
use larm_db::catalog_acquisition::{CatalogAcquisition, SqlCatalogAcquisitions};
use larm_domain::acquisition::{
    catalog_reference, ModelAcquisitionArtifact, ModelAcquisitionManifest, SourceCatalogReference,
};
use larm_domain::artifacts::Ownership;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    capabilities, ArtifactRole, ArtifactSizeEstimate, CanonicalModel, ModelArtifact,
    ModelDistribution, ModelRevision, ModelVariant, RevisionState,
};
use larm_domain::ports::{ModelRepository, MutationContext, VariantRepository};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone)]
pub struct SourceObservationQualification {
    pub adapter_installation_id: String,
    pub adapter_installation_revision: String,
    pub adapter_installation_sha256: String,
    pub conformance_run_id: String,
    pub conformance_run_sha256: String,
}

#[derive(Debug, Clone)]
pub struct SourceCatalogObservation {
    pub source_revision: i64,
    /// Stable across retries/replay of this observation, not a plugin UUID.
    pub id: String,
    pub observed_at: String,
    pub reference: SourceCatalogReference,
    pub hit: ModelVariant,
    pub inspection: ModelInspection,
    pub manifest: ArtifactManifest,
    /// Historical fingerprint only: SQLite verifies the current qualification
    /// and holds its physical guard until COMMIT; these fields grant nothing.
    pub qualification: SourceObservationQualification,
}

#[derive(Debug, Clone)]
pub struct RegisteredAcquisition {
    pub model: CanonicalModel,
    pub variant: ModelVariant,
    pub manifest: ModelAcquisitionManifest,
    pub report: ImportReport,
}

#[derive(Clone)]
pub struct SourceCatalogIngestor {
    pub catalog: SqlCatalogAcquisitions,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Las observaciones del catálogo no son coherentes",
        reason,
    )
}
fn changed(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "El catálogo observado cambió",
        reason,
    )
}
fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn now() -> String {
    larm_domain::bridge::timestamp(chrono::Utc::now())
}

impl SourceCatalogObservation {
    fn validate(&self) -> Result<(), LarmError> {
        self.reference.validate()?;
        self.hit.validate()?;
        self.inspection.validate()?;
        self.manifest.validate()?;
        if catalog_reference(&self.hit)?.as_ref() != Some(&self.reference)
            || self.reference.revision_key.starts_with("pending:")
            || self.reference.revision_key != self.manifest.revision_key
            || self.manifest.artifacts.is_empty()
            || self.manifest.artifacts.len() > 4096
            || !self.manifest.artifacts.iter().any(|a| a.required)
        {
            return Err(invalid("source_catalog_observation_binding_invalid"));
        }
        for (hit, inspected) in [
            (&self.hit.architecture, &self.inspection.architecture),
            (&self.hit.format, &self.inspection.format),
            (&self.hit.quantization, &self.inspection.quantization),
        ] {
            if matches!((hit, inspected),(Some(a),Some(b)) if a != b) {
                return Err(changed("source_catalog_inspection_changed"));
            }
        }
        // get_model may describe the repository's union of several bundles.
        // A pinned manifest selects a subset and defines requiredness itself.
        for file in &self.manifest.artifacts {
            let model_file = self
                .inspection
                .artifacts
                .iter()
                .find(|a| a.relative_path == file.relative_path)
                .ok_or_else(|| changed("source_catalog_inspection_artifact_missing"))?;
            if model_file.role != file.role
                || matches!((model_file.size_bytes,file.size_bytes),(Some(a),Some(b)) if a!=b)
                || matches!((&model_file.sha256,&file.sha256),(Some(a),Some(b)) if a!=b)
            {
                return Err(changed("source_catalog_inspection_changed"));
            }
            let _: ArtifactRole = serde_json::from_value(json!(file.role))
                .map_err(|_| invalid("source_catalog_artifact_role_invalid"))?;
        }
        Ok(())
    }

    fn metadata(&self) -> Result<Map<String, Value>, LarmError> {
        let mut source = SourceMetadata::read(&self.hit.metadata)?
            .ok_or_else(|| invalid("source_catalog_metadata_missing"))?;
        // Preserve all original search/inspection metadata behind the shared
        // recursive redactor. Plugin IDs remain descriptive data, never keys.
        source
            .original
            .insert("catalog_hit_metadata".into(), json!(self.hit.metadata));
        source
            .original
            .insert("model_inspection".into(), json!(self.inspection));
        let mut metadata = Map::new();
        source.attach(&mut metadata)?;
        Ok(metadata)
    }

    fn identity_import(&self) -> Result<ModelImport, LarmError> {
        let artifacts = self
            .manifest
            .artifacts
            .iter()
            .map(|a| {
                Ok(ArtifactImport {
                    filename: a.relative_path.clone(),
                    role: serde_json::from_value(json!(a.role))
                        .map_err(|_| invalid("source_catalog_artifact_role_invalid"))?,
                    format: None,
                    quantization: None,
                    size_bytes: a.size_bytes,
                    sha256: a.sha256.clone(),
                    source_url: None,
                    required: a.required,
                    metadata: Map::new(),
                })
            })
            .collect::<Result<Vec<_>, LarmError>>()?;
        let role_hash = |role: &str| {
            let files = self
                .manifest
                .artifacts
                .iter()
                .filter(|a| a.required && a.role == role)
                .collect::<Vec<_>>();
            if files.len() == 1 {
                files[0].sha256.clone()
            } else {
                None
            }
        };
        let name = self
            .hit
            .metadata
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(&self.reference.upstream_model_id)
            .trim()
            .to_owned();
        let metadata = self.metadata()?;
        Ok(ModelImport {
            name,
            upstream_identity: self.inspection.upstream_identity.clone(),
            capabilities: self.inspection.capabilities.clone(),
            metadata: metadata.clone(),
            evidence: Some(EvidenceImport {
                architecture: self
                    .hit
                    .architecture
                    .clone()
                    .or_else(|| self.inspection.architecture.clone()),
                config_hash: role_hash("config"),
                tokenizer_hash: role_hash("tokenizer"),
                ..Default::default()
            }),
            distributions: vec![DistributionImport {
                source_id: self.reference.source_id.clone(),
                upstream_namespace: self.reference.upstream_namespace.clone(),
                upstream_model_id: self.reference.upstream_model_id.clone(),
                license_id: self.inspection.license.clone(),
                metadata: metadata.clone(),
                revisions: vec![RevisionImport {
                    revision_key: self.reference.revision_key.clone(),
                    upstream_commit: None,
                    manifest_hash: self.manifest.manifest_hash.clone(),
                    state: RevisionState::Candidate,
                    metadata: metadata.clone(),
                    supersedes_current: false,
                    variants: vec![VariantImport {
                        format: self
                            .hit
                            .format
                            .clone()
                            .or_else(|| self.inspection.format.clone()),
                        quantization: self
                            .hit
                            .quantization
                            .clone()
                            .or_else(|| self.inspection.quantization.clone()),
                        architecture: self
                            .hit
                            .architecture
                            .clone()
                            .or_else(|| self.inspection.architecture.clone()),
                        capabilities: self.hit.capabilities.clone(),
                        ownership: Some(Ownership::LarmManaged),
                        model_limits: Some(self.hit.model_limits.clone()),
                        metadata,
                        artifacts,
                    }],
                }],
            }],
        })
    }
}

impl SourceCatalogIngestor {
    /// At most three identity preparations. Retry only a detected central
    /// concurrency conflict; never refetch a different revision or drop a guard.
    pub async fn ingest(
        &self,
        observation: &SourceCatalogObservation,
        ctx: &MutationContext,
    ) -> Result<RegisteredAcquisition, LarmError> {
        observation.validate()?;
        let import = observation.identity_import()?;
        for attempt in 0..3 {
            match self.ingest_once(observation, &import, ctx).await {
                Err(error) if attempt < 2 && retry_identity(&error) => {
                    tokio::task::yield_now().await;
                }
                result => return result,
            }
        }
        Err(changed("source_catalog_identity_retry_exhausted"))
    }

    async fn ingest_once(
        &self,
        observed: &SourceCatalogObservation,
        import: &ModelImport,
        ctx: &MutationContext,
    ) -> Result<RegisteredAcquisition, LarmError> {
        let expected_catalog_revision = self.catalog.resolution_revision().await?;
        let models = larm_db::models::SqlModels {
            pool: self.catalog.pool.clone(),
        };
        let variants = larm_db::variants::SqlVariants {
            pool: self.catalog.pool.clone(),
        };
        let (model, mut report) = registry::prepare_canonical(&models, &variants, import).await?;
        let mut identity_updates = Vec::new();
        if report.model_created {
            for other in &report.possible_duplicates {
                identity_updates.push(
                    models
                        .get_model(other)
                        .await?
                        .ok_or_else(|| changed("catalog_acquisition_identity_candidate_changed"))?,
                );
            }
        }
        let d = &import.distributions[0];
        let r = &d.revisions[0];
        let v = &r.variants[0];
        let distribution = match models
            .distribution_by_identity(&d.source_id, &d.upstream_namespace, &d.upstream_model_id)
            .await?
        {
            Some(found) if found.canonical_model_id == model.id => found,
            Some(_) => return Err(changed("source_catalog_distribution_identity_conflict")),
            None => {
                let t = now();
                ModelDistribution {
                    id: id(),
                    created_at: t.clone(),
                    updated_at: t,
                    row_version: 1,
                    metadata: d.metadata.clone(),
                    canonical_model_id: model.id.clone(),
                    source_id: d.source_id.clone(),
                    upstream_namespace: d.upstream_namespace.clone(),
                    upstream_model_id: d.upstream_model_id.clone(),
                    license_id: d.license_id.clone(),
                }
            }
        };
        let revision = match models
            .revision_by_key(&distribution.id, &r.revision_key)
            .await?
        {
            Some(found) if found.manifest_hash == r.manifest_hash => found,
            Some(_) => return Err(changed("source_catalog_revision_immutable")),
            None => {
                let t = now();
                ModelRevision {
                    id: id(),
                    created_at: t.clone(),
                    updated_at: t,
                    row_version: 1,
                    metadata: r.metadata.clone(),
                    distribution_id: distribution.id.clone(),
                    revision_key: r.revision_key.clone(),
                    upstream_commit: None,
                    manifest_hash: r.manifest_hash.clone(),
                    state: RevisionState::Candidate,
                }
            }
        };
        let observed_variant = variants
            .observed_variant(
                &distribution.source_id,
                &revision.id,
                &observed.manifest.manifest_hash,
                v.format.as_deref(),
                v.quantization.as_deref(),
                v.architecture.as_deref(),
            )
            .await?;
        let existing = if observed_variant.is_some() {
            observed_variant
        } else {
            variants
                .find_variant(
                    &revision.id,
                    v.format.as_deref(),
                    v.quantization.as_deref(),
                    v.architecture.as_deref(),
                    None,
                )
                .await?
        };
        let variant = if let Some(variant) = existing {
            let snapshots = SqlAcquisitionManifests {
                pool: self.catalog.pool.clone(),
                qualification: self.catalog.qualification.clone(),
            };
            let snapshot = snapshots
                .latest(&variant.id)
                .await?
                .ok_or_else(|| changed("source_catalog_existing_variant_has_no_acquisition"))?;
            if snapshot.source_reference != observed.reference
                || snapshot.upstream_manifest_hash != observed.manifest.manifest_hash
            {
                return Err(changed("source_catalog_existing_variant_binding_changed"));
            }
            variant
        } else {
            let t = now();
            ModelVariant {
                id: id(),
                created_at: t.clone(),
                updated_at: t,
                row_version: 1,
                metadata: v.metadata.clone(),
                revision_id: revision.id.clone(),
                format: v.format.clone(),
                quantization: v.quantization.clone(),
                architecture: v.architecture.clone(),
                bundle_hash: None,
                lifecycle: "DISCOVERED".into(),
                availability: "UNKNOWN".into(),
                ownership: Ownership::LarmManaged,
                promotion: "NONE".into(),
                system_role: "GENERAL".into(),
                capabilities: capabilities(&v.capabilities)?,
                model_limits: v.model_limits.clone().unwrap_or_default(),
                system1_state: None,
                update_available: false,
                status: "DISCOVERED".into(),
            }
        };
        let mut artifacts = variants.artifacts_of(&variant.id).await?;
        if artifacts.is_empty() {
            let t = now();
            artifacts = observed
                .manifest
                .artifacts
                .iter()
                .map(|a| ModelArtifact {
                    id: id(),
                    created_at: t.clone(),
                    updated_at: t.clone(),
                    row_version: 1,
                    metadata: Map::from_iter([("role".into(), json!(a.role))]),
                    variant_id: variant.id.clone(),
                    filename: a.relative_path.clone(),
                    format: None,
                    quantization: None,
                    expected_size: a.size_bytes,
                    actual_size: None,
                    size_estimate: a.size_estimate.clone().unwrap_or_else(|| {
                        ArtifactSizeEstimate::unknown(
                            "La fuente no publicó certeza de tamaño",
                            "source:artifact-size-estimate-absent",
                        )
                    }),
                    sha256: a.sha256.clone(),
                    source_url: None,
                    required: a.required,
                })
                .collect();
        }
        let mut manifest = ModelAcquisitionManifest {
            id: observed.id.clone(),
            variant_id: variant.id.clone(),
            revision_id: revision.id.clone(),
            source_id: observed.reference.source_id.clone(),
            source_reference: observed.reference.clone(),
            upstream_manifest_hash: observed.manifest.manifest_hash.clone(),
            snapshot_hash: String::new(),
            observed_at: observed.observed_at.clone(),
            required_artifact_ids: vec![],
            optional_artifact_ids: vec![],
            artifacts: vec![],
            adapter_installation_id: observed.qualification.adapter_installation_id.clone(),
            adapter_installation_revision: observed
                .qualification
                .adapter_installation_revision
                .clone(),
            adapter_installation_sha256: observed.qualification.adapter_installation_sha256.clone(),
            conformance_run_id: observed.qualification.conformance_run_id.clone(),
            conformance_run_sha256: observed.qualification.conformance_run_sha256.clone(),
        };
        for a in &observed.manifest.artifacts {
            let stored = artifacts
                .iter()
                .find(|row| row.filename == a.relative_path)
                .ok_or_else(|| changed("source_catalog_artifact_identity_changed"))?;
            let estimate = a.size_estimate.clone().unwrap_or_else(|| {
                ArtifactSizeEstimate::unknown(
                    "La fuente no publicó certeza de tamaño",
                    "source:artifact-size-estimate-absent",
                )
            });
            manifest.artifacts.push(ModelAcquisitionArtifact {
                artifact_id: stored.id.clone(),
                relative_path: a.relative_path.clone(),
                size_bytes: a.size_bytes,
                sha256: a.sha256.clone(),
                required: a.required,
                role: a.role.clone(),
                size_estimate: estimate,
            });
            if a.required {
                manifest.required_artifact_ids.push(stored.id.clone());
            } else {
                manifest.optional_artifact_ids.push(stored.id.clone());
            }
        }
        let manifest = manifest.seal()?;
        let receipt = self
            .catalog
            .record(
                &CatalogAcquisition {
                    expected_catalog_revision,
                    expected_source_revision: observed.source_revision,
                    identity_updates: &identity_updates,
                    model: &model,
                    distribution: &distribution,
                    revision: &revision,
                    variant: &variant,
                    artifacts: &artifacts,
                    manifest: &manifest,
                },
                ctx,
            )
            .await?;
        report.model_created = receipt.model_created;
        report.distributions_created = usize::from(receipt.distribution_created);
        report.revisions_created = usize::from(receipt.revision_created);
        report.variants_created = usize::from(receipt.variant_created);
        report.variant_ids = vec![variant.id.clone()];
        Ok(RegisteredAcquisition {
            model,
            variant,
            manifest,
            report,
        })
    }
}

fn retry_identity(error: &LarmError) -> bool {
    matches!(
        error.details.reason.as_str(),
        "catalog_acquisition_resolution_changed"
            | "catalog_acquisition_model_changed"
            | "catalog_acquisition_distribution_changed"
            | "catalog_acquisition_revision_changed"
            | "catalog_acquisition_variant_changed"
            | "catalog_acquisition_identity_changed"
            | "catalog_acquisition_identity_candidate_changed"
            | "canonical_model_exists"
            | "distribution_exists"
            | "revision_exists"
            | "variant_exists"
    )
}
