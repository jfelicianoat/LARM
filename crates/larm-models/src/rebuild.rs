//! Reconstrucción del registro desde los manifiestos de una raíz (P018;
//! spec §134). Se ejecuta contra una base de **staging**, nunca contra la
//! viva: el operador la revisa y la fusiona con un restore verificado.
//!
//! - Variante con todos sus blobs presentes y con sus bytes: se registra
//!   con las reglas de identidad (P040/P041) y sus `artifact_location`
//!   `LARM_MANAGED` (o `SHARED` si otro manifiesto cita el mismo blob).
//! - Con algún blob ausente o corrupto: no se registra; queda en el
//!   informe como incompleta.
//! - Manifiestos ilegibles y blobs que ningún manifiesto cita: al informe.
//!   No se inventan licencias ni referencias upstream que no estén.

use std::collections::BTreeMap;

use larm_domain::artifacts::{ArtifactLocation, ArtifactState, Ownership};
use larm_domain::error::LarmError;
use larm_domain::model::{ArtifactRole, RevisionState};
use larm_domain::ports::{
    ArtifactLocationRepository, ModelRepository, MutationContext, VariantRepository,
};
use larm_domain::units::DecimalU64;
use larm_storage::rebuild::{BlobStatus, RootScan};
use serde::Serialize;

use crate::identity::IdentityEvidence;
use crate::registry::{
    register, ArtifactImport, DistributionImport, EvidenceImport, ModelImport, RevisionImport,
    VariantImport,
};

/// Variante que no se pudo recuperar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Incomplete {
    pub manifest: String,
    pub variant_id: String,
    pub blobs: Vec<(String, BlobStatus)>,
}

/// Informe de la reconstrucción.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    /// `(id del manifiesto, id nuevo de la variante)`.
    pub recovered: Vec<(String, String)>,
    pub incomplete: Vec<Incomplete>,
    pub invalid_manifests: Vec<(String, String)>,
    pub orphan_blobs: Vec<String>,
    /// Manifiestos que chocaron con la identidad de otros: `(fichero, motivo)`.
    pub conflicts: Vec<(String, String)>,
}

/// Registra en la base dada lo recuperable de `scan` (raíz de la ubicación
/// `storage_location_id`).
///
/// # Errors
/// Fallo de la base al registrar ubicaciones.
pub async fn rebuild(
    models: &dyn ModelRepository,
    variants: &dyn VariantRepository,
    locations: &dyn ArtifactLocationRepository,
    storage_location_id: &str,
    scan: &RootScan,
    ctx: &MutationContext,
) -> Result<RebuildReport, LarmError> {
    let mut report = RebuildReport {
        invalid_manifests: scan.invalid.clone(),
        orphan_blobs: scan.orphan_blobs.clone(),
        ..RebuildReport::default()
    };
    // Blobs citados por más de un manifiesto: compartidos.
    let mut citations: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &scan.manifests {
        for a in &m.manifest.artifacts {
            *citations.entry(a.blob.as_str()).or_default() += 1;
        }
    }
    for scanned in &scan.manifests {
        let m = &scanned.manifest;
        if !scanned.complete() {
            report.incomplete.push(Incomplete {
                manifest: scanned.file.clone(),
                variant_id: m.variant_id.clone(),
                blobs: scanned.blobs.clone(),
            });
            continue;
        }
        let evidence: IdentityEvidence =
            serde_json::from_value(m.model.evidence.clone()).unwrap_or_default();
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "recovered_from_manifest".into(),
            m.variant_id.clone().into(),
        );
        metadata.insert("registration_mode".into(), "MANAGED".into());
        let import = ModelImport {
            name: m.model.name.clone(),
            upstream_identity: m.model.upstream_identity.clone(),
            capabilities: vec![],
            metadata: serde_json::Map::new(),
            evidence: Some(EvidenceImport {
                architecture: evidence.architecture,
                base_model: evidence.base_model,
                author: evidence.author,
                config_hash: evidence.config_hash,
                tokenizer_hash: evidence.tokenizer_hash,
                parent_hashes: evidence.parent_hashes.into_iter().collect(),
            }),
            distributions: vec![DistributionImport {
                source_id: m.distribution.source_id.clone(),
                upstream_namespace: m.distribution.upstream_namespace.clone(),
                upstream_model_id: m.distribution.upstream_model_id.clone(),
                license_id: m.distribution.license_id.clone(),
                metadata: serde_json::Map::new(),
                revisions: vec![RevisionImport {
                    revision_key: m.revision.revision_key.clone(),
                    upstream_commit: m.revision.upstream_commit.clone(),
                    manifest_hash: m.revision.manifest_hash.clone(),
                    state: RevisionState::Current,
                    metadata: serde_json::Map::new(),
                    variants: vec![VariantImport {
                        format: m.variant.format.clone(),
                        quantization: m.variant.quantization.clone(),
                        architecture: m.variant.architecture.clone(),
                        capabilities: m.variant.capabilities.clone(),
                        ownership: Some(Ownership::LarmManaged),
                        model_limits: None,
                        metadata,
                        artifacts: m
                            .artifacts
                            .iter()
                            .map(|a| ArtifactImport {
                                filename: a.filename.clone(),
                                role: serde_json::from_value(serde_json::Value::String(
                                    a.role.clone(),
                                ))
                                .unwrap_or(ArtifactRole::Other),
                                format: None,
                                quantization: None,
                                size_bytes: DecimalU64::new(a.size_bytes),
                                sha256: Some(a.sha256.clone()),
                                source_url: None,
                                required: a.required,
                                metadata: serde_json::Map::new(),
                            })
                            .collect(),
                    }],
                    supersedes_current: false,
                }],
            }],
        };
        let registered = match register(models, variants, &import, ctx).await {
            Ok(r) => r,
            Err(e) => {
                report
                    .conflicts
                    .push((scanned.file.clone(), e.details.reason.clone()));
                continue;
            }
        };
        let Some(variant_id) = registered.variant_ids.first().cloned() else {
            continue;
        };
        let artifacts = variants.artifacts_of(&variant_id).await?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let rows: Vec<ArtifactLocation> = m
            .artifacts
            .iter()
            .filter_map(|a| {
                let artifact = artifacts.iter().find(|x| x.filename == a.filename)?;
                let mut metadata = serde_json::Map::new();
                metadata.insert("registration_mode".into(), "MANAGED".into());
                Some(ArtifactLocation {
                    id: uuid::Uuid::now_v7().to_string(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    row_version: 1,
                    metadata,
                    artifact_id: artifact.id.clone(),
                    storage_location_id: storage_location_id.to_string(),
                    relative_path: a.blob.clone(),
                    file_size: DecimalU64::new(a.size_bytes),
                    sha256: Some(a.sha256.clone()),
                    state: ArtifactState::Online,
                    // Los bytes se acaban de releer en el escaneo.
                    verified_at: Some(now.clone()),
                    ownership: if citations.get(a.blob.as_str()).copied().unwrap_or(0) > 1 {
                        Ownership::Shared
                    } else {
                        Ownership::LarmManaged
                    },
                })
            })
            .collect();
        locations.insert_locations(&rows, ctx).await?;
        report.recovered.push((m.variant_id.clone(), variant_id));
    }
    Ok(report)
}
