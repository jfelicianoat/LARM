//! Public cooperative cancellation: real P026 qualification, RUNNING attempt
//! with the private ledger held as an observable window, operator cancel by
//! id, witness-closed CANCELLED terminal release and queued cancellation.
//! No Core mapping/admission from the driver.
use super::public_case::Inputs;
use super::*;
use larm_db::transformations::SqlTransformationJobs;
use larm_domain::{
    conformance::Run,
    conformance_profiles::ConformanceProfile,
    transformations::{TransformationPlanResponse, TransformationState},
};
use sqlx::Connection;

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}

async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM model_variant),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)").fetch_one(&db.pool).await.unwrap()
}

async fn get(
    http: &reqwest::Client,
    port: u16,
    token: &str,
    path: &str,
) -> (reqwest::StatusCode, Value) {
    let response = http
        .get(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

async fn cancel(
    http: &reqwest::Client,
    port: u16,
    token: &str,
    job: &str,
) -> (reqwest::StatusCode, Value) {
    let reply = http
        .post(format!("http://127.0.0.1:{port}/api/v1/jobs/{job}/cancel"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = reply.status();
    (status, reply.json().await.unwrap())
}

async fn job_state(db: &larm_db::pool::Db, job: &str) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM transformation_job WHERE id=?")
        .bind(job)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
}

pub(super) fn start(
    input: Inputs,
    job_principal: download_restart::TemporaryPrincipal,
) -> tokio::task::JoinHandle<Value> {
    let Inputs {
        backend_principal,
        daemon,
        db,
        context,
        repo,
        output,
        data: _,
        settings: _,
        port,
        installation,
        run,
        profile,
        fixture,
    } = input;
    tokio::spawn(Box::pin(exercise(
        backend_principal,
        job_principal,
        daemon,
        db,
        context,
        repo,
        output,
        port,
        installation,
        run,
        profile,
        fixture,
    )))
}

#[allow(clippy::too_many_arguments)]
async fn exercise(
    mut backend_principal: download_restart::TemporaryPrincipal,
    mut principal: download_restart::TemporaryPrincipal,
    mut daemon: daemon_process::RunningDaemon,
    db: larm_db::pool::Db,
    context: MutationContext,
    _repo: PathBuf,
    output: PathBuf,
    port: u16,
    installation: AdapterInstallation,
    run: Run,
    profile: ConformanceProfile,
    fixture: Fixture,
) -> Value {
    let output = output.join("public-cancel");
    std::fs::create_dir(&output).unwrap();
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: id(),
    };
    let request = TransformationRequest {
        input_variant_id: fixture.data["profile"]["input_variant_id"]
            .as_str()
            .unwrap()
            .into(),
        adapter_id: installation.adapter_id.clone(),
        tool_version: "1.0.0".into(),
        target_format: "BASE64".into(),
        parameters: json!({}),
        storage_location_id: fixture.data["profile"]["storage_location_id"]
            .as_str()
            .unwrap()
            .into(),
        hard_budget_bytes: None,
    };
    let original = Box::pin(registered_case::seed_public_input(&db, &fixture, &request)).await;
    std::fs::write(output.join("input.bin"), &original).unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap();
    let body = json!(request);
    let mut negatives = Vec::new();
    // La cancelación oculta los trabajos ajenos y los desconocidos.
    let foreign = cancel(&http, port, &backend_principal.token, &id()).await;
    assert_eq!(
        foreign.0,
        reqwest::StatusCode::NOT_FOUND,
        "foreign: {}",
        foreign.1
    );
    negatives.push(json!({"case":"unknown_job_cancel","status":404}));
    let foreign = cancel(
        &http,
        port,
        &backend_principal.token,
        &uuid::Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(foreign.0, reqwest::StatusCode::NOT_FOUND);
    negatives.push(json!({"case":"other_actor_hidden","status":404}));

    // Plan y confirmación del trabajo que se cancelará en ejecución.
    let (status, plan) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &id(),
        &body,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan: {plan}");
    let plan: TransformationPlanResponse = serde_json::from_value(plan).unwrap();
    let confirmation =
        json!({"plan_id":plan.plan_id,"plan_hash":plan.plan_hash,"approval_id":null});
    let (status, receipt) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmation,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation: {receipt}"
    );
    let job = receipt["job_id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let state = job_state(&db, &job).await;
        if state.as_deref() == Some("RUNNING") {
            break;
        }
        assert!(
            !matches!(
                state.as_deref(),
                Some("FAILED" | "FAILED_RECOVERABLE" | "COMPLETED" | "CANCELLED")
            ),
            "job advanced before the cancellation window: {state:?}"
        );
        assert!(Instant::now() < deadline, "job did not reach RUNNING");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Ventana observable: el ledger privado del worker retiene la conversión
    // mientras el operador pide la cancelación por API. Sin gancho de prueba.
    let mut ledger = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("state/transform.sqlite"))
            .busy_timeout(Duration::from_secs(10)),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut ledger)
        .await
        .unwrap();
    let (status, receipt) = cancel(&http, port, &principal.token, &job).await;
    assert_eq!(status, reqwest::StatusCode::OK, "cancel: {receipt}");
    assert_eq!(receipt["job_id"], json!(job));
    negatives.push(json!({"case":"running_cancel_requested","status":200}));
    let (status, job_api) = get(
        &http,
        port,
        &principal.token,
        &format!("/api/v1/jobs/{job}"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(job_api["state"], "RUNNING", "aún en cancelación: {job_api}");
    let deadline = Instant::now() + Duration::from_secs(240);
    let cancelled_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        if value["state"] == "CANCELLED" {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "la cancelación no cerró: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        cancelled_api["progress"]["transformation_state"],
        "CANCELLED"
    );
    sqlx::query("ROLLBACK").execute(&mut ledger).await.unwrap();
    drop(ledger);
    let cancelled = SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(cancelled.state, TransformationState::Cancelled);
    assert_eq!(cancelled.attempt, 1);
    assert_eq!(counts(&db).await, (1, 1, 0, 0));
    assert_eq!(
        cancelled.metadata["attempt_closed"]["state"],
        json!("CANCELLED")
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=?")
        .bind(&job)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)")
        .bind(&job)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, outbox, "eventos sin outbox");
    let dispatch: i64 = sqlx::query_scalar("SELECT count(*) FROM operation_journal WHERE kind='transformation.dispatch' AND json_extract(payload,'$.job_id')=?").bind(&job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(dispatch, 1, "la cancelación no añade señales de despacho");
    // La cancelación de un terminal se rechaza; el original sigue intacto.
    let (status, again) = cancel(&http, port, &principal.token, &job).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "replay: {again}");
    negatives.push(json!({"case":"terminal_cancel_conflict","status":409}));
    assert_eq!(
        std::fs::read(fixture.work.join("input/fixture.bin")).unwrap(),
        original
    );

    // Segundo escenario: cancelación en cola, antes de que el consumidor
    // reclame. El alta y la liberación son atómicas en la señal. El permiso
    // del motor puede seguir retenido por el cierre anterior: el 409
    // transformation_backend_busy es reintentable por contrato.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (_status, second_plan) = loop {
        let attempt = download_restart::post_json(
            &http,
            port,
            &principal,
            "/api/v1/transform-plans",
            &id(),
            &body,
            &ctx,
        )
        .await;
        match attempt.0 {
            reqwest::StatusCode::CREATED => break attempt,
            reqwest::StatusCode::CONFLICT
                if attempt.1["details"]["reason"] == "transformation_backend_busy"
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            _ => panic!("plan2: {}", attempt.1),
        }
    };
    let confirmation = json!({"plan_id":second_plan["plan_id"],"plan_hash":second_plan["plan_hash"],"approval_id":null});
    let (_status, second_receipt) = loop {
        let attempt = download_restart::post_json(
            &http,
            port,
            &principal,
            "/api/v1/transformations",
            &id(),
            &confirmation,
            &ctx,
        )
        .await;
        match attempt.0 {
            reqwest::StatusCode::ACCEPTED => break attempt,
            reqwest::StatusCode::CONFLICT
                if attempt.1["details"]["reason"] == "transformation_backend_busy"
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            _ => panic!("activation2: {}", attempt.1),
        }
    };
    let second = second_receipt["job_id"].as_str().unwrap().to_owned();
    let (status, queued_receipt) = cancel(&http, port, &principal.token, &second).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "queued cancel: {queued_receipt}"
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let state = job_state(&db, &second).await;
        if state.as_deref() == Some("CANCELLED") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "la cancelación en cola no cerró: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(counts(&db).await, (2, 1, 0, 0));
    negatives.push(json!({"case":"queued_cancel_released","status":200}));
    let result = json!({"status":"PASS","scope":"Public cooperative cancellation by id: real P026 qualification, RUNNING attempt with the private ledger held as the observable window, operator cancel through the public API, witness-closed CANCELLED terminal with full resource release, queued cancellation released atomically; catalogue seed and private-ledger contention are explicit fixtures","job":cancelled,"job_api":cancelled_api,"queued_receipt":queued_receipt,"second_job":second,"negatives":negatives,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"backend_actor":context.actor_id,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"runtime_acceptance":false});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    assert!(principal.revoke());
    assert!(backend_principal.revoke());
    daemon.stop();
    db.pool.close().await;
    result
}
