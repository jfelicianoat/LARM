//! Bundle de una variante (P041; spec §21, contrato 01): construcción
//! determinista desde sus artefactos y verificación contra los bytes en
//! disco.
//!
//! - El `manifest_hash` del bundle cubre todos los descriptores (requeridos
//!   y opcionales); el `bundle_hash` de la variante, solo los requeridos (su
//!   identidad).
//! - Sin sha256 o tamaño de algún artefacto no hay bundle: no se inventa.
//! - Un requerido ausente (un shard, el tokenizador, el mmproj…) deja el
//!   bundle INCOMPLETE; unos bytes distintos lo dejan CORRUPT. También es
//!   INCOMPLETE si falta un rol que la variante necesita para ejecutarse.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    bundle_hash, bundle_state, missing_roles, required_hash, ArtifactRole, BundleEntry,
    BundleState, ModelArtifact, ModelBundle, Observed,
};
use larm_domain::ports::{MutationContext, VariantRepository};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Descriptores del bundle; `None` si algún artefacto no tiene sha256 o
/// tamaño esperado.
#[must_use]
pub fn entries(artifacts: &[ModelArtifact]) -> Option<Vec<BundleEntry>> {
    artifacts
        .iter()
        .map(|a| {
            Some(BundleEntry {
                relative_path: a.filename.clone(),
                sha256: a.sha256.clone()?,
                size_bytes: a.expected_size?.get(),
                required: a.required,
            })
        })
        .collect()
}

/// `bundle_hash` de la variante y su bundle (estado INCOMPLETE hasta
/// verificar los bytes). `(None, None)` si faltan hashes o tamaños.
///
/// # Errors
/// Descriptores no válidos (rutas, duplicados, hashes).
pub fn build(
    variant_id: &str,
    artifacts: &[ModelArtifact],
) -> Result<(Option<String>, Option<ModelBundle>), LarmError> {
    let Some(list) = entries(artifacts) else {
        return Ok((None, None));
    };
    let identity = required_hash(&list)?;
    let manifest = bundle_hash(&list)?;
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let ids = |required: bool| {
        let mut v: Vec<String> = artifacts
            .iter()
            .filter(|a| a.required == required)
            .map(|a| a.id.clone())
            .collect();
        v.sort();
        v
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "roles".into(),
        serde_json::to_value(
            artifacts
                .iter()
                .map(|a| (a.filename.clone(), a.role()))
                .collect::<BTreeMap<String, ArtifactRole>>(),
        )
        .unwrap_or_default(),
    );
    Ok((
        Some(identity),
        Some(ModelBundle {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            variant_id: variant_id.to_string(),
            required_artifact_ids: ids(true),
            optional_artifact_ids: ids(false),
            manifest_hash: manifest,
            state: BundleState::Incomplete,
        }),
    ))
}

fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

/// Hashea los ficheros del bundle presentes bajo `dir` (los ausentes no
/// aparecen). Un fichero que existe pero no se puede leer es un error.
///
/// # Errors
/// E/S distinta de «no existe».
pub fn observe_dir(
    dir: &Path,
    list: &[BundleEntry],
) -> Result<BTreeMap<String, Observed>, LarmError> {
    let mut out = BTreeMap::new();
    for e in list {
        let path = dir.join(e.relative_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        match sha256_file(&path) {
            Ok((sha256, size_bytes)) => {
                out.insert(e.relative_path.clone(), Observed { sha256, size_bytes });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo leer un fichero del bundle",
                    "io_failed",
                )
                .with_technical(format!("{}: {err}", e.relative_path))
                .retryable(true));
            }
        }
    }
    Ok(out)
}

/// Resultado de verificar un bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub bundle_id: String,
    pub state: BundleState,
    pub row_version: i64,
    pub missing: Vec<String>,
    pub mismatched: Vec<String>,
    /// Roles necesarios para ejecutar que la variante no declara; `null` si
    /// el formato no se conoce.
    pub missing_roles: Option<Vec<ArtifactRole>>,
}

/// Verifica el bundle de la variante contra los ficheros de `dir` y guarda
/// el estado (CAS y evento).
///
/// # Errors
/// Variante o bundle desconocidos, E/S o conflicto de versión.
pub async fn verify(
    repo: &dyn VariantRepository,
    variant_id: &str,
    dir: &Path,
    ctx: &MutationContext,
) -> Result<Verification, LarmError> {
    let variant = repo.get_variant(variant_id).await?.ok_or_else(|| {
        LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
            .with_resource(variant_id.to_string())
    })?;
    let bundle = repo.bundle_of(variant_id).await?.ok_or_else(|| {
        LarmError::new(
            ErrorCode::InvalidStateTransition,
            "La variante no tiene bundle: faltan hashes o tamaños de sus artefactos",
            "bundle_missing",
        )
        .with_resource(variant_id.to_string())
    })?;
    let artifacts = repo.bundle_artifacts(&bundle).await?;
    let list = entries(&artifacts).ok_or_else(|| {
        LarmError::new(
            ErrorCode::Internal,
            "Bundle sin descriptores completos",
            "bundle_inconsistent",
        )
    })?;
    if bundle_hash(&list)? != bundle.manifest_hash {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "Los artefactos ya no corresponden al manifiesto del bundle",
            "manifest_mismatch",
        )
        .with_resource(bundle.id));
    }
    let (dir_owned, list_owned) = (dir.to_path_buf(), list.clone());
    let observed = tokio::task::spawn_blocking(move || observe_dir(&dir_owned, &list_owned))
        .await
        .map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Verificación interrumpida", "join")
                .with_technical(e.to_string())
        })??;
    let missing: Vec<String> = list
        .iter()
        .filter(|e| e.required && !observed.contains_key(&e.relative_path))
        .map(|e| e.relative_path.clone())
        .collect();
    let mismatched: Vec<String> = list
        .iter()
        .filter(|e| {
            observed
                .get(&e.relative_path)
                .is_some_and(|o| o.sha256 != e.sha256 || o.size_bytes != e.size_bytes)
        })
        .map(|e| e.relative_path.clone())
        .collect();
    let roles = missing_roles(variant.format.as_deref(), &variant.capabilities, &artifacts);
    let mut state = bundle_state(&list, &observed);
    if state == BundleState::Verified && roles.as_ref().is_some_and(|r| !r.is_empty()) {
        state = BundleState::Incomplete;
    }
    let evidence = serde_json::json!({
        "verified_at": larm_domain::bridge::timestamp(chrono::Utc::now()),
        "missing": missing,
        "mismatched": mismatched,
        "missing_roles": roles,
        "observed": observed.iter().map(|(k, v)| (k.clone(), serde_json::json!({"sha256": v.sha256, "size_bytes": v.size_bytes}))).collect::<serde_json::Map<_, _>>(),
    });
    let row_version = repo
        .set_bundle_state(&bundle.id, bundle.row_version, state, evidence, ctx)
        .await?;
    Ok(Verification {
        bundle_id: bundle.id,
        state,
        row_version,
        missing,
        mismatched,
        missing_roles: roles,
    })
}
