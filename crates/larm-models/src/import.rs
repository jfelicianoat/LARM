//! Importación de modelos existentes desde una carpeta (P017; spec §41,
//! §42; contratos 01 y 02).
//!
//! - **ADOPT** (por defecto): no mueve ni copia ni modifica nada. Registra
//!   cada fichero donde está, con `registration_mode = ADOPTED` en su
//!   metadata, y `EXTERNAL` o `HOST_MANAGED` como propiedad. La carpeta
//!   tiene que estar dentro de una ubicación registrada.
//! - **COPY**: copia verificada al CAS de la ubicación destino, que pasa a
//!   ser `LARM_MANAGED`. Si el blob ya estaba, no se duplica: las filas de
//!   esa ruta pasan a `SHARED`.
//!
//! El hash se calcula en streaming; la caché por tamaño + mtime (guardada
//! en `metadata.hash_cache`) solo evita releer en una reimportación y nunca
//! cuenta como verificación (`verified_at` queda null en ese caso).
//! Repetir la importación no crea nada.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use larm_domain::artifacts::{ArtifactLocation, ArtifactState, Ownership};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{bundle_hash, BundleEntry, RevisionState};
use larm_domain::ports::{
    ArtifactLocationRepository, ModelRepository, MutationContext, VariantRepository,
};
use larm_domain::units::DecimalU64;
use larm_storage::cas::{hash_with_cache, ingest, HashCache, Ingested};
use serde::Serialize;

use crate::registry::{
    register, ArtifactImport, DistributionImport, EvidenceImport, ImportReport, ModelImport,
    RevisionImport, VariantImport,
};
use crate::scan::{detect_files, group, Ignored, Kind, ScannedVariant};

/// Archivo reconocido que la interfaz puede revisar antes de importar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPreviewFile {
    pub relative_path: String,
    pub kind: Kind,
    pub role: larm_domain::model::ArtifactRole,
    pub required: bool,
    pub size_bytes: DecimalU64,
    pub facts: BTreeMap<String, String>,
}

/// Variante detectada en una exploración sin registrar ni copiar datos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPreviewVariant {
    pub dir: String,
    pub name: String,
    pub format: String,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub size_bytes: DecimalU64,
    pub files: Vec<ImportPreviewFile>,
}

/// Elemento no seguido o ilegible durante la exploración.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPreviewSkipped {
    pub relative_path: String,
    pub reason: String,
}

/// Archivo legible sin modelo o formato reconocido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPreviewIgnored {
    pub relative_path: String,
    pub reason: String,
}

/// Resultado read-only de detectar una carpeta local; no incluye rutas absolutas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportScanPreview {
    pub files_seen: usize,
    pub recognized_files: usize,
    pub bytes_seen: DecimalU64,
    pub recognized_bytes: DecimalU64,
    pub variants: Vec<ImportPreviewVariant>,
    pub ignored: Vec<ImportPreviewIgnored>,
    pub skipped: Vec<ImportPreviewSkipped>,
}

struct LocalScan {
    found: Vec<larm_storage::import::Found>,
    paths: BTreeMap<String, PathBuf>,
    variants: Vec<ScannedVariant>,
    ignored: Vec<Ignored>,
    skipped: Vec<ImportPreviewSkipped>,
}

fn size_overflow() -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "El tamaño total de la carpeta excede el límite admitido",
        "size_overflow",
    )
}

fn add_size(total: u64, size: u64) -> Result<u64, LarmError> {
    total.checked_add(size).ok_or_else(size_overflow)
}

fn decimal_size(size: u64) -> Result<DecimalU64, LarmError> {
    DecimalU64::new(size).ok_or_else(size_overflow)
}

fn scan_directory(dir: &Path) -> Result<LocalScan, LarmError> {
    let (found, skipped) =
        larm_storage::import::walk(dir).map_err(|e| io("recorriendo la carpeta", &e))?;
    let mut paths = BTreeMap::new();
    for file in &found {
        paths.insert(file.relative.clone(), file.path.clone());
    }
    let candidates: Vec<_> = found
        .iter()
        .map(|file| (file.relative.as_str(), file.head.as_slice(), file.size))
        .collect();
    let (detected, ignored) = detect_files(&candidates);
    let skipped = skipped
        .into_iter()
        .map(|item| ImportPreviewSkipped {
            relative_path: item.relative,
            reason: item.reason,
        })
        .collect();
    Ok(LocalScan {
        found,
        paths,
        variants: group(&detected),
        ignored,
        skipped,
    })
}

/// Detecta modelos y ficheros sin calcular hashes, escribir archivos ni tocar la base.
///
/// # Errors
/// `RESOURCE_UNAVAILABLE` si no se puede recorrer la carpeta;
/// `VALIDATION_FAILED` si el acumulado excede el máximo contractual.
pub fn preview_dir(dir: &Path) -> Result<ImportScanPreview, LarmError> {
    let scan = scan_directory(dir)?;
    let by_path: BTreeMap<&str, u64> = scan
        .found
        .iter()
        .map(|item| (item.relative.as_str(), item.size))
        .collect();
    let bytes_seen = scan
        .found
        .iter()
        .try_fold(0u64, |total, file| add_size(total, file.size))?;
    let mut recognized_paths = BTreeSet::new();
    let mut variants = Vec::with_capacity(scan.variants.len());
    for variant in scan.variants {
        let mut size_bytes = 0u64;
        let mut files = Vec::with_capacity(variant.files.len());
        for file in variant.files {
            let size = *by_path
                .get(file.relative.as_str())
                .ok_or_else(size_overflow)?;
            size_bytes = add_size(size_bytes, size)?;
            recognized_paths.insert(file.relative.clone());
            files.push(ImportPreviewFile {
                relative_path: file.relative,
                kind: file.kind,
                role: file.role,
                required: file.required,
                size_bytes: decimal_size(size)?,
                facts: file.facts,
            });
        }
        variants.push(ImportPreviewVariant {
            dir: variant.dir,
            name: variant.name,
            format: variant.format,
            quantization: variant.quantization,
            architecture: variant.architecture,
            size_bytes: decimal_size(size_bytes)?,
            files,
        });
    }
    let recognized_bytes = recognized_paths.iter().try_fold(0u64, |total, path| {
        by_path
            .get(path.as_str())
            .copied()
            .map_or_else(|| Err(size_overflow()), |size| add_size(total, size))
    })?;
    Ok(ImportScanPreview {
        files_seen: scan.found.len() + scan.skipped.len(),
        recognized_files: recognized_paths.len(),
        bytes_seen: decimal_size(bytes_seen)?,
        recognized_bytes: decimal_size(recognized_bytes)?,
        variants,
        ignored: scan
            .ignored
            .into_iter()
            .map(|item| ImportPreviewIgnored {
                relative_path: item.relative,
                reason: item.reason,
            })
            .collect(),
        skipped: scan.skipped,
    })
}

/// Cómo se importa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImportMode {
    Adopt,
    Copy,
}

/// Parámetros de una importación.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub dir: PathBuf,
    /// Ubicación que contiene `dir` (ADOPT) o destino del CAS (COPY).
    pub storage_location_id: String,
    /// Raíz actual de esa ubicación en disco.
    pub location_root: PathBuf,
    /// Fuente registrada a la que se atribuyen las distribuciones.
    pub source_id: String,
    pub mode: ImportMode,
    /// Si se indica, limita el importador a las variantes de ese formato detectado.
    pub format: Option<String>,
    /// Licencia declarada para la distribución importada; no se verifica aquí.
    pub license_id: Option<String>,
    /// Propiedad al adoptar: `EXTERNAL` o `HOST_MANAGED`.
    pub adopt_as: Ownership,
}

/// Variante importada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedVariant {
    pub dir: String,
    pub name: String,
    pub format: String,
    pub files: usize,
    pub variant_id: String,
}

/// Informe de la importación.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DirImportReport {
    pub variants: Vec<ImportedVariant>,
    pub models: Vec<ImportReport>,
    /// Ficheros no reconocidos: no se registran.
    pub ignored: Vec<Ignored>,
    /// Enlaces y junctions no seguidos, ficheros ilegibles.
    pub skipped: Vec<(String, String)>,
    pub hashed: usize,
    pub hashed_from_cache: usize,
    pub locations_created: usize,
    pub blobs_copied: usize,
    pub blobs_already_present: usize,
}

fn io(what: &str, e: &std::io::Error) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo leer la carpeta o un fichero",
        "io_failed",
    )
    .with_technical(format!("{what}: {e}"))
    .retryable(true)
}

fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(path).ok()?;
    let rest = path.strip_prefix(&root).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Canonicaliza la carpeta antes de crear su fuente de procedencia. En modo
/// ADOPT también garantiza que siga dentro de la ubicación registrada.
///
/// # Errors
/// `RESOURCE_UNAVAILABLE` si no existe o no puede leerse la ruta;
/// `VALIDATION_FAILED` si no es una carpeta o ADOPT sale de la ubicación.
pub fn canonical_source_dir(
    dir: &Path,
    location_root: &Path,
    mode: ImportMode,
) -> Result<PathBuf, LarmError> {
    let canonical =
        std::fs::canonicalize(dir).map_err(|error| io("validando la carpeta", &error))?;
    if !canonical.is_dir() {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "La ruta de origen debe ser una carpeta",
            "import_source_path_invalid",
        ));
    }
    if mode == ImportMode::Adopt && relative_to(location_root, &canonical).is_none() {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "La carpeta no está dentro de la ubicación indicada",
            "outside_location",
        ));
    }
    Ok(canonical)
}

struct Hashed {
    sha256: String,
    size: u64,
    cache: HashCache,
    from_cache: bool,
}

/// Importa la carpeta.
///
/// # Errors
/// Carpeta fuera de la ubicación (ADOPT), E/S, validación del dominio o
/// conflictos de identidad.
#[allow(clippy::too_many_lines)]
pub async fn import_dir(
    models: &dyn ModelRepository,
    variants: &dyn VariantRepository,
    locations: &dyn ArtifactLocationRepository,
    options: &ImportOptions,
    ctx: &MutationContext,
) -> Result<DirImportReport, LarmError> {
    if !matches!(
        options.adopt_as,
        Ownership::External | Ownership::HostManaged
    ) {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Adoptar solo admite EXTERNAL o HOST_MANAGED",
            "adopt_ownership_invalid",
        ));
    }
    let base = match options.mode {
        ImportMode::Adopt => Some(
            relative_to(&options.location_root, &options.dir).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "La carpeta no está dentro de la ubicación indicada",
                    "outside_location",
                )
            })?,
        ),
        ImportMode::Copy => None,
    };
    let dir = options.dir.clone();
    let scan = tokio::task::spawn_blocking(move || scan_directory(&dir))
        .await
        .map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Recorrido interrumpido", "join")
                .with_technical(e.to_string())
        })??;
    let mut report = DirImportReport {
        skipped: scan
            .skipped
            .iter()
            .map(|item| (item.relative_path.clone(), item.reason.clone()))
            .collect(),
        ignored: scan.ignored,
        ..DirImportReport::default()
    };
    let paths = scan.paths;
    let mut scanned = scan.variants;
    if let Some(format) = options.format.as_deref() {
        let format = format.trim();
        if format.is_empty() {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El filtro de formato no puede estar vacío",
                "import_format_invalid",
            ));
        }
        scanned.retain(|variant| variant.format.eq_ignore_ascii_case(format));
        if scanned.is_empty() {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La carpeta no contiene variantes del formato seleccionado",
                "import_format_not_found",
            ));
        }
    }
    // Ruta registrada de cada fichero adoptado (relativa a la ubicación).
    let registered = |relative: &str| -> String {
        match &base {
            Some(b) if !b.is_empty() => format!("{b}/{relative}"),
            _ => relative.to_string(),
        }
    };
    // Hash de cada fichero usado, con la caché de una adopción anterior.
    let mut hashes: BTreeMap<String, Hashed> = BTreeMap::new();
    for v in &scanned {
        for f in &v.files {
            if hashes.contains_key(&f.relative) {
                continue;
            }
            let cached = if base.is_some() {
                locations
                    .at_path(&options.storage_location_id, &registered(&f.relative))
                    .await?
                    .into_iter()
                    .find_map(|l| l.metadata.get("hash_cache").cloned())
                    .and_then(|c| serde_json::from_value::<HashCache>(c).ok())
            } else {
                None
            };
            let path = paths.get(&f.relative).cloned().unwrap_or_default();
            let h = tokio::task::spawn_blocking(move || hash_with_cache(&path, cached.as_ref()))
                .await
                .map_err(|e| {
                    LarmError::new(ErrorCode::Internal, "Hash interrumpido", "join")
                        .with_technical(e.to_string())
                })?
                .map_err(|e| io(&f.relative, &e))?;
            if h.from_cache {
                report.hashed_from_cache += 1;
            } else {
                report.hashed += 1;
            }
            hashes.insert(
                f.relative.clone(),
                Hashed {
                    sha256: h.sha256,
                    size: h.size,
                    cache: h.cache,
                    from_cache: h.from_cache,
                },
            );
        }
    }
    // Un modelo por (carpeta, nombre); cada variante con su contenido.
    let mut by_model: BTreeMap<(String, String), Vec<&ScannedVariant>> = BTreeMap::new();
    for v in &scanned {
        by_model
            .entry((v.dir.clone(), v.name.clone()))
            .or_default()
            .push(v);
    }
    let ownership = match options.mode {
        ImportMode::Adopt => options.adopt_as,
        ImportMode::Copy => Ownership::LarmManaged,
    };
    for ((dir, name), group) in by_model {
        let mut variant_imports = Vec::new();
        let mut manifests = Vec::new();
        for v in &group {
            let within = |relative: &str| -> String {
                relative
                    .strip_prefix(&format!("{dir}/"))
                    .unwrap_or(relative)
                    .to_string()
            };
            let entries: Vec<BundleEntry> = v
                .files
                .iter()
                .map(|f| BundleEntry {
                    relative_path: within(&f.relative),
                    sha256: hashes[&f.relative].sha256.clone(),
                    size_bytes: hashes[&f.relative].size,
                    required: f.required,
                })
                .collect();
            manifests.push(bundle_hash(&entries)?);
            let mut metadata = serde_json::Map::new();
            metadata.insert(
                "registration_mode".into(),
                serde_json::Value::String(
                    match options.mode {
                        ImportMode::Adopt => "ADOPTED",
                        ImportMode::Copy => "MANAGED",
                    }
                    .into(),
                ),
            );
            metadata.insert("scanned_format".into(), v.format.clone().into());
            variant_imports.push(VariantImport {
                format: Some(v.format.clone()),
                quantization: v.quantization.clone(),
                architecture: v.architecture.clone(),
                capabilities: vec![],
                ownership: Some(ownership),
                model_limits: None,
                metadata,
                artifacts: v
                    .files
                    .iter()
                    .map(|f| {
                        let mut metadata = serde_json::Map::new();
                        metadata.insert(
                            "detected_kind".into(),
                            serde_json::to_value(f.kind).unwrap_or_default(),
                        );
                        for (k, val) in &f.facts {
                            metadata.insert(k.clone(), val.clone().into());
                        }
                        ArtifactImport {
                            filename: within(&f.relative),
                            role: f.role,
                            format: None,
                            quantization: None,
                            size_bytes: DecimalU64::new(hashes[&f.relative].size),
                            sha256: Some(hashes[&f.relative].sha256.clone()),
                            source_url: None,
                            required: f.required,
                            metadata,
                        }
                    })
                    .collect(),
            });
        }
        manifests.sort();
        let revision_hash = larm_domain::canonical::sha256_hex(&serde_json::json!(manifests))
            .map_err(|e| {
                LarmError::new(ErrorCode::Internal, "Hash de revisión", "canonical")
                    .with_technical(e)
            })?;
        let architecture = group.iter().find_map(|v| v.architecture.clone());
        let import = ModelImport {
            name: name.clone(),
            upstream_identity: None,
            capabilities: vec![],
            metadata: serde_json::Map::new(),
            evidence: Some(EvidenceImport {
                architecture,
                ..EvidenceImport::default()
            }),
            distributions: vec![DistributionImport {
                source_id: options.source_id.clone(),
                upstream_namespace: "local".into(),
                upstream_model_id: if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}#{name}")
                },
                license_id: options.license_id.clone(),
                metadata: serde_json::Map::new(),
                revisions: vec![RevisionImport {
                    revision_key: format!("content:{revision_hash}"),
                    upstream_commit: None,
                    manifest_hash: revision_hash,
                    state: RevisionState::Current,
                    metadata: serde_json::Map::new(),
                    variants: variant_imports,
                    // El contenido de una carpeta local puede cambiar: la
                    // revisión nueva sustituye a la anterior.
                    supersedes_current: true,
                }],
            }],
        };
        let model_report = register(models, variants, &import, ctx).await?;
        for (v, variant_id) in group.iter().zip(&model_report.variant_ids) {
            let artifacts = variants.artifacts_of(variant_id).await?;
            let mut rows = Vec::new();
            for f in &v.files {
                let filename = f
                    .relative
                    .strip_prefix(&format!("{dir}/"))
                    .unwrap_or(&f.relative);
                let Some(artifact) = artifacts.iter().find(|a| a.filename == filename) else {
                    continue;
                };
                let h = &hashes[&f.relative];
                let now = larm_domain::bridge::timestamp(chrono::Utc::now());
                let mut metadata = serde_json::Map::new();
                let (relative_path, row_ownership) =
                    match options.mode {
                        ImportMode::Adopt => {
                            metadata.insert("registration_mode".into(), "ADOPTED".into());
                            metadata.insert(
                                "hash_cache".into(),
                                serde_json::to_value(&h.cache).unwrap_or_default(),
                            );
                            (registered(&f.relative), ownership)
                        }
                        ImportMode::Copy => {
                            metadata.insert("registration_mode".into(), "MANAGED".into());
                            let (root, source, sha, job) = (
                                options.location_root.clone(),
                                paths[&f.relative].clone(),
                                h.sha256.clone(),
                                ctx.correlation_id.clone(),
                            );
                            let (blob, how) = tokio::task::spawn_blocking(move || {
                                ingest(&root, &source, &sha, &job)
                            })
                            .await
                            .map_err(|e| {
                                LarmError::new(ErrorCode::Internal, "Copia interrumpida", "join")
                                    .with_technical(e.to_string())
                            })?
                            .map_err(|e| io(&f.relative, &e))?;
                            let shared = how == Ingested::AlreadyPresent
                                && locations
                                    .at_path(&options.storage_location_id, &blob)
                                    .await?
                                    .iter()
                                    .any(|l| l.artifact_id != artifact.id);
                            match how {
                                Ingested::Copied => report.blobs_copied += 1,
                                Ingested::AlreadyPresent => report.blobs_already_present += 1,
                            }
                            if shared {
                                locations
                                    .mark_shared(&options.storage_location_id, &blob, ctx)
                                    .await?;
                            }
                            (
                                blob,
                                if shared {
                                    Ownership::Shared
                                } else {
                                    Ownership::LarmManaged
                                },
                            )
                        }
                    };
                rows.push(ArtifactLocation {
                    id: uuid::Uuid::now_v7().to_string(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    row_version: 1,
                    metadata,
                    artifact_id: artifact.id.clone(),
                    storage_location_id: options.storage_location_id.clone(),
                    relative_path,
                    file_size: DecimalU64::new(h.size),
                    sha256: Some(h.sha256.clone()),
                    state: ArtifactState::Online,
                    // La caché no es verificación.
                    verified_at: (!h.from_cache).then_some(now),
                    ownership: row_ownership,
                });
            }
            report.locations_created += locations.insert_locations(&rows, ctx).await?;
            if options.mode == ImportMode::Copy {
                // Manifiesto junto al CAS: permite reconstruir sin base (P018).
                let distribution = &import.distributions[0];
                let revision = &distribution.revisions[0];
                let manifest = larm_storage::rebuild::Manifest {
                    schema_version: larm_storage::rebuild::MANIFEST_VERSION,
                    variant_id: variant_id.clone(),
                    written_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
                    model: larm_storage::rebuild::ManifestModel {
                        name: import.name.clone(),
                        upstream_identity: import.upstream_identity.clone(),
                        evidence: serde_json::to_value(&import.evidence).unwrap_or_default(),
                    },
                    distribution: larm_storage::rebuild::ManifestDistribution {
                        source_id: distribution.source_id.clone(),
                        upstream_namespace: distribution.upstream_namespace.clone(),
                        upstream_model_id: distribution.upstream_model_id.clone(),
                        license_id: distribution.license_id.clone(),
                    },
                    revision: larm_storage::rebuild::ManifestRevision {
                        revision_key: revision.revision_key.clone(),
                        manifest_hash: revision.manifest_hash.clone(),
                        upstream_commit: revision.upstream_commit.clone(),
                    },
                    variant: larm_storage::rebuild::ManifestVariant {
                        format: Some(v.format.clone()),
                        quantization: v.quantization.clone(),
                        architecture: v.architecture.clone(),
                        capabilities: vec![],
                    },
                    artifacts: artifacts
                        .iter()
                        .filter_map(|a| {
                            let row = rows.iter().find(|r| r.artifact_id == a.id)?;
                            Some(larm_storage::rebuild::ManifestArtifact {
                                filename: a.filename.clone(),
                                role: a.role().as_str().into(),
                                sha256: a.sha256.clone()?,
                                size_bytes: a.expected_size?.get(),
                                required: a.required,
                                blob: row.relative_path.clone(),
                            })
                        })
                        .collect(),
                };
                let root = options.location_root.clone();
                tokio::task::spawn_blocking(move || {
                    larm_storage::rebuild::write_manifest(&root, &manifest)
                })
                .await
                .map_err(|e| {
                    LarmError::new(ErrorCode::Internal, "Manifiesto interrumpido", "join")
                        .with_technical(e.to_string())
                })?
                .map_err(|e| io("escribiendo el manifiesto", &e))?;
            }
            report.variants.push(ImportedVariant {
                dir: v.dir.clone(),
                name: v.name.clone(),
                format: v.format.clone(),
                files: v.files.len(),
                variant_id: variant_id.clone(),
            });
        }
        report.models.push(model_report);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn preview_detecta_formatos_y_tamanos_sin_exponer_ruta_absoluta() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("Qwen")).unwrap();
        std::fs::write(root.path().join("Qwen/model-Q4_K_M.gguf"), b"GGUFweights").unwrap();
        std::fs::write(root.path().join("Qwen/README.md"), b"notas").unwrap();
        std::fs::write(
            root.path().join("Qwen/archive.zip"),
            b"\x50\x4b\x03\x04ignored",
        )
        .unwrap();
        let before: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        let preview = preview_dir(root.path()).unwrap();
        let after: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        assert_eq!(before, after, "la exploración no crea ni elimina nada");
        assert_eq!(preview.files_seen, 3);
        assert_eq!(preview.recognized_files, 2);
        assert_eq!(preview.bytes_seen.get(), 11 + 5 + 11);
        assert_eq!(preview.recognized_bytes.get(), 16);
        assert_eq!(preview.variants.len(), 1);
        assert_eq!(preview.variants[0].name, "model-Q4_K_M");
        assert_eq!(preview.variants[0].format, "GGUF");
        assert_eq!(preview.variants[0].size_bytes.get(), 16);
        assert_eq!(preview.ignored[0].relative_path, "Qwen/archive.zip");
        assert!(preview
            .variants
            .iter()
            .flat_map(|v| &v.files)
            .all(|f| !std::path::Path::new(&f.relative_path).is_absolute()));
    }

    #[test]
    fn preview_incluye_bundles_ct2_ncnn_y_mnn_con_tamanos_exactos() {
        let root = tempfile::tempdir().unwrap();
        for dir in ["ct2", "ncnn", "mnn"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let ct2_config = br#"{"model_type":"nllb"}"#;
        let mut ct2_model = 6u32.to_le_bytes().to_vec();
        ct2_model.extend(12u16.to_le_bytes());
        ct2_model.extend(b"transformer\0");
        ct2_model.extend(1u32.to_le_bytes());
        ct2_model.extend(1u32.to_le_bytes());
        let ncnn_param = b"7767517\n1 1\nInput input 0 1 data\n";
        let mnn_model = [12, 0, 0, 0, 6, 0, 4, 0, 0, 0, 0, 0, 8, 0, 0, 0];
        for (path, bytes) in [
            ("ct2/config.json", ct2_config.as_slice()),
            ("ct2/model.bin", ct2_model.as_slice()),
            ("ct2/source_vocabulary.json", b"{}".as_slice()),
            ("ncnn/net.param", ncnn_param.as_slice()),
            ("ncnn/net.bin", b"raw ncnn weights".as_slice()),
            ("mnn/config.json", b"{}".as_slice()),
            ("mnn/llm.mnn", mnn_model.as_slice()),
            ("mnn/llm.mnn.weight", b"external weights".as_slice()),
            ("mnn/llm_config.json", b"{}".as_slice()),
            ("mnn/tokenizer.txt", b"tokens".as_slice()),
            ("mnn/embeddings_bf16.bin", b"embeddings".as_slice()),
        ] {
            std::fs::write(root.path().join(path), bytes).unwrap();
        }

        let preview = preview_dir(root.path()).unwrap();
        assert_eq!(preview.files_seen, 11);
        assert_eq!(preview.recognized_files, 11);
        assert_eq!(preview.bytes_seen.get(), preview.recognized_bytes.get());
        assert!(preview.ignored.is_empty());
        assert_eq!(
            preview
                .variants
                .iter()
                .map(|variant| variant.format.as_str())
                .collect::<Vec<_>>(),
            ["CTranslate2", "MNN", "NCNN"]
        );
        for variant in &preview.variants {
            assert_eq!(
                variant.size_bytes.get(),
                variant
                    .files
                    .iter()
                    .map(|file| file.size_bytes.get())
                    .sum::<u64>()
            );
        }
    }

    #[test]
    fn preview_es_determinista_y_no_suma_ficheros_compartidos_dos_veces() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("Qwen")).unwrap();
        std::fs::write(root.path().join("Qwen/model-Q4_K_M.gguf"), b"GGUFq4").unwrap();
        std::fs::write(root.path().join("Qwen/model-Q8_0.gguf"), b"GGUFq8").unwrap();
        std::fs::write(root.path().join("Qwen/README.md"), b"shared").unwrap();

        let one = preview_dir(root.path()).unwrap();
        let two = preview_dir(root.path()).unwrap();

        assert_eq!(one, two);
        assert_eq!(one.variants.len(), 2);
        assert_eq!(
            one.recognized_files, 3,
            "README compartido cuenta una vez en el resumen de carpeta"
        );
        assert_eq!(one.bytes_seen.get(), 18);
        assert_eq!(one.recognized_bytes.get(), 18);
    }

    #[test]
    fn add_size_rechaza_overflow() {
        assert_eq!(
            add_size(u64::MAX, 1).unwrap_err().details.reason,
            "size_overflow"
        );
        assert_eq!(
            decimal_size(i64::MAX as u64 + 1)
                .unwrap_err()
                .details
                .reason,
            "size_overflow"
        );
    }
}
