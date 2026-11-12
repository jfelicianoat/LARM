#![allow(clippy::unwrap_used, clippy::panic)]
use larm_adapter_sdk::source::*;
use larm_domain::error::ErrorCode;
use larm_domain::estimate::Confidence;
use larm_domain::units::DecimalU64;
use serde_json::{json, Map, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json")).unwrap()
}

#[test]
fn catalog_reference_is_optional_but_present_reference_must_be_typed_and_bound() {
    let query = request("source.search");
    let mut page = fixture()["responses"]["source.search"].clone();
    assert!(SourceReply::parse(&query, page.clone()).is_ok());
    let SourceRequest::Search(search) = &query else {
        panic!("search")
    };
    let reference = json!({"source_id":search.source_id,"upstream_namespace":"fixture",
        "upstream_model_id":"model","revision_key":"r1"});
    page["items"][0]["metadata"]["source_v1"] =
        json!({"normalized":{"catalog_reference":reference}});
    let parsed = SourceReply::parse(&query, page.clone()).unwrap();
    let SourceReply::Search(result) = parsed else {
        panic!("search reply")
    };
    assert_eq!(
        larm_domain::acquisition::catalog_reference(&result.items[0])
            .unwrap()
            .unwrap()
            .source_id,
        search.source_id
    );
    for (field, value) in [
        ("source_id", json!("0192aaaa-0000-7000-8000-000000000099")),
        ("revision_key", json!(" r1")),
        ("upstream_model_id", json!("")),
        ("extra", json!(true)),
    ] {
        let mut bad = page.clone();
        bad["items"][0]["metadata"]["source_v1"]["normalized"]["catalog_reference"][field] = value;
        assert_eq!(
            SourceReply::parse(&query, bad).unwrap_err().code,
            ErrorCode::ProtocolError
        );
    }
}
fn request(method: &str) -> SourceRequest {
    SourceRequest::parse(method, fixture()["requests"][method].clone()).unwrap()
}
fn reply(method: &str) -> SourceReply {
    SourceReply::parse(&request(method), fixture()["responses"][method].clone()).unwrap()
}

#[test]
fn recibo_worker_no_inventa_una_ruta_de_job_del_core() {
    let req = request("source.download");
    let mut value = fixture()["responses"]["source.download"].clone();
    value["state"] = json!("COMPLETED");
    for url in [
        "file:///C:/reference/job-status/worker.json",
        "https://worker.example.invalid/jobs/worker",
    ] {
        value["status_url"] = json!(url);
        assert_eq!(
            SourceReply::parse(&req, value.clone())
                .unwrap()
                .value()
                .unwrap(),
            value
        );
    }
    value["status_url"] = json!("");
    assert!(SourceReply::parse(&req, value).is_err());
}

#[test]
fn bloque_descarga_valida_base64_offset_limites_eof_y_source() {
    let req = request("source.download_chunk");
    let mut response = fixture()["responses"]["source.download_chunk"].clone();
    let SourceReply::DownloadChunk(chunk) = SourceReply::parse(&req, response.clone()).unwrap()
    else {
        panic!("chunk");
    };
    assert_eq!(chunk.decoded_bytes().unwrap(), b"abc");

    for (field, value) in [
        ("data_base64", json!("YWJj\n")),
        ("offset", json!("1")),
        ("total_bytes", json!("3")),
    ] {
        let mut invalid = response.clone();
        invalid[field] = value;
        assert_eq!(
            SourceReply::parse(&req, invalid).unwrap_err().code,
            ErrorCode::ProtocolError,
            "{field}"
        );
    }
    response["eof"] = json!(true);
    assert!(SourceReply::parse(&req, response).is_err());

    let mut too_small = req.clone();
    let SourceRequest::DownloadChunk(chunk_req) = &mut too_small else {
        panic!("chunk request");
    };
    chunk_req.max_bytes = DecimalU64::new(2).unwrap();
    assert!(SourceReply::parse(
        &too_small,
        fixture()["responses"]["source.download_chunk"].clone()
    )
    .is_err());

    let mut bad_etag = req;
    let SourceRequest::DownloadChunk(chunk_req) = &mut bad_etag else {
        panic!("chunk request");
    };
    chunk_req.validator = Some(SourceDownloadResumeValidator::StrongEtag {
        value: "W/weak".into(),
    });
    assert!(chunk_req.validate().is_err());
}

#[test]
fn diez_firmas_roundtrip_exacto_y_requests_estrictas() {
    let fixture = fixture();
    let table = larm_adapter_sdk::manifest::MethodTable::contract();
    let expected: Vec<_> = table
        .methods
        .iter()
        .filter(|method| method.family == "source")
        .map(|method| method.method.as_str())
        .collect();
    assert_eq!(
        fixture["requests"].as_object().unwrap().len(),
        expected.len()
    );
    for method in expected {
        let input = fixture["requests"][method].clone();
        let req = SourceRequest::parse(method, input.clone()).unwrap();
        assert_eq!(req.method(), method);
        assert_eq!(req.params().unwrap(), input);
        let response = SourceReply::parse(&req, fixture["responses"][method].clone()).unwrap();
        assert_eq!(response.value().unwrap(), fixture["responses"][method]);
        for key in input.as_object().unwrap().keys() {
            let mut missing = input.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                SourceRequest::parse(method, missing).is_err(),
                "{method}.{key}"
            );
        }
        let mut extra = input;
        extra["other"] = json!(true);
        assert!(SourceRequest::parse(method, extra).is_err());
    }
    assert_eq!(
        SourceRequest::parse("source.probe", json!({}))
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
}

#[test]
fn respuestas_no_admiten_campos_ausentes_extra_o_tipo_de_otro_metodo() {
    let fixture = fixture();
    for (method, response) in fixture["responses"].as_object().unwrap() {
        let req = request(method);
        for key in response.as_object().unwrap().keys() {
            if method == "source.sync" && key == "rate_limit" {
                continue;
            }
            let mut missing = response.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert_eq!(
                SourceReply::parse(&req, missing).unwrap_err().code,
                ErrorCode::ProtocolError,
                "{method}.{key}"
            );
        }
        let mut extra = response.clone();
        extra["unexpected"] = json!(1);
        assert!(SourceReply::parse(&req, extra).is_err());
    }
    assert_eq!(
        reply("source.auth_probe")
            .validate_for(&request("source.search"))
            .unwrap_err()
            .code,
        ErrorCode::ProtocolError
    );
    let mut page = fixture["responses"]["source.search"].clone();
    page["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("quantization");
    assert!(
        SourceReply::parse(&request("source.search"), page).is_err(),
        "Option compartida no oculta required"
    );
}

#[test]
fn sync_admite_rate_limit_opcional_y_valida_observaciones_presentes() {
    let req = request("source.sync");
    let mut response = fixture()["responses"]["source.sync"].clone();
    let SourceReply::Sync(old_adapter) = SourceReply::parse(&req, response.clone()).unwrap() else {
        panic!("sync result");
    };
    assert!(old_adapter.rate_limit.is_none());

    let mut explicitly_unknown = response.clone();
    explicitly_unknown["rate_limit"] = Value::Null;
    let SourceReply::Sync(explicitly_unknown) =
        SourceReply::parse(&req, explicitly_unknown).unwrap()
    else {
        panic!("sync result with explicit unknown telemetry");
    };
    assert_eq!(explicitly_unknown.rate_limit, Some(None));

    response["rate_limit"] = json!({
        "limit":"120", "remaining":"0", "reset_at":"2026-10-25T12:05:00.000Z", "retry_after_seconds":"60"
    });
    let SourceReply::Sync(updated_adapter) = SourceReply::parse(&req, response.clone()).unwrap()
    else {
        panic!("sync result with telemetry");
    };
    assert_eq!(
        updated_adapter
            .rate_limit
            .as_ref()
            .and_then(Option::as_ref)
            .unwrap()
            .remaining
            .unwrap()
            .get(),
        0
    );

    response["rate_limit"]["remaining"] = json!("121");
    assert_eq!(
        SourceReply::parse(&req, response).unwrap_err().code,
        ErrorCode::ProtocolError
    );
}

#[test]
fn manifiesto_publica_intervalos_por_artefacto_y_no_confunde_exactitud() {
    let req = request("source.get_manifest");
    let mut response = fixture()["responses"]["source.get_manifest"].clone();
    response["artifacts"][0]["size_bytes"] = Value::Null;
    response["artifacts"][0]["size_estimate"] = json!({
        "confidence": "ESTIMATED",
        "bytes": null,
        "lower_bound_bytes": "100",
        "upper_bound_bytes": "200",
        "source": "SOURCE_METADATA",
        "evidence_refs": ["source:revision:file-0"],
        "reason": "El upstream publica un intervalo"
    });
    let parsed = SourceReply::parse(&req, response.clone()).unwrap();
    assert_eq!(parsed.value().unwrap(), response);

    response["artifacts"][0]["size_bytes"] = json!("99");
    response["artifacts"][0]["size_estimate"] = json!({
        "confidence": "EXACT",
        "bytes": "100",
        "lower_bound_bytes": "100",
        "upper_bound_bytes": "100",
        "source": "MANIFEST",
        "evidence_refs": ["manifest:revision:file-0"],
        "reason": null
    });
    assert!(SourceReply::parse(&req, response).is_err());

    let inspection_req = request("source.get_model");
    let mut inspection = fixture()["responses"]["source.get_model"].clone();
    inspection["artifacts"][0]["size_estimate"] = json!({
        "confidence": "ESTIMATED",
        "bytes": null,
        "lower_bound_bytes": "100",
        "upper_bound_bytes": "200",
        "source": "SOURCE_METADATA",
        "evidence_refs": ["source:revision:file-0"],
        "reason": "El upstream publica un intervalo"
    });
    assert!(SourceReply::parse(&inspection_req, inspection).is_err());
}

#[test]
fn referencias_hashes_tipos_y_enum_auth_no_tienen_fallbacks() {
    let fixture = fixture();
    for (method, field, bad) in [
        ("source.get_model", "source_id", json!("bad-id")),
        ("source.get_model", "upstream_namespace", json!("")),
        ("source.get_manifest", "revision_key", json!("")),
        ("source.download", "plan_hash", json!("A".repeat(64))),
        ("source.download", "approval_id", json!("not-uuid")),
        ("source.download_plan", "hard_budget_bytes", json!(123)),
        ("source.download_plan", "priority", json!("URGENT")),
        ("source.sync", "cursor", json!("")),
        ("source.sync", "query", json!([])),
        (
            "source.auth_probe",
            "credential_reference_id",
            json!("larm/plaintext"),
        ),
        ("source.auth_probe", "account_id", json!("not-a-uuid")),
        ("source.auth_probe", "auth_mode", json!("MAGIC")),
    ] {
        let mut input = fixture["requests"][method].clone();
        input[field] = bad;
        assert!(
            SourceRequest::parse(method, input).is_err(),
            "{method}.{field}"
        );
    }
    for (account_id, credential_reference_id) in [
        (json!(null), json!("0192aaaa-0000-7000-8000-000000000099")),
        (json!("0192aaaa-0000-7000-8000-000000000098"), json!(null)),
    ] {
        let mut input = fixture["requests"]["source.auth_probe"].clone();
        input["auth_mode"] = json!("BASIC");
        input["account_id"] = account_id;
        input["credential_reference_id"] = credential_reference_id;
        assert_eq!(
            SourceRequest::parse("source.auth_probe", input)
                .unwrap_err()
                .details
                .reason,
            "auth_probe_credential_binding"
        );
    }
    for mode in [
        "NONE",
        "TOKEN",
        "BEARER",
        "API_TOKEN",
        "OAUTH_DEVICE",
        "OAUTH_AUTH_CODE_PKCE",
        "BASIC",
        "SSH",
        "COOKIE",
        "GIT_LFS",
        "WINDOWS_INTEGRATED",
    ] {
        let mut input = fixture["requests"]["source.auth_probe"].clone();
        input["auth_mode"] = json!(mode);
        if mode == "NONE" {
            input["account_id"] = Value::Null;
            input["credential_reference_id"] = Value::Null;
        } else {
            input["account_id"] = json!("0192aaaa-0000-7000-8000-000000000098");
            input["credential_reference_id"] = json!("0192aaaa-0000-7000-8000-000000000099");
        }
        assert_eq!(
            SourceRequest::parse("source.auth_probe", input.clone())
                .unwrap()
                .params()
                .unwrap(),
            input
        );
    }
}

#[test]
fn metadata_parcial_y_desconocida_permanece_sin_valores_inventados() {
    let SourceReply::GetModel(model) = reply("source.get_model") else {
        panic!("model");
    };
    assert!(
        model.architecture.is_none() && model.license.is_none() && model.quantization.is_none()
    );
    assert_eq!(model.limits["vendor_new_field"]["context"], Value::Null);
    let observation = SourceMetadata {
        original: serde_json::from_value(json!({"new_vendor": {"nested": [1, true, null]}, "token": "fixture-sensitive", "headers": {"Authorization": "secret-fixture"}, "tokenizer": "keep-me"})).unwrap(),
        normalized: serde_json::from_value(json!({"architecture": null, "capabilities": ["text.generate"]})).unwrap(),
        limitations: vec!["architecture_unknown".into()], rate_limit: None,
    };
    let mut metadata = Map::new();
    metadata.insert("other_namespace".into(), json!({"retain": true}));
    observation.attach(&mut metadata).unwrap();
    assert_eq!(metadata["other_namespace"], json!({"retain": true}));
    let saved = SourceMetadata::read(&metadata).unwrap().unwrap();
    assert_eq!(
        saved.original["new_vendor"],
        observation.original["new_vendor"]
    );
    assert_eq!(saved.original["tokenizer"], "keep-me");
    assert_eq!(saved.original["token"], "[REDACTED]");
    let encoded = serde_json::to_string(&metadata).unwrap();
    assert!(!encoded.contains("fixture-sensitive") && !encoded.contains("secret-fixture"));
    assert_eq!(saved.normalized["architecture"], Value::Null);
}

#[test]
fn rate_limit_visible_en_error_y_metadata_sin_inventar_delay() {
    let rate: RateLimit = serde_json::from_value(fixture()["rate_limit"].clone()).unwrap();
    let error = rate.to_error().unwrap();
    assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
    assert!(error.retryable);
    let wire = serde_json::to_value(larm_adapter_sdk::protocol::RpcError::domain(error)).unwrap();
    let decoded: larm_adapter_sdk::protocol::RpcError = serde_json::from_value(wire).unwrap();
    assert_eq!(RateLimit::from_error(&decoded.to_larm()).unwrap(), rate);
    assert_eq!(rate.retry_delay().unwrap().as_secs(), 60);
    assert_eq!(
        rate.retry_after_seconds.unwrap().get(),
        900,
        "observación original conservada"
    );
    let unknown: RateLimit = serde_json::from_value(
        json!({"limit":null,"remaining":null,"reset_at":null,"retry_after_seconds":null}),
    )
    .unwrap();
    assert!(unknown.retry_delay().is_none());
    assert_eq!(
        RateLimit::from_error(&unknown.to_error().unwrap()),
        Some(unknown)
    );
    let mut invalid = rate;
    invalid.remaining = DecimalU64::new(101);
    assert!(invalid.to_error().is_err());
    let metadata =
        json!({"source_v1":{"original":{},"normalized":{},"limitations":[],"rate_limit":invalid}});
    assert!(SourceMetadata::read(metadata.as_object().unwrap()).is_err());
}

#[test]
fn paginas_preservan_snapshot_cursores_y_datos_y_terminan_sin_saltos() {
    let SourceRequest::Search(query) = request("source.search") else {
        panic!("request");
    };
    let SourceReply::Search(first) = reply("source.search") else {
        panic!("page");
    };
    let mut pages = SourcePagination::new(query.clone(), "a".repeat(64)).unwrap();
    assert_eq!(
        pages.next_request(&"a".repeat(64)).unwrap(),
        Some(query.clone())
    );
    pages.accept(&query, &first).unwrap();
    let next = pages.next_request(&"a".repeat(64)).unwrap().unwrap();
    assert_eq!(next.cursor.as_deref(), Some("page-2"));
    let mut last = first.clone();
    last.items[0].id = "0192aaaa-0000-7000-8000-000000000012".into();
    last.next_cursor = None;
    assert_eq!(
        last.items[0].metadata["vendor_unrecognized"],
        json!({"nested":[1,true,null]})
    );
    pages.accept(&next, &last).unwrap();
    assert!(pages.next_request(&"a".repeat(64)).unwrap().is_none());
    assert!(pages.accept(&next, &last).is_err());
}

#[test]
fn pagina_invalida_no_consume_cursor_ni_se_declara_busqueda_completa() {
    let SourceRequest::Search(query) = request("source.search") else {
        panic!("request");
    };
    let SourceReply::Search(first) = reply("source.search") else {
        panic!("page");
    };
    let mut pages = SourcePagination::new(query.clone(), "a".repeat(64)).unwrap();
    assert!(pages.next_request(&"b".repeat(64)).is_err());
    pages.accept(&query, &first).unwrap();
    let next = pages.next_request(&"a".repeat(64)).unwrap().unwrap();
    assert!(pages.accept(&next, &first).is_err(), "cursor repetido");
    let mut last = first.clone();
    last.next_cursor = None;
    assert!(pages.accept(&next, &last).is_err(), "id repetido");
    last.items[0].id = "0192aaaa-0000-7000-8000-000000000012".into();
    last.snapshot_at = "2026-10-17T00:00:00.000Z".into();
    assert_eq!(
        pages.accept(&next, &last).unwrap_err().details.reason,
        "source_snapshot_changed"
    );
    assert_eq!(
        pages.next_request(&"a".repeat(64)).unwrap(),
        Some(next.clone())
    );
    last.snapshot_at = first.snapshot_at;
    pages.accept(&next, &last).unwrap();
}

#[test]
fn artefactos_hostiles_duplicados_y_hashes_invalidos_no_se_aceptan() {
    for path in [
        "../escape",
        "C:/escape",
        "weights.gguf:ads",
        "a\\b",
        "a//b",
        "/abs",
    ] {
        let mut response = fixture()["responses"]["source.get_manifest"].clone();
        response["artifacts"][0]["relative_path"] = json!(path);
        assert!(
            SourceReply::parse(&request("source.get_manifest"), response).is_err(),
            "{path}"
        );
    }
    let mut response = fixture()["responses"]["source.get_manifest"].clone();
    let mut duplicate = response["artifacts"][0].clone();
    duplicate["relative_path"] = json!("WEIGHTS.GGUF");
    response["artifacts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(SourceReply::parse(&request("source.get_manifest"), response).is_err());
    let mut partial = fixture()["responses"]["source.get_model"].clone();
    partial["artifacts"][0]["size_bytes"] = Value::Null;
    partial["artifacts"][0]["sha256"] = Value::Null;
    assert_eq!(
        SourceReply::parse(&request("source.get_model"), partial.clone())
            .unwrap()
            .value()
            .unwrap(),
        partial
    );
}

#[test]
fn plan_enumera_archivos_auth_licencia_reanudacion_y_hash_independiente() {
    let SourceReply::DownloadPlan(plan) = reply("source.download_plan") else {
        panic!("plan");
    };
    assert_eq!(
        plan_hash(&plan).unwrap(),
        fixture()["responses"]["source.download_plan"]["plan_hash"]
    );
    let spec = download_spec(&plan).unwrap();
    assert_eq!(spec.download_bytes().unwrap().unwrap().get(), 18);
    assert_eq!(spec.artifacts.len(), 1);
    assert!(!spec.resumable);
    spec.check_execution_requirements(Confidence::Exact)
        .unwrap();
    let mut altered = plan.clone();
    altered.steps[0].request[SOURCE_DOWNLOAD_SPEC]["resumable"] = json!(true);
    assert!(SourceReply::parse(
        &request("source.download_plan"),
        serde_json::to_value(altered).unwrap()
    )
    .is_err());
    let mut bad_size = plan;
    bad_size.storage_estimate.download_bytes = DecimalU64::new(0);
    bad_size.plan_hash = plan_hash(&bad_size).unwrap();
    assert!(SourceReply::parse(
        &request("source.download_plan"),
        serde_json::to_value(bad_size).unwrap()
    )
    .is_err());
}

#[test]
fn presupuesto_y_prioridad_no_desaparecen_y_unknown_no_es_cero() {
    let SourceReply::DownloadPlan(mut plan) = reply("source.download_plan") else {
        panic!("plan");
    };
    let SourceRequest::DownloadPlan(mut req) = request("source.download_plan") else {
        panic!("req");
    };
    let mut spec = download_spec(&plan).unwrap();
    spec.artifacts[0].size_bytes = None;
    spec.artifacts[0].sha256 = None;
    assert!(spec.download_bytes().unwrap().is_none());
    assert_eq!(
        spec.check_execution_requirements(Confidence::Unknown)
            .unwrap_err()
            .code,
        ErrorCode::UnknownSize
    );
    req.hard_budget_bytes = DecimalU64::new(100);
    req.priority = DownloadPriority::Background;
    spec.hard_budget_bytes = req.hard_budget_bytes;
    spec.priority = req.priority;
    spec.check_execution_requirements(Confidence::Unknown)
        .unwrap();
    plan.steps.clear();
    plan.storage_estimate.download_bytes = None;
    plan.storage_estimate.confidence = Confidence::Unknown;
    let bound = spec.bind(&req, plan, "fixture.source").unwrap();
    assert_eq!(download_spec(&bound).unwrap(), spec);
    let mut changed = req;
    changed.priority = DownloadPriority::High;
    assert!(validate_source_plan(&changed, &bound).is_err());
    spec.auth.auth_required = true;
    assert_eq!(
        spec.check_execution_requirements(Confidence::Unknown)
            .unwrap_err()
            .code,
        ErrorCode::AuthRequired
    );
    spec.auth.authenticated = true;
    spec.auth.license_required = true;
    let error = spec
        .check_execution_requirements(Confidence::Unknown)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LicenseRequired);
    assert!(!error.retryable);
}

#[test]
fn exact_exige_hash_y_overflow_no_se_truncan() {
    let SourceReply::DownloadPlan(mut plan) = reply("source.download_plan") else {
        panic!("plan");
    };
    let SourceRequest::DownloadPlan(req) = request("source.download_plan") else {
        panic!("req");
    };
    let mut spec = download_spec(&plan).unwrap();
    spec.artifacts[0].sha256 = None;
    plan.steps.clear();
    assert!(spec.bind(&req, plan, "fixture.source").is_err());
    spec.artifacts[0].size_bytes = DecimalU64::new(i64::MAX as u64);
    let mut extra = spec.artifacts[0].clone();
    extra.relative_path = "another.gguf".into();
    extra.size_bytes = DecimalU64::new(1);
    spec.artifacts.push(extra);
    assert!(spec.download_bytes().is_err());
}
