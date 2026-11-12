//! Valida los nuevos DTOs HTTP P028 contra el schema contractual publicado.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use jsonschema::{Retrieve, Uri};
use serde_json::{json, Value};

struct ContractRetriever;

impl Retrieve for ContractRetriever {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        if uri.as_str().contains("domain.schema.json") {
            return serde_json::from_str(include_str!(
                "../../../docs/contracts/schemas/domain.schema.json"
            ))
            .map_err(Into::into);
        }
        Err(format!("schema contractual no disponible: {uri}").into())
    }
}

fn definition(name: &str) -> jsonschema::Validator {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/schemas/larm.schema.json"
    ))
    .unwrap();
    let schema = json!({
        "$schema": contract["$schema"],
        "$ref": format!("#/$defs/{name}"),
        "$defs": contract["$defs"],
    });
    jsonschema::draft202012::options()
        .with_base_uri("https://larm.invalid/contracts/schemas/larm.schema.json")
        .with_retriever(ContractRetriever)
        .build(&schema)
        .unwrap()
}

#[test]
fn sync_request_receipt_y_estado_cumplen_schemas_cerrados() {
    let request = definition("RegistrySyncRequest");
    assert!(request.is_valid(&json!({})));
    assert!(!request.is_valid(&json!({"registry_url":"https://attacker.invalid/"})));

    let receipt = definition("RegistrySyncReceipt");
    let value = json!({
        "registry_id":"fixture-v1",
        "sequence":1,
        "payload_sha256":"a".repeat(64),
        "accepted_at":"2026-10-24T10:00:00.000Z",
        "expires_at":"2026-10-25T10:00:00.000Z",
        "package_count":0,
        "revocation_count":0,
        "replay":false
    });
    assert!(receipt.is_valid(&value));
    assert!(!receipt.is_valid(&json!({"registry_id":"fixture-v1"})));
    let mut extra = value.clone();
    extra["trusted"] = json!(true);
    assert!(!receipt.is_valid(&extra));

    let status = definition("RegistryStatus");
    assert!(status.is_valid(&json!({
        "enabled":false,
        "registry_url":null,
        "checkpoint":null,
        "revocations":[]
    })));
}
