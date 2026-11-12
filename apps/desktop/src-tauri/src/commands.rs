//! Comandos Tauri expuestos al frontend. Sin logica de negocio.
#![forbid(unsafe_code)]

use serde::Deserialize;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use zip::ZipArchive;

const MAX_PREVIEW_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterPackagePermissionsPreview {
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub network_origins: Vec<String>,
    pub process_allowlist: Vec<String>,
    pub secret_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PackageManifestInput {
    id: String,
    version: String,
    family: String,
    protocol_version: String,
    publisher: String,
    permissions: AdapterPackagePermissionsPreview,
    signature: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AdapterPackageManifestPreview {
    pub id: String,
    pub version: String,
    pub family: String,
    pub protocol_version: String,
    pub publisher: String,
    pub signature_declared: bool,
    pub permissions: AdapterPackagePermissionsPreview,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AdapterPackageFingerprint {
    pub size_bytes: String,
    pub sha256: String,
    pub manifest: AdapterPackageManifestPreview,
}

fn read_manifest_preview(path: &std::path::Path) -> Result<AdapterPackageManifestPreview, String> {
    let file = std::fs::File::open(path).map_err(|_| "no se pudo abrir el paquete ZIP")?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| "el archivo no es un paquete ZIP válido")?;
    let mut manifests = 0;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| "el archivo ZIP contiene una entrada inválida")?;
        let entry_name = entry
            .name()
            .map_err(|_| "el archivo ZIP contiene un nombre inválido")?;
        if entry_name.as_ref() == "manifest.json" {
            manifests += 1;
        }
    }
    if manifests != 1 {
        return Err("el paquete debe incluir un único manifest.json".into());
    }
    let entry = archive
        .by_name("manifest.json")
        .map_err(|_| "no se pudo leer el manifiesto del paquete")?;
    if entry.size() > MAX_PREVIEW_MANIFEST_BYTES {
        return Err("el manifiesto supera el tamaño permitido para la vista previa".into());
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .take(MAX_PREVIEW_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "no se pudo leer el manifiesto del paquete")?;
    if bytes.len() as u64 > MAX_PREVIEW_MANIFEST_BYTES {
        return Err("el manifiesto supera el tamaño permitido para la vista previa".into());
    }
    let parsed: PackageManifestInput = serde_json::from_slice(&bytes)
        .map_err(|_| "el manifiesto no tiene los campos necesarios para la vista previa")?;
    Ok(AdapterPackageManifestPreview {
        id: parsed.id,
        version: parsed.version,
        family: parsed.family,
        protocol_version: parsed.protocol_version,
        publisher: parsed.publisher,
        signature_declared: parsed.signature.is_some(),
        permissions: parsed.permissions,
    })
}

fn hash_local_file(path: &std::path::Path) -> Result<(u64, String), String> {
    let mut file = std::fs::File::open(path).map_err(|_| "no se pudo abrir el archivo indicado")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    let mut bytes_read = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "no se pudo leer el archivo indicado")?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(count as u64)
            .ok_or_else(|| "el tamaño del archivo no es válido".to_owned())?;
        hasher.update(&buffer[..count]);
    }
    Ok((bytes_read, hex::encode(hasher.finalize())))
}

/// Lee una huella exacta del ZIP que el operador eligió para importar.
/// El daemon vuelve a comprobar el hash antes de extraer el paquete.
#[tauri::command]
pub fn inspect_adapter_package(package_path: String) -> Result<AdapterPackageFingerprint, String> {
    let raw = package_path.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err("ruta de paquete no válida".into());
    }
    let path = std::fs::canonicalize(raw).map_err(|_| "no se pudo abrir el archivo indicado")?;
    let before = std::fs::metadata(&path).map_err(|_| "no se pudo leer el archivo indicado")?;
    if !before.is_file() {
        return Err("el paquete debe ser un archivo ZIP".into());
    }
    let (bytes_before_preview, hash_before_preview) = hash_local_file(&path)?;
    let manifest = read_manifest_preview(&path)?;
    let (bytes_read, hash_after_preview) = hash_local_file(&path)?;
    let after = std::fs::metadata(&path).map_err(|_| "no se pudo releer el archivo indicado")?;
    if bytes_before_preview != bytes_read
        || hash_before_preview != hash_after_preview
        || bytes_read != before.len()
        || after.len() != before.len()
    {
        return Err("el archivo cambió mientras se calculaba su huella".into());
    }

    Ok(AdapterPackageFingerprint {
        size_bytes: bytes_read.to_string(),
        sha256: hash_after_preview,
        manifest,
    })
}

/// Variable de entorno que activa la prueba de humo del shell (P001).
pub const SMOKE_ENV: &str = "LARM_DESKTOP_SMOKE";

/// Ping minimo exigido por P001.
#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

fn smoke_requested() -> bool {
    std::env::var(SMOKE_ENV).is_ok_and(|v| v == "1")
}

/// Indica al frontend si debe devolver el resultado del ping (solo con LARM_DESKTOP_SMOKE=1).
#[tauri::command]
pub fn smoke_enabled() -> bool {
    smoke_requested()
}

/// Recibe el resultado del ping hecho por el webview real, lo imprime y cierra el shell.
/// Sin LARM_DESKTOP_SMOKE=1 se rechaza: no altera el uso normal.
#[tauri::command]
pub fn smoke_report(app: tauri::AppHandle, result: String) -> Result<(), String> {
    if !smoke_requested() {
        return Err("prueba de humo desactivada".into());
    }
    println!("LARM_DESKTOP_SMOKE {result}");
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_preview_zip(path: &std::path::Path, manifest: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(manifest.as_bytes()).unwrap();
        archive.finish().unwrap();
    }

    #[test]
    fn ping_devuelve_pong() {
        assert_eq!(ping(), "pong");
    }

    #[test]
    fn fingerprint_local_entrega_tamano_y_sha256_exactos() {
        let path = std::env::temp_dir().join(format!(
            "larm-adapter-import-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_preview_zip(
            &path,
            r#"{"id":"fixture.local","version":"1.2.3","family":"source","protocol_version":"1.0","publisher":"Equipo local","permissions":{"read_roots":["$models"],"write_roots":[],"network_origins":["https://example.invalid"],"process_allowlist":[],"secret_refs":["credential-reference"]},"signature":null}"#,
        );

        let fingerprint = inspect_adapter_package(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(
            fingerprint.size_bytes,
            std::fs::metadata(&path).unwrap().len().to_string()
        );
        assert_eq!(fingerprint.sha256.len(), 64);
        assert_eq!(fingerprint.manifest.id, "fixture.local");
        assert_eq!(fingerprint.manifest.version, "1.2.3");
        assert_eq!(fingerprint.manifest.family, "source");
        assert_eq!(fingerprint.manifest.publisher, "Equipo local");
        assert!(!fingerprint.manifest.signature_declared);
        assert_eq!(fingerprint.manifest.permissions.read_roots, ["$models"]);
        assert_eq!(
            fingerprint.manifest.permissions.secret_refs,
            ["credential-reference"]
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fingerprint_rechaza_zip_sin_manifiesto_y_permisos_cerrados_invalidos() {
        let path = std::env::temp_dir().join(format!(
            "larm-adapter-preview-invalid-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        zip::ZipWriter::new(file).finish().unwrap();
        assert!(inspect_adapter_package(path.to_string_lossy().into_owned()).is_err());
        std::fs::remove_file(&path).unwrap();

        let path = std::env::temp_dir().join(format!(
            "larm-adapter-preview-permissions-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_preview_zip(
            &path,
            r#"{"id":"x","version":"1","family":"source","protocol_version":"1.0","publisher":"x","permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[],"unexpected":true},"signature":null}"#,
        );
        assert!(inspect_adapter_package(path.to_string_lossy().into_owned()).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fingerprint_rechaza_ruta_vacia_archivo_inexistente_y_directorio() {
        assert!(inspect_adapter_package(String::new()).is_err());
        assert!(inspect_adapter_package("C:\\LARM\\missing.zip".into()).is_err());
        assert!(
            inspect_adapter_package(std::env::temp_dir().to_string_lossy().into_owned()).is_err()
        );
    }

    #[test]
    fn smoke_desactivado_sin_variable() {
        std::env::remove_var(SMOKE_ENV);
        assert!(!smoke_enabled());
    }
}
