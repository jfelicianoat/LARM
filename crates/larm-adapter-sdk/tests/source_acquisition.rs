#![allow(clippy::unwrap_used, clippy::panic)]
use larm_adapter_sdk::source::*;
use larm_domain::artifacts::Plan;
use serde_json::{json, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/source-acquisition-plan.json"
    ))
    .unwrap()
}
fn request() -> DownloadPlanRequest {
    let SourceRequest::DownloadPlan(request) =
        SourceRequest::parse("source.download_plan", fixture()["request"].clone()).unwrap()
    else {
        panic!("source plan request")
    };
    request
}
fn manifest() -> ArtifactManifest {
    serde_json::from_value(fixture()["manifest"].clone()).unwrap()
}
fn plan_for(request: &DownloadPlanRequest) -> Plan {
    let artifacts = request
        .acquisition
        .as_ref()
        .unwrap()
        .select(request, &manifest())
        .unwrap();
    let total: u64 = artifacts.iter().map(|a| a.size_bytes.unwrap().get()).sum();
    let spec = SourceDownloadSpec {
        variant_id: request.variant_id.clone(),
        storage_location_id: request.storage_location_id.clone(),
        revision: serde_json::from_value(json!(request.acquisition.as_ref().unwrap().revision))
            .unwrap(),
        artifacts,
        auth: AuthResult {
            authenticated: false,
            auth_required: false,
            license_required: false,
            reason: "fixture".into(),
        },
        resumable: true,
        priority: request.priority,
        hard_budget_bytes: request.hard_budget_bytes,
    };
    let mut plan: Plan = serde_json::from_value(
        serde_json::from_str::<Value>(include_str!("../../../tests/fixtures/source/protocol.json"))
            .unwrap()["responses"]["source.download_plan"]
            .clone(),
    )
    .unwrap();
    plan.steps.clear();
    plan.resource_ids = vec![
        request.variant_id.clone(),
        request.storage_location_id.clone(),
    ];
    for field in [
        "download_bytes",
        "final_artifact_bytes",
        "peak_extra_bytes",
        "required_free_bytes",
        "upper_bound_bytes",
    ] {
        let mut value = json!(plan);
        value["storage_estimate"][field] = json!(total.to_string());
        plan = serde_json::from_value(value).unwrap();
    }
    spec.bind(request, plan, "fixture.source").unwrap()
}

#[test]
fn public_binding_selects_central_ids_without_a_private_variant_lookup() {
    let request = request();
    let binding = request.acquisition.as_ref().unwrap();
    let selected = binding.select(&request, &manifest()).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|a| a.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["config.json", "weights.bin"]
    );
    let plan = plan_for(&request);
    validate_source_plan(&request, &plan).unwrap();
    assert_eq!(download_spec(&plan).unwrap().variant_id, request.variant_id);
    assert_eq!(
        plan.steps[0].request[SOURCE_ACQUISITION_SPEC],
        json!(binding)
    );
    let mut required_only = request.clone();
    required_only.optional_artifact_ids = None;
    assert_eq!(
        binding.select(&required_only, &manifest()).unwrap().len(),
        1
    );
    validate_source_plan(&required_only, &plan_for(&required_only)).unwrap();
}

#[test]
fn invalid_binding_never_becomes_an_ignored_optional_member() {
    let original = fixture()["request"].clone();
    for mutator in 0..9 {
        let mut value = original.clone();
        match mutator {
            0 => value["acquisition"] = Value::Null,
            1 => value["source_id"] = Value::Null,
            2 => {
                value["acquisition"]["revision"]["source_id"] =
                    json!("0192aaaa-0000-7000-8000-000000000099")
            }
            3 => {
                value["acquisition"]["revision"]["revision_key"] =
                    json!("pending:0192aaaa-0000-7000-8000-000000000099")
            }
            4 => {
                value["acquisition"]["artifacts"][1]["artifact_id"] =
                    value["acquisition"]["artifacts"][0]["artifact_id"].clone()
            }
            5 => value["acquisition"]["artifacts"][1]["relative_path"] = json!("../weights.bin"),
            6 => value["optional_artifact_ids"] = json!(["0192aaaa-0000-7000-8000-000000000004"]),
            7 => value["optional_artifact_ids"] = json!(["0192aaaa-0000-7000-8000-000000000099"]),
            _ => value["acquisition"]["extra"] = true.into(),
        }
        assert!(
            SourceRequest::parse("source.download_plan", value).is_err(),
            "{mutator}"
        );
    }
}

#[test]
fn current_manifest_must_match_the_complete_pinned_mapping() {
    let request = request();
    let binding = request.acquisition.as_ref().unwrap();
    for mutator in 0..5 {
        let mut current = manifest();
        match mutator {
            0 => current.manifest_hash = "d".repeat(64),
            1 => current.revision_key = "r2".into(),
            2 => {
                current.artifacts.pop();
            }
            3 => current.artifacts[0].required = false,
            _ => current.artifacts[0].relative_path = "different.bin".into(),
        }
        assert!(binding.select(&request, &current).is_err(), "{mutator}");
    }
}

#[test]
fn returned_plan_must_echo_and_seal_exact_binding_and_selected_paths() {
    let request = request();
    let original = plan_for(&request);
    for mutator in 0..5 {
        let mut plan = original.clone();
        match mutator {
            0 => {
                plan.steps[0].request.remove(SOURCE_ACQUISITION_SPEC);
            }
            1 => {
                plan.steps[0].request[SOURCE_ACQUISITION_SPEC]["manifest_hash"] =
                    json!("e".repeat(64))
            }
            2 => {
                plan.steps[0].request[SOURCE_ACQUISITION_SPEC]["artifacts"][0]["artifact_id"] =
                    json!("0192aaaa-0000-7000-8000-000000000099")
            }
            3 => {
                plan.steps[0].request[SOURCE_DOWNLOAD_SPEC]["revision"]["revision_key"] =
                    json!("r2")
            }
            _ => {
                plan.steps[0].request[SOURCE_DOWNLOAD_SPEC]["artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
        }
        plan.plan_hash = plan_hash(&plan).unwrap();
        assert!(validate_source_plan(&request, &plan).is_err(), "{mutator}");
    }
    let mut old_request = request;
    old_request.acquisition = None;
    assert!(validate_source_plan(&old_request, &original).is_err());
}
