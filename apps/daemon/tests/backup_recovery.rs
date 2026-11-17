//! P133: updater, backup y rebuild REAL sobre el daemon real del PC IA:
//! update con lease activa y sin confirmación bloqueado, update con
//! confirmación que verifica el staged y deja backup+bundle verificados,
//! import corrupto rechazado, restore a temporal y rebuild del registro en
//! staging desde las ubicaciones.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/daemon.rs"]
#[allow(dead_code)]
mod daemon_process;
#[path = "support/download_restart.rs"]
#[allow(dead_code)]
mod download_restart;

use daemon_process::RunningDaemon;
use download_restart::{config, TemporaryPrincipal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{Duration, Instant};

async fn post_json(port: u16, token: &str, path: &str, body: &Value) -> (u16, Value) {
    post_json_key(
        port,
        token,
        path,
        body,
        "0192aaaa-0000-7000-8000-000000000133",
    )
    .await
}

async fn post_json_key(
    port: u16,
    token: &str,
    path: &str,
    body: &Value,
    idempotency: &str,
) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(token)
        .header("idempotency-key", idempotency)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    (status, value)
}

fn v7() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Inserta una lease ACTIVA directamente en la base real del daemon.
async fn insert_active_lease(data: &Path) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}?mode=rw",
            data.join("larm.sqlite").display()
        ))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runtime_lease(id,created_at,updated_at,row_version,metadata,owner_id,request_id,generation,state,constraints) \
         VALUES(?1,?2,?2,1,'{}','broker-external',?3,0,'ACTIVE','{}')",
    )
    .bind(v7())
    .bind("2026-11-16T12:00:00.000Z")
    .bind(v7())
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

/// Fase del job más reciente del journal (`kind`).
async fn job_phase(data: &Path, kind: &str) -> String {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}?mode=rw",
            data.join("larm.sqlite").display()
        ))
        .await
        .unwrap();
    let phase: String = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE kind = ?1 \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;
    phase
}

/// Motivo del error del job más reciente (`kind`), del payload del journal.
async fn job_error_reason(data: &Path, kind: &str) -> String {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}?mode=rw",
            data.join("larm.sqlite").display()
        ))
        .await
        .unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM operation_journal WHERE kind = ?1 \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;
    serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)["error"]["details"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn wait_phase(data: &Path, kind: &str, terminal: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let phase = job_phase(data, kind).await;
        if terminal.contains(&phase.as_str()) {
            return phase;
        }
        assert!(
            Instant::now() < deadline,
            "el job {kind} no terminó: {phase}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updater_backup_y_rebuild_reales_con_guarda_de_leases() {
    // 1) Datos reales del PC IA y daemon real en loopback con un principal
    //    admin temporal real (CLI enroll-principal).
    let dir = tempfile::TempDir::new().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let cfg = dir.path().join("larm.json");
    config(&cfg, None);
    let mut principal = TemporaryPrincipal::enroll_desktop(&data, &cfg);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut daemon_process = RunningDaemon::start(&data, &cfg, port, data.join("p133-daemon.log"));
    daemon_process
        .wait_live(port, Duration::from_secs(45))
        .unwrap();

    let plan_id = v7();
    let plan_hash = sha256_hex(b"p133-update-plan");

    // 2) Lease ACTIVA + update sin confirmación → el updater no fuerza nada.
    insert_active_lease(&data).await;
    let (status, body) = post_json(
        port,
        &principal.token,
        "/api/v1/updates/larm",
        &json!({"plan_id": plan_id, "plan_hash": plan_hash, "approval_id": null}),
    )
    .await;
    assert_eq!(status, 409, "lease activa sin confirmación: {body}");
    assert_eq!(body["details"]["reason"], "update_blocked_active_leases");
    println!("REAL P133: update con lease activa sin confirmacion bloqueado (update_blocked_active_leases)");

    // 3) Staged real del updater con su sha256 de referencia correcto.
    let staging = data.join("updates").join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let staged_bytes = b"LARM-UPDATE-PAYLOAD-p133";
    std::fs::write(staging.join("larm-update.bin"), staged_bytes).unwrap();
    std::fs::write(
        staging.join("larm-update.bin.sha256"),
        sha256_hex(staged_bytes),
    )
    .unwrap();

    // 4) Con confirmación (aprobación REAL ligada al plan_hash): 202 + job
    //    COMPLETED con backup verificado y bundle determinista — la lease
    //    sigue activa y la confirmación del operador la autoriza.
    let (status, receipt) = post_json_key(
        port,
        &principal.token,
        "/api/v1/approvals",
        &json!({
            "operation_digest": plan_hash,
            "resource_ids": ["larm"],
            "code_hash": null,
            "permissions": {},
            "approved": true,
        }),
        "0192aaaa-0000-7000-8000-0000000000a1",
    )
    .await;
    assert_eq!(status, 201, "aprobación: {receipt}");
    let approval_id = receipt["resource_id"].as_str().unwrap().to_string();
    let (status, receipt) = post_json(
        port,
        &principal.token,
        "/api/v1/updates/larm",
        &json!({"plan_id": plan_id, "plan_hash": plan_hash, "approval_id": approval_id}),
    )
    .await;
    assert_eq!(status, 202, "update aprobado: {receipt}");
    assert_eq!(receipt["state"], "RUNNING");
    let job_id = receipt["job_id"].as_str().unwrap().to_string();
    assert_eq!(job_phase(&data, "larm.update").await, "RUNNING");
    let phase = wait_phase(&data, "larm.update", &["COMPLETED", "FAILED"]).await;
    assert_eq!(phase, "COMPLETED", "update: {}", job_id);

    // 5) El backup previo y el bundle existen y el bundle valida; el staged
    //    quedó verificado con su hash real.
    let backups = data.join("backups");
    let bundle = backups
        .read_dir()
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("bundle-"))
        })
        .expect("bundle del update");
    let manifest = larm_core::backup::validate_bundle(&bundle).expect("bundle válido");
    assert!(manifest.files.contains_key("db.sqlite"));
    let backup = backups
        .read_dir()
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("pre-update-"))
        })
        .expect("backup previo al update");
    println!(
        "REAL P133: update COMPLETED con backup verificado ({}) y bundle ({} ficheros)",
        backup.display(),
        manifest.files.len()
    );

    // 6) Restore a temporal (oráculo Backup/restore temp) + import corrupto.
    let restored = dir.path().join("restored.sqlite");
    let report = larm_db::backup::restore(&backup, &restored).await.unwrap();
    assert_eq!(report.integrity, "ok", "el restore verifica integridad");
    assert!(
        !report.rows.is_empty(),
        "el restore compara filas tabla a tabla"
    );
    println!("REAL P133: restore a temporal verificado tabla a tabla");
    std::fs::write(bundle.join("db.sqlite"), b"tampered").unwrap();
    assert_eq!(
        larm_core::backup::validate_bundle(&bundle)
            .unwrap_err()
            .details
            .reason,
        "backup_import_digest_mismatch",
        "import corrupto rechazado"
    );
    println!("REAL P133: import con bytes alterados rechazado (backup_import_digest_mismatch)");

    // 7) Rebuild del registro en staging desde las ubicaciones (raíz vacía):
    //    202 + job COMPLETED con base de staging creada.
    let (status, receipt) = post_json(
        port,
        &principal.token,
        "/api/v1/recovery/rebuild",
        &json!({"plan_id": v7(), "plan_hash": sha256_hex(b"p133-rebuild"), "approval_id": null}),
    )
    .await;
    assert_eq!(status, 202, "rebuild: {receipt}");
    let phase = wait_phase(&data, "recovery.rebuild", &["COMPLETED", "FAILED"]).await;
    assert_eq!(phase, "COMPLETED");
    let recovery = data.join("recovery");
    let staging_db = recovery
        .read_dir()
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rebuild-staging-"))
        })
        .expect("base de staging del rebuild");
    assert!(staging_db.is_file());
    println!(
        "REAL P133: rebuild en staging COMPLETED ({})",
        staging_db.display()
    );

    // 8) Staged con hash corrupto → job FAILED con la razón del contrato.
    std::fs::write(staging.join("larm-update.bin"), b"corrupted-payload").unwrap();
    let new_hash = sha256_hex(b"p133-update-plan-2");
    let (status, receipt) = post_json_key(
        port,
        &principal.token,
        "/api/v1/approvals",
        &json!({
            "operation_digest": new_hash,
            "resource_ids": ["larm"],
            "code_hash": null,
            "permissions": {},
            "approved": true,
        }),
        "0192aaaa-0000-7000-8000-0000000000a2",
    )
    .await;
    assert_eq!(status, 201);
    let (status, _) = post_json(
        port,
        &principal.token,
        "/api/v1/updates/larm",
        &json!({"plan_id": v7(), "plan_hash": new_hash,
                "approval_id": receipt["resource_id"].as_str().unwrap()}),
    )
    .await;
    assert_eq!(status, 202);
    let phase = wait_phase(&data, "larm.update", &["COMPLETED", "FAILED"]).await;
    assert_eq!(phase, "FAILED", "staged corrupto debe fallar");
    assert_eq!(
        job_error_reason(&data, "larm.update").await,
        "update_staged_hash_mismatch"
    );
    println!("REAL P133: staged con hash corrupto → job FAILED (update_staged_hash_mismatch)");

    principal.revoke();
    daemon_process.stop();
}
