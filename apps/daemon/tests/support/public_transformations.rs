//! Real public P093 producer/consumer. SQL seeds only catalogue fixtures and
//! holds an explicit private-ledger fault; no direct Core admission/closure.
use super::*;
use larm_db::transformations::SqlTransformationJobs;
use larm_domain::{
    conformance::Run,
    conformance_profiles::ConformanceProfile,
    transformations::{TransformationJob, TransformationPlanResponse, TransformationState},
};
use sqlx::{Connection, Row};

pub(super) struct Inputs {
    pub backend_principal: download_restart::TemporaryPrincipal,
    pub daemon: daemon_process::RunningDaemon,
    pub db: larm_db::pool::Db,
    pub context: MutationContext,
    pub repo: PathBuf,
    pub output: PathBuf,
    pub data: PathBuf,
    pub settings: PathBuf,
    pub port: u16,
    pub installation: AdapterInstallation,
    pub run: Run,
    pub profile: ConformanceProfile,
    pub fixture: Fixture,
}
pub(super) fn start(
    input: Inputs,
    job_principal: download_restart::TemporaryPrincipal,
) -> tokio::task::JoinHandle<Value> {
    tokio::spawn(Box::pin(exercise(input, job_principal)))
}
fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM model_variant),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)").fetch_one(&db.pool).await.unwrap()
}
async fn get(
    http: &reqwest::Client,
    port: u16,
    principal: &download_restart::TemporaryPrincipal,
    path: &str,
) -> (reqwest::StatusCode, Value) {
    let response = http
        .get(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}
async fn post(
    http: &reqwest::Client,
    i: &Inputs,
    principal: &download_restart::TemporaryPrincipal,
    path: &str,
    key: &str,
    body: &Value,
    ctx: &MutationContext,
) -> (reqwest::StatusCode, Value) {
    download_restart::post_json(http, i.port, principal, path, key, body, ctx).await
}
async fn restart(
    http: &reqwest::Client,
    i: &Inputs,
    principal: &download_restart::TemporaryPrincipal,
    target: (&str, i64),
    key: &str,
    body: &Value,
    ctx: &MutationContext,
) -> (reqwest::StatusCode, Value) {
    let (job, revision) = target;
    let reply = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/transformations/{job}/restart",
            i.port
        ))
        .bearer_auth(&principal.token)
        .header("Idempotency-Key", key)
        .header("If-Match", revision.to_string())
        .header("X-Correlation-Id", &ctx.correlation_id)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = reply.status();
    (status, reply.json().await.unwrap())
}
async fn current(db: &larm_db::pool::Db, job: &str) -> TransformationJob {
    SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(job)
    .await
    .unwrap()
    .claim
    .job
}

async fn exercise(mut i: Inputs, mut principal: download_restart::TemporaryPrincipal) -> Value {
    let output = i.output.join("public-api");
    std::fs::create_dir(&output).unwrap();
    assert_ne!(principal.actor_id, i.context.actor_id);
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: id(),
    };
    let request = TransformationRequest {
        input_variant_id: i.fixture.data["profile"]["input_variant_id"]
            .as_str()
            .unwrap()
            .into(),
        adapter_id: i.installation.adapter_id.clone(),
        tool_version: "1.0.0".into(),
        target_format: "BASE64".into(),
        parameters: json!({}),
        storage_location_id: i.fixture.data["profile"]["storage_location_id"]
            .as_str()
            .unwrap()
            .into(),
        hard_budget_bytes: None,
    };
    let original = Box::pin(registered_case::seed_public_input(
        &i.db, &i.fixture, &request,
    ))
    .await;
    std::fs::write(output.join("input.bin"), &original).unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap();
    let body = json!(request);
    let mut negatives = Vec::new();
    for (name, changed, expected) in [
        (
            "unknown_adapter",
            json!({"adapter_id":"missing.transformation"}),
            reqwest::StatusCode::NOT_FOUND,
        ),
        (
            "wrong_tool",
            json!({"tool_version":"not-the-qualified-version"}),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "wrong_format",
            json!({"target_format":"GGUF"}),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "hard_budget",
            json!({"hard_budget_bytes":"1"}),
            reqwest::StatusCode::CONFLICT,
        ),
        (
            "unknown_field",
            json!({"approve_and_run":true}),
            reqwest::StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut bad = body.clone();
        for (key, value) in changed.as_object().unwrap() {
            bad[key] = value.clone();
        }
        let (status, value) = post(
            &http,
            &i,
            &principal,
            "/api/v1/transform-plans",
            &id(),
            &bad,
            &ctx,
        )
        .await;
        assert_eq!(status, expected, "{name}: {value}");
        assert_eq!(counts(&i.db).await, (0, 1, 0, 0));
        negatives.push(json!({"case":name,"status":status.as_u16(),"no_jobs_or_resources":true}));
    }
    let plan_key = id();
    let (status, first) = post(
        &http,
        &i,
        &principal,
        "/api/v1/transform-plans",
        &plan_key,
        &body,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "public plan: {first}");
    let first_plan: TransformationPlanResponse = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(counts(&i.db).await, (0, 1, 0, 0));
    assert!(first_plan.storage_estimate.upper_bound_bytes.is_some());
    let (status, replayed_plan) = post(
        &http,
        &i,
        &principal,
        "/api/v1/transform-plans",
        &plan_key,
        &body,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    assert_eq!(first, replayed_plan);
    let execute =
        json!({"plan_id":first_plan.plan_id,"plan_hash":first_plan.plan_hash,"approval_id":null});
    let (status, _) = post(
        &http,
        &i,
        &i.backend_principal,
        "/api/v1/transformations",
        &id(),
        &execute,
        &i.context,
    )
    .await;
    assert!(matches!(
        status,
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
    ));
    assert_eq!(counts(&i.db).await, (0, 1, 0, 0));
    negatives.push(json!({"case":"other_actor_confirmation","status":status.as_u16(),"no_jobs_or_resources":true}));
    let mut bad = execute.clone();
    bad["plan_hash"] = json!("0".repeat(64));
    let (status, _) = post(
        &http,
        &i,
        &principal,
        "/api/v1/transformations",
        &id(),
        &bad,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(counts(&i.db).await, (0, 1, 0, 0));
    negatives.push(json!({"case":"changed_plan_hash","status":409,"no_jobs_or_resources":true}));
    let activation_key = id();
    let (status, receipt) = post(
        &http,
        &i,
        &principal,
        "/api/v1/transformations",
        &activation_key,
        &execute,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "public activation: {receipt}"
    );
    let job = receipt["job_id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let state: Option<String> =
            sqlx::query_scalar("SELECT state FROM transformation_job WHERE id=?")
                .bind(&job)
                .fetch_optional(&i.db.pool)
                .await
                .unwrap();
        if state.as_deref() == Some("RUNNING") {
            break;
        }
        assert!(
            !matches!(
                state.as_deref(),
                Some("FAILED" | "FAILED_RECOVERABLE" | "COMPLETED")
            ),
            "job advanced before crash checkpoint: {state:?}"
        );
        assert!(
            Instant::now() < deadline,
            "public consumer did not claim job"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Progress relay: the reference worker emits TRANSFORM_ENCODING per
    // durable block before the contention window; GET /jobs/{id} observes it.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut progress_observations = Vec::new();
    loop {
        let (status, value) = get(&http, i.port, &principal, &format!("/api/v1/jobs/{job}")).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        if let Some(worker) = value["progress"]["worker"].as_object() {
            // El preflight observa el input sin bytes convertidos; la fase de
            // codificación avanza por bloques duraderos hasta el total.
            if worker["phase"] == "TRANSFORM_ENCODING"
                && worker["completed_bytes"].as_u64().is_some_and(|n| n > 0)
            {
                assert!(worker["total_bytes"].as_u64().is_some());
                progress_observations.push(json!(worker));
                if progress_observations.len() >= 2 {
                    break;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "el progreso del worker no llegó: {value}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Real backend ledger contention, outside Core. Holding it after RUNNING
    // creates an observable interruption window without a production test hook.
    let mut ledger = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(i.fixture.work.join("state/transform.sqlite"))
            .busy_timeout(Duration::from_secs(10)),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut ledger)
        .await
        .unwrap();
    let running = current(&i.db, &job).await;
    assert_eq!(running.state, TransformationState::Running);
    assert_eq!(counts(&i.db).await, (1, 1, 1, 4));
    i.daemon.stop();
    i.daemon = daemon_process::RunningDaemon::start(
        &i.data,
        &i.settings,
        i.port,
        output.join("restarted-daemon.log"),
    );
    i.daemon.wait_live(i.port, Duration::from_secs(45)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut recovery_observations = Vec::new();
    let recovered = loop {
        let row=sqlx::query("SELECT state,row_version,metadata,(SELECT count(*) FROM volume_reservation WHERE job_id=t.id) AS reservations,(SELECT count(*) FROM resource_lock WHERE owner_id=t.id) AS locks FROM transformation_job t WHERE id=?").bind(&job).fetch_one(&i.db.pool).await.unwrap();
        let metadata: Value =
            serde_json::from_str(row.get::<String, _>("metadata").as_str()).unwrap();
        let closed = metadata.get("attempt_closed").is_some_and(|v| !v.is_null());
        let res: i64 = row.get("reservations");
        let locks: i64 = row.get("locks");
        let state: String = row.get("state");
        recovery_observations.push(json!({"state":state,"version":row.get::<i64,_>("row_version"),"closed":closed,"reservations":res,"locks":locks}));
        if closed {
            assert_eq!((res, locks), (0, 0));
            break current(&i.db, &job).await;
        }
        assert_eq!((res, locks), (1, 4));
        assert!(
            Instant::now() < deadline,
            "daemon did not reconcile public job"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(recovered.state, TransformationState::FailedRecoverable);
    assert_eq!(
        recovered.error.as_ref().unwrap()["code"],
        "WORKER_INTERRUPTED"
    );
    let (status, recovered_api) =
        get(&http, i.port, &principal, &format!("/api/v1/jobs/{job}")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(recovered_api["state"], "FAILED");
    assert_eq!(
        recovered_api["progress"]["transformation_state"],
        "FAILED_RECOVERABLE"
    );
    sqlx::query("ROLLBACK").execute(&mut ledger).await.unwrap();
    drop(ledger);
    assert_eq!(
        std::fs::read(i.fixture.work.join("input/fixture.bin")).unwrap(),
        original
    );
    let (status, new_plan) = post(
        &http,
        &i,
        &principal,
        "/api/v1/transform-plans",
        &id(),
        &body,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "restart plan: {new_plan}"
    );
    assert_ne!(new_plan["plan_id"], first["plan_id"]);
    assert_ne!(new_plan["plan_hash"], first["plan_hash"]);
    let confirmation =
        json!({"plan_id":new_plan["plan_id"],"plan_hash":new_plan["plan_hash"],"approval_id":null});
    let (status, _) = restart(
        &http,
        &i,
        &principal,
        (&job, recovered.row_version - 1),
        &id(),
        &confirmation,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(counts(&i.db).await, (1, 1, 0, 0));
    negatives.push(json!({"case":"stale_restart_revision","status":409,"no_new_resources":true}));
    let restart_key = id();
    let (status, restarted) = restart(
        &http,
        &i,
        &principal,
        (&job, recovered.row_version),
        &restart_key,
        &confirmation,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "restart: {restarted}"
    );
    assert_eq!(restarted["job_id"], job);
    let deadline = Instant::now() + Duration::from_secs(180);
    let completed_api = loop {
        let (status, value) = get(&http, i.port, &principal, &format!("/api/v1/jobs/{job}")).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(value["state"], "FAILED", "public execution failed: {value}");
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "public execution did not finish: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let completed = current(&i.db, &job).await;
    assert_eq!(completed.state, TransformationState::Completed);
    assert_eq!(completed.attempt, 2);
    assert_eq!(counts(&i.db).await, (1, 2, 0, 0));
    assert_eq!(
        completed.metadata["attempt_history"][0]["error"],
        json!(recovered.error)
    );
    let record = completed.metadata["publication"].clone();
    assert_eq!(record["variant"]["id"], json!(completed.output_variant_id));
    let mut artifacts = Vec::new();
    for (index, loc) in record["locations"].as_array().unwrap().iter().enumerate() {
        let raw = std::fs::read(
            i.fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let file = output.join(format!("published-{index}.bin"));
        std::fs::write(&file, &raw).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw)),
            loc["sha256"].as_str().unwrap()
        );
        artifacts.push(json!({"file":file.strip_prefix(&i.repo).unwrap(),"location":loc,"artifact":record["artifacts"][index],"provenance":record["provenance"][index]}));
    }
    std::fs::write(
        output.join("publication-record.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    std::fs::copy(
        i.fixture.work.join("output/manifests").join(format!(
            "{}.json",
            completed.output_variant_id.as_ref().unwrap()
        )),
        output.join("published-manifest.json"),
    )
    .unwrap();
    let (status, _) = get(
        &http,
        i.port,
        &i.backend_principal,
        &format!("/api/v1/jobs/{job}"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    negatives.push(json!({"case":"other_actor_job_hidden","status":404}));
    // Exact restart replay must not touch the input or re-qualify a backend.
    // Wait only for owned pins to retire; never infer Core closure from it.
    let source = i.fixture.work.join("input/fixture.bin");
    let hidden = i.fixture.work.join("input/fixture-hidden.bin");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::rename(&source, &hidden).is_err() {
        assert!(Instant::now() < deadline, "input pins were not retired");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut drift = i.profile.clone();
    drift.configuration["timeout_seconds"] = json!(119);
    sqlx::query("UPDATE operation_journal SET payload=?,plan_hash=? WHERE id=?")
        .bind(serde_json::to_string(&drift).unwrap())
        .bind(larm_domain::conformance::hash(&drift).unwrap())
        .bind(&drift.id)
        .execute(&i.db.pool)
        .await
        .unwrap();
    let (status, replay) = restart(
        &http,
        &i,
        &principal,
        (&job, recovered.row_version),
        &restart_key,
        &confirmation,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "replay after backend/input change: {replay}"
    );
    assert_eq!(restarted, replay);
    let unchanged = current(&i.db, &job).await;
    assert_eq!(json!(completed), json!(unchanged));
    sqlx::query("UPDATE operation_journal SET payload=?,plan_hash=? WHERE id=?")
        .bind(serde_json::to_string(&i.profile).unwrap())
        .bind(larm_domain::conformance::hash(&i.profile).unwrap())
        .bind(&i.profile.id)
        .execute(&i.db.pool)
        .await
        .unwrap();
    std::fs::rename(&hidden, &source).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let events: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&job)
    .fetch_all(&i.db.pool)
    .await
    .unwrap();
    let outbox:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)").bind(&job).fetch_one(&i.db.pool).await.unwrap();
    assert_eq!(outbox, events.len() as i64);
    let delivery:i64=sqlx::query_scalar("SELECT count(*) FROM operation_journal WHERE kind='transformation.dispatch' AND json_extract(payload,'$.job_id')=?").bind(&job).fetch_one(&i.db.pool).await.unwrap();
    assert_eq!(delivery, 2);
    assert!(principal.revoke());
    assert!(i.backend_principal.revoke());
    i.daemon.stop();
    let result = json!({"status":"PASS","scope":"Public HTTP producer/consumer with real qualified native reference worker, automatic API crash reconciliation, explicit same-job restart under a new public plan and managed publication; catalogue seed and private-ledger contention are explicit fixtures",
        "request":request,"first_plan":first,"first_receipt":receipt,"running":running,"recovered":recovered,"recovered_api":recovered_api,"new_plan":new_plan,"restart_receipt":restarted,"completed":completed,"completed_api":completed_api,"record":record,"artifacts":artifacts,"events":events,"outbox_count":outbox,"recovery_observations":recovery_observations,"negatives":negatives,"counts_final":counts(&i.db).await,"dispatch_records":delivery,"worker_progress":progress_observations,"progress_relayed":true,
        "installation":i.installation,"run":i.run,"profile":i.profile,"backend_actor":i.context.actor_id,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"public_plans_not_test_mapped":true,"public_admission_not_test_direct":true,"native_consumer_published":true,"restart_replay_with_missing_input_and_changed_profile":true,"original_intact":true,"principals_revoked":true,"runtime_acceptance":false,"partial_conversion_crash_accepted":false});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    i.db.pool.close().await;
    result
}
