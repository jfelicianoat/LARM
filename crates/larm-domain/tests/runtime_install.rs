#![allow(clippy::unwrap_used, clippy::expect_used)]
use chrono::{DateTime, Utc};
use larm_domain::artifacts::Plan;
use larm_domain::error::ErrorCode;
use larm_domain::runtime::InstallationStrategy;
use larm_domain::runtime_install::{
    plan_hash, validate_plan_reply, ExecutePlan, InstallRequest, VersionQuery,
};
use serde_json::{json, Value};

fn request() -> Value {
    json!({"runtime_id":"0192aaaa-0000-7000-8000-000000000001", "upstream_version":"b7142+vendor/42", "strategy":"UV", "storage_location_id":"0192aaaa-0000-7000-8000-000000000002", "profile_lock_hash":"a".repeat(64), "external_host_id":null})
}

fn plan() -> Plan {
    let mut plan: Plan = serde_json::from_value(json!({
        "plan_id":"0192aaaa-0000-7000-8000-000000000003",
        "plan_hash":"",
        "operation":"runtime.install",
        "resource_ids":[request()["runtime_id"],request()["storage_location_id"]],
        "steps":[{"phase":"DOWNLOAD","adapter_id":"fixture.runtime","request":{}}],
        "expires_at":"2026-10-20T14:15:00.000Z",
        "approval_required":false,
        "storage_estimate":{"download_bytes":null,"final_artifact_bytes":null,"installed_bytes":null,"derived_cache_bytes":null,"temporary_peak_bytes":null,"existing_bytes":null,"peak_extra_bytes":null,"required_free_bytes":null,"upper_bound_bytes":null,"confidence":"UNKNOWN","reason":"fixture_only","evidence_refs":[],"per_volume":[]}
    }))
    .unwrap();
    plan.plan_hash = plan_hash(&plan).unwrap();
    plan
}
fn planning_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-20T14:00:00.000Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn plan_de_instalador_valida_hash_operacion_recursos_y_expiracion() {
    let request: InstallRequest = serde_json::from_value(request()).unwrap();
    validate_plan_reply(&request, &plan(), planning_time()).unwrap();

    let mut bad = plan();
    bad.plan_hash = "0".repeat(64);
    assert_eq!(
        validate_plan_reply(&request, &bad, planning_time())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_plan_hash_mismatch"
    );

    let mut bad = plan();
    bad.operation = "runtime.update".into();
    bad.plan_hash = plan_hash(&bad).unwrap();
    assert_eq!(
        validate_plan_reply(&request, &bad, planning_time())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_plan_reply_invalid"
    );

    let mut bad = plan();
    bad.resource_ids
        .retain(|id| id != &request.storage_location_id);
    bad.plan_hash = plan_hash(&bad).unwrap();
    assert_eq!(
        validate_plan_reply(&request, &bad, planning_time())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_plan_reply_invalid"
    );

    let mut bad = plan();
    bad.expires_at = "not-a-time".into();
    bad.plan_hash = plan_hash(&bad).unwrap();
    assert_eq!(
        validate_plan_reply(&request, &bad, planning_time())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_plan_expiry_invalid"
    );

    let mut expired = plan();
    expired.expires_at = "2026-10-20T13:59:59.000Z".into();
    expired.plan_hash = plan_hash(&expired).unwrap();
    let error = validate_plan_reply(&request, &expired, planning_time()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.details.reason, "runtime_install_plan_expired");
}

#[test]
fn quince_estrategias_del_schema_conservan_solicitud_y_declaracion() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/schemas/larm.schema.json"
    ))
    .unwrap();
    let strategies = schema["$defs"]["InstallRequest"]["properties"]["strategy"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(strategies.len(), 15);
    for strategy in strategies {
        let mut value = request();
        value["strategy"] = strategy.clone();
        let parsed: InstallRequest = serde_json::from_value(value.clone()).unwrap();
        parsed.validate().unwrap();
        parsed.require_advertised(&[parsed.strategy]).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        assert_eq!(
            parsed.require_advertised(&[]).unwrap_err().code,
            ErrorCode::UnsupportedCapability
        );
        let alternative = if parsed.strategy == InstallationStrategy::Uv {
            InstallationStrategy::PythonVenv
        } else {
            InstallationStrategy::Uv
        };
        assert_eq!(
            parsed
                .require_advertised(&[alternative])
                .unwrap_err()
                .details
                .reason,
            "installation_strategy_not_advertised"
        );
    }
}

#[test]
fn solicitud_cerrada_exige_todos_los_campos_incluso_host_nulo() {
    for name in request().as_object().unwrap().keys() {
        let mut bad = request();
        bad.as_object_mut().unwrap().remove(name);
        assert!(
            serde_json::from_value::<InstallRequest>(bad).is_err(),
            "{name}"
        );
    }
    let mut bad = request();
    bad["commands"] = json!(["arbitrary shell"]);
    assert!(serde_json::from_value::<InstallRequest>(bad).is_err());
    for alias in ["ARCHIVE", "BUILD_FROM_SOURCE", "WSL2", "uv", "UNKNOWN"] {
        let mut bad = request();
        bad["strategy"] = json!(alias);
        assert!(
            serde_json::from_value::<InstallRequest>(bad).is_err(),
            "{alias}"
        );
    }
}

#[test]
fn ids_hashes_y_versiones_se_validan_sin_exigir_semver() {
    for (name, value) in [
        ("runtime_id", "bad"),
        (
            "storage_location_id",
            "0192aaaa-0000-4000-8000-000000000002",
        ),
        ("upstream_version", ""),
        ("profile_lock_hash", &"A".repeat(64)),
        ("external_host_id", "host-name"),
    ] {
        let mut bad = request();
        bad[name] = json!(value);
        assert!(
            serde_json::from_value::<InstallRequest>(bad)
                .unwrap()
                .validate()
                .is_err(),
            "{name}"
        );
    }
    for version in ["b7142", "vendor:next/2026.10", "commit-abc+cuda13"] {
        let mut value = request();
        value["upstream_version"] = json!(version);
        serde_json::from_value::<InstallRequest>(value)
            .unwrap()
            .validate()
            .unwrap();
    }
}

#[test]
fn consulta_exige_cursor_nullable_y_canal_exacto() {
    let base = json!({"runtime_id": request()["runtime_id"], "channel":"commit", "cursor":null});
    serde_json::from_value::<VersionQuery>(base.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("cursor");
    assert!(serde_json::from_value::<VersionQuery>(missing).is_err());
    for bad_channel in ["COMMIT", "experimental", ""] {
        let mut bad = base.clone();
        bad["channel"] = json!(bad_channel);
        assert!(serde_json::from_value::<VersionQuery>(bad).is_err());
    }
    let mut bad = base;
    bad["cursor"] = json!("");
    assert!(serde_json::from_value::<VersionQuery>(bad)
        .unwrap()
        .validate()
        .is_err());
}

#[test]
fn ejecutar_plan_no_acepta_aprobacion_implicita_ni_campos_adicionales() {
    let base =
        json!({"plan_id":request()["runtime_id"], "plan_hash":"b".repeat(64), "approval_id":null});
    serde_json::from_value::<ExecutePlan>(base.clone())
        .unwrap()
        .validate()
        .unwrap();
    for name in ["plan_id", "plan_hash", "approval_id"] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(serde_json::from_value::<ExecutePlan>(missing).is_err());
    }
    let mut bad = base.clone();
    bad["approved"] = json!(true);
    assert!(serde_json::from_value::<ExecutePlan>(bad).is_err());
    let mut bad = base;
    bad["approval_id"] = json!("operator");
    assert!(serde_json::from_value::<ExecutePlan>(bad)
        .unwrap()
        .validate()
        .is_err());
}
