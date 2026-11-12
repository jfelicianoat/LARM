//! Interactive acceptance with the actual Tauri transport and owned daemon.
//! No browser JS bridge, no seeded Core model/bundle, no product auth bypass.
use super::daemon_process::RunningDaemon;
use super::download_restart::{post_json, TemporaryPrincipal};
use super::*;
use std::time::Instant;

fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

async fn checkpoint(db: &larm_db::pool::Db, label: &str) -> Value {
    let raw: String = sqlx::query_scalar("SELECT json_object('models',(SELECT count(*) FROM model_variant),'bundles',(SELECT count(*) FROM model_bundle),'locations',(SELECT count(*) FROM artifact_location),'reservations',(SELECT count(*) FROM volume_reservation),'locks',(SELECT count(*) FROM resource_lock),'jobs',json((SELECT json_group_array(json_object('id',id,'state',state,'attempt',attempt)) FROM download_job)),'verifications',json((SELECT json_group_array(json_object('id',id,'phase',phase)) FROM operation_journal WHERE kind='model.verify')))")
        .fetch_one(&db.pool).await.unwrap();
    json!({"label":label,"at":chrono::Utc::now().to_rfc3339(),"core":serde_json::from_str::<Value>(&raw).unwrap()})
}

pub(super) async fn exercise(
    db: &larm_db::pool::Db,
    data_root: &Path,
    fixture: &Fixture,
    installation_id: &str,
    source_id: &str,
    initial_ctx: &MutationContext,
) -> Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let id = uuid::Uuid::now_v7().to_string();
    let output = repo
        .join("docs/reports/P044_runs/native-models-gui")
        .join(&id);
    std::fs::create_dir_all(&output).unwrap();
    let config = data_root.join("models-gui-config.json");
    download_restart::live_config(&config);
    let mut principal = TemporaryPrincipal::enroll_desktop(data_root, &config);
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let location =
        download_restart::create_location(db, &data_root.join("gui-models"), initial_ctx).await;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let log = output.join("daemon.log");
    let mut daemon = RunningDaemon::start_desktop_fixture(data_root, &config, port, log);
    daemon.wait_live(port, Duration::from_secs(45)).unwrap();
    assert!(
        daemon
            .log_text()
            .contains("bootstrap del escritorio disponible"),
        "the owned daemon must have exclusive bootstrap ownership before opening the desktop"
    );
    assert!(!daemon.log_text().contains("el pipe de bootstrap se detuvo"));
    let qualification =
        download_restart::qualify_from_daemon(db, port, installation_id, &principal, &ctx).await;
    write(&output.join("qualification.json"), &qualification);
    let initial = checkpoint(db, "qualified-before-api-ingestion").await;
    write(&output.join("initial.json"), &initial);
    assert_eq!(initial["core"]["models"], 0);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let query = json!({"q":"","capabilities":[],"source_ids":[source_id],"compatibility":"ALL","architectures":[],"formats":[],"quantizations":[],"licenses":[],"runtime_ids":[],"parameter_count":{"minimum":null,"maximum":null},"size_bytes":{"minimum":null,"maximum":null},"trust_score":{"minimum":null,"maximum":null},"certification_statuses":[],"only_executable":false,"only_recommended":false,"show_incompatible":true,"cursor":null,"limit":6});
    let (status, search) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/models/search",
        &uuid::Uuid::now_v7().to_string(),
        &query,
        &ctx,
    )
    .await;
    write(
        &output.join("search-response.json"),
        &json!({"status":status.as_u16(),"body":search}),
    );
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "real search failed: {search}"
    );
    assert_eq!(search["items"].as_array().unwrap().len(), 6);
    write(&output.join("search.json"), &search);
    let ready_state = checkpoint(db, "ready-no-physical-bundle-or-download").await;
    assert_eq!(ready_state["core"]["models"], 6);
    for key in ["bundles", "locations", "reservations", "locks"] {
        assert_eq!(ready_state["core"][key], 0);
    }
    assert!(ready_state["core"]["jobs"].as_array().unwrap().is_empty());
    let ready = json!({"run_id":id,"output":output,"data_root":data_root,"port":port,"daemon_pid":daemon.child.id(),"source_id":source_id,"storage_id":location.id,"actor_id":principal.actor_id,"command_path":output.join("command.json"),"deadline_seconds":720,"state":ready_state});
    write(&output.join("ready.json"), &ready);
    write(
        &repo.join("docs/reports/P044_runs/native-models-gui.ready.json"),
        &ready,
    );
    println!("GUI_READY run_id={id} output={}", output.display());
    let mut checkpoints = vec![initial, ready_state];
    let deadline = Instant::now() + Duration::from_secs(720);
    let mut sequence = 0u64;
    loop {
        assert!(
            Instant::now() < deadline,
            "native GUI acceptance timed out; scoped daemon/principal are cleaned by Drop"
        );
        if let Ok(raw) = std::fs::read(output.join("command.json")) {
            if let Ok(command) = serde_json::from_slice::<Value>(&raw) {
                if let Some(next) = command["sequence"].as_u64().filter(|next| *next > sequence) {
                    sequence = next;
                    let label = command["label"].as_str().unwrap_or("GUI checkpoint");
                    let state = checkpoint(db, label).await;
                    checkpoints.push(state.clone());
                    write(
                        &output.join("checkpoint.json"),
                        &json!({"sequence":sequence,"state":state}),
                    );
                    write(&output.join("checkpoints.json"), &json!(checkpoints));
                    if command["finish"] == true {
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    daemon.stop();
    assert!(principal.revoke());
    let backup = output.join("accepted.sqlite");
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    std::fs::copy(
        fixture.data["requests"].as_str().unwrap(),
        output.join("requests.ndjson"),
    )
    .unwrap();
    let last = checkpoints.last().unwrap();
    let jobs = last["core"]["jobs"].as_array().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "only the explicitly confirmed GUI download may exist"
    );
    assert_eq!(jobs[0]["state"], "COMPLETED");
    assert_eq!(last["core"]["reservations"], 0);
    assert_eq!(last["core"]["locks"], 0);
    assert!(last["core"]["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["phase"] == "COMPLETED"));
    let (root, relative, sha): (String, String, String) = sqlx::query_as("SELECT s.canonical_path,l.relative_path,l.sha256 FROM artifact_location l JOIN storage_location s ON s.id=l.storage_location_id").fetch_one(&db.pool).await.unwrap();
    let bytes = std::fs::read(Path::new(&root).join(&relative)).unwrap();
    let expected = std::fs::read(fixture.data["expected_files"][0].as_str().unwrap()).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(hex::encode(Sha256::digest(&bytes)), sha);
    std::fs::write(output.join("accepted-artifact.bin"), &bytes).unwrap();
    let result = json!({"status":"PASS","run_id":id,"output":output,"qualification":qualification,"checkpoints":checkpoints,"artifact_bytes":bytes.len(),"artifact_sha256":sha,"sqlite_sha256":hex::encode(Sha256::digest(std::fs::read(backup).unwrap())),"temporary_desktop_principal_revoked":true,"scope":"native Tauri UI -> native bootstrap/transport -> actual TCP API, controlled qualified source; ingestion through API before opening GUI; no Discover UI/upstream/runtime acceptance"});
    write(&output.join("result.json"), &result);
    result
}
