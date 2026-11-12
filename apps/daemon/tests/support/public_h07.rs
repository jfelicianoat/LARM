//! Dedicated public H07 acceptance: real daemon crash with committed worker
//! blocks (partial conversion bytes in the worker's private staging), the
//! same-job restart running a fresh non-resumable worker job whose namespace
//! is bound to its own id, and CAS publication of the complete conversion.
//! The private reference ledger is an explicit declared fixture; no Core
//! mapping/admission from the driver.
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
        backend_principal: _,
        daemon,
        db,
        context,
        repo,
        output,
        data,
        settings,
        port,
        installation,
        run,
        profile,
        fixture,
    } = input;
    tokio::spawn(Box::pin(exercise(
        job_principal,
        daemon,
        db,
        context,
        repo,
        output,
        data,
        settings,
        port,
        installation,
        run,
        profile,
        fixture,
    )))
}

#[allow(clippy::too_many_arguments)]
async fn exercise(
    mut principal: download_restart::TemporaryPrincipal,
    mut daemon: daemon_process::RunningDaemon,
    db: larm_db::pool::Db,
    _context: MutationContext,
    _repo: PathBuf,
    output: PathBuf,
    data: PathBuf,
    settings: PathBuf,
    port: u16,
    installation: AdapterInstallation,
    run: Run,
    profile: ConformanceProfile,
    fixture: Fixture,
) -> Value {
    let output = output.join("public-h07");
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

    // Plan y confirmación del trabajo que caerá con bytes parciales.
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
            "job advanced before the partial-bytes window: {state:?}"
        );
        assert!(Instant::now() < deadline, "job did not reach RUNNING");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Bytes parciales reales: el worker compromete bloques duraderos y el
    // relé de progreso los observa antes de la caída.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut partial_bytes = 0_u64;
    loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        if let Some(worker) = value["progress"]["worker"].as_object() {
            if worker["phase"] == "TRANSFORM_ENCODING" {
                let completed = worker["completed_bytes"].as_u64().unwrap_or(0);
                let total = worker["total_bytes"].as_u64().unwrap_or(0);
                assert!(total > 0);
                partial_bytes = partial_bytes.max(completed);
                if completed >= 3 * 512 * 1024 {
                    break;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "el worker no comprometió bytes parciales observables"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // El ledger privado declarado da los oráculos worker-side del intento 1.
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
    let (worker_job, worker_attempt): (String, i64) = sqlx::query_as(
        "SELECT id,attempt FROM ref_job WHERE phase IN ('RUNNING','VERIFYING') ORDER BY rowid DESC LIMIT 1",
    )
    .fetch_one(&mut ledger)
    .await
    .unwrap();
    let (_checkpoint_offset,): (i64,) =
        sqlx::query_as("SELECT offset FROM ref_checkpoint WHERE job_id=? AND attempt=?")
            .bind(&worker_job)
            .bind(worker_attempt)
            .fetch_one(&mut ledger)
            .await
            .unwrap();
    let journal_payload: String =
        sqlx::query_scalar("SELECT payload FROM ref_journal WHERE job_id=? AND attempt=?")
            .bind(&worker_job)
            .bind(worker_attempt)
            .fetch_one(&mut ledger)
            .await
            .unwrap();
    let paths: Value = serde_json::from_str(&journal_payload).unwrap();
    let attempt1_staging = paths["staging"].as_str().unwrap().to_owned();
    let attempt1_namespace = paths["output"].as_str().unwrap().to_owned();
    // Los bloques duraderos viven en el STAGING del intento; el namespace
    // output/ sólo se crea al verificar. La conversión escribe el Base64 con
    // fsync por bloque: el fichero parcial existe con 0 < size < total.
    let staging_root = fixture.work.join("state");
    let attempt1_dir = staging_root.join(&attempt1_staging);
    assert!(
        attempt1_dir.is_dir(),
        "falta el staging {attempt1_staging}; work={:?}",
        std::fs::read_dir(&fixture.work)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    );
    let mut attempt1_files = Vec::new();
    for entry in std::fs::read_dir(&attempt1_dir).unwrap().flatten() {
        let metadata = entry.metadata().unwrap();
        if metadata.is_file() {
            attempt1_files.push(json!({
                "file": entry.file_name().to_string_lossy(),
                "size_bytes": metadata.len(),
            }));
        }
    }
    assert!(
        attempt1_files
            .iter()
            .any(|file| file["size_bytes"].as_u64().unwrap_or(0) > 0),
        "el intento 1 no dejó bytes provisionales: {:?}",
        attempt1_files
    );
    let output_root = fixture.work.join("output");
    sqlx::query("ROLLBACK").execute(&mut ledger).await.unwrap();
    drop(ledger);

    // Caída real del daemon con el worker dentro de la conversión.
    daemon.stop();
    daemon = daemon_process::RunningDaemon::start(
        &data,
        &settings,
        port,
        output.join("restarted-daemon.log"),
    );
    daemon.wait_live(port, Duration::from_secs(45)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    // Revisión y cierre observados en la MISMA fila: la revisión del If-Match
    // nunca puede quedarse atrás del cierre observado.
    let (recovered_version, recovered_error) = loop {
        let row: Option<(String, Option<String>, i64, Option<String>)> = sqlx::query_as(
            "SELECT state,json_extract(metadata,'$.attempt_closed'),row_version,error FROM transformation_job WHERE id=?",
        )
        .bind(&job)
        .fetch_optional(&db.pool)
        .await
        .unwrap();
        let (state, closed, version, error) =
            row.expect("el trabajo desapareció durante la reconciliación");
        if state == "FAILED_RECOVERABLE" && closed.is_some() {
            // El cierre del intento (WORKER_INTERRUPTED, recursos liberados)
            // ya es duradero en esta misma revisión.
            break (version, error.expect("cierre sin causa"));
        }
        assert!(
            !matches!(state.as_str(), "COMPLETED" | "CANCELLED"),
            "el trabajo avanzó sin reconciliarse: {state}"
        );
        assert!(
            Instant::now() < deadline,
            "sin reconciliación tras la caída"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let recovered_error: Value = serde_json::from_str(&recovered_error).unwrap();
    assert_eq!(recovered_error["code"], "WORKER_INTERRUPTED");

    // Reinicio del MISMO trabajo con plan nuevo; el worker reutiliza su job
    // (namespace {id}-{intento+1}) y la publicación CAS sale del intento 2.
    let (status, new_plan) = download_restart::post_json(
        &http,
        port,
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
    assert_ne!(new_plan["plan_id"], plan.plan_id);
    let confirmation =
        json!({"plan_id":new_plan["plan_id"],"plan_hash":new_plan["plan_hash"],"approval_id":null});
    let restart_key = id();
    let reply = http
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/transformations/{job}/restart"
        ))
        .bearer_auth(&principal.token)
        .header("Idempotency-Key", &restart_key)
        .header("If-Match", recovered_version.to_string())
        .header("X-Correlation-Id", &ctx.correlation_id)
        .json(&confirmation)
        .send()
        .await
        .unwrap();
    let status = reply.status();
    let restarted: Value = reply.json().await.unwrap();
    if status != reqwest::StatusCode::ACCEPTED {
        let current: (i64, String, i64) =
            sqlx::query_as("SELECT row_version,state,attempt FROM transformation_job WHERE id=?")
                .bind(&job)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let events: Vec<(i64, String)> = sqlx::query_as(
            "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
        )
        .bind(&job)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        panic!(
            "restart: {restarted}; if_match={}; current={current:?}; events={events:?}",
            recovered_version
        );
    }
    assert_eq!(restarted["job_id"], json!(job));
    let deadline = Instant::now() + Duration::from_secs(240);
    let completed_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job}"),
        )
        .await;
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
    let completed = SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed.state, TransformationState::Completed);
    assert_eq!(completed.attempt, 2);
    assert_eq!(counts(&db).await, (1, 2, 0, 0));
    assert_eq!(
        completed.metadata["attempt_history"][0]["error"],
        json!(recovered_error)
    );

    // Oráculo H07 (contrato 11: la herramienta no reanudable no retoma
    // salida incompleta): el reinicio ejecuta un job worker NUEVO cuyo
    // namespace es ajeno al provisional del intento interrumpido, y los
    // bytes publicados son la recodificación COMPLETA del original.
    let record = completed.metadata["publication"].clone();
    let mut namespaces = std::collections::BTreeSet::new();
    let mut worker_job_ids = std::collections::BTreeSet::new();
    for (index, loc) in record["locations"].as_array().unwrap().iter().enumerate() {
        let raw = std::fs::read(output_root.join(loc["relative_path"].as_str().unwrap())).unwrap();
        let file = output.join(format!("published-{index}.bin"));
        std::fs::write(&file, &raw).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw)),
            loc["sha256"].as_str().unwrap()
        );
        if record["artifacts"][index]["role"] == "DATA" {
            use base64::Engine as _;
            let expected = base64::engine::general_purpose::STANDARD.encode(&original);
            assert_eq!(
                raw,
                expected.as_bytes(),
                "los bytes publicados no son la conversión completa del original"
            );
        }
        if record["measured"]["artifacts"][index]["worker_role"] == "PROVENANCE" {
            let lineage: Value = serde_json::from_slice(&raw).unwrap();
            worker_job_ids.insert(lineage["job_id"].as_str().unwrap().to_owned());
        }
        let worker_path = record["measured"]["artifacts"][index]["worker_relative_path"]
            .as_str()
            .unwrap_or_default();
        let first = worker_path.split('/').next().unwrap_or("").to_owned();
        assert!(
            !first.is_empty(),
            "artefacto sin namespace worker: {worker_path}"
        );
        namespaces.insert(first);
    }
    assert_eq!(
        worker_job_ids.len(),
        1,
        "procedencia de jobs worker mezclados"
    );
    let restarted_worker_job = worker_job_ids.iter().next().unwrap().clone();
    assert_ne!(
        restarted_worker_job, worker_job,
        "el reinicio reutilizó el job del worker interrumpido"
    );
    assert_eq!(
        namespaces,
        std::collections::BTreeSet::from([format!("{restarted_worker_job}-1")]),
        "la publicación no sale del namespace del job worker nuevo: {namespaces:?}"
    );
    let events: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&job)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)").bind(&job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(outbox, events.len() as i64);
    let dispatch: i64 = sqlx::query_scalar("SELECT count(*) FROM operation_journal WHERE kind='transformation.dispatch' AND json_extract(payload,'$.job_id')=?").bind(&job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(dispatch, 2, "reinicio añade exactamente otra señal");
    assert_eq!(
        std::fs::read(fixture.work.join("input/fixture.bin")).unwrap(),
        original
    );
    let result = json!({"status":"PASS","scope":"Dedicated public H07: real daemon crash with committed worker blocks (partial conversion bytes observed through the progress relay and the declared private ledger), automatic reconciliation with WORKER_INTERRUPTED, same-job restart whose fresh non-resumable worker job publishes the complete conversion by CAS from a namespace bound to its own id, without resuming the interrupted provisional output; seeded catalogue and private ledger are explicit fixtures","request":request,"plan":plan,"receipt":receipt,"partial_bytes_observed":partial_bytes,"worker_job":worker_job,"restarted_worker_job":restarted_worker_job,"worker_attempt":worker_attempt,"attempt1_namespace":attempt1_namespace,"attempt1_files":attempt1_files,"recovered_error":recovered_error,"recovered_version":recovered_version,"new_plan":new_plan,"restart_receipt":restarted,"completed_api":completed_api,"completed":completed,"record":record,"published_namespaces":namespaces,"events":events,"outbox_count":outbox,"dispatch_records":dispatch,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"runtime_acceptance":false});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    assert!(principal.revoke());
    daemon.stop();
    db.pool.close().await;
    result
}
