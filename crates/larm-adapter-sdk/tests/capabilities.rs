//! Mismo descriptor/fixture/JCS que Python; no declara conducta de inferencia.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use larm_adapter_sdk::capabilities::{digest, CapabilityRegistry, Estimator};
use larm_domain::error::LarmError;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/adapters/capability-registry.json"
    ))
    .unwrap()
}
fn estimate(value: &Value) -> Result<Value, LarmError> {
    Ok(
        json!({"weights_bytes":null,"kv_bytes":null,"activation_bytes":null,"workspace_bytes":(larm_domain::canonical::canonicalize(value).unwrap().len()+32768).to_string(),"overhead_bytes":null,"total_bytes":null,"multimodal_bytes":null,"temporary_bytes":null,"safety_margin_bytes":null,"per_pool":[],"confidence":"UNKNOWN","evidence_refs":["fixture:memory-unknown"]}),
    )
}
fn registry(entries: Vec<Value>) -> Result<CapabilityRegistry, LarmError> {
    let estimators: BTreeMap<String, Estimator> = entries
        .iter()
        .map(|e| {
            (
                e["estimator_id"].as_str().unwrap().into(),
                Arc::new(estimate) as Estimator,
            )
        })
        .collect();
    let mappings: BTreeSet<_> = entries
        .iter()
        .map(|e| e["mapping"].as_str().unwrap().into())
        .collect();
    CapabilityRegistry::new(
        json!({"schema_version":"1.0","entries":entries}),
        estimators,
        &mappings,
    )
}
fn rejected(entry: Value, reason: &str) {
    let error = registry(vec![entry]).err().expect("descriptor rechazado");
    assert_eq!(error.details.reason, reason);
}
#[test]
fn hashes_de_schema_y_fixture_con_floats_y_utf16_conservan_jcs() {
    for vector in fixtures()["jcs_vectors"].as_array().unwrap() {
        assert_eq!(
            larm_domain::canonical::canonicalize(&vector["value"]).unwrap(),
            vector["canonical"].as_str().unwrap()
        );
        assert_eq!(
            digest(&vector["value"]).unwrap(),
            vector["sha256"].as_str().unwrap()
        );
    }
    assert!(digest(&json!(9_007_199_254_740_993_u64)).is_err());
}
#[test]
fn schemas_estimador_fixture_y_alias_canonico_sin_cualificacion_automatica() {
    let f = fixtures();
    let first = registry(vec![f["echo"].clone(), f["embedding"].clone()]).unwrap();
    let second = registry(vec![f["embedding"].clone(), f["echo"].clone()]).unwrap();
    assert_eq!(first.profile_hash, second.profile_hash);
    first
        .check_advertised(&["vendor.reference.echo".into(), "embedding.create".into()])
        .unwrap();
    assert_eq!(
        first.descriptor("embedding").unwrap().id,
        "embedding.create"
    );
    assert_eq!(
        first.schema_id("embedding", false).unwrap(),
        first.schema_id("embedding.create", false).unwrap()
    );
    let memory = first
        .estimate("embedding", &f["embedding"]["fixture"]["input"])
        .unwrap();
    assert_eq!(memory["confidence"], "UNKNOWN");
    assert!(memory["total_bytes"].is_null());
    assert!(first.check_advertised(&["embedding".into()]).is_err());
    assert!(first
        .check_advertised(&["vendor.not_registered".into()])
        .is_err());
}
#[test]
fn manipulacion_de_schema_fixture_alias_y_capacidad_builtin_se_rechaza() {
    let good = fixtures()["echo"].clone();
    let mut bad = good.clone();
    bad["input_sha256"] = json!("b".repeat(64));
    rejected(bad, "capability_schema_hash_mismatch");
    let mut bad = good.clone();
    bad["input_schema"] = json!({"$ref":"file:///C:/never"});
    bad["input_sha256"] = json!(digest(&bad["input_schema"]).unwrap());
    rejected(bad, "capability_descriptor_invalid");
    let mut bad = good.clone();
    bad["fixture"]["input"]["text"] = json!(4);
    rejected(bad, "capability_fixture_invalid");
    let mut bad = good.clone();
    bad["fixture_sha256"] = json!("b".repeat(64));
    rejected(bad, "capability_fixture_hash_mismatch");
    let mut bad = good.clone();
    bad["id"] = json!("embedding.create");
    rejected(bad, "capability_builtin_schema_mismatch");
    let mut bad = good.clone();
    bad["id"] = json!("embedding");
    rejected(bad, "capability_descriptor_invalid");
    let mut bad = good.clone();
    bad["qualified"] = json!(true);
    rejected(bad, "capability_descriptor_invalid");
    assert_eq!(
        registry(vec![good.clone(), good])
            .err()
            .unwrap()
            .details
            .reason,
        "capability_descriptor_duplicate"
    );
}
#[test]
fn no_hay_mapping_o_estimador_implicito_y_un_estimador_invalido_no_se_anuncia() {
    let good = fixtures()["echo"].clone();
    let bundle = json!({"schema_version":"1.0","entries":[good]});
    let mappings = BTreeSet::from(["reference.http.echo.v1".into()]);
    assert_eq!(
        CapabilityRegistry::new(bundle.clone(), BTreeMap::new(), &mappings)
            .err()
            .unwrap()
            .details
            .reason,
        "capability_estimator_missing"
    );
    let estimators = BTreeMap::from([(
        "reference.echo.buffers.v1".into(),
        Arc::new(estimate) as Estimator,
    )]);
    assert_eq!(
        CapabilityRegistry::new(bundle.clone(), estimators, &BTreeSet::new())
            .err()
            .unwrap()
            .details
            .reason,
        "capability_mapping_missing"
    );
    let estimators = BTreeMap::from([(
        "reference.echo.buffers.v1".into(),
        Arc::new(|_: &Value| Ok(json!({}))) as Estimator,
    )]);
    let bad = CapabilityRegistry::new(bundle, estimators, &mappings).unwrap();
    assert_eq!(
        bad.check_advertised(&["vendor.reference.echo".into()])
            .err()
            .unwrap()
            .details
            .reason,
        "capability_estimator_failed"
    );
}
#[test]
fn input_precede_al_estimador_y_output_privado_no_sale_en_el_error() {
    let f = fixtures();
    let registry = registry(vec![f["echo"].clone()]).unwrap();
    assert_eq!(
        registry
            .estimate("vendor.reference.echo", &json!({"text":1}))
            .err()
            .unwrap()
            .details
            .reason,
        "capability_input_invalid"
    );
    let marker = "private-fixture-value-no-error-echo";
    let error = registry
        .output("vendor.reference.echo", &json!({"echo":{"secret":marker}}))
        .unwrap_err();
    assert_eq!(error.details.reason, "capability_output_invalid");
    assert!(!serde_json::to_string(&error).unwrap().contains(marker));
}
