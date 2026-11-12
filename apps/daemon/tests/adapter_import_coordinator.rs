//! P028: importación inicial con filesystem y SQLite reales, sin ejecutar el worker.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use larm_adapter_host::import_service::ImportService;
use larm_adapter_host::imports::{ImportCoordinator, ImportPorts};
use larm_adapter_host::package::PackageControl;
use larm_adapter_sdk::manifest::{AdapterManifest, Family, MethodTable};
use larm_adapter_sdk::permissions::TrustStore;
use larm_db::adapter_imports::SqlAdapterImports;
use larm_domain::adapter_updates::AdapterImport;
use larm_domain::ports::{
    AdapterImportRepository, AdapterImporter, AdapterPackageSpace, MutationContext, VolumeDemand,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

const WORKER: &[u8] = b"MZ fixture; import tests never execute it";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "p028-import-test-operator".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn manifest(version: &str) -> AdapterManifest {
    let table = MethodTable::contract();
    let mut methods = table.mandatory_common.clone();
    methods.extend(
        table
            .required_for(Family::Runtime)
            .iter()
            .map(|method| method.to_string()),
    );
    let package_hash = hex::encode(Sha256::digest(
        format!("bin/worker.exe\0{}\n", hex::encode(Sha256::digest(WORKER))).as_bytes(),
    ));
    serde_json::from_value(json!({
        "id":"acme.p028-import-fixture",
        "version":version,
        "family":"runtime",
        "protocol_version":"1.0",
        "publisher":"Fixture",
        "executable":{"relative_path":"bin/worker.exe","args":[]},
        "platforms":["windows"],
        "architectures":["x86_64"],
        "methods":methods,
        "permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
        "capabilities":["runtime.gguf"],
        "package_sha256":package_hash,
        "signature":null,
        "installation_strategies":[]
    }))
    .unwrap()
}

fn package(path: &Path, manifest: &AdapterManifest) -> AdapterImport {
    let mut archive = zip::ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("manifest.json", options).unwrap();
    archive
        .write_all(serde_json::to_string(manifest).unwrap().as_bytes())
        .unwrap();
    archive.start_file("bin/worker.exe", options).unwrap();
    archive.write_all(WORKER).unwrap();
    archive.finish().unwrap();
    AdapterImport {
        package_path: path.to_string_lossy().into_owned(),
        expected_hash: hex::encode(Sha256::digest(std::fs::read(path).unwrap())),
        approval_id: None,
    }
}

struct Space;
#[async_trait]
impl AdapterPackageSpace for Space {
    async fn measure_package_space(
        &self,
        _root: &str,
        required_bytes: u64,
    ) -> Result<(VolumeDemand, chrono::DateTime<chrono::Utc>), larm_domain::error::LarmError> {
        Ok((
            VolumeDemand {
                volume_key: "p028-import-volume".into(),
                required_bytes,
                os_free: 100 << 30,
                policy_minimum: 0,
            },
            chrono::Utc::now(),
        ))
    }
}

async fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    larm_db::pool::Db,
    Arc<SqlAdapterImports>,
    Arc<ImportCoordinator>,
) {
    let base =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/p028-import-tests");
    std::fs::create_dir_all(&base).unwrap();
    let base = std::fs::canonicalize(base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let (dbdir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let jobs = Arc::new(SqlAdapterImports {
        pool: db.pool.clone(),
    });
    let coordinator = ImportCoordinator::open(
        dir.path().join("data/adapters"),
        ImportPorts {
            jobs: jobs.clone(),
            reservations: Arc::new(larm_db::reservations::SqlVolumeReservations {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            space: Arc::new(Space),
        },
        TrustStore::default(),
    )
    .unwrap();
    (dir, dbdir, db, jobs, coordinator)
}

#[tokio::test]
async fn importa_paquete_verificado_y_crea_instalacion_installed_no_promovida() {
    let (dir, _dbdir, db, jobs, coordinator) = fixture().await;
    let source = dir.path().join("fixture.zip");
    let request = package(&source, &manifest("1.0"));
    let service = ImportService::new(coordinator, jobs.clone());
    service.start().await;
    let body = serde_json::to_vec(&request).unwrap();
    let receipt = service
        .clone()
        .request_import(
            &request,
            &hex::encode(Sha256::digest(&body)),
            "physical-import",
            &ctx(),
        )
        .await
        .unwrap();
    let state = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = jobs.import_job(&receipt.job_id).await.unwrap().unwrap();
            if matches!(
                state.step,
                larm_domain::adapter_updates::UpdateStep::Completed
                    | larm_domain::adapter_updates::UpdateStep::Failed
            ) {
                break state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    service.finish_shutdown().await;
    assert_eq!(
        state.step,
        larm_domain::adapter_updates::UpdateStep::Completed
    );

    let destination = dir
        .path()
        .join("data/adapters/acme.p028-import-fixture/1.0");
    assert!(destination.join("bin/worker.exe").is_file());
    let row: (String, String, String) = sqlx::query_as(
        "SELECT state,trust,adapter_id FROM adapter_installation WHERE version='1.0'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "INSTALLED".into(),
            "UNTRUSTED".into(),
            "acme.p028-import-fixture".into()
        )
    );
    assert_eq!(
        jobs.import_job(&receipt.job_id)
            .await
            .unwrap()
            .unwrap()
            .step,
        larm_domain::adapter_updates::UpdateStep::Completed
    );
    let events: Vec<String> = sqlx::query_scalar("SELECT event_type FROM event ORDER BY event_id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, ["adapter.import.queued", "adapter.imported"]);
    let leftovers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn hash_incorrecto_falla_sin_publicar_ni_dejar_reserva() {
    let (dir, _dbdir, db, jobs, coordinator) = fixture().await;
    let source = dir.path().join("tampered.zip");
    let mut request = package(&source, &manifest("1.1"));
    request.expected_hash = "b".repeat(64);
    let (receipt, _) = jobs
        .enqueue_import(&request, &"c".repeat(64), "invalid-import", &ctx())
        .await
        .unwrap();
    assert!(coordinator
        .clone()
        .execute(receipt.job_id.clone(), PackageControl::default())
        .await
        .is_err());
    assert!(!dir
        .path()
        .join("data/adapters/acme.p028-import-fixture/1.1")
        .exists());
    let row_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM adapter_installation WHERE adapter_id='acme.p028-import-fixture'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(row_count, 0);
    let state = jobs.import_job(&receipt.job_id).await.unwrap().unwrap();
    assert_eq!(state.step, larm_domain::adapter_updates::UpdateStep::Failed);
    let reservations: i64 = sqlx::query_scalar("SELECT count(*) FROM volume_reservation")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(reservations, 0);
}
