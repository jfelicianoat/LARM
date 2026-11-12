//! Manifiestos de variante y lectura de raíces para reconstruir el
//! registro sin base de datos (P018; spec §134; contrato 02:
//! `root/manifests/<variant_id>.json` con checksums y procedencia).
//!
//! La reconstrucción lee los manifiestos, **relee los bytes** de cada blob
//! (sha256 y tamaño) y lista los blobs que ningún manifiesto cita. No
//! escribe en las raíces: el registro se reconstruye en una base de
//! staging que el operador fusiona con un restore verificado.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Versión del formato de manifiesto.
pub const MANIFEST_VERSION: u32 = 1;
/// Tamaño máximo de un manifiesto (no se lee más).
pub const MANIFEST_MAX_BYTES: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestModel {
    pub name: String,
    pub upstream_identity: Option<String>,
    /// Evidencia de identidad (P041) tal como se guardó.
    pub evidence: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestDistribution {
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
    pub license_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRevision {
    pub revision_key: String,
    pub manifest_hash: String,
    pub upstream_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestVariant {
    pub format: Option<String>,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestArtifact {
    pub filename: String,
    pub role: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub required: bool,
    /// Ruta POSIX del blob dentro de la raíz.
    pub blob: String,
}

/// Manifiesto de una variante gestionada por LARM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub variant_id: String,
    pub written_at: String,
    pub model: ManifestModel,
    pub distribution: ManifestDistribution,
    pub revision: ManifestRevision,
    pub variant: ManifestVariant,
    pub artifacts: Vec<ManifestArtifact>,
}

fn native(root: &Path, relative: &str) -> std::path::PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Escribe `root/manifests/<variant_id>.json` de forma atómica.
///
/// # Errors
/// E/S o un id de variante que no es UUIDv7.
pub fn write_manifest(root: &Path, manifest: &Manifest) -> std::io::Result<()> {
    if !larm_domain::bridge::is_uuid_v7(&manifest.variant_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "variant_id no es UUIDv7",
        ));
    }
    let dir = root.join("manifests");
    std::fs::create_dir_all(&dir)?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{}.json.partial", manifest.variant_id));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(format!("{}.json", manifest.variant_id)))
}

/// Estado de un blob citado por un manifiesto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlobStatus {
    Ok,
    Missing,
    Corrupt,
}

/// Manifiesto leído con el estado de sus blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedManifest {
    pub file: String,
    pub manifest: Manifest,
    pub blobs: Vec<(String, BlobStatus)>,
}

impl ScannedManifest {
    /// Todos sus blobs están y tienen sus bytes.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.blobs.iter().all(|(_, s)| *s == BlobStatus::Ok)
    }
}

/// Lo que se encontró en una raíz.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RootScan {
    pub manifests: Vec<ScannedManifest>,
    /// Manifiestos que no se pudieron usar: `(fichero, motivo)`.
    pub invalid: Vec<(String, String)>,
    /// Blobs que ningún manifiesto cita.
    pub orphan_blobs: Vec<String>,
}

/// Lee los manifiestos de `root` y comprueba los bytes de cada blob.
///
/// # Errors
/// E/S al listar la raíz.
pub fn scan_root(root: &Path) -> std::io::Result<RootScan> {
    let mut scan = RootScan::default();
    let mut checked: BTreeMap<String, BlobStatus> = BTreeMap::new();
    let dir = root.join("manifests");
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e),
    };
    files.sort();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if m.len() > MANIFEST_MAX_BYTES {
                    Err("manifest_too_large".to_string())
                } else {
                    std::fs::read(&path).map_err(|e| e.to_string())
                }
            })
            .and_then(|bytes| {
                serde_json::from_slice::<Manifest>(&bytes)
                    .map_err(|e| format!("manifest_invalid: {e}"))
            })
            .and_then(|m| {
                if m.schema_version == MANIFEST_VERSION {
                    Ok(m)
                } else {
                    Err(format!("manifest_version_unknown: {}", m.schema_version))
                }
            });
        let manifest = match parsed {
            Ok(m) => m,
            Err(reason) => {
                scan.invalid.push((name, reason));
                continue;
            }
        };
        let mut blobs = Vec::new();
        for a in &manifest.artifacts {
            let valid = crate::cas::blob_relative(&a.sha256).is_ok_and(|b| b == a.blob);
            let status = if !valid {
                BlobStatus::Corrupt
            } else if let Some(s) = checked.get(&a.blob) {
                *s
            } else {
                let s = match crate::cas::sha256_file(&native(root, &a.blob)) {
                    Ok((sha, size)) if sha == a.sha256 && size == a.size_bytes => BlobStatus::Ok,
                    Ok(_) => BlobStatus::Corrupt,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => BlobStatus::Missing,
                    Err(_) => BlobStatus::Corrupt,
                };
                checked.insert(a.blob.clone(), s);
                s
            };
            blobs.push((a.blob.clone(), status));
        }
        scan.manifests.push(ScannedManifest {
            file: name,
            manifest,
            blobs,
        });
    }
    let cited: BTreeSet<String> = scan
        .manifests
        .iter()
        .flat_map(|m| m.manifest.artifacts.iter().map(|a| a.blob.clone()))
        .collect();
    let blob_root = root.join("blobs");
    if blob_root.is_dir() {
        for f in crate::analyzer::list_root(&blob_root)? {
            let relative = format!("blobs/{}", f.relative);
            if !cited.contains(&relative) {
                scan.orphan_blobs.push(relative);
            }
        }
    }
    Ok(scan)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn manifest(variant: &str, blob: &str, sha: &str, size: u64) -> Manifest {
        Manifest {
            schema_version: MANIFEST_VERSION,
            variant_id: variant.into(),
            written_at: "2026-10-11T00:00:00.000Z".into(),
            model: ManifestModel {
                name: "Qwen X".into(),
                upstream_identity: None,
                evidence: serde_json::json!({}),
            },
            distribution: ManifestDistribution {
                source_id: "0192f000-0000-7000-8000-00000000f001".into(),
                upstream_namespace: "local".into(),
                upstream_model_id: "qwen".into(),
                license_id: None,
            },
            revision: ManifestRevision {
                revision_key: format!("content:{}", "a".repeat(64)),
                manifest_hash: "a".repeat(64),
                upstream_commit: None,
            },
            variant: ManifestVariant {
                format: Some("GGUF".into()),
                quantization: Some("Q4_K_M".into()),
                architecture: Some("qwen2".into()),
                capabilities: vec![],
            },
            artifacts: vec![ManifestArtifact {
                filename: "m.gguf".into(),
                role: "weights".into(),
                sha256: sha.into(),
                size_bytes: size,
                required: true,
                blob: blob.into(),
            }],
        }
    }

    #[test]
    fn manifiestos_blobs_y_huerfanos() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src.gguf");
        std::fs::write(&src, b"GGUF pesos").unwrap();
        let (sha, size) = crate::cas::sha256_file(&src).unwrap();
        let (blob, _) = crate::cas::ingest(root, &src, &sha, "j").unwrap();
        let orphan = root.join("huerfano.bin");
        std::fs::write(&orphan, b"nadie").unwrap();
        let (osha, _) = crate::cas::sha256_file(&orphan).unwrap();
        crate::cas::ingest(root, &orphan, &osha, "j").unwrap();
        let v1 = "0192f000-0000-7000-8000-000000000a01";
        let v2 = "0192f000-0000-7000-8000-000000000a02";
        write_manifest(root, &manifest(v1, &blob, &sha, size)).unwrap();
        // Un manifiesto que cita un blob que no existe.
        let gone = "e".repeat(64);
        write_manifest(
            root,
            &manifest(v2, &crate::cas::blob_relative(&gone).unwrap(), &gone, 3),
        )
        .unwrap();
        std::fs::write(root.join("manifests").join("roto.json"), b"{no es json").unwrap();
        assert!(write_manifest(root, &manifest("x", &blob, &sha, size)).is_err());
        let scan = scan_root(root).unwrap();
        assert_eq!(scan.manifests.len(), 2);
        let ok = scan
            .manifests
            .iter()
            .find(|m| m.manifest.variant_id == v1)
            .unwrap();
        assert!(ok.complete());
        let missing = scan
            .manifests
            .iter()
            .find(|m| m.manifest.variant_id == v2)
            .unwrap();
        assert_eq!(missing.blobs[0].1, BlobStatus::Missing);
        assert_eq!(scan.invalid.len(), 1);
        assert!(scan.invalid[0].1.starts_with("manifest_invalid"));
        assert_eq!(
            scan.orphan_blobs,
            vec![crate::cas::blob_relative(&osha).unwrap()]
        );
        // Un blob con otros bytes es CORRUPT (se relee siempre).
        let blob_path = native(root, &blob);
        let mut perms = std::fs::metadata(&blob_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&blob_path, perms).unwrap();
        std::fs::write(&blob_path, b"GGUF otros!").unwrap();
        let scan = scan_root(root).unwrap();
        let ok = scan
            .manifests
            .iter()
            .find(|m| m.manifest.variant_id == v1)
            .unwrap();
        assert_eq!(ok.blobs[0].1, BlobStatus::Corrupt);
    }
}
