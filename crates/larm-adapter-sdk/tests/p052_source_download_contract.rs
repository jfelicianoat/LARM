use larm_adapter_sdk::source::{DownloadPlanRequest, SourceArtifact, SourceRequest};
use larm_domain::model::{ArtifactSizeConfidence, ArtifactSizeEstimate, ArtifactSizeSource};
use larm_domain::units::DecimalU64;
use serde_json::json;

const VARIANT: &str = "019b0f9a-2d00-7000-8000-000000000001";
const LOCATION: &str = "019b0f9a-2d00-7000-8000-000000000002";
const SOURCE: &str = "019b0f9a-2d00-7000-8000-000000000003";
const OPTIONAL: &str = "019b0f9a-2d00-7000-8000-000000000004";

#[test]
fn source_download_request_preserves_optional_ids_and_omission(
) -> Result<(), Box<dyn std::error::Error>> {
    let explicit = json!({
        "variant_id": VARIANT,
        "storage_location_id": LOCATION,
        "hard_budget_bytes": null,
        "source_id": SOURCE,
        "priority": "NORMAL",
        "optional_artifact_ids": [OPTIONAL]
    });
    let parsed = SourceRequest::parse("source.download_plan", explicit.clone())?;
    assert_eq!(parsed.params()?, explicit);

    let omitted = json!({
        "variant_id": VARIANT,
        "storage_location_id": LOCATION,
        "hard_budget_bytes": null,
        "source_id": SOURCE,
        "priority": "NORMAL"
    });
    let parsed = SourceRequest::parse("source.download_plan", omitted.clone())?;
    assert_eq!(parsed.params()?, omitted);
    Ok(())
}

#[test]
fn source_download_request_rejects_duplicate_and_invalid_optional_ids() {
    for ids in [json!([OPTIONAL, OPTIONAL]), json!(["not-a-uuidv7"])] {
        let request = json!({
            "variant_id": VARIANT,
            "storage_location_id": LOCATION,
            "hard_budget_bytes": null,
            "source_id": SOURCE,
            "priority": "NORMAL",
            "optional_artifact_ids": ids
        });
        assert!(SourceRequest::parse("source.download_plan", request).is_err());
    }
}

#[test]
fn source_artifact_accepts_an_evidenced_size_interval() -> Result<(), Box<dyn std::error::Error>> {
    let artifact: SourceArtifact = serde_json::from_value(json!({
        "relative_path": "model/weights.bin",
        "size_bytes": null,
        "size_estimate": {
            "confidence": "ESTIMATED",
            "bytes": null,
            "lower_bound_bytes": "1024",
            "upper_bound_bytes": "2048",
            "source": "LOCAL_OBSERVATION",
            "evidence_refs": ["fixture:range-1"],
            "reason": null
        },
        "sha256": null,
        "required": false,
        "role": "weights"
    }))?;
    let estimate = artifact.size_estimate.ok_or("size estimate is required")?;
    assert_eq!(estimate.confidence, ArtifactSizeConfidence::Estimated);
    assert_eq!(estimate.lower_bound_bytes, DecimalU64::new(1024));
    assert_eq!(estimate.upper_bound_bytes, DecimalU64::new(2048));
    assert_eq!(estimate.source, ArtifactSizeSource::LocalObservation);
    estimate.validate()?;
    Ok(())
}

#[test]
fn source_artifact_rejects_a_reversed_size_interval() {
    let estimate = ArtifactSizeEstimate {
        confidence: ArtifactSizeConfidence::Estimated,
        bytes: None,
        lower_bound_bytes: DecimalU64::new(2048),
        upper_bound_bytes: DecimalU64::new(1024),
        source: ArtifactSizeSource::LocalObservation,
        evidence_refs: vec!["fixture:bad-range".into()],
        reason: None,
    };
    assert!(estimate.validate().is_err());
}

#[test]
fn download_plan_request_type_matches_contract_fields() -> Result<(), Box<dyn std::error::Error>> {
    let request: DownloadPlanRequest = serde_json::from_value(json!({
        "variant_id": VARIANT,
        "storage_location_id": LOCATION,
        "hard_budget_bytes": null,
        "source_id": SOURCE,
        "priority": "NORMAL",
        "optional_artifact_ids": []
    }))?;
    assert_eq!(request.optional_artifact_ids, Some(vec![]));
    Ok(())
}
