//! Fallos HTTP reales contra el daemon y el SourceWorker cualificado.
use super::*;

#[path = "download_ranges.rs"]
mod ranges;

pub(super) struct Case<'a> {
    pub db: &'a larm_db::pool::Db,
    pub data: &'a Value,
    pub daemon: &'a RunningDaemon,
    pub port: u16,
    pub principal: &'a TemporaryPrincipal,
    pub request: &'a DownloadPlanRequest,
    pub ctx: &'a MutationContext,
    pub location: &'a StorageLocation,
    pub artifact_ids: &'a [String],
}

struct Job {
    id: String,
    plan_id: String,
    artifact_id: String,
    partial: PathBuf,
}

fn source_mode(data: &Value, mode: &str) {
    std::fs::write(
        data["control"].as_str().unwrap(),
        json!({"mode":mode,"probe_mode":"normal"}).to_string(),
    )
    .unwrap();
}

fn requests(data: &Value) -> Vec<Value> {
    std::fs::read_to_string(data["requests"].as_str().unwrap())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(super) async fn set_bandwidth(
    port: u16,
    principal: &TemporaryPrincipal,
    limit: Option<u64>,
) -> Value {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/api/v1/settings");
    let before = client
        .get(&url)
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    assert_eq!(before.status(), reqwest::StatusCode::OK);
    let etag = before.headers()["etag"].to_str().unwrap().to_owned();
    let mut settings: Value = before.json().await.unwrap();
    settings["storage"]["download_bandwidth_limit_bytes_per_second"] =
        json!(limit.map(|bytes| bytes.to_string()));
    let after = client
        .patch(&url)
        .bearer_auth(&principal.token)
        .header("if-match", &etag)
        .json(&settings)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::OK);
    let revision = after.headers()["etag"].to_str().unwrap().to_owned();
    let saved: Value = after.json().await.unwrap();
    assert_eq!(
        saved["storage"]["download_bandwidth_limit_bytes_per_second"],
        settings["storage"]["download_bandwidth_limit_bytes_per_second"]
    );
    json!({"before_etag":etag,"after_etag":revision,"limit_bytes_per_second":saved["storage"]["download_bandwidth_limit_bytes_per_second"]})
}

async fn new_job(case: &Case<'_>, index: usize) -> Job {
    let mut request = case.request.clone();
    request.variant_id = case.data["catalog"]["entries"][index]["model"]["id"]
        .as_str()
        .unwrap()
        .into();
    let client = reqwest::Client::new();
    let (status, plan) = post_json(
        &client,
        case.port,
        case.principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(request).unwrap(),
        case.ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "preflight: {plan}");
    let plan: DownloadPlanResponse = serde_json::from_value(plan).unwrap();
    let execute = ExecutePlan {
        plan_id: plan.plan_id.unwrap(),
        plan_hash: plan.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let (status, receipt) = post_json(
        &client,
        case.port,
        case.principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(&execute).unwrap(),
        case.ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "admisión: {receipt}");
    let id = receipt["job_id"].as_str().unwrap().to_owned();
    let artifact_id = case.artifact_ids[index].clone();
    let partial = Path::new(&case.location.canonical_path)
        .join("staging")
        .join(&id)
        .join(format!("{artifact_id}.partial"));
    Job {
        id,
        plan_id: execute.plan_id,
        artifact_id,
        partial,
    }
}

async fn control(case: &Case<'_>, job: &Job, action: &str) {
    let (status, _) = post_json(
        &reqwest::Client::new(),
        case.port,
        case.principal,
        &format!("/api/v1/downloads/{}/{action}", job.id),
        &uuid::Uuid::now_v7().to_string(),
        &json!({"reason":"operator_request","approval_id":null}),
        case.ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
}

async fn wait_bytes(case: &Case<'_>, job: &Job) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let snapshot = job_snapshot(case.db, &job.id).await;
        assert_ne!(
            snapshot.0, "FAILED",
            "falló antes del checkpoint: {snapshot:?}"
        );
        if snapshot.0 == "RUNNING"
            && snapshot.2["artifacts"][&job.artifact_id]["durable_offset"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|n| n > 0)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sin bytes durables: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn terminal(case: &Case<'_>, job: &Job, expected: &str) -> (Value, Vec<Value>) {
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut samples = Vec::new();
    loop {
        let snapshot = job_snapshot(case.db, &job.id).await;
        let checkpoint = snapshot.2["artifacts"][&job.artifact_id].clone();
        if checkpoint.is_object() && samples.last() != Some(&checkpoint) {
            samples.push(checkpoint);
        }
        if snapshot.0 == expected {
            let error = snapshot
                .3
                .as_deref()
                .map(|s| serde_json::from_str::<Value>(s).unwrap());
            return (
                json!({"state":snapshot.0,"attempt":snapshot.1,"progress":snapshot.2,"error":error}),
                samples,
            );
        }
        assert!(
            !matches!(snapshot.0.as_str(), "FAILED" | "COMPLETED" | "CANCELLED"),
            "terminal inesperado: {snapshot:?}"
        );
        assert!(
            Instant::now() < deadline,
            "sin {expected}: {snapshot:?}; log={}",
            case.daemon.log_text()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn durable_closure(case: &Case<'_>, job: &Job, phase: &str) -> Value {
    let (journal,reservations,unpaired):(String,i64,i64) = sqlx::query_as("SELECT phase,(SELECT count(*) FROM volume_reservation WHERE job_id=?),(SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND o.event_record_id IS NULL) FROM operation_journal WHERE id=? AND kind='download.plan'")
        .bind(&job.id).bind(&job.id).bind(&job.plan_id).fetch_one(&case.db.pool).await.unwrap();
    assert_eq!(journal, phase);
    assert_eq!((reservations, unpaired), (0, 0));
    json!({"journal_phase":journal,"reservations":reservations,"unpaired_outbox_events":unpaired})
}

pub(super) async fn exercise(case: Case<'_>, paused_id: &str, paused_file: &Path) -> Value {
    eprintln!("P051 integrity: preparando reanudación con ETag cambiado");
    let preserved = std::fs::read(paused_file).unwrap();
    let unlimited = set_bandwidth(case.port, case.principal, None).await;
    let etag_job = new_job(&case, 1).await;
    wait_bytes(&case, &etag_job).await;
    control(&case, &etag_job, "pause").await;
    let paused = wait_state(case.db, &etag_job.id, "PAUSED").await;
    let saved = paused.2["artifacts"][&etag_job.artifact_id].clone();
    let offset = saved["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let original = std::fs::read(case.data["expected_files"][1].as_str().unwrap()).unwrap();
    assert!(offset > 0 && offset < (original.len() as u64));
    let before = requests(case.data).len();
    source_mode(case.data, "etag_changed");
    control(&case, &etag_job, "resume").await;
    let (etag_terminal, samples) = terminal(&case, &etag_job, "COMPLETED").await;
    assert!(
        samples
            .iter()
            .any(|c| c["durable_offset"] == "0" && c["resume_validator"].is_null()),
        "reinicio durable a cero: {samples:?}"
    );
    let observed = requests(case.data)
        .into_iter()
        .skip(before)
        .filter(|r| r["path"] == "/artifact/1")
        .collect::<Vec<_>>();
    assert!(observed.len() >= 3);
    assert_eq!(observed[0]["range"], format!("bytes={offset}-"));
    assert_eq!(observed[0]["if_range"], saved["resume_validator"]["value"]);
    assert_eq!(observed[0]["status"], 200);
    assert_ne!(observed[0]["response_etag"], observed[0]["if_range"]);
    assert!(observed[1]["range"].is_null() && observed[1]["if_range"].is_null());
    assert!(observed
        .iter()
        .skip(2)
        .any(|r| r["status"] == 206 && r["if_range"] == observed[0]["response_etag"]));
    let relative: String =
        sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
            .bind(&etag_job.artifact_id)
            .fetch_one(&case.db.pool)
            .await
            .unwrap();
    let actual = std::fs::read(Path::new(&case.location.canonical_path).join(relative)).unwrap();
    assert_eq!(actual, original);
    assert!(!etag_job.partial.exists());
    let etag_closure = durable_closure(&case, &etag_job, "COMPLETED").await;
    eprintln!("P051 integrity: reinicio a cero y hash final verificados");
    source_mode(case.data, "corrupt");
    let hash_start = requests(case.data).len();
    let hash_job = new_job(&case, 2).await;
    let (hash_terminal, _) = terminal(&case, &hash_job, "FAILED").await;
    assert_eq!(hash_terminal["error"]["code"], "INTEGRITY_FAILED");
    assert_eq!(
        hash_terminal["error"]["details"]["reason"],
        "download_artifact_integrity_failed"
    );
    assert!(!hash_job.partial.exists());
    let (locations,provenance):(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM artifact_location WHERE artifact_id=?),(SELECT count(*) FROM artifact_provenance WHERE artifact_id=?)")
        .bind(&hash_job.artifact_id).bind(&hash_job.artifact_id).fetch_one(&case.db.pool).await.unwrap();
    assert_eq!((locations, provenance), (0, 0));
    let expected_hash = case.data["catalog"]["entries"][2]["inspection"]["artifacts"][0]["sha256"]
        .as_str()
        .unwrap();
    let hash_requests = requests(case.data)
        .into_iter()
        .skip(hash_start)
        .filter(|r| r["path"] == "/artifact/2")
        .collect::<Vec<_>>();
    assert!(!hash_requests.is_empty());
    let corrupt_hash = hash_requests[0]["body_sha256"].as_str().unwrap();
    assert_ne!(corrupt_hash, expected_hash);
    for hash in [expected_hash, corrupt_hash] {
        assert!(!Path::new(&case.location.canonical_path)
            .join(larm_storage::cas::blob_relative(hash).unwrap())
            .exists());
    }
    let hash_closure = durable_closure(&case, &hash_job, "FAILED").await;
    eprintln!("P051 integrity: hash corrupto rechazado sin publicación ni reserva");
    let after = requests(case.data);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        requests(case.data),
        after,
        "sin nuevas lecturas después del fallo terminal"
    );
    source_mode(case.data, "normal");
    let restored = set_bandwidth(case.port, case.principal, Some(64 << 10)).await;
    assert_eq!(std::fs::read(paused_file).unwrap(), preserved);
    assert_eq!(job_snapshot(case.db, paused_id).await.0, "PAUSED");
    let reserved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(paused_id)
            .fetch_one(&case.db.pool)
            .await
            .unwrap();
    assert_eq!(reserved, 1);
    json!({"etag_change":{"job_id":etag_job.id,"saved_checkpoint":saved,"progress_samples":samples,"requests":observed,"terminal":etag_terminal,"closure":etag_closure,"sha256":hex::encode(Sha256::digest(actual)),"partial_removed":true},"hash_mismatch":{"job_id":hash_job.id,"requests":hash_requests,"terminal":hash_terminal,"closure":hash_closure,"expected_sha256":expected_hash,"observed_body_sha256":corrupt_hash,"partial_removed":true,"artifact_locations":locations,"provenance_rows":provenance,"no_cas_blob":true,"no_new_reads_after_failure":true},"paused_download_preserved":true,"paused_download_reservations":reserved,"bandwidth_updates":[unlimited,restored]})
}

pub(super) async fn exercise_ranges(case: Case<'_>, paused_id: &str, paused_file: &Path) -> Value {
    let preserved = std::fs::read(paused_file).unwrap();
    let unlimited = set_bandwidth(case.port, case.principal, None).await;
    let proof = ranges::exercise(&case).await;
    let restored = set_bandwidth(case.port, case.principal, Some(64 << 10)).await;
    assert_eq!(std::fs::read(paused_file).unwrap(), preserved);
    assert_eq!(job_snapshot(case.db, paused_id).await.0, "PAUSED");
    let reserved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(paused_id)
            .fetch_one(&case.db.pool)
            .await
            .unwrap();
    assert_eq!(reserved, 1);
    json!({"range_416":proof,"paused_download_preserved":true,"paused_download_reservations":reserved,"bandwidth_updates":[unlimited,restored]})
}
