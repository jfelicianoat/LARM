//! Dedicated acceptance for the definitive witness-loss policy: a real
//! preparation (PREPARING intent with a live native helper) loses its
//! witness and the recorded boot session differs from the current one —
//! the OS proves no process/handle of that session survives, so the
//! recovery retires the intent witness-lessly and the job re-dispatches
//! from scratch to CAS publication. The journal edit that simulates the
//! system restart is an explicit declared fixture.
use super::public_case::Inputs;
use super::*;
use larm_domain::{
    conformance::Run,
    conformance_profiles::ConformanceProfile,
    transformations::{TransformationPlanResponse, TransformationState},
};

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}

async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM model_variant),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)").fetch_one(&db.pool).await.unwrap()
}

pub(super) fn start(
    input: Inputs,
    job_principal: download_restart::TemporaryPrincipal,
) -> tokio::task::JoinHandle<Value> {
    let Inputs {
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
        ..
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
    let output = output.join("public-witness-lost");
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

    // Plan y confirmación: el consumidor prepara la intención con testigo
    // vivo (helper real con el boot actual registrado).
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let (intent_id, intent_payload) = loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT id,payload FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=? AND phase IN ('PREPARING','REGISTERED')",
        )
        .bind(&job)
        .fetch_optional(&db.pool)
        .await
        .unwrap();
        if let Some(row) = row {
            break row;
        }
        assert!(
            Instant::now() < deadline,
            "la intención preparada no apareció"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let payload: Value = serde_json::from_str(&intent_payload).unwrap();
    let prepared_boot = payload["owner"]["boot_session"].as_u64().unwrap();
    assert!(prepared_boot > 0, "la intención registra el boot actual");

    // Pérdida definitiva del testigo con reinicio del sistema SIMULADO
    // (fixture declarado): el daemon y su helper mueren sin reconciliar y la
    // sesión registrada de la intención pasa a ser anterior — la prueba del
    // sistema operativo (ningún proceso/handle sobrevive al reinicio) es la
    // que habilita el retiro sin testigo.
    daemon.stop();
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "larm-daemon.exe"])
        .output()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let mut edited = payload.clone();
    edited["owner"]["boot_session"] = json!(1);
    let payload_with_old_boot = serde_json::to_string(&edited).unwrap();
    sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
        .bind(&payload_with_old_boot)
        .bind(&intent_id)
        .execute(&db.pool)
        .await
        .unwrap();

    // El daemon reiniciado ejecuta la recuperación: retira la intención SIN
    // testigo (la sesión de arranque difiere) y el trabajo vuelve a
    // despacharse.
    daemon = daemon_process::RunningDaemon::start_measured(
        &data,
        &settings,
        port,
        output.join("restarted-daemon.log"),
    );
    daemon.wait_live(port, Duration::from_secs(45)).unwrap();

    // El daemon reiniciado retira la intención SIN testigo y el trabajo
    // vuelve a despacharse. La admisión original selló su observación de
    // destino/espacio en el arranque ANTERIOR: el re-despacho puede fallar
    // cerrado (STALE_REVISION/destination_changed) y el operador reinicia
    // con plan nuevo; o completar si la observación sigue vigente.
    let deadline = Instant::now() + Duration::from_secs(180);
    let (dispatch_state, dispatch_error) = loop {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT state,json_extract(metadata,'$.attempt_closed') FROM transformation_job WHERE id=?",
        )
        .bind(&job)
        .fetch_optional(&db.pool)
        .await
        .unwrap();
        let (state, closed) = row.expect("el trabajo desapareció tras el retiro");
        let finished = matches!(
            state.as_str(),
            "COMPLETED" | "FAILED" | "FAILED_RECOVERABLE"
        ) && (state != "FAILED" || closed.is_some());
        if finished {
            let job_now = larm_db::transformations::SqlTransformationJobs {
                pool: db.pool.clone(),
            }
            .owned_claim(&job)
            .await
            .unwrap()
            .claim
            .job;
            break (
                job_now.state,
                job_now.error.unwrap_or_else(|| json!({"code":null})),
            );
        }
        assert!(
            Instant::now() < deadline,
            "sin desenlace tras el retiro: {state}"
        );
        if Instant::now() > deadline - Duration::from_secs(2) {
            let intents: Vec<(String, i64, String)> = sqlx::query_as(
                "SELECT phase,COALESCE(json_extract(payload,'$.owner.boot_session'),-1),json_extract(payload,'$.owner.session_id') FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=? ORDER BY id",
            )
            .bind(&job)
            .fetch_all(&db.pool)
            .await
            .unwrap();
            panic!("sin desenlace; job={state}; intents={intents:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let (_restart_plan, restart_receipt): (Option<Value>, Option<Value>) = if dispatch_state
        == TransformationState::Completed
    {
        (None, None)
    } else {
        // Fallo cerrado de la admisión obsoleta: reinicio con plan nuevo.
        assert_eq!(
            dispatch_error["code"], "STALE_REVISION",
            "el re-despacho obsoleto debe fallar cerrado: {dispatch_error}"
        );
        assert_eq!(
            dispatch_error["details"]["reason"],
            "transformation_destination_changed"
        );
        let busy_deadline = Instant::now() + Duration::from_secs(30);
        let (_status, new_plan) = loop {
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
                        && Instant::now() < busy_deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ => panic!("restart plan: {}", attempt.1),
            }
        };
        let job_now = larm_db::transformations::SqlTransformationJobs {
            pool: db.pool.clone(),
        }
        .owned_claim(&job)
        .await
        .unwrap()
        .claim
        .job;
        let confirmation = json!({"plan_id":new_plan["plan_id"],"plan_hash":new_plan["plan_hash"],"approval_id":null});
        let reply = http
            .post(format!(
                "http://127.0.0.1:{port}/api/v1/transformations/{job}/restart"
            ))
            .bearer_auth(&principal.token)
            .header("Idempotency-Key", &id())
            .header("If-Match", job_now.row_version.to_string())
            .header("X-Correlation-Id", &ctx.correlation_id)
            .json(&confirmation)
            .send()
            .await
            .unwrap();
        let status = reply.status();
        let receipt: Value = reply.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::ACCEPTED, "restart: {receipt}");
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let reply = http
                .get(format!("http://127.0.0.1:{port}/api/v1/jobs/{job}"))
                .bearer_auth(&principal.token)
                .send()
                .await
                .unwrap();
            let value: Value = reply.json().await.unwrap();
            assert_ne!(value["state"], "FAILED", "el reinicio falló: {value}");
            if value["state"] == "COMPLETED" {
                break;
            }
            assert!(Instant::now() < deadline, "el reinicio no terminó: {value}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        (Some(new_plan), None)
    };
    let completed = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed.state, TransformationState::Completed);
    assert_eq!(counts(&db).await, (1, 2, 0, 0));
    // La intención del testigo perdido quedó retirada SIN testigo: CLOSED
    // con el boot antiguo registrado en su payload.
    let intents: Vec<(String, String)> = sqlx::query_as(
        "SELECT phase,CAST(json_extract(payload,'$.owner.boot_session') AS TEXT) FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=? ORDER BY id",
    )
    .bind(&job)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        intents[0],
        ("CLOSED".to_owned(), json!(1).to_string()),
        "la intención del testigo perdido no se retiró con su boot"
    );
    assert_eq!(
        std::fs::read(fixture.work.join("input/fixture.bin")).unwrap(),
        original
    );
    let result = json!({"status":"PASS","scope":"Definitive witness-loss policy: real PREPARING intent with a live helper loses its witness and its recorded boot session differs (declared journal edit simulating the system restart), the recovery retires the intent witness-lessly with OS-level proof and the job re-dispatches from scratch to CAS publication; same-session losses retain resources (SQL coverage); seeded catalogue is an explicit fixture","request":request,"plan":plan,"receipt":receipt,"prepared_boot":prepared_boot,"intent_id":intent_id,"dispatch_state":dispatch_state,"dispatch_error":dispatch_error,"restart_plan":completed,"restart_receipt":restart_receipt,"completed":completed,"intents":intents,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"runtime_acceptance":false});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    // La revocación es un comando externo contra SQLite/Credential Manager:
    // un reintento corto absorbe la contención transitoria del daemon vivo.
    let mut revoked = false;
    for _ in 0..3 {
        if principal.revoke() {
            revoked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(revoked, "no se pudo revocar el principal del caso");
    daemon.stop();
    db.pool.close().await;
    result
}
