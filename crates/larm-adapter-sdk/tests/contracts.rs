//! El mismo corpus pasa por dos motores independientes, sin usar serde DTO como filtro.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use larm_adapter_sdk::contracts::Contracts;
use larm_domain::error::ErrorCode;
use serde_json::{json, Value};

#[test]
fn cuarenta_firmas_conservan_el_veredicto_python_independiente() {
    let contracts = Contracts::shared().unwrap();
    assert!(std::ptr::eq(contracts, Contracts::shared().unwrap()));
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/adapters/method-schema-corpus.json"
    ))
    .unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert!(cases.len() > 5000);
    let mut directions = std::collections::BTreeSet::new();
    let mut accepted = 0;
    for case in cases {
        let method = case["method"].as_str().unwrap();
        let direction = case["direction"].as_str().unwrap();
        let expected = case["valid"].as_bool().unwrap();
        let result = match direction {
            "request" => contracts.request(method, &case["value"]),
            "response" => contracts.response(method, &case["value"]),
            _ => panic!("dirección no publicada"),
        };
        assert_eq!(result.is_ok(), expected, "{method} {direction}");
        if expected {
            accepted += 1;
            directions.insert((method, direction));
        }
    }
    assert_eq!(directions.len(), 80);
    // Canonical oracle: includes the public acquisition fixture and its mutants.
    assert_eq!(accepted, 1000);
}

#[test]
fn errores_no_incluyen_el_documento_y_no_hay_fallback_para_metodos() {
    let contracts = Contracts::shared().unwrap();
    let marker = "private-p026-value-never-in-error";
    let error = contracts
        .request("benchmark.run", &json!({"token":marker}))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    assert!(!serde_json::to_string(&error).unwrap().contains(marker));
    assert_eq!(
        contracts
            .response("benchmark.run", &json!({}))
            .unwrap_err()
            .code,
        ErrorCode::ProtocolError
    );
    assert_eq!(
        contracts
            .request("benchmark.unknown", &json!({}))
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
}
