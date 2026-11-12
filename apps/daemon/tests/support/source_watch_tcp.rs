//! P050/P127: scheduler real, P048 original, P022 y snapshot durable por TCP.
use super::daemon_process::RunningDaemon;
use super::download_restart::{live_config, post_json, TemporaryPrincipal};
use super::p048_download::{qualify, register_profile};
use super::*;
use larm_adapter_host::conformance::source_http::HttpSourceConfig;
use std::time::Instant;

struct Archive {
    output: PathBuf,
    data: PathBuf,
    requests: PathBuf,
}
impl Drop for Archive {
    fn drop(&mut self) {
        for name in ["watch-first.log", "watch-restarted.log"] {
            if self.data.join(name).is_file() {
                let _ = std::fs::copy(self.data.join(name), self.output.join(name));
            }
        }
        if self.requests.is_file() {
            let _ = std::fs::copy(&self.requests, self.output.join("requests.ndjson"));
        }
    }
}

async fn get(port: u16, principal: &TemporaryPrincipal, path: &str) -> Value {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test]
async fn source_watch_tcp_sin_capacidad_duplicada_con_actor_aprobado_y_reinicio() {
    let (repo, fixture) = super::source_http_product::http_fixture();
    assert!(fixture.manifest.capabilities.is_empty());
    assert!(fixture.manifest.methods.iter().any(|m| m == "source.sync"));
    let cfg: HttpSourceConfig = serde_json::from_value(fixture.data["config"].clone()).unwrap();
    let data = fixture._root.path().join("data");
    let config = data.join("watch-config.json");
    live_config(&config);
    let db = larm_db::pool::Db::open(&data.join("larm.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let mut principal = TemporaryPrincipal::enroll(&data, &config);
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let installation = register_profile(&db, &fixture, &cfg, &ctx, &repo).await;
    let id = uuid::Uuid::now_v7().to_string();
    let output = repo
        .join("docs/reports/P127_runs/source-watch-tcp")
        .join(&id);
    std::fs::create_dir_all(&output).unwrap();
    let _archive = Archive {
        output: output.clone(),
        data: data.clone(),
        requests: cfg.requests.clone(),
    };
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first = RunningDaemon::start(&data, &config, port, data.join("watch-first.log"));
    first.wait_live(port, Duration::from_secs(45)).unwrap();
    let qualified = qualify(port, &principal, &installation, &ctx, &fixture, &cfg).await;
    let run = qualified["result"]["conformance"]["run_id"]
        .as_str()
        .unwrap();
    std::fs::copy(
        cfg.evidence_root.join(run).join("source-http.json"),
        output.join("source-http.json"),
    )
    .unwrap();
    // Source binding is fixture preparation with the profile's fixed source
    // ID. Source creation/remote catalog ingestion is not claimed by this case.
    let now = timestamp(chrono::Utc::now());
    let adapter = uuid::Uuid::now_v7().to_string();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) SELECT ?,?,?,1,?,id,'HTTP',json_extract(manifest,'$.capabilities') FROM adapter_installation WHERE id=? AND state='VALIDATED'")
        .bind(&adapter).bind(&now).bind(&now).bind(json!({"source_protocol_v1":{"search_applicable":false,"supported_filters":[],"limitations":["explicit_url_import_only"]}}).to_string()).bind(&installation).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,?,?,1,'{}','P048 owned watcher',?,?,0,1,'NONE','{}')")
        .bind(&cfg.source_id).bind(&now).bind(&now).bind(&adapter).bind(&cfg.url).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let source_path = format!("/api/v1/sources/{}", cfg.source_id);
    let before = get(port, &principal, &source_path).await;
    assert_eq!(before["operational"]["status"], "UNKNOWN");
    assert!(before["operational"]["last_sync_at"].is_null());
    let request = json!({"source_id":cfg.source_id,"query":{},"interval_seconds":3600,"policy":{},"enabled":true});
    let key = uuid::Uuid::now_v7().to_string();
    let client = reqwest::Client::new();
    let (status, created) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/source-watches",
        &key,
        &request,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "create watch: {created}"
    );
    let (status, replay) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/source-watches",
        &key,
        &request,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    assert_eq!(replay, created);
    let watch = created["id"].as_str().unwrap();
    let reads_before = std::fs::read_to_string(&cfg.requests)
        .unwrap()
        .lines()
        .count();
    // Prepare a due row instead of waiting an hour; the scheduler and all
    // execution/clock/results remain the real daemon, not an injected result.
    sqlx::query("UPDATE source_watch SET next_run_at=? WHERE id=?")
        .bind(timestamp(chrono::Utc::now() - chrono::Duration::seconds(1)))
        .bind(watch)
        .execute(&db.pool)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    let synced = loop {
        let row = get(port, &principal, &format!("/api/v1/source-watches/{watch}")).await;
        if row["metadata"]["watcher"]["last_success_at"].is_string() {
            break row;
        }
        if row["metadata"]["watcher"]["last_error"].is_string() {
            std::fs::write(
                output.join("failed-watch.json"),
                serde_json::to_vec_pretty(&row).unwrap(),
            )
            .unwrap();
            panic!(
                "watch failed: {}; {}",
                row["metadata"]["watcher"],
                first.log_text()
            );
        }
        assert!(
            Instant::now() < deadline,
            "watch did not finish: {row}; {}",
            first.log_text()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let source = get(port, &principal, &source_path).await;
    assert_eq!(source["operational"]["status"], "HEALTHY");
    assert_eq!(
        source["operational"]["last_sync_at"],
        synced["metadata"]["watcher"]["last_success_at"]
    );
    assert!(source["operational"]["rate_limit"].is_null());
    assert_eq!(synced["metadata"]["watcher"]["failure_count"], 0);
    assert!(synced["metadata"]["watcher"]["last_error"].is_null());
    let events: Vec<(String,String)> = sqlx::query_as("SELECT event_type,actor_id FROM event WHERE resource_id=? AND event_type IN ('source.sync_started','source.sync_completed') ORDER BY event_id")
        .bind(watch).fetch_all(&db.pool).await.unwrap();
    assert_eq!(
        events,
        vec![
            ("source.sync_started".into(), "system:source-watcher".into()),
            (
                "source.sync_completed".into(),
                "system:source-watcher".into()
            )
        ]
    );
    let new_reads = std::fs::read_to_string(&cfg.requests)
        .unwrap()
        .lines()
        .skip(reads_before)
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert!(new_reads.iter().any(|r| r["method"] == "HEAD"));
    assert!(!new_reads.iter().any(|r| r["method"] == "GET"));
    let counts:(i64,i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM canonical_model),(SELECT count(*) FROM model_artifact),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0, 0));
    let notifications: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notification WHERE json_extract(metadata,'$.source_id')=?",
    )
    .bind(&cfg.source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(notifications, 1);
    let unpaired:i64=sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE e.resource_id=? AND o.event_id IS NULL").bind(watch).fetch_one(&db.pool).await.unwrap();
    assert_eq!(unpaired, 0);
    first.stop();
    let mut restarted =
        RunningDaemon::start(&data, &config, port, data.join("watch-restarted.log"));
    restarted.wait_live(port, Duration::from_secs(45)).unwrap();
    assert_eq!(
        get(port, &principal, &source_path).await["operational"],
        source["operational"]
    );
    let after = get(port, &principal, &format!("/api/v1/source-watches/{watch}")).await;
    assert_eq!(after, synced);
    let deleted = client
        .delete(format!(
            "http://127.0.0.1:{port}/api/v1/source-watches/{watch}"
        ))
        .bearer_auth(&principal.token)
        .header(
            "if-match",
            format!("\"{}\"", after["row_version"].as_i64().unwrap()),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    restarted.stop();
    assert!(principal.revoke());
    let receipt = json!({"id":id,"observed_at":timestamp(chrono::Utc::now()),"scope":"actual daemon scheduler/API/SQLite and original P048 HTTP executable with real P022/P026, empty optional capabilities, approved execution actor, durable operational snapshot and restart; installation/source binding/due time are test preparation; no catalog ingestion, private provider or GUI acceptance","daemon_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),"product_sha256":hex::encode(Sha256::digest(std::fs::read(fixture.package.join(&fixture.manifest.executable.relative_path)).unwrap())),"conformance":qualified["result"]["conformance"],"source_id":cfg.source_id,"watch_id":watch,"source_capabilities":fixture.manifest.capabilities,"execution_actor":principal.actor_id,"scheduler_actor":"system:source-watcher","watch_create_replay_unchanged":true,"operational_before":before["operational"],"operational_after":source["operational"],"watch_after_sync":synced,"snapshot_preserved_after_restart":true,"source_reads":new_reads,"notifications":notifications,"unpaired_outbox_events":unpaired,"no_models_artifacts_locations_downloads_reservations":true,"watch_deleted":true,"temporary_principal_revoked":true});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/reports/P127_runs/source-watch-tcp.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    db.pool.close().await;
    eprintln!("Source watcher TCP: original P048 synchronized with approved actor and empty capabilities; durable snapshot survived restart");
}
