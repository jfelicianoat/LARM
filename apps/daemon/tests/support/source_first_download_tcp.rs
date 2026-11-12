//! Full remote acquisition over actual daemon TCP; no seeded model/bundle.
use super::daemon_process::RunningDaemon;
use super::download_restart::{job_snapshot, post_json, wait_state, TemporaryPrincipal};
use super::*;
use larm_domain::ports::VariantRepository;
use std::collections::BTreeSet;
use std::io::Write;
use std::time::Instant;

struct Archive {
    output: PathBuf,
    data: PathBuf,
    requests: PathBuf,
}
impl Drop for Archive {
    fn drop(&mut self) {
        for name in ["remote-first.log", "remote-restarted.log"] {
            if self.data.join(name).is_file() {
                let _ = std::fs::copy(self.data.join(name), self.output.join(name));
            }
        }
        if self.requests.is_file() {
            let _ = std::fs::copy(&self.requests, self.output.join("requests.ndjson"));
        }
    }
}

async fn get(
    client: &reqwest::Client,
    port: u16,
    principal: &TemporaryPrincipal,
    path: &str,
) -> Value {
    let response = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn finished(
    client: &reqwest::Client,
    port: u16,
    principal: &TemporaryPrincipal,
    path: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let value = get(client, port, principal, path).await;
        assert_ne!(value["state"], "FAILED", "HTTP job failed: {value}");
        if value["state"] == "COMPLETED" {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "HTTP job did not complete: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn trace(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

async fn physical_counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM model_bundle),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation)")
        .fetch_one(&db.pool).await.unwrap()
}

async fn live_ownership(db: &larm_db::pool::Db, job_id: &str, plan_id: &str) -> (i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM volume_reservation WHERE job_id=?),(SELECT count(*) FROM resource_lock WHERE owner_id=?)")
        .bind(job_id).bind(plan_id).fetch_one(&db.pool).await.unwrap()
}

async fn progressed(
    db: &larm_db::pool::Db,
    job: &str,
    artifact: &str,
    after: u64,
) -> (String, i64, Value, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let current = job_snapshot(db, job).await;
        assert_ne!(current.0, "FAILED", "remote download failed: {current:?}");
        let offset = current.2["artifacts"][artifact]["durable_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if current.0 == "RUNNING" && offset > after {
            return current;
        }
        assert_ne!(
            current.0, "COMPLETED",
            "fixture must interrupt before completion"
        );
        assert!(
            Instant::now() < deadline,
            "remote progress did not advance: {current:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
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
        .join("docs/reports/P044_runs/remote-tcp-acquisition")
        .join(&id);
    std::fs::create_dir_all(&output).unwrap();
    let config = data_root.join("remote-tcp-config.json");
    download_restart::live_config(&config);
    let mut principal = TemporaryPrincipal::enroll(data_root, &config);
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let requests = PathBuf::from(fixture.data["requests"].as_str().unwrap());
    let _archive = Archive {
        output: output.clone(),
        data: data_root.to_path_buf(),
        requests: requests.clone(),
    };
    let location =
        download_restart::create_location(db, &data_root.join("remote-models"), initial_ctx).await;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first =
        RunningDaemon::start_measured(data_root, &config, port, data_root.join("remote-first.log"));
    first.wait_live(port, Duration::from_secs(45)).unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    // Requalify in the actual daemon execution context with an independent,
    // approved workspace and temporary principal. Parent receipt is not reused.
    let qualification =
        download_restart::qualify_from_daemon(db, port, installation_id, &principal, &ctx).await;
    download_restart::set_bandwidth(port, &principal, Some(64 << 10)).await;
    assert_eq!(physical_counts(db).await, (0, 0, 0, 0));
    let models: i64 = sqlx::query_scalar("SELECT count(*) FROM model_variant")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(models, 0);
    let query = json!({"q":"","capabilities":[],"source_ids":[source_id],"compatibility":"ALL",
        "architectures":[],"formats":[],"quantizations":[],"licenses":[],"runtime_ids":[],
        "parameter_count":{"minimum":null,"maximum":null},"size_bytes":{"minimum":null,"maximum":null},
        "trust_score":{"minimum":null,"maximum":null},"certification_statuses":[],"only_executable":false,
        "only_recommended":false,"show_incompatible":true,"cursor":null,"limit":6});
    let before_search = trace(&requests);
    let search_key = uuid::Uuid::now_v7().to_string();
    let started = Instant::now();
    let (status, search) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/models/search",
        &search_key,
        &query,
        &ctx,
    )
    .await;
    let search_ms = started.elapsed().as_millis();
    std::fs::write(
        output.join("search.json"),
        serde_json::to_vec_pretty(
            &json!({"status":status.as_u16(),"body":search,"elapsed_ms":search_ms}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "TCP qualified search: {search}"
    );
    assert_eq!(search["items"].as_array().unwrap().len(), 6);
    assert!(search_ms < 60_000);
    let central: BTreeSet<String> = search["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            m["metadata"]["search"]["best_variant"]["variant_id"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    assert_eq!(central.len(), 6);
    let private: BTreeSet<&str> = fixture.data["catalog"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["model"]["id"].as_str().unwrap())
        .collect();
    assert!(central.iter().all(|id| !private.contains(id.as_str())));
    let events_before: i64 = sqlx::query_scalar("SELECT count(*) FROM event")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let (_, search_replay) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/models/search",
        &search_key,
        &query,
        &ctx,
    )
    .await;
    assert_eq!(search_replay, search);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        events_before
    );
    assert_eq!(physical_counts(db).await, (0, 0, 0, 0));
    assert!(trace(&requests)[before_search.len()..]
        .iter()
        .all(|r| !r["path"].as_str().unwrap().starts_with("/artifact/")));
    let variants = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let ids:Vec<(String,String)>=sqlx::query_as("SELECT DISTINCT variant_id,json_extract(source_reference,'$.upstream_model_id') FROM model_acquisition_manifest WHERE source_id=? ORDER BY json_extract(source_reference,'$.upstream_model_id')")
        .bind(source_id).fetch_all(&db.pool).await.unwrap();
    assert_eq!(ids.len(), 6);
    assert_eq!(ids[0].1, "fixture-0");
    let variant_id = &ids[0].0;
    let acquisitions = larm_db::acquisition::SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: None,
    };
    let snapshot = acquisitions.latest(variant_id).await.unwrap().unwrap();
    let artifact_id = &snapshot.required_artifact_ids[0];
    let request = json!({"variant_id":variant_id,"storage_location_id":location.id,"hard_budget_bytes":"8388608","source_id":source_id,"priority":"NORMAL","optional_artifact_ids":[]});
    let before_plan = trace(&requests).len();
    let (status, plan) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &request,
        &ctx,
    )
    .await;
    std::fs::write(
        output.join("plan.json"),
        serde_json::to_vec_pretty(&json!({"status":status.as_u16(),"body":plan})).unwrap(),
    )
    .unwrap();
    assert_eq!(status, reqwest::StatusCode::CREATED, "TCP preview: {plan}");
    assert_eq!(plan["acquisition"], json!(snapshot.binding()));
    assert!(plan["plan_id"].is_string());
    assert_eq!(physical_counts(db).await, (0, 0, 0, 0));
    assert_eq!(trace(&requests).len(), before_plan);
    let body =
        json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"operator_confirmed":false});
    let (status, _) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &body,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(physical_counts(db).await, (0, 0, 0, 0));
    let mut execute = body.clone();
    execute["operator_confirmed"] = json!(true);
    let key = uuid::Uuid::now_v7().to_string();
    let (status, accepted) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &key,
        &execute,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "TCP acceptance: {accepted}"
    );
    let (_, replay) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &key,
        &execute,
        &ctx,
    )
    .await;
    assert_eq!(replay, accepted);
    let job = accepted["job_id"].as_str().unwrap();
    let plan_id = plan["plan_id"].as_str().unwrap();
    progressed(db, job, artifact_id, 0).await;
    let control = json!({"reason":"operator_request","approval_id":null});
    let (status, pause) = post_json(
        &client,
        port,
        &principal,
        &format!("/api/v1/downloads/{job}/pause"),
        &uuid::Uuid::now_v7().to_string(),
        &control,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "TCP pause: {pause}");
    let paused = wait_state(db, job, "PAUSED").await;
    let paused_offset = paused.2["artifacts"][artifact_id]["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(paused_offset > 0 && paused_offset < 2 << 20);
    assert_eq!(live_ownership(db, job, plan_id).await, (1, 3));
    let partial = Path::new(&location.canonical_path)
        .join("staging")
        .join(job)
        .join(format!("{artifact_id}.partial"));
    assert_eq!(std::fs::metadata(&partial).unwrap().len(), paused_offset);
    assert_eq!(physical_counts(db).await, (0, 0, 1, 1));
    // A competing download of this same physical identity cannot get through.
    let (_, conflict_plan) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &request,
        &ctx,
    )
    .await;
    let conflicting = json!({"plan_id":conflict_plan["plan_id"],"plan_hash":conflict_plan["plan_hash"],"operator_confirmed":true});
    let (status, conflict) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &conflicting,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(conflict["details"]["reason"], "resource_locked");
    assert_eq!(physical_counts(db).await, (0, 0, 1, 1));
    // Another model can be admitted and cancelled without touching the paused
    // model's partial, reservation or locks.
    let mut other = request.clone();
    other["variant_id"] = json!(ids[1].0);
    let (_, other_plan) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &other,
        &ctx,
    )
    .await;
    let other_execute = json!({"plan_id":other_plan["plan_id"],"plan_hash":other_plan["plan_hash"],"operator_confirmed":true});
    let (status, other_job) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &other_execute,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "other remote admission: {other_job}"
    );
    let cancelled_id = other_job["job_id"].as_str().unwrap();
    let (status, _) = post_json(
        &client,
        port,
        &principal,
        &format!("/api/v1/downloads/{cancelled_id}/cancel"),
        &uuid::Uuid::now_v7().to_string(),
        &control,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    wait_state(db, cancelled_id, "CANCELLED").await;
    assert_eq!(
        live_ownership(db, cancelled_id, other_plan["plan_id"].as_str().unwrap()).await,
        (0, 0)
    );
    assert_eq!(live_ownership(db, job, plan_id).await, (1, 3));
    assert_eq!(std::fs::metadata(&partial).unwrap().len(), paused_offset);
    assert_eq!(physical_counts(db).await, (0, 0, 2, 1));
    let (status, resume) = post_json(
        &client,
        port,
        &principal,
        &format!("/api/v1/downloads/{job}/resume"),
        &uuid::Uuid::now_v7().to_string(),
        &control,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "TCP resume: {resume}"
    );
    progressed(db, job, artifact_id, paused_offset).await;
    first.stop(); // actual abrupt process termination, not graceful job pause
    let interrupted = job_snapshot(db, job).await;
    assert_eq!(interrupted.0, "RUNNING");
    assert_eq!(interrupted.1, 2);
    let offset = interrupted.2["artifacts"][artifact_id]["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(offset > paused_offset && offset < 2 << 20);
    let mut tail = std::fs::OpenOptions::new()
        .append(true)
        .open(&partial)
        .unwrap();
    tail.write_all(b"owned unacknowledged tail").unwrap();
    tail.sync_all().unwrap();
    drop(tail);
    let before_restart = trace(&requests).len();
    assert_eq!(live_ownership(db, job, plan_id).await, (1, 3));
    download_restart::config(&config, None);
    let mut restarted = RunningDaemon::start_measured(
        data_root,
        &config,
        port,
        data_root.join("remote-restarted.log"),
    );
    restarted.wait_live(port, Duration::from_secs(45)).unwrap();
    let completed = finished(
        &client,
        port,
        &principal,
        accepted["status_url"].as_str().unwrap(),
    )
    .await;
    let state = job_snapshot(db, job).await;
    assert_eq!(state.0, "COMPLETED");
    assert_eq!(state.1, 3);
    assert_eq!(state.2["recovered_after_restart"], true);
    let after = trace(&requests);
    let resumed = after[before_restart..]
        .iter()
        .find(|r| r["path"] == "/artifact/0")
        .unwrap();
    let expected = std::fs::read(fixture.data["expected_files"][0].as_str().unwrap()).unwrap();
    let expected_sha = hex::encode(Sha256::digest(&expected));
    assert_eq!(resumed["range"], format!("bytes={offset}-"));
    assert_eq!(resumed["if_range"], format!("\"{expected_sha}\""));
    let (blob,sha,trust):(String,String,String)=sqlx::query_as("SELECT l.relative_path,l.sha256,p.trust FROM artifact_location l JOIN artifact_provenance p ON p.artifact_id=l.artifact_id WHERE l.artifact_id=?")
        .bind(artifact_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        std::fs::read(Path::new(&location.canonical_path).join(&blob)).unwrap(),
        expected
    );
    assert_eq!(sha, expected_sha);
    assert_eq!(trust, "VERIFIED");
    assert_eq!(physical_counts(db).await, (1, 1, 2, 0));
    assert_eq!(live_ownership(db, job, plan_id).await, (0, 0));
    let binding: String = sqlx::query_scalar(
        "SELECT json_extract(metadata,'$.acquisition') FROM download_job WHERE id=?",
    )
    .bind(job)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&binding).unwrap(),
        plan["acquisition"]
    );
    let bundle = variants.bundle_of(variant_id).await.unwrap().unwrap();
    assert_eq!(bundle.state, larm_domain::model::BundleState::Verified);
    let (status, verification) = post_json(
        &client,
        port,
        &principal,
        &format!("/api/v1/models/{variant_id}/verify"),
        &uuid::Uuid::now_v7().to_string(),
        &json!({"resource_id":variant_id,"profile_id":null}),
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    finished(
        &client,
        port,
        &principal,
        verification["status_url"].as_str().unwrap(),
    )
    .await;
    let before_observation = trace(&requests).len();
    let (status, reobserved) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/models/search",
        &uuid::Uuid::now_v7().to_string(),
        &query,
        &ctx,
    )
    .await;
    std::fs::write(
        output.join("reobserved.json"),
        serde_json::to_vec_pretty(&json!({"status":status.as_u16(),"body":reobserved})).unwrap(),
    )
    .unwrap();
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let reobserved_ids: BTreeSet<String> = reobserved["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            m["metadata"]["search"]["best_variant"]["variant_id"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    assert_eq!(reobserved_ids, central);
    let fresh_snapshot = acquisitions.latest(variant_id).await.unwrap().unwrap();
    assert_ne!(
        fresh_snapshot.id, snapshot.id,
        "a fresh post-restart observation must reach P041"
    );
    assert_eq!(
        fresh_snapshot.required_artifact_ids,
        snapshot.required_artifact_ids
    );
    assert_eq!(
        acquisitions.get(&snapshot.id).await.unwrap().unwrap(),
        snapshot
    );
    assert_eq!(physical_counts(db).await, (1, 1, 2, 0));
    assert!(trace(&requests)[before_observation..]
        .iter()
        .all(|r| !r["path"].as_str().unwrap().starts_with("/artifact/")));
    let final_variant = variants.get_variant(variant_id).await.unwrap().unwrap();
    assert_eq!(final_variant.lifecycle, "VERIFIED");
    assert_eq!(final_variant.availability, "UNKNOWN");
    assert_eq!(
        variants.bundle_of(variant_id).await.unwrap().unwrap().id,
        bundle.id
    );
    let unpaired:i64=sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE o.event_id IS NULL").fetch_one(&db.pool).await.unwrap();
    assert_eq!(unpaired, 0);
    restarted.stop();
    assert!(principal.revoke());
    let backup = output.join("accepted.sqlite");
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let result = json!({"id":id,"status":"PASS","scope":"actual daemon TCP + qualified original source + public search/P041/first remote acquisition/pause/cancel/crash/restart/Range/CAS/P042/fresh observation; own fixture bytes, no GUI/upstream/private-auth/runtime acceptance",
        "qualification":qualification["result"]["conformance"],"source_id":source_id,"central_variant_ids":central,"variant_id":variant_id,"artifact_id":artifact_id,
        "search_ms":search_ms,"search_replay_unchanged":true,"no_bytes_during_metadata_or_preparation":true,"plan":plan,"job_id":job,
        "pause_offset":paused_offset.to_string(),"crash_checkpoint":offset.to_string(),"attempts":state.1,"recovered_after_restart":true,
        "resumed_request":resumed,"discarded_unacknowledged_tail":true,"completed":completed,"cancelled_job_id":cancelled_id,
        "same_variant_conflict":conflict,"sha256":sha,"size_bytes":expected.len().to_string(),"trust":trust,"bundle":bundle,
        "final_variant":final_variant,"reobservation_same_core_ids":true,"fresh_snapshot_id":fresh_snapshot.id,"final_physical_counts":[1,1,2,0],"unpaired_outbox":unpaired,
        "temporary_principal_revoked":true,"sqlite_sha256":hex::encode(Sha256::digest(std::fs::read(&backup).unwrap()))});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}
