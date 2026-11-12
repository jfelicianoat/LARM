#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use larm_adapter_host::package::{
    export_published_package, ArchivePlan, PackageControl, StagedPackage,
};
use larm_adapter_sdk::manifest::{AdapterManifest, Family, MethodTable};
use larm_adapter_sdk::permissions::{package_hash, TrustStore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn manifest(version: &str, hash: &str) -> Value {
    let table = MethodTable::contract();
    let mut methods = table.mandatory_common.clone();
    methods.extend(
        table
            .required_for(Family::Runtime)
            .iter()
            .map(|s| s.to_string()),
    );
    json!({"id":"test.runtime","version":version,"family":"runtime","protocol_version":"1.0",
        "publisher":"Test publisher","executable":{"relative_path":"bin/worker.exe","args":[]},
        "platforms":["windows"],"architectures":["x86_64"],"methods":methods,
        "permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
        "capabilities":["runtime.gguf"],"package_sha256":hash,"signature":null,"installation_strategies":[]})
}
fn write_zip(path: &Path, document: &Value, extra: &[(&str, &[u8])]) -> String {
    let mut out = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    out.start_file("manifest.json", options).unwrap();
    out.write_all(document.to_string().as_bytes()).unwrap();
    out.start_file("bin/worker.exe", options).unwrap();
    out.write_all(b"MZ test worker").unwrap();
    for (name, data) in extra {
        out.start_file(*name, options).unwrap();
        out.write_all(data).unwrap();
    }
    out.finish().unwrap();
    hash(path)
}
fn hash(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}
fn fixture(root: &Path, version: &str) -> (std::path::PathBuf, String, Value) {
    let payload = root.join(format!("payload-{version}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/worker.exe"), b"MZ test worker").unwrap();
    let document = manifest(
        version,
        &package_hash(&payload, &["manifest.json"]).unwrap(),
    );
    let zip = root.join(format!("package-{version}.zip"));
    let digest = write_zip(&zip, &document, &[]);
    (zip, digest, document)
}

#[test]
fn side_by_side_sin_sobrescribir_y_rollback_de_publicacion_no_confirmada() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("adapters");
    let control = PackageControl::default();
    for version in ["1.0", "2.0"] {
        let (zip, digest, _) = fixture(dir.path(), version);
        let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
        assert_eq!(
            plan.required_bytes(),
            plan.archive_bytes + plan.unpacked_bytes + (1 << 30)
        );
        let staged = StagedPackage::extract(plan, &root, &TrustStore::default(), &control).unwrap();
        staged.publish(&root, &control).unwrap().commit();
    }
    assert_eq!(
        std::fs::read(root.join("test.runtime/1.0/bin/worker.exe")).unwrap(),
        b"MZ test worker"
    );
    assert!(root.join("test.runtime/2.0/manifest.json").exists());
    let (zip, digest, _) = fixture(dir.path(), "3.0");
    let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
    let provisional = StagedPackage::extract(plan.clone(), &root, &TrustStore::default(), &control)
        .unwrap()
        .publish(&root, &control)
        .unwrap();
    assert!(provisional.destination.exists());
    drop(provisional); // El commit de la base falla: sólo desaparece la nueva versión.
    assert!(!root.join("test.runtime/3.0").exists());
    assert!(root.join("test.runtime/1.0").exists());
    let existing = ArchivePlan::inspect(
        &dir.path().join("package-1.0.zip"),
        &hash(&dir.path().join("package-1.0.zip")),
        &control,
    )
    .unwrap();
    let err = StagedPackage::extract(existing, &root, &TrustStore::default(), &control)
        .unwrap()
        .publish(&root, &control)
        .err()
        .unwrap();
    assert_eq!(err.details.reason, "adapter_version_exists");
    assert_eq!(std::fs::read_dir(root.join(".staging")).unwrap().count(), 0);
}

#[test]
fn exportar_paquete_verificado_es_reproducible_y_se_puede_reimportar() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("adapters");
    let control = PackageControl::default();
    let (zip, digest, manifest_document) = fixture(dir.path(), "export");
    let expected_manifest = AdapterManifest::parse(&manifest_document.to_string()).unwrap();
    let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
    let published = StagedPackage::extract(plan, &root, &TrustStore::default(), &control)
        .unwrap()
        .publish(&root, &control)
        .unwrap();
    let installed = published.destination.clone();
    published.commit();

    let first_path = dir.path().join("first.zip");
    let second_path = dir.path().join("second.zip");
    let first = export_published_package(&installed, &first_path, &control).unwrap();
    let second = export_published_package(&installed, &second_path, &control).unwrap();
    assert_eq!(first.manifest, expected_manifest);
    assert_eq!(first.archive_hash, second.archive_hash);
    assert_eq!(
        std::fs::read(&first_path).unwrap(),
        std::fs::read(&second_path).unwrap()
    );

    let exported_plan = ArchivePlan::inspect(&first_path, &first.archive_hash, &control).unwrap();
    assert_eq!(exported_plan.manifest, expected_manifest);
    let imported = StagedPackage::extract(
        exported_plan,
        &dir.path().join("reimported"),
        &TrustStore::default(),
        &control,
    )
    .unwrap();
    assert_eq!(
        larm_adapter_host::package::controlled_hash(&imported.payload, &control).unwrap(),
        expected_manifest.package_sha256
    );
}

#[test]
fn export_rechaza_destino_existente_paquete_alterado_y_destino_dentro_del_paquete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("adapters");
    let control = PackageControl::default();
    let (zip, digest, _) = fixture(dir.path(), "export-guards");
    let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
    let published = StagedPackage::extract(plan, &root, &TrustStore::default(), &control)
        .unwrap()
        .publish(&root, &control)
        .unwrap();
    let installed = published.destination.clone();
    published.commit();

    let existing = dir.path().join("existing.zip");
    std::fs::write(&existing, b"keep this file").unwrap();
    let error = export_published_package(&installed, &existing, &control).unwrap_err();
    assert_eq!(error.details.reason, "export_destination_exists");
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep this file");

    let inside = installed.join("export.zip");
    let error = export_published_package(&installed, &inside, &control).unwrap_err();
    assert_eq!(error.details.reason, "export_destination_inside_package");
    assert!(!inside.exists());

    std::fs::write(installed.join("bin/worker.exe"), b"modified worker").unwrap();
    let altered = dir.path().join("altered.zip");
    let error = export_published_package(&installed, &altered, &control).unwrap_err();
    assert_eq!(error.details.reason, "package_hash_mismatch");
    assert!(!altered.exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".larm-export-"))
            .count(),
        0
    );
}

#[test]
fn rutas_hostiles_y_colisiones_rechazadas_antes_de_extraer() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, doc) = fixture(dir.path(), "base");
    for (index, path) in [
        "../escape.exe",
        "/absolute.exe",
        "C:/escape.exe",
        "bin\\..\\escape.exe",
        "bin/x:ads",
        "bin/NUL.txt",
        "bin/CONIN$",
        "bin/CONOUT$.txt",
        "bin/CON .txt",
        "bin/a.",
        "bin/a ",
        "BIN/WORKER.EXE",
        "bin/worker.exe/child",
    ]
    .iter()
    .enumerate()
    {
        let zip = dir.path().join(format!("hostile-{index}.zip"));
        let digest = write_zip(&zip, &doc, &[(path, b"bad")]);
        assert!(
            ArchivePlan::inspect(&zip, &digest, &PackageControl::default()).is_err(),
            "{path}"
        );
    }
    assert!(!dir.path().join("escape.exe").exists());
    assert!(!dir.path().join("adapters").exists());
}

#[test]
fn protocolo_schema_hash_y_snapshot_cambiado() {
    let dir = tempfile::tempdir().unwrap();
    let (zip, digest, doc) = fixture(dir.path(), "valid");
    let control = PackageControl::default();
    assert_eq!(
        ArchivePlan::inspect(&zip, &"0".repeat(64), &control)
            .unwrap_err()
            .details
            .reason,
        "archive_hash_mismatch"
    );
    for (field, value) in [
        ("protocol_version", json!("2.0")),
        ("unknown", json!(true)),
        ("version", json!("../evil")),
    ] {
        let mut altered = doc.clone();
        altered[field] = value;
        let bad = dir.path().join(format!("{field}.zip"));
        let hash = write_zip(&bad, &altered, &[]);
        assert!(ArchivePlan::inspect(&bad, &hash, &control).is_err());
    }
    let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
    std::fs::write(&zip, b"changed").unwrap();
    let err = StagedPackage::extract(
        plan,
        &dir.path().join("adapters"),
        &TrustStore::default(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(err.details.reason, "archive_changed");
    assert_eq!(
        std::fs::read_dir(dir.path().join("adapters/.staging"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn enlace_simbolico_y_bomba_de_compresion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("symlink.zip");
    let (_, _, doc) = fixture(dir.path(), "link");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file("manifest.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(doc.to_string().as_bytes()).unwrap();
    zip.add_symlink(
        "bin/worker.exe",
        "../../outside.exe",
        SimpleFileOptions::default(),
    )
    .unwrap();
    zip.finish().unwrap();
    assert_eq!(
        ArchivePlan::inspect(&path, &hash(&path), &PackageControl::default())
            .unwrap_err()
            .details
            .reason,
        "package_link_rejected"
    );
    let path = dir.path().join("bomb.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file(
        "bomb.bin",
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&vec![0u8; 4 << 20]).unwrap();
    zip.finish().unwrap();
    assert_eq!(
        ArchivePlan::inspect(&path, &hash(&path), &PackageControl::default())
            .unwrap_err()
            .details
            .reason,
        "package_bomb_rejected"
    );
}

#[test]
fn cancelacion_timeout_integridad_y_fallo_io_no_publican() {
    let dir = tempfile::tempdir().unwrap();
    let (zip, digest, mut doc) = fixture(dir.path(), "cancel");
    let control = PackageControl::default();
    control.cancel();
    assert_eq!(
        ArchivePlan::inspect(&zip, &digest, &control)
            .unwrap_err()
            .details
            .reason,
        "operation_cancelled"
    );
    assert_eq!(
        ArchivePlan::inspect(&zip, &digest, &PackageControl::with_timeout(Duration::ZERO))
            .unwrap_err()
            .details
            .reason,
        "package_timeout"
    );
    doc["package_sha256"] = json!("f".repeat(64));
    let hash = write_zip(&zip, &doc, &[]);
    let control = PackageControl::default();
    let plan = ArchivePlan::inspect(&zip, &hash, &control).unwrap();
    let root = dir.path().join("adapters");
    assert_eq!(
        StagedPackage::extract(plan, &root, &TrustStore::default(), &control)
            .err()
            .unwrap()
            .details
            .reason,
        "package_hash_mismatch"
    );
    assert!(!root.join("test.runtime").exists());
    let root = dir.path().join("file");
    std::fs::write(&root, b"file").unwrap();
    let plan = ArchivePlan::inspect(&zip, &hash, &control).unwrap();
    assert!(StagedPackage::extract(plan, &root, &TrustStore::default(), &control).is_err());
}

#[tokio::test]
async fn registro_local_independiente_y_versiones_ambiguas() {
    use larm_adapter_host::registry::{LocalFolderRegistry, RegistrySource};
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), "2.0");
    fixture(dir.path(), "1.0");
    let registry = LocalFolderRegistry {
        root: dir.path().to_path_buf(),
    };
    let first = registry.packages().await.unwrap();
    assert_eq!(
        first
            .iter()
            .map(|p| p.manifest.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0", "2.0"]
    );
    assert!(
        !dir.path().join("adapters").exists(),
        "discovery no instala"
    );
    let second = registry.packages().await.unwrap();
    assert_eq!(
        first.iter().map(|p| &p.archive_hash).collect::<Vec<_>>(),
        second.iter().map(|p| &p.archive_hash).collect::<Vec<_>>()
    );
    std::fs::copy(
        dir.path().join("package-1.0.zip"),
        dir.path().join("duplicate.ZIP"),
    )
    .unwrap();
    assert_eq!(
        registry.packages().await.unwrap_err().details.reason,
        "registry_version_duplicate"
    );
}

#[test]
fn directorio_central_acotado_antes_de_reservar_el_indice() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _, _) = fixture(dir.path(), "metadata");
    let mut bytes = std::fs::read(&path).unwrap();
    let eocd = bytes.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    bytes[eocd + 12..eocd + 16].copy_from_slice(&(65u32 << 20).to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(
        ArchivePlan::inspect(&path, &hash(&path), &PackageControl::default())
            .unwrap_err()
            .details
            .reason,
        "package_metadata_limit"
    );
    let mut zip64 = vec![0u8; 98];
    zip64[..4].copy_from_slice(b"PK\x06\x06");
    zip64[4..12].copy_from_slice(&44u64.to_le_bytes());
    zip64[24..32].copy_from_slice(&100001u64.to_le_bytes());
    zip64[32..40].copy_from_slice(&100001u64.to_le_bytes());
    zip64[56..60].copy_from_slice(b"PK\x06\x07");
    zip64[72..76].copy_from_slice(&1u32.to_le_bytes());
    zip64[76..80].copy_from_slice(b"PK\x05\x06");
    zip64[84..88].copy_from_slice(&[255; 4]);
    std::fs::write(&path, zip64).unwrap();
    assert_eq!(
        ArchivePlan::inspect(&path, &hash(&path), &PackageControl::default())
            .unwrap_err()
            .details
            .reason,
        "package_file_limit"
    );
}

#[test]
fn cambio_del_staging_no_se_publica() {
    let dir = tempfile::tempdir().unwrap();
    let (zip, digest, _) = fixture(dir.path(), "tamper");
    let control = PackageControl::default();
    let plan = ArchivePlan::inspect(&zip, &digest, &control).unwrap();
    let root = dir.path().join("adapters");
    let stage = StagedPackage::extract(plan, &root, &TrustStore::default(), &control).unwrap();
    std::fs::write(stage.payload.join("bin/worker.exe"), b"tampered").unwrap();
    assert_eq!(
        stage.publish(&root, &control).err().unwrap().details.reason,
        "package_changed_before_publish"
    );
    assert!(!root.join("test.runtime/tamper").exists());
}

#[test]
fn manifiesto_del_staging_que_crece_no_se_lee_sin_limite_ni_se_publica() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("adapters");
    let control = PackageControl::default();
    let (zip, digest, _) = fixture(dir.path(), "manifest-grown");
    let staged = StagedPackage::extract(
        ArchivePlan::inspect(&zip, &digest, &control).unwrap(),
        &root,
        &TrustStore::default(),
        &control,
    )
    .unwrap();
    std::fs::write(
        staged.payload.join("manifest.json"),
        vec![b' '; (1 << 20) + 1],
    )
    .unwrap();
    let error = staged.publish(&root, &control).err().unwrap();
    assert_eq!(error.details.reason, "package_size_limit");
    assert!(!root.join("test.runtime/manifest-grown").exists());
}
