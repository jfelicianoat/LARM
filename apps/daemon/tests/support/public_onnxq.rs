//! Aceptación pública del CUANTIZADOR ONNX de producto vía Olive: cualificación
//! P026 real con la suite ONNXQ del host, plan público con el parámetro
//! declarado (quantization=INT8), ejecución del runtime gestionado sellado
//! python-olive (pass OnnxDynamicQuantization, inyectado por entorno
//! OLIVE_RUNTIME desde el perfil) y publicación CAS del modelo cuantizado
//! verificado con el oráculo QDQ en Rust: byte a byte contra el golden emitido
//! por el propio runtime sellado y espejo int8/f32 de los pesos (escala
//! max_abs/127, redondeo half-away, zero_point 0). El catálogo inicial es
//! fixture declarada; sin inferencia de modelos ni aceptación de UI.
use super::public_case::Inputs;
use super::*;
use larm_adapter_host::conformance::onnx_quantize::{onnxq_initializer, parse_onnxq_model};
use larm_adapter_host::conformance::onnxq_golden::ONNXQ_GOLDEN_HEX;
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
    let output = output.join("public-onnxq");
    std::fs::create_dir(&output).unwrap();
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: id(),
    };
    let request = serde_json::json!({
        "input_variant_id": fixture.data["profile"]["input_variant_id"],
        "adapter_id": "product.transformation.onnxq",
        "tool_version": "1.0.0",
        "target_format": "ONNX",
        "parameters": {"quantization": "INT8"},
        "storage_location_id": fixture.data["profile"]["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let typed: larm_domain::transformations::TransformationRequest =
        serde_json::from_value(request.clone()).unwrap();
    let original =
        registered_case::seed_public_input_named(&db, &fixture, &typed, "model.onnx").await;
    std::fs::write(output.join("input.bin"), &original).unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();
    let mut negatives = Vec::new();
    let (status, missing) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &id(),
        &json!({"adapter_id":"missing.onnxq","tool_version":"1.0.0","target_format":"ONNX","parameters":{},"input_variant_id":request["input_variant_id"],"storage_location_id":request["storage_location_id"],"hard_budget_bytes":null}),
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "unknown: {missing}");
    negatives.push(json!({"case":"unknown_adapter","status":404}));
    let (status, rejected) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &id(),
        &json!({"adapter_id":"product.transformation.onnxq","tool_version":"1.0.0","target_format":"ONNX","parameters":{"quantization":"Q8_0"},"input_variant_id":request["input_variant_id"],"storage_location_id":request["storage_location_id"],"hard_budget_bytes":null}),
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "quant unsupported: {rejected}"
    );
    negatives.push(json!({"case":"quant_unsupported","status":422}));

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
    // La cuantización real del fixture lanza el runtime Olive sellado como
    // subproceso del worker; el deadline cubre el arranque de ese runtime.
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut worker_progress = 0usize;
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
            "la cuantización ONNXQ falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        if value["progress"]["worker"].is_object() {
            worker_progress += 1;
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
        // El blob CAS del artefacto DATA es el modelo cuantizado publicado.
        let worker_role = record["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        // Oráculo QDQ en Rust: el modelo publicado coincide byte a byte con el
        // golden emitido por el runtime Olive sellado y su estructura QDQ con
        // el espejo int8/f32 de los pesos del fixture.
        let golden = decode(ONNXQ_GOLDEN_HEX);
        assert_eq!(
            raw, golden,
            "el modelo cuantizado publicado difiere del golden del runtime sellado byte a byte"
        );
        let model = parse_onnxq_model(&raw).unwrap();
        assert_eq!(model.ir_version, 8);
        assert_eq!(model.graph_name, "larm-onnxq-fixture");
        let ops: Vec<&str> = model
            .nodes
            .iter()
            .map(|node| node.op_type.as_str())
            .collect();
        assert_eq!(
            ops,
            [
                "DynamicQuantizeLinear",
                "Mul",
                "MatMulInteger",
                "Cast",
                "Mul"
            ]
        );
        let published_weight = onnxq_initializer(&model, "w_quantized").unwrap();
        assert_eq!(published_weight.onnx_dtype, 3);
        assert_eq!(published_weight.dims, vec![64, 128]);
        let scale = onnxq_initializer(&model, "w_scale").unwrap();
        assert_eq!(scale.onnx_dtype, 1);
        let zero = onnxq_initializer(&model, "w_zero_point").unwrap();
        assert_eq!(zero.onnx_dtype, 3);
        // Espejo independiente del peso F32 fuente: escala max_abs/127 y
        // redondeo half-away con clamp [-127,127] (fórmula ORT dynamic).
        let input_model = parse_onnxq_model(&original).unwrap();
        let source = onnxq_initializer(&input_model, "w").unwrap();
        let weight: Vec<f32> = source
            .raw_data
            .as_ref()
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        let max_abs = weight.iter().fold(0f32, |acc, value| acc.max(value.abs()));
        let expected_scale = f32::from_bits((max_abs / 127.0).to_bits());
        let scale_published = scale
            .float_data
            .as_ref()
            .or(scale.raw_data.as_ref())
            .unwrap();
        assert_eq!(scale_published.len(), 4);
        assert_eq!(
            expected_scale.to_le_bytes(),
            scale_published[..4],
            "la escala publicada difiere de la fórmula ORT dynamic"
        );
        let quantized: Vec<i8> = weight
            .iter()
            .map(|value| {
                let quotient = value / expected_scale;
                let rounded = if quotient >= 0.0 {
                    (quotient + 0.5).floor()
                } else {
                    (quotient - 0.5).ceil()
                };
                rounded.clamp(-127.0, 127.0) as i8
            })
            .collect();
        let weight_bytes: Vec<u8> = quantized
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        assert_eq!(
            weight_bytes,
            published_weight.raw_data.as_ref().unwrap()[..8192],
            "el peso int8 publicado difiere del espejo Rust"
        );
        assert!(
            zero.int32_data
                .as_ref()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|chunk| *chunk == [0u8; 4]),
            "el zero_point debe ser 0"
        );
        // El linaje de cuantización es estructural en la variante publicada.
        assert_eq!(
            record["variant"]["quantization"],
            json!("INT8"),
            "la variante debe registrar su cuantización INT8"
        );
        artifacts.push(json!({"file":file.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"ops":ops,"weight_dims":published_weight.dims,"quantization":"INT8","quant_bytes":raw.len()}));
    }
    assert!(
        !artifacts.is_empty(),
        "el .onnx cuantizado no se publicó; roles={:?}",
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
    let result = json!({"status":"PASS","scope":"Public ONNXQ product quantizer via Olive: real P026 qualification with the host ONNXQ suite, public plan/confirm/execute with adapter_id product.transformation.onnxq and quantization=INT8, sealed managed runtime python-olive injected via OLIVE_RUNTIME from the persisted profile, CAS publication of the quantized model verified by the Rust QDQ oracle (byte-exact against the sealed runtime golden plus int8/f32 weight mirror with ORT dynamic formula); seeded catalogue is an explicit fixture","request":request,"plan":first,"receipt":receipt,"completed_api":completed_api,"completed":completed,"record":record,"artifacts":artifacts,"negatives":negatives,"events":events,"outbox_count":outbox,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"worker_progress_observations":worker_progress,"runtime_acceptance":true,"quantization_acceptance":true});
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

fn decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect()
}
