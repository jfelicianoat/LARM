//! Sólo datos de transporte: no benchmark de inferencia ni cualificación.
#![allow(dead_code)]
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn request(amount: u64) -> Value {
    let id = "019a0000-0000-7000-8000-000000000001";
    json!({"variant_id":id,"runtime_version_id":id,"backend_id":id,"launch_profile_id":id,
        "suite_id":format!("fixture.transport:{amount}"),"fixture_hash":"a".repeat(64),"settings_hash":"b".repeat(64)})
}
pub fn amount(request: &Value) -> u64 {
    request["suite_id"]
        .as_str()
        .and_then(|s| s.strip_prefix("fixture.transport:"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
pub fn describe(env: Vec<String>) -> Value {
    json!({"suite_id":"fixture.transport","suite_hash":hex::encode(Sha256::digest(b"fixture.transport")),
        "capabilities":[],"settings_schema":{"env":env,"scope":"transport fixture only"}})
}
pub fn output(request: &Value, metrics: Value) -> Value {
    let components = json!({"fixture":{"value":"transport fixture only","status":"DECLARED","evidence_refs":[]}});
    let hash = hex::encode(Sha256::digest(components.to_string().as_bytes()));
    json!({"benchmark_id":uuid::Uuid::now_v7().to_string(),"fingerprint":{"hash":hash,"components":components},
        "suite_hash":hex::encode(Sha256::digest(b"fixture.transport")),"fixture_hash":request["fixture_hash"],"settings_hash":request["settings_hash"],
        "raw_samples":[],"artifact_refs":[],"metrics":metrics,"status":"BLOCKED_REAL"})
}

/// Contrato explícito de la fixture de transporte, nunca de inferencia.
pub fn registry(
    capabilities: &[String],
) -> Result<Option<larm_adapter_sdk::capabilities::CapabilityRegistry>, larm_domain::error::LarmError>
{
    use larm_adapter_sdk::capabilities::{digest, CapabilityRegistry, Estimator};
    use larm_domain::error::{ErrorCode, LarmError};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    if capabilities.is_empty() {
        return Ok(None);
    }
    let input: Value = serde_json::from_str(include_str!(
        "../../../docs/adapters/method-schemas/benchmark.run.request.schema.json"
    ))
    .map_err(|_| {
        LarmError::new(
            ErrorCode::Internal,
            "Schema fixture inválido",
            "fixture_contract_invalid",
        )
    })?;
    let output_schema: Value = serde_json::from_str(include_str!(
        "../../../docs/adapters/method-schemas/benchmark.run.response.schema.json"
    ))
    .map_err(|_| {
        LarmError::new(
            ErrorCode::Internal,
            "Schema fixture inválido",
            "fixture_contract_invalid",
        )
    })?;
    let request = request(0);
    let witness = json!({"input":request,"output":output(&request,json!({}))});
    let entries=capabilities.iter().map(|id| Ok(json!({"id":id,"version":"1.0","input_schema":input,"output_schema":output_schema,"input_sha256":digest(&input)?,"output_sha256":digest(&output_schema)?,"mapping":"fixture.transport.benchmark.v1","estimator_id":"fixture.transport.buffers.v1","fixture":witness,"fixture_sha256":digest(&witness)?}))).collect::<Result<Vec<_>,LarmError>>()?;
    let estimate: Estimator = Arc::new(|_| {
        Ok(
            json!({"weights_bytes":null,"kv_bytes":null,"activation_bytes":null,"workspace_bytes":"1048576","overhead_bytes":null,"total_bytes":null,"multimodal_bytes":null,"temporary_bytes":null,"safety_margin_bytes":null,"per_pool":[],"confidence":"UNKNOWN","evidence_refs":["fixture:transport-only-process-memory-unknown"]}),
        )
    });
    Ok(Some(CapabilityRegistry::new(
        json!({"schema_version":"1.0","entries":entries}),
        BTreeMap::from([("fixture.transport.buffers.v1".into(), estimate)]),
        &BTreeSet::from(["fixture.transport.benchmark.v1".into()]),
    )?))
}
