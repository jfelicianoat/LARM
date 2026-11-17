//! Bundle de backup/export de LARM y guardas del updater (P133; spec §132,
//! §133 y §134).
//!
//! El bundle es la unidad de backup y de exportación: un `manifest.json`
//! determinista (BTreeMap, claves ordenadas) con la versión del bundle, la
//! versión del contrato sellado vigente y el SHA-256 de cada fichero incluido
//! (snapshot de la base, configuración y manifiestos del operador). La base
//! exportada no contiene blobs (viven en el CAS del almacenamiento) ni
//! secretos (los tokens viven en el backend seguro del sistema operativo), así
//! que el bundle cumple §133 sin filtrar nada: los modelos se reindexan desde
//! sus ubicaciones (§134, P018) y las credenciales se recrean.
//!
//! El updater (§132) nunca fuerza un reinicio con leases o jobs activas sin
//! confirmación explícita del operador: [`restart_decision`] decide.

use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Versión del formato del bundle.
pub const BUNDLE_VERSION: &str = "1";

/// Versión del contrato sellado vigente; se actualiza con cada revisión.
pub const LARM_CONTRACT_VERSION: &str = "3.16.0";

/// Error del módulo con razón estable.
fn backup_error(reason: &'static str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

/// Digest de un fichero del bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    pub sha256: String,
    pub bytes: u64,
}

/// Manifiesto del bundle: determinista y verificable byte a byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub bundle_version: String,
    pub larm_contract_version: String,
    /// RFC 3339 UTC del momento del bundle.
    pub created_at: String,
    /// Nombre relativo dentro del bundle → digest del contenido.
    pub files: BTreeMap<String, FileDigest>,
}

fn digest_of(bytes: &[u8]) -> (String, u64) {
    (hex::encode(Sha256::digest(bytes)), bytes.len() as u64)
}

/// Escribe el bundle en `dest_dir`: copia cada `(nombre, fichero)`, calcula
/// sus digests y deja el `manifest.json`. Sobrescribe un bundle previo en la
/// misma carpeta.
///
/// # Errors
/// `IO` del dominio si falla la lectura o la escritura.
pub fn write_bundle(
    dest_dir: &Path,
    created_at: &str,
    files: &[(String, std::path::PathBuf)],
) -> Result<BundleManifest, LarmError> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| backup_error("io_failed", &format!("creando el bundle: {e}")))?;
    let mut digests = BTreeMap::new();
    for (name, source) in files {
        let bytes = std::fs::read(source)
            .map_err(|e| backup_error("io_failed", &format!("leyendo {name}: {e}")))?;
        let (sha256, size) = digest_of(&bytes);
        let target = dest_dir.join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| backup_error("io_failed", &format!("creando {name}: {e}")))?;
        }
        std::fs::write(&target, &bytes)
            .map_err(|e| backup_error("io_failed", &format!("escribiendo {name}: {e}")))?;
        digests.insert(
            name.clone(),
            FileDigest {
                sha256,
                bytes: size,
            },
        );
    }
    let manifest = BundleManifest {
        bundle_version: BUNDLE_VERSION.to_string(),
        larm_contract_version: LARM_CONTRACT_VERSION.to_string(),
        created_at: created_at.to_string(),
        files: digests,
    };
    let rendered = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| backup_error("io_failed", &format!("serializando el manifiesto: {e}")))?;
    std::fs::write(dest_dir.join("manifest.json"), rendered)
        .map_err(|e| backup_error("io_failed", &format!("escribiendo el manifiesto: {e}")))?;
    Ok(manifest)
}

/// Lee y valida el manifiesto de un bundle para importarlo: la versión del
/// bundle y la del contrato deben ser las vigentes y cada fichero debe
/// coincidir byte a byte con su digest.
///
/// # Errors
/// `backup_import_incomplete` si falta el manifiesto o un fichero;
/// `backup_import_version_unsupported` si el bundle o el contrato no
/// coinciden con la instalación; `backup_import_digest_mismatch` si un
/// fichero no coincide con su digest (bundle corrompido o alterado).
pub fn validate_bundle(source_dir: &Path) -> Result<BundleManifest, LarmError> {
    let raw = std::fs::read(source_dir.join("manifest.json")).map_err(|_| {
        backup_error(
            "backup_import_incomplete",
            "El bundle no tiene manifest.json",
        )
    })?;
    let manifest: BundleManifest = serde_json::from_slice(&raw).map_err(|e| {
        backup_error(
            "backup_import_incomplete",
            &format!("manifest.json no es un bundle válido: {e}"),
        )
    })?;
    if manifest.bundle_version != BUNDLE_VERSION
        || manifest.larm_contract_version != LARM_CONTRACT_VERSION
    {
        return Err(backup_error(
            "backup_import_version_unsupported",
            &format!(
                "El bundle {} / contrato {} no es compatible con esta instalación (bundle {} / contrato {})",
                manifest.bundle_version,
                manifest.larm_contract_version,
                BUNDLE_VERSION,
                LARM_CONTRACT_VERSION
            ),
        ));
    }
    for (name, digest) in &manifest.files {
        let bytes = std::fs::read(source_dir.join(name)).map_err(|_| {
            backup_error(
                "backup_import_incomplete",
                &format!("El bundle no contiene {name}"),
            )
        })?;
        let (actual, size) = digest_of(&bytes);
        if actual != digest.sha256 || size != digest.bytes {
            return Err(backup_error(
                "backup_import_digest_mismatch",
                &format!("{name} no coincide con el digest del manifiesto"),
            ));
        }
    }
    Ok(manifest)
}

/// Copia un bundle validado a la carpeta de staging (el import nunca toca la
/// instalación viva: el operador revisa el staging y fusiona).
///
/// # Errors
/// Los de [`validate_bundle`]; `io_failed` al copiar.
pub fn stage_import(source_dir: &Path, staging_dir: &Path) -> Result<BundleManifest, LarmError> {
    let manifest = validate_bundle(source_dir)?;
    std::fs::create_dir_all(staging_dir)
        .map_err(|e| backup_error("io_failed", &format!("creando el staging: {e}")))?;
    for name in manifest.files.keys() {
        std::fs::copy(source_dir.join(name), staging_dir.join(name))
            .map_err(|e| backup_error("io_failed", &format!("copiando {name} al staging: {e}")))?;
    }
    Ok(manifest)
}

/// Guarda del updater (§132): con leases o jobs activas no se fuerza el
/// reinicio sin confirmación explícita del operador.
///
/// # Errors
/// `Conflict` `update_blocked_active_leases` si hay trabajo activo y no hay
/// confirmación.
pub fn restart_decision(
    active_leases: u64,
    active_jobs: u64,
    confirmed: bool,
) -> Result<(), LarmError> {
    if (active_leases > 0 || active_jobs > 0) && !confirmed {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            format!(
                "Hay {active_leases} leases y {active_jobs} jobs activas: el updater no fuerza el reinicio sin confirmación"
            ),
            "update_blocked_active_leases",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const TS: &str = "2026-11-16T12:00:00.000Z";

    fn bundle_dir(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("origin.sqlite");
        std::fs::write(&db, b"sqlite-bytes").unwrap();
        let config = dir.join("settings.json");
        std::fs::write(&config, b"{\"api\":{}}").unwrap();
        dir.join("bundle")
    }

    #[test]
    fn el_manifiesto_es_determinista_y_el_bundle_ronda() {
        let dir = tempfile::tempdir().unwrap();
        let dest = bundle_dir(dir.path());
        let first = write_bundle(
            &dest,
            TS,
            &[
                ("db.sqlite".into(), dir.path().join("origin.sqlite")),
                (
                    "config/settings.json".into(),
                    dir.path().join("settings.json"),
                ),
            ],
        )
        .unwrap();
        // Determinismo: mismo contenido → mismo manifiesto (salvo el campo
        // created_at, fijado por el test).
        let second = write_bundle(
            &dest,
            TS,
            &[
                ("db.sqlite".into(), dir.path().join("origin.sqlite")),
                (
                    "config/settings.json".into(),
                    dir.path().join("settings.json"),
                ),
            ],
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.bundle_version, BUNDLE_VERSION);
        assert_eq!(first.larm_contract_version, LARM_CONTRACT_VERSION);
        // El manifiesto serializado es estable byte a byte entre lecturas.
        let raw = std::fs::read(dest.join("manifest.json")).unwrap();
        let reparsed: BundleManifest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(reparsed, first);
        // Validación del bundle recién escrito: OK.
        assert_eq!(validate_bundle(&dest).unwrap(), first);
    }

    #[test]
    fn un_fichero_alterado_falla_con_digest_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let dest = bundle_dir(dir.path());
        write_bundle(
            &dest,
            TS,
            &[("db.sqlite".into(), dir.path().join("origin.sqlite"))],
        )
        .unwrap();
        std::fs::write(dest.join("db.sqlite"), b"tampered").unwrap();
        let error = validate_bundle(&dest).unwrap_err();
        assert_eq!(error.details.reason, "backup_import_digest_mismatch");
    }

    #[test]
    fn un_bundle_de_otra_version_o_contrato_no_se_importa() {
        let dir = tempfile::tempdir().unwrap();
        let dest = bundle_dir(dir.path());
        let manifest = write_bundle(
            &dest,
            TS,
            &[("db.sqlite".into(), dir.path().join("origin.sqlite"))],
        )
        .unwrap();
        // Bundle de otra versión del formato.
        let mut other = manifest.clone();
        other.bundle_version = "999".into();
        std::fs::write(
            dest.join("manifest.json"),
            serde_json::to_vec_pretty(&other).unwrap(),
        )
        .unwrap();
        assert_eq!(
            validate_bundle(&dest).unwrap_err().details.reason,
            "backup_import_version_unsupported"
        );
        // Bundle de otra versión del contrato.
        let mut other = manifest;
        other.larm_contract_version = "3.15.0".into();
        std::fs::write(
            dest.join("manifest.json"),
            serde_json::to_vec_pretty(&other).unwrap(),
        )
        .unwrap();
        assert_eq!(
            validate_bundle(&dest).unwrap_err().details.reason,
            "backup_import_version_unsupported"
        );
    }

    #[test]
    fn un_bundle_incompleto_no_se_importa() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            validate_bundle(dir.path()).unwrap_err().details.reason,
            "backup_import_incomplete"
        );
        // Manifiesto que cita un fichero ausente.
        let manifest = BundleManifest {
            bundle_version: BUNDLE_VERSION.into(),
            larm_contract_version: LARM_CONTRACT_VERSION.into(),
            created_at: TS.into(),
            files: BTreeMap::from([(
                "db.sqlite".into(),
                FileDigest {
                    sha256: "0".repeat(64),
                    bytes: 0,
                },
            )]),
        };
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            validate_bundle(dir.path()).unwrap_err().details.reason,
            "backup_import_incomplete"
        );
    }

    #[test]
    fn el_staging_copia_lo_validado() {
        let dir = tempfile::tempdir().unwrap();
        let dest = bundle_dir(dir.path());
        write_bundle(
            &dest,
            TS,
            &[("db.sqlite".into(), dir.path().join("origin.sqlite"))],
        )
        .unwrap();
        let staging = dir.path().join("staging");
        stage_import(&dest, &staging).unwrap();
        assert_eq!(
            std::fs::read(staging.join("db.sqlite")).unwrap(),
            b"sqlite-bytes".to_vec()
        );
    }

    #[test]
    fn el_updater_no_fuerza_reinicio_con_trabajo_activo_sin_confirmacion() {
        // Sin trabajo activo: listo con o sin confirmación.
        assert!(restart_decision(0, 0, false).is_ok());
        assert!(restart_decision(0, 0, true).is_ok());
        // Con trabajo activo y sin confirmación: bloqueado.
        let error = restart_decision(2, 1, false).unwrap_err();
        assert_eq!(error.details.reason, "update_blocked_active_leases");
        assert_eq!(error.code, larm_domain::error::ErrorCode::Conflict);
        // Con confirmación explícita del operador: permitido.
        assert!(restart_decision(2, 1, true).is_ok());
    }
}
