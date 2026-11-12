//! P003: Settings durable, overrides de fichero y reinicios por TCP real.
use super::daemon_process::RunningDaemon;
use super::download_restart::{config, TemporaryPrincipal};
use larm_domain::ports::SettingsRepository;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

pub(super) async fn read(port: u16, principal: &TemporaryPrincipal) -> (String, Value) {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{port}/api/v1/settings"))
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    (etag, response.json().await.unwrap())
}

async fn patch(port: u16, principal: &TemporaryPrincipal, value: u64) -> (String, Value) {
    let (etag, mut settings) = read(port, principal).await;
    settings["storage"]["download_bandwidth_limit_bytes_per_second"] = json!(value.to_string());
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .patch(format!("http://127.0.0.1:{port}/api/v1/settings"))
        .bearer_auth(&principal.token)
        .header("if-match", etag)
        .json(&settings)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    (etag, response.json().await.unwrap())
}

fn observed(etag: &str, settings: &Value) -> Value {
    json!({"etag":etag,"limit":settings["storage"]["download_bandwidth_limit_bytes_per_second"]})
}

fn start(data: &Path, cfg: &Path, port: u16, name: &str) -> RunningDaemon {
    let mut daemon = RunningDaemon::start(data, cfg, port, data.join(name));
    daemon.wait_live(port, Duration::from_secs(45)).unwrap();
    daemon
}

#[tokio::test]
async fn p003_settings_tcp_respeta_fichero_y_recupera_valor_persistido() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let owned = tempfile::Builder::new()
        .prefix("p003-settings-")
        .tempdir_in(repo.join("target"))
        .unwrap();
    let data = owned.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let cfg = data.join("settings.json");
    config(&cfg, Some(8192));
    let mut principal = TemporaryPrincipal::enroll(&data, &cfg);
    let db = larm_db::pool::Db::open(&data.join("larm.sqlite"))
        .await
        .unwrap();
    let repository = larm_db::settings::SqlSettings {
        pool: db.pool.clone(),
    };
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first = start(&data, &cfg, port, "settings-first.log");
    let (etag, initial) = read(port, &principal).await;
    assert_eq!(etag, "\"1\"");
    assert_eq!(
        initial["storage"]["download_bandwidth_limit_bytes_per_second"],
        "8192"
    );
    let initial = observed(&etag, &initial);
    let (etag, response) = patch(port, &principal, 65536).await;
    assert_eq!(etag, "\"2\"");
    assert_eq!(
        response["storage"]["download_bandwidth_limit_bytes_per_second"],
        "8192"
    );
    let updated = observed(&etag, &response);
    assert_eq!(read(port, &principal).await, (etag, response));
    let persisted = repository.get_or_initialize(json!({})).await.unwrap();
    assert_eq!(persisted.revision, 2);
    assert_eq!(
        persisted.payload["storage"]["download_bandwidth_limit_bytes_per_second"],
        "65536"
    );
    first.stop();
    let mut second = start(&data, &cfg, port, "settings-file-restart.log");
    let (etag, response) = read(port, &principal).await;
    assert_eq!(observed(&etag, &response), updated);
    second.stop();

    config(&cfg, None);
    let mut third = start(&data, &cfg, port, "settings-null-restart.log");
    let (etag, response) = read(port, &principal).await;
    assert_eq!(etag, "\"2\"");
    assert!(response["storage"]["download_bandwidth_limit_bytes_per_second"].is_null());
    let null_override = observed(&etag, &response);
    third.stop();

    super::download_restart::live_config(&cfg);
    let mut fourth = start(&data, &cfg, port, "settings-omitted-restart.log");
    let (etag, response) = read(port, &principal).await;
    assert_eq!(etag, "\"2\"");
    assert_eq!(
        response["storage"]["download_bandwidth_limit_bytes_per_second"],
        "65536"
    );
    let restored = observed(&etag, &response);
    let (etag, response) = patch(port, &principal, 32768).await;
    assert_eq!(etag, "\"3\"");
    assert_eq!(
        response["storage"]["download_bandwidth_limit_bytes_per_second"],
        "32768"
    );
    let live = observed(&etag, &response);
    fourth.stop();
    let mut fifth = start(&data, &cfg, port, "settings-persisted-restart.log");
    let (etag, response) = read(port, &principal).await;
    assert_eq!(observed(&etag, &response), live);
    let persisted = repository.get_or_initialize(json!({})).await.unwrap();
    assert_eq!(persisted.revision, 3);
    assert_eq!(
        persisted.payload["storage"]["download_bandwidth_limit_bytes_per_second"],
        "32768"
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='settings.changed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let unpaired: i64 = sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE e.event_type='settings.changed' AND o.event_id IS NULL").fetch_one(&db.pool).await.unwrap();
    assert_eq!((events, unpaired), (2, 0));
    fifth.stop();
    assert!(principal.revoke());

    let id = uuid::Uuid::now_v7().to_string();
    let output = repo.join("docs/reports/P003_runs/settings-tcp").join(&id);
    std::fs::create_dir_all(&output).unwrap();
    for name in [
        "settings-first.log",
        "settings-file-restart.log",
        "settings-null-restart.log",
        "settings-omitted-restart.log",
        "settings-persisted-restart.log",
    ] {
        std::fs::copy(data.join(name), output.join(name)).unwrap();
    }
    let receipt = json!({
        "scope":"actual Windows daemon TCP, persisted Settings/CAS/outbox and file overrides across five process starts; unit hooks cover live limiter application, no aggregate network-rate claim",
        "id":id,"observed_at":larm_domain::bridge::timestamp(chrono::Utc::now()),
        "daemon_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),
        "initial":initial,"patch_under_file":updated,"after_same_file_restart":updated,"after_null_override":null_override,"after_removing_override":restored,"unoverridden_patch_and_restart":live,
        "persisted_revision":persisted.revision,"persisted_limit":persisted.payload["storage"]["download_bandwidth_limit_bytes_per_second"],"settings_changed_events":events,"unpaired_outbox_events":unpaired,"temporary_principal_revoked":true
    });
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/reports/P003_runs/settings-tcp.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    db.pool.close().await;
    eprintln!("P003 TCP: fichero, null y valor persistido coherentes tras cinco arranques; principal revocado");
}
