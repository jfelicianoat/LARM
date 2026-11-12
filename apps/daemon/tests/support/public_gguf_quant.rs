//! Aceptación pública de la CUANTIZACIÓN REAL Q8_0 del conversor GGUF de
//! producto: cualificación P026 real con la suite GGUF del host, plan con el
//! parámetro declarado (quantization=Q8_0), ejecución con progreso GGUF_TENSOR
//! y publicación CAS del contenedor cuantizado verificado con el oráculo GGUF
//! en Rust (byte a byte contra la emisión espejo con cuantización: weight.big
//! F32 → Q8_0, weight.scale F16 y n_positions I64 → verbatim, file_type 7).
use super::public_case::Inputs;
use super::*;
use larm_adapter_host::conformance::gguf::{
    expected_gguf_quantized, parse_gguf, parse_safetensors,
};
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
    let output = output.join("public-gguf-quant");
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
        "parameters": {"quantization": "Q8_0"},
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
        &json!({"adapter_id":"missing.gguf","tool_version":"1.1.0","target_format":"GGUF","parameters":{},"input_variant_id":request["input_variant_id"],"storage_location_id":request["storage_location_id"],"hard_budget_bytes":null}),
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
        &json!({"adapter_id":"product.transformation.gguf","tool_version":"1.1.0","target_format":"GGUF","parameters":{"quantization":"Q2_K"},"input_variant_id":request["input_variant_id"],"storage_location_id":request["storage_location_id"],"hard_budget_bytes":null}),
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
        // Oráculo GGUF cuantizado en Rust: el contenedor publicado coincide
        // byte a byte con la emisión espejo del oráculo con Q8_0 (weight.big
        // F32 [983040] → Q8_0; weight.scale F16 [4] y n_positions I64 →
        // verbatim; file_type 7) y la estructura se parsea contra la fuente.
        let source = parse_safetensors(&original).unwrap();
        let expected = expected_gguf_quantized(&source, &original, Some("Q8_0"));
        assert_eq!(
            raw, expected,
            "el contenedor cuantizado publicado difiere del oráculo Rust byte a byte"
        );
        let container = parse_gguf(&raw).unwrap();
        assert_eq!(container.version, 3);
        assert_eq!(container.alignment, 32);
        assert_eq!(container.tensors.len(), source.tensors.len());
        let mut quantized_count = 0usize;
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
            let fast_axis = source_tensor.shape[source_tensor.shape.len() - 1];
            if (source_tensor.dtype == "F32" || source_tensor.dtype == "F16")
                && fast_axis > 0
                && fast_axis.is_multiple_of(32)
            {
                assert_eq!(
                    tensor.ggml_type, 8,
                    "tensor {} debía quedar Q8_0",
                    tensor.name
                );
                quantized_count += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantized_count, 1, "sólo weight.big es cuantizable");
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización"
        );
        // El linaje de cuantización es estructural en la variante publicada.
        assert_eq!(
            record["variant"]["quantization"],
            json!("Q8_0"),
            "la variante debe registrar su cuantización Q8_0"
        );
        artifacts.push(json!({"file":file.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container.tensors.len(),"quantized":quantized_count,"quant_format":"Q8_0","quant_bytes":expected.len()}));
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

    // Segundo ciclo: cuantización Q4_0 (type 2, 18 bytes por bloque). La
    // variante Q8_0 ya publicada queda intacta; el plan nuevo produce una
    // variante adicional con su propio trabajo.
    let request4 = serde_json::json!({
        "input_variant_id": request["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {"quantization": "Q4_0"},
        "storage_location_id": request["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let plan4_key = id();
    let (status, plan4) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan4_key,
        &request4,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan q4: {plan4}");
    let confirmation4 =
        json!({"plan_id":plan4["plan_id"],"plan_hash":plan4["plan_hash"],"approval_id":null});
    let (status, receipt4) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmation4,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation q4: {receipt4}"
    );
    let job4 = receipt4["job_id"].as_str().unwrap().to_owned();
    let deadline4 = Instant::now() + Duration::from_secs(120);
    let completed4_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job4}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión Q4_0 falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(Instant::now() < deadline4, "sin desenlace Q4_0: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completed4_api["progress"]["transformation_state"],
        "COMPLETED"
    );
    let completed4 = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job4)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed4.state, TransformationState::Completed);
    assert_eq!(
        counts(&db).await,
        (2, 3, 0, 0),
        "dos trabajos y tres variantes"
    );
    let record4 = completed4.metadata["publication"].clone();
    let mut published_q4 = 0usize;
    for (index, loc) in record4["locations"].as_array().unwrap().iter().enumerate() {
        let raw4 = std::fs::read(
            fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let file4 = output.join(format!("published-q4-{index}.bin"));
        std::fs::write(&file4, &raw4).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw4)),
            loc["sha256"].as_str().unwrap()
        );
        let worker_role = record4["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record4["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        let source = parse_safetensors(&original).unwrap();
        let expected4 = expected_gguf_quantized(&source, &original, Some("Q4_0"));
        assert_eq!(
            raw4, expected4,
            "el contenedor Q4_0 publicado difiere del oráculo Rust byte a byte"
        );
        let container4 = parse_gguf(&raw4).unwrap();
        let mut quantized4 = 0usize;
        for tensor in &container4.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            if tensor.ggml_type == 2 {
                quantized4 += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantized4, 1, "sólo weight.big es cuantizable a Q4_0");
        assert_eq!(
            container4.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización Q4_0"
        );
        assert_eq!(
            record4["variant"]["quantization"],
            json!("Q4_0"),
            "la variante debe registrar su cuantización Q4_0"
        );
        published_q4 += 1;
        artifacts.push(json!({"file":file4.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container4.tensors.len(),"quantized":quantized4,"quant_format":"Q4_0","quant_bytes":expected4.len()}));
    }
    assert_eq!(published_q4, 1, "el .gguf Q4_0 no se publicó");

    // Tercer ciclo: cuantización Q5_0 (type 6, 22 bytes por bloque con uint32
    // de bits altos). Tres variantes cuantizadas en la misma sesión.
    let request5 = serde_json::json!({
        "input_variant_id": request["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {"quantization": "Q5_0"},
        "storage_location_id": request["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let plan5_key = id();
    let (status, plan5) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan5_key,
        &request5,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan q5: {plan5}");
    let confirmation5 =
        json!({"plan_id":plan5["plan_id"],"plan_hash":plan5["plan_hash"],"approval_id":null});
    let (status, receipt5) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmation5,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation q5: {receipt5}"
    );
    let job5 = receipt5["job_id"].as_str().unwrap().to_owned();
    let deadline5 = Instant::now() + Duration::from_secs(120);
    let completed5_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job5}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión Q5_0 falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(Instant::now() < deadline5, "sin desenlace Q5_0: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completed5_api["progress"]["transformation_state"],
        "COMPLETED"
    );
    let completed5 = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job5)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed5.state, TransformationState::Completed);
    assert_eq!(
        counts(&db).await,
        (3, 4, 0, 0),
        "tres trabajos y cuatro variantes"
    );
    let record5 = completed5.metadata["publication"].clone();
    let mut published_q5 = 0usize;
    for (index, loc) in record5["locations"].as_array().unwrap().iter().enumerate() {
        let raw5 = std::fs::read(
            fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let file5 = output.join(format!("published-q5-{index}.bin"));
        std::fs::write(&file5, &raw5).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw5)),
            loc["sha256"].as_str().unwrap()
        );
        let worker_role = record5["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record5["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        let source = parse_safetensors(&original).unwrap();
        let expected5 = expected_gguf_quantized(&source, &original, Some("Q5_0"));
        assert_eq!(
            raw5, expected5,
            "el contenedor Q5_0 publicado difiere del oráculo Rust byte a byte"
        );
        let container5 = parse_gguf(&raw5).unwrap();
        let mut quantized5 = 0usize;
        for tensor in &container5.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            if tensor.ggml_type == 6 {
                quantized5 += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantized5, 1, "sólo weight.big es cuantizable a Q5_0");
        assert_eq!(
            container5.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización Q5_0"
        );
        assert_eq!(
            record5["variant"]["quantization"],
            json!("Q5_0"),
            "la variante debe registrar su cuantización Q5_0"
        );
        published_q5 += 1;
        artifacts.push(json!({"file":file5.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container5.tensors.len(),"quantized":quantized5,"quant_format":"Q5_0","quant_bytes":expected5.len()}));
    }
    assert_eq!(published_q5, 1, "el .gguf Q5_0 no se publicó");

    // Cuarto ciclo: cuantización Q4_K (type 12, super-bloques de 256
    // elementos → 144 bytes). Cuatro variantes cuantizadas en la sesión.
    let requestk = serde_json::json!({
        "input_variant_id": request["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {"quantization": "Q4_K"},
        "storage_location_id": request["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let plank_key = id();
    let (status, plank) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plank_key,
        &requestk,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan q4k: {plank}");
    let confirmationk =
        json!({"plan_id":plank["plan_id"],"plan_hash":plank["plan_hash"],"approval_id":null});
    let (status, receiptk) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmationk,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation q4k: {receiptk}"
    );
    let jobk = receiptk["job_id"].as_str().unwrap().to_owned();
    // La conversión Q4_K del fixture de 12 MB legítimamente tarda minutos en
    // Python stdlib (super-bloques K): el deadline es un detector de cuelgue.
    let deadlinek = Instant::now() + Duration::from_secs(600);
    let completedk_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{jobk}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión Q4_K falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(Instant::now() < deadlinek, "sin desenlace Q4_K: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completedk_api["progress"]["transformation_state"],
        "COMPLETED"
    );
    let completedk = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&jobk)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completedk.state, TransformationState::Completed);
    assert_eq!(
        counts(&db).await,
        (4, 5, 0, 0),
        "cuatro trabajos y cinco variantes"
    );
    let recordk = completedk.metadata["publication"].clone();
    let mut published_q4k = 0usize;
    for (index, loc) in recordk["locations"].as_array().unwrap().iter().enumerate() {
        let rawk = std::fs::read(
            fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let filek = output.join(format!("published-q4k-{index}.bin"));
        std::fs::write(&filek, &rawk).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&rawk)),
            loc["sha256"].as_str().unwrap()
        );
        let worker_role = recordk["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(recordk["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        let source = parse_safetensors(&original).unwrap();
        let expectedk = expected_gguf_quantized(&source, &original, Some("Q4_K"));
        assert_eq!(
            rawk, expectedk,
            "el contenedor Q4_K publicado difiere del oráculo Rust byte a byte"
        );
        let containerk = parse_gguf(&rawk).unwrap();
        let mut quantizedk = 0usize;
        for tensor in &containerk.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            if tensor.ggml_type == 12 {
                quantizedk += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantizedk, 1, "sólo weight.big es cuantizable a Q4_K");
        assert_eq!(
            containerk.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización Q4_K"
        );
        assert_eq!(
            recordk["variant"]["quantization"],
            json!("Q4_K"),
            "la variante debe registrar su cuantización Q4_K"
        );
        published_q4k += 1;
        artifacts.push(json!({"file":filek.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":containerk.tensors.len(),"quantized":quantizedk,"quant_format":"Q4_K","quant_bytes":expectedk.len()}));
    }
    assert_eq!(published_q4k, 1, "el .gguf Q4_K no se publicó");

    // Quinto ciclo: cuantización Q5_K (type 13, super-bloques de 5 bits).
    let request5k = serde_json::json!({
        "input_variant_id": request["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {"quantization": "Q5_K"},
        "storage_location_id": request["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let plan5k_key = id();
    let (status, plan5k) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan5k_key,
        &request5k,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan q5k: {plan5k}");
    let confirmation5k =
        json!({"plan_id":plan5k["plan_id"],"plan_hash":plan5k["plan_hash"],"approval_id":null});
    let (status, receipt5k) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmation5k,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation q5k: {receipt5k}"
    );
    let job5k = receipt5k["job_id"].as_str().unwrap().to_owned();
    let deadline5k = Instant::now() + Duration::from_secs(600);
    let completed5k_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job5k}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión Q5_K falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(Instant::now() < deadline5k, "sin desenlace Q5_K: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completed5k_api["progress"]["transformation_state"],
        "COMPLETED"
    );
    let completed5k = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job5k)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed5k.state, TransformationState::Completed);
    assert_eq!(
        counts(&db).await,
        (5, 6, 0, 0),
        "cinco trabajos y seis variantes"
    );
    let record5k = completed5k.metadata["publication"].clone();
    let mut published_q5k = 0usize;
    for (index, loc) in record5k["locations"].as_array().unwrap().iter().enumerate() {
        let raw5k = std::fs::read(
            fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let file5k = output.join(format!("published-q5k-{index}.bin"));
        std::fs::write(&file5k, &raw5k).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw5k)),
            loc["sha256"].as_str().unwrap()
        );
        let worker_role = record5k["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record5k["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        let source = parse_safetensors(&original).unwrap();
        let expected5k = expected_gguf_quantized(&source, &original, Some("Q5_K"));
        assert_eq!(
            raw5k, expected5k,
            "el contenedor Q5_K publicado difiere del oráculo Rust byte a byte"
        );
        let container5k = parse_gguf(&raw5k).unwrap();
        let mut quantized5k = 0usize;
        for tensor in &container5k.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            if tensor.ggml_type == 13 {
                quantized5k += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantized5k, 1, "sólo weight.big es cuantizable a Q5_K");
        assert_eq!(
            container5k.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización Q5_K"
        );
        assert_eq!(
            record5k["variant"]["quantization"],
            json!("Q5_K"),
            "la variante debe registrar su cuantización Q5_K"
        );
        published_q5k += 1;
        artifacts.push(json!({"file":file5k.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container5k.tensors.len(),"quantized":quantized5k,"quant_format":"Q5_K","quant_bytes":expected5k.len()}));
    }
    assert_eq!(published_q5k, 1, "el .gguf Q5_K no se publicó");

    // Sexto ciclo: cuantización Q6_K (type 14, super-bloques simétricos de
    // 6 bits). Seis variantes cuantizadas en la sesión.
    let request6k = serde_json::json!({
        "input_variant_id": request["input_variant_id"],
        "adapter_id": "product.transformation.gguf",
        "tool_version": "1.1.0",
        "target_format": "GGUF",
        "parameters": {"quantization": "Q6_K"},
        "storage_location_id": request["storage_location_id"],
        "hard_budget_bytes": serde_json::Value::Null,
    });
    let plan6k_key = id();
    let (status, plan6k) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transform-plans",
        &plan6k_key,
        &request6k,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "plan q6k: {plan6k}");
    let confirmation6k =
        json!({"plan_id":plan6k["plan_id"],"plan_hash":plan6k["plan_hash"],"approval_id":null});
    let (status, receipt6k) = download_restart::post_json(
        &http,
        port,
        &principal,
        "/api/v1/transformations",
        &id(),
        &confirmation6k,
        &ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "activation q6k: {receipt6k}"
    );
    let job6k = receipt6k["job_id"].as_str().unwrap().to_owned();
    let deadline6k = Instant::now() + Duration::from_secs(600);
    let completed6k_api = loop {
        let (status, value) = get(
            &http,
            port,
            &principal.token,
            &format!("/api/v1/jobs/{job6k}"),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_ne!(
            value["state"], "FAILED",
            "la conversión Q6_K falló: {value}"
        );
        if value["state"] == "COMPLETED" {
            break value;
        }
        assert!(Instant::now() < deadline6k, "sin desenlace Q6_K: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        completed6k_api["progress"]["transformation_state"],
        "COMPLETED"
    );
    let completed6k = larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    }
    .owned_claim(&job6k)
    .await
    .unwrap()
    .claim
    .job;
    assert_eq!(completed6k.state, TransformationState::Completed);
    assert_eq!(
        counts(&db).await,
        (6, 7, 0, 0),
        "seis trabajos y siete variantes"
    );
    let record6k = completed6k.metadata["publication"].clone();
    let mut published_q6k = 0usize;
    for (index, loc) in record6k["locations"].as_array().unwrap().iter().enumerate() {
        let raw6k = std::fs::read(
            fixture
                .work
                .join("output")
                .join(loc["relative_path"].as_str().unwrap()),
        )
        .unwrap();
        let file6k = output.join(format!("published-q6k-{index}.bin"));
        std::fs::write(&file6k, &raw6k).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&raw6k)),
            loc["sha256"].as_str().unwrap()
        );
        let worker_role = record6k["artifacts"][index]["metadata"]["worker_role"]
            .as_str()
            .or(record6k["measured"]["artifacts"][index]["worker_role"].as_str())
            .unwrap_or_default();
        if worker_role != "DATA" {
            continue;
        }
        let source = parse_safetensors(&original).unwrap();
        let expected6k = expected_gguf_quantized(&source, &original, Some("Q6_K"));
        assert_eq!(
            raw6k, expected6k,
            "el contenedor Q6_K publicado difiere del oráculo Rust byte a byte"
        );
        let container6k = parse_gguf(&raw6k).unwrap();
        let mut quantized6k = 0usize;
        for tensor in &container6k.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap_or_else(|| panic!("tensor ausente: {}", tensor.name));
            if tensor.ggml_type == 14 {
                quantized6k += 1;
            } else {
                assert_eq!(
                    tensor.ggml_type, source_tensor.ggml_type,
                    "tensor {} debía ir verbatim",
                    tensor.name
                );
            }
        }
        assert_eq!(quantized6k, 1, "sólo weight.big es cuantizable a Q6_K");
        assert_eq!(
            container6k.metadata["general.file_type"],
            (serde_json::json!(7), 4),
            "file_type debe reflejar la cuantización Q6_K"
        );
        assert_eq!(
            record6k["variant"]["quantization"],
            json!("Q6_K"),
            "la variante debe registrar su cuantización Q6_K"
        );
        published_q6k += 1;
        artifacts.push(json!({"file":file6k.file_name().unwrap().to_str().unwrap(),"sha256":loc["sha256"],"tensors":container6k.tensors.len(),"quantized":quantized6k,"quant_format":"Q6_K","quant_bytes":expected6k.len()}));
    }
    assert_eq!(published_q6k, 1, "el .gguf Q6_K no se publicó");
    let events: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&job)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)").bind(&job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(outbox, events.len() as i64);
    let result = json!({"status":"PASS","scope":"Public GGUF REAL QUANTIZATION (Q8_0, Q4_0, Q5_0, Q4_K, Q5_K AND Q6_K): real P026 qualification with the host GGUF suite, six public plan/confirm/execute cycles with adapter_id product.transformation.gguf and parameters quantization=Q8_0, Q4_0, Q5_0, Q4_K, Q5_K then Q6_K, GGUF_TENSOR progress, CAS publication of all six quantized containers verified byte-exact by the Rust GGUF oracle mirror emissions (mixed rule; ggml-compatible half-pair packing, Q5_0 uint32 high bits and Q4_K/Q5_K/Q6_K 256-element super-blocks (6-bit sub-scales via make_qkx2_quants; 5-bit levels + qh bits for Q5_K; symmetric make_qx_quants with signed 8-bit scales for Q6_K)) plus structural parse; seeded catalogue is an explicit fixture","request":request,"plan":first,"receipt":receipt,"completed_api":completed_api,"completed":completed,"record":record,"artifacts":artifacts,"negatives":negatives,"events":events,"outbox_count":outbox,"counts_final":counts(&db).await,"installation":installation,"run":run,"profile":profile,"job_actor":ctx.actor_id,"input_sha256":hex::encode(Sha256::digest(&original)),"original_intact":true,"principals_revoked":true,"gguf_tensor_progress_observations":tensor_progress,"runtime_acceptance":false,"quantization_acceptance":true});
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
