//! Aceptación pública del conversor GGUF de producto: cualificación P026 real
//! con la suite GGUF del host, plan/confirmación públicas con adapter_id
//! product.transformation.gguf, ejecución con progreso GGUF_TENSOR y
//! publicación CAS del contenedor verificado con el oráculo GGUF en Rust
//! (bytes de tensor idénticos al safetensors fuente). El catálogo inicial es
//! fixture declarada; sin cuantización real de modelos ni aceptación de UI.
use super::public_case::Inputs;
use super::*;
use larm_adapter_host::conformance::gguf::{parse_gguf, parse_safetensors};
use larm_domain::transformations::{TransformationPlanResponse, TransformationState};

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
    port: u16,
    installation: AdapterInstallation,
    run: Run,
    profile: ConformanceProfile,
    fixture: Fixture,
) -> Value {
    let output = output.join("public-gguf");
    std::fs::create_dir(&output).unwrap();
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: id(),
    };
    let request = serde_json::json!({
        "input_variant_id": fixture.data["profile"]["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {},
        "storage_location_id": fixture.data["profile"]["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let typed: larm_domain::transformations::TransformationRequest =
        serde_json::from_value(request.clone()).unwrap();
    let original =
        registered_case::seed_public_input_named(&db, &fixture, &typed, "fixture.safetensors")
            .await;
    std::fs::write(output.join("input.bin"), &original).unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap();
    let mut negatives = Vec::new();
    let (status, missing) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &id(),
        &json!({"adapter_id":"missing.gguf","tool_version":"1.1.0","target_format":"GGUF","parameters":{},"input_variant_id":request["input_variant_id"],"storage_location_id":request["storage_location_id"],"hard_budget_bytes":null}),
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "unknown: {missing}");
    negatives.push(json!({"case":"unknown_adapter","status":404}));

    let plan_key = id();
    let (status, first) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan_key,
        &request,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan: {first}");
    let plan: TransformationPlanResponse = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(counts(&db).await, (0, 1, 0, 0));
    let (status, replayed) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan_key,
        &request,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    assert_eq!(first, replayed, "replay del plan");
    negatives.push(json!({"case":"plan_replay","status":201}));

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
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut tensor_progress = 0usize;
    let completed_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión GGUF falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        if value["progress"]["worker"].is_object() {
            tensor_progress += 1;
        }
        assert!(Instant::now() < deadline, "sin desenlace: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completed_api["progress"]["transformation_state"],
        "COMPLETED"
    );
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
    let record = completed.metadata["publication"].clone();
    assert_eq!(record["variant"]["id"], json!(completed.output_variant_id));
    let mut artifacts = Vec::new();
    for (index, loc) in record["locations"].as_array().unwrap().iter().enumerate() {
        let raw = std::fs::read(
            fixture
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
        // El blob CAS del artefacto DATA es el contenedor GGUF publicado.
        let worker_role = record["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        // Oráculo GGUF en Rust: el contenedor publicado se parsea y sus
        // tensores se contrastan con el safetensors fuente.
        let source = parse_safetensors(&original).unwrap();
        let container = parse_gguf(&raw).unwrap();
        assert_eq!(container.version, 3);
        assert_eq!(container.alignment, 32);
        assert_eq!(container.tensors.len(), source.tensors.len());
        for tensor in &container.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            assert_eq!(
                tensor.dims,
                source_tensor
                    .shape
                    .iter()
                    .rev()
                    .copied()
                    .collect::<Vec<_>>()
            );
            assert_eq!(tensor.ggml_type, source_tensor.ggml_type);
            let begin = container.data_begin + tensor.offset as usize;
            let end = begin + source_tensor.length;
            assert_eq!(
                raw[begin..end],
                original[source.data_start + source_tensor.source_offset
                    ..source.data_start + source_tensor.source_offset + source_tensor.length],
                "bytes del tensor {} alterados",
                tensor.name
            );
        }
        // La conversión verbatim no lleva cuantización: el campo queda null.
        assert_eq!(
            record["variant"]["quantization"],
            json!(null),
            "la variante verbatim no debe declarar cuantización"
        );
        artifacts.push(json!({"file":file.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container.tensors.len()}));
    }
    assert!(
        !artifacts.is_empty(),
        "el .gguf no se publicó; roles={:?}",
        record["measured"]["artifacts"]
            .as_array()
            .map(|items| items
                .iter()
                .map(|item| item["worker_role"].clone())
                .collect::<Vec<_>>())
            .unwrap_or_default()
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
    let result = json!({"status":"PASS","scope":"Public GGUF product converter: real P026 qualification with the host GGUF suite, public plan/confirm/execute with adapter_id product.transformation.gguf, GGUF_TENSOR progress, CAS publication of the container verified by the Rust GGUF oracle against the source safetensors; seeded catalogue is an explicit fixture","request":request,"plan":first,"receipt":receipt,"completed_api":completed_api,"completed":completed,"record":record,"artifacts":artifacts,"negatives":negatives,"events":events,"outbox_count":outbox,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"gguf_tensor_progress_observations":tensor_progress,"runtime_acceptance":false,"quantization_acceptance":false});
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
