//! Alta de modelos con las reglas de identidad del contrato 01 (P040, P041).
//!
//! - Un modelo con `upstream_identity` explícita se agrupa con el que ya la
//!   tiene: varias distribuciones, un modelo canónico.
//! - Sin ella, se resuelve la identidad (§18) frente a los candidatos: los
//!   homónimos y los modelos que comparten algún hash de artefacto. Solo un
//!   resultado `IDENTICAL` (los mismos bytes requeridos, configuración y
//!   tokenizador) agrupa. `PROBABLE_SAME_MODEL` y la mera coincidencia de
//!   nombre se publican como `possible_duplicates`; `DERIVED_MODEL` queda
//!   en `metadata.lineage`; `UNRELATED` no se anota. Los motivos se guardan.
//! - Una distribución ya registrada para otro modelo es un conflicto.
//! - Una revisión ya registrada con otro `manifest_hash` es un conflicto.
//! - Cada variante se registra con sus artefactos y su bundle en una
//!   transacción; su identidad incluye el hash de los descriptores requeridos.
//!
//! El alta es idempotente: repetir la misma importación no crea nada.

use larm_domain::artifacts::Ownership;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    capabilities, ArtifactRole, ArtifactSizeEstimate, CanonicalModel, ModelArtifact,
    ModelDistribution, ModelLimits, ModelRevision, ModelVariant, RevisionState,
};
use larm_domain::ports::{ModelRepository, MutationContext, VariantRepository};
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::identity::{resolve, IdentityEvidence, IdentityGrade};

/// Artefacto a registrar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactImport {
    /// Ruta POSIX relativa dentro del bundle.
    pub filename: String,
    pub role: ArtifactRole,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub quantization: Option<String>,
    pub size_bytes: Option<DecimalU64>,
    pub sha256: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    pub required: bool,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// Variante a registrar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantImport {
    pub format: Option<String>,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub ownership: Option<Ownership>,
    #[serde(default)]
    pub model_limits: Option<ModelLimits>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub artifacts: Vec<ArtifactImport>,
}

/// Revisión a registrar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionImport {
    pub revision_key: String,
    pub upstream_commit: Option<String>,
    pub manifest_hash: String,
    pub state: RevisionState,
    /// Metadata original de la fuente, tal cual.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub variants: Vec<VariantImport>,
    /// Si es CURRENT y la distribución ya tiene otra CURRENT, la sustituye
    /// (la anterior pasa a SUPERSEDED). Por defecto, conflicto.
    #[serde(default)]
    pub supersedes_current: bool,
}

/// Distribución a registrar (publicación en una fuente).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionImport {
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
    pub license_id: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub revisions: Vec<RevisionImport>,
}

/// Evidencia de identidad declarada por la fuente (§18). Los hashes de los
/// artefactos requeridos se toman de las variantes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceImport {
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub base_model: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub tokenizer_hash: Option<String>,
    #[serde(default)]
    pub parent_hashes: Vec<String>,
}

/// Modelo a registrar con sus distribuciones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelImport {
    pub name: String,
    pub upstream_identity: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub evidence: Option<EvidenceImport>,
    pub distributions: Vec<DistributionImport>,
}

/// Resolución frente a otro modelo, tal como se guarda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRecord {
    pub model_id: String,
    pub grade: IdentityGrade,
    pub reasons: Vec<String>,
}

/// Qué hizo el alta.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub model_id: String,
    pub model_created: bool,
    pub possible_duplicates: Vec<String>,
    pub distributions_created: usize,
    pub revisions_created: usize,
    pub variants_created: usize,
    pub variant_ids: Vec<String>,
    /// Capacidades sin contrato: aceptadas, no soportadas.
    pub unqualified_capabilities: Vec<String>,
    /// Resoluciones de identidad frente a los candidatos.
    pub resolutions: Vec<ResolutionRecord>,
}

fn now() -> String {
    larm_domain::bridge::timestamp(chrono::Utc::now())
}

/// Evidencia del modelo importado: la declarada más los sha256 de los
/// artefactos requeridos de sus variantes.
#[must_use]
pub fn evidence_of(import: &ModelImport) -> IdentityEvidence {
    let declared = import.evidence.clone().unwrap_or_default();
    IdentityEvidence {
        name: import.name.trim().to_string(),
        upstream_identity: import.upstream_identity.clone(),
        architecture: declared.architecture.or_else(|| {
            import
                .distributions
                .iter()
                .flat_map(|d| &d.revisions)
                .flat_map(|r| &r.variants)
                .find_map(|v| v.architecture.clone())
        }),
        base_model: declared.base_model,
        author: declared.author,
        config_hash: declared.config_hash,
        tokenizer_hash: declared.tokenizer_hash,
        required_hashes: {
            let required = import
                .distributions
                .iter()
                .flat_map(|d| &d.revisions)
                .flat_map(|r| &r.variants)
                .flat_map(|v| &v.artifacts)
                .filter(|a| a.required)
                .collect::<Vec<_>>();
            if required.iter().all(|a| a.sha256.is_some()) {
                required.iter().filter_map(|a| a.sha256.clone()).collect()
            } else {
                Default::default()
            }
        },
        parent_hashes: declared.parent_hashes.into_iter().collect(),
    }
}

fn stored_evidence(model: &CanonicalModel) -> IdentityEvidence {
    model
        .metadata
        .get("identity_evidence")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| IdentityEvidence {
            name: model.name.clone(),
            upstream_identity: model.upstream_identity.clone(),
            ..IdentityEvidence::default()
        })
}

/// Historical metadata may contain only the known subset of required hashes.
/// Compare it with the full current catalog before treating it as byte identity.
async fn complete_stored_evidence(
    variants: &dyn VariantRepository,
    model: &CanonicalModel,
) -> Result<IdentityEvidence, LarmError> {
    let mut evidence = stored_evidence(model);
    if evidence.required_hashes.is_empty() {
        return Ok(evidence);
    }
    let filter = larm_domain::ports::VariantFilter {
        model_id: Some(model.id.clone()),
        ..Default::default()
    };
    let mut after = None;
    let mut hashes = std::collections::BTreeSet::new();
    let mut all_complete = true;
    loop {
        let page = variants.list_variants(after, 256, &filter).await?;
        for variant in &page {
            let mut variant_hashes = std::collections::BTreeSet::new();
            let mut complete = true;
            for artifact in variants.artifacts_of(&variant.id).await? {
                if artifact.required {
                    if let Some(hash) = artifact.sha256 {
                        hashes.insert(hash.clone());
                        variant_hashes.insert(hash);
                    } else {
                        complete = false;
                        all_complete = false;
                    }
                }
            }
            // A canonical model may gain other quantizations/distributions;
            // its original complete variant remains valid identity evidence.
            if complete && variant_hashes == evidence.required_hashes {
                return Ok(evidence);
            }
        }
        if page.len() < 256 {
            break;
        }
        after = page.last().map(|v| (v.created_at.clone(), v.id.clone()));
    }
    if !all_complete || hashes != evidence.required_hashes {
        evidence.required_hashes.clear();
    }
    Ok(evidence)
}

/// Busca el modelo existente al que pertenece la importación.
async fn existing_model(
    models: &dyn ModelRepository,
    variants: &dyn VariantRepository,
    import: &ModelImport,
    evidence: &IdentityEvidence,
    report: &mut ImportReport,
) -> Result<Option<CanonicalModel>, LarmError> {
    if let Some(upstream) = &import.upstream_identity {
        if let Some(model) = models.model_by_upstream(upstream).await? {
            return Ok(Some(model));
        }
    }
    // Una publicación ya registrada identifica su modelo: repetir la
    // importación no crea otro (ni un falso duplicado).
    for d in &import.distributions {
        if let Some(found) = models
            .distribution_by_identity(&d.source_id, &d.upstream_namespace, &d.upstream_model_id)
            .await?
        {
            let model = models.get_model(&found.canonical_model_id).await?;
            if model.as_ref().is_some_and(|m| {
                m.upstream_identity.is_none() || m.upstream_identity == import.upstream_identity
            }) {
                return Ok(model);
            }
        }
    }
    // Candidatos: homónimos y modelos que comparten bytes.
    let mut candidates = models.models_named(&import.name).await?;
    let hashes: Vec<String> = evidence
        .required_hashes
        .iter()
        .chain(&evidence.parent_hashes)
        .cloned()
        .collect();
    let mut found = variants.models_with_artifact_hashes(&hashes).await?;
    // Evidencia declarada: misma arquitectura, autor, base, configuración o
    // tokenizador; y los modelos cuya base es este (o este, su base).
    let lookups = [
        ("architecture", evidence.architecture.as_ref()),
        ("author", evidence.author.as_ref()),
        ("base_model", evidence.base_model.as_ref()),
        ("config_hash", evidence.config_hash.as_ref()),
        ("tokenizer_hash", evidence.tokenizer_hash.as_ref()),
        ("upstream_identity", evidence.base_model.as_ref()),
        ("base_model", evidence.upstream_identity.as_ref()),
    ];
    for (field, value) in lookups {
        if let Some(value) = value {
            found.extend(models.models_by_evidence(field, value).await?);
        }
    }
    for m in found {
        if !candidates.iter().any(|c| c.id == m.id) {
            candidates.push(m);
        }
    }
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let mut identical = None;
    for c in candidates {
        let r = resolve(evidence, &complete_stored_evidence(variants, &c).await?);
        report.resolutions.push(ResolutionRecord {
            model_id: c.id.clone(),
            grade: r.grade,
            reasons: r.reasons,
        });
        if r.auto_merge && identical.is_none() {
            identical = Some(c);
        }
    }
    Ok(identical)
}

/// Registra un modelo, sus distribuciones, revisiones y variantes.
///
/// # Errors
/// Validación del dominio, conflictos de identidad o fallo de la base.
pub async fn register(
    models: &dyn ModelRepository,
    variants: &dyn VariantRepository,
    import: &ModelImport,
    ctx: &MutationContext,
) -> Result<ImportReport, LarmError> {
    let (model, mut report) = prepare_canonical(models, variants, import).await?;
    if report.model_created {
        models.insert_model(&model, ctx).await?;
        for other in &report.possible_duplicates {
            models.add_possible_duplicate(other, &model.id, ctx).await?;
        }
    }
    let model_id = model.id;
    for d in &import.distributions {
        let distribution_id = distribution(models, &model_id, d, ctx, &mut report).await?;
        for r in &d.revisions {
            let revision_id = revision(models, &distribution_id, r, ctx, &mut report).await?;
            for v in &r.variants {
                variant(variants, &revision_id, v, ctx, &mut report).await?;
            }
        }
    }
    Ok(report)
}

async fn distribution(
    models: &dyn ModelRepository,
    model_id: &str,
    d: &DistributionImport,
    ctx: &MutationContext,
    report: &mut ImportReport,
) -> Result<String, LarmError> {
    match models
        .distribution_by_identity(&d.source_id, &d.upstream_namespace, &d.upstream_model_id)
        .await?
    {
        Some(found) if found.canonical_model_id == model_id => Ok(found.id),
        Some(found) => Err(LarmError::new(
            ErrorCode::Conflict,
            "Esa publicación ya pertenece a otro modelo",
            "distribution_belongs_to_other_model",
        )
        .with_resource(found.id)),
        None => {
            let t = now();
            let distribution = ModelDistribution {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: t.clone(),
                updated_at: t,
                row_version: 1,
                metadata: d.metadata.clone(),
                canonical_model_id: model_id.to_string(),
                source_id: d.source_id.clone(),
                upstream_namespace: d.upstream_namespace.clone(),
                upstream_model_id: d.upstream_model_id.clone(),
                license_id: d.license_id.clone(),
            };
            models.insert_distribution(&distribution, ctx).await?;
            report.distributions_created += 1;
            Ok(distribution.id)
        }
    }
}

async fn revision(
    models: &dyn ModelRepository,
    distribution_id: &str,
    r: &RevisionImport,
    ctx: &MutationContext,
    report: &mut ImportReport,
) -> Result<String, LarmError> {
    match models
        .revision_by_key(distribution_id, &r.revision_key)
        .await?
    {
        Some(found) if found.manifest_hash == r.manifest_hash => Ok(found.id),
        Some(found) => Err(LarmError::new(
            ErrorCode::Conflict,
            "La clave de revisión es inmutable y ya tiene otro contenido",
            "revision_key_immutable",
        )
        .with_resource(found.id)),
        None => {
            let t = now();
            let revision = ModelRevision {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: t.clone(),
                updated_at: t,
                row_version: 1,
                metadata: r.metadata.clone(),
                distribution_id: distribution_id.to_string(),
                revision_key: r.revision_key.clone(),
                upstream_commit: r.upstream_commit.clone(),
                manifest_hash: r.manifest_hash.clone(),
                state: r.state,
            };
            if r.supersedes_current && r.state == RevisionState::Current {
                models.replace_current_revision(&revision, ctx).await?;
            } else {
                models.insert_revision(&revision, ctx).await?;
            }
            report.revisions_created += 1;
            Ok(revision.id)
        }
    }
}

async fn variant(
    repo: &dyn VariantRepository,
    revision_id: &str,
    v: &VariantImport,
    ctx: &MutationContext,
    report: &mut ImportReport,
) -> Result<(), LarmError> {
    let id = uuid::Uuid::now_v7().to_string();
    let t = now();
    let artifacts: Vec<ModelArtifact> = v
        .artifacts
        .iter()
        .map(|a| {
            let mut metadata = a.metadata.clone();
            metadata.insert("role".into(), Value::String(a.role.as_str().into()));
            ModelArtifact {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: t.clone(),
                updated_at: t.clone(),
                row_version: 1,
                metadata,
                variant_id: id.clone(),
                filename: a.filename.clone(),
                format: a.format.clone(),
                quantization: a.quantization.clone(),
                expected_size: a.size_bytes,
                actual_size: None,
                size_estimate: ArtifactSizeEstimate::unknown(
                    "El tamaño importado no tiene evidencia de confianza fijada",
                    "import:size_bytes_unqualified",
                ),
                sha256: a.sha256.clone(),
                source_url: a.source_url.clone(),
                required: a.required,
            }
        })
        .collect();
    let (bundle_hash, bundle) = crate::bundle::build(&id, &artifacts)?;
    if let Some(found) = repo
        .find_variant(
            revision_id,
            v.format.as_deref(),
            v.quantization.as_deref(),
            v.architecture.as_deref(),
            bundle_hash.as_deref(),
        )
        .await?
    {
        report.variant_ids.push(found.id);
        return Ok(());
    }
    let variant = ModelVariant {
        id: id.clone(),
        created_at: t.clone(),
        updated_at: t,
        row_version: 1,
        metadata: v.metadata.clone(),
        revision_id: revision_id.to_string(),
        format: v.format.clone(),
        quantization: v.quantization.clone(),
        architecture: v.architecture.clone(),
        bundle_hash,
        lifecycle: "DISCOVERED".into(),
        availability: "UNKNOWN".into(),
        ownership: v.ownership.unwrap_or(Ownership::LarmManaged),
        promotion: "NONE".into(),
        system_role: "GENERAL".into(),
        capabilities: capabilities(&v.capabilities)?,
        model_limits: v.model_limits.clone().unwrap_or_default(),
        system1_state: None,
        update_available: false,
        status: "DISCOVERED".into(),
    };
    repo.insert_variant(&variant, &artifacts, bundle.as_ref(), ctx)
        .await?;
    report.variants_created += 1;
    report.variant_ids.push(id);
    Ok(())
}

/// Resolve and prepare P041 identity without writing. Shared by legacy imports
/// and the atomic remote-catalog path; names never imply an automatic merge.
pub(crate) async fn prepare_canonical(
    models: &dyn ModelRepository,
    variants: &dyn VariantRepository,
    import: &ModelImport,
) -> Result<(CanonicalModel, ImportReport), LarmError> {
    let caps = capabilities(&import.capabilities)?;
    let mut report = ImportReport {
        unqualified_capabilities: caps
            .iter()
            .filter(|c| !larm_domain::model::KNOWN_CAPABILITIES.contains(&c.as_str()))
            .cloned()
            .collect(),
        ..ImportReport::default()
    };
    let evidence = evidence_of(import);
    let existing = existing_model(models, variants, import, &evidence, &mut report).await?;
    let model = if let Some(model) = existing {
        model
    } else {
        let id = uuid::Uuid::now_v7().to_string();
        let t = now();
        let mut duplicates: Vec<String> = report
            .resolutions
            .iter()
            // Probable, o solo el nombre coincide: revisión humana. Un
            // desconocido por falta de evidencia no se anota.
            .filter(|r| {
                r.grade == IdentityGrade::ProbableSameModel
                    || (r.grade == IdentityGrade::Unknown
                        && r.reasons.iter().any(|x| x == "name_only"))
            })
            .map(|r| r.model_id.clone())
            .collect();
        duplicates.sort();
        let lineage: Vec<&ResolutionRecord> = report
            .resolutions
            .iter()
            .filter(|r| r.grade == IdentityGrade::DerivedModel)
            .collect();
        let mut metadata = import.metadata.clone();
        metadata.insert(
            "identity_evidence".into(),
            serde_json::to_value(&evidence).unwrap_or_default(),
        );
        if !report.resolutions.is_empty() {
            metadata.insert(
                "identity_resolutions".into(),
                serde_json::to_value(&report.resolutions).unwrap_or_default(),
            );
        }
        if !lineage.is_empty() {
            metadata.insert(
                "lineage".into(),
                serde_json::to_value(&lineage).unwrap_or_default(),
            );
        }
        let model = CanonicalModel {
            id: id.clone(),
            created_at: t.clone(),
            updated_at: t,
            row_version: 1,
            metadata,
            name: import.name.trim().to_string(),
            upstream_identity: import.upstream_identity.clone(),
            capabilities: caps,
            possible_duplicates: duplicates.clone(),
        };
        report.model_created = true;
        report.possible_duplicates = duplicates;
        model
    };
    report.model_id.clone_from(&model.id);
    Ok((model, report))
}
