#![allow(clippy::unwrap_used)]
use larm_domain::evaluation::{EvaluationDirection, EvaluationReference};
use serde_json::Value;

fn reference() -> EvaluationReference {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/evaluation-reference.json"
    ))
    .unwrap()
}

#[test]
fn fixture_roundtrip_preserves_the_existing_closed_contract() {
    let original: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/evaluation-reference.json"
    ))
    .unwrap();
    let value = reference();
    value.validate().unwrap();
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(
        larm_domain::canonical::sha256_hex(&encoded).unwrap(),
        larm_domain::canonical::sha256_hex(&original).unwrap()
    );
    assert_eq!(
        serde_json::from_value::<EvaluationReference>(encoded).unwrap(),
        value
    );
}

#[test]
fn explicit_scales_normalize_both_directions_and_endpoints() {
    let mut r = reference();
    assert_eq!(r.normalized_score().unwrap(), Some(75.0));
    r.scale.minimum = -2.0;
    r.scale.maximum = 2.0;
    r.score = Some(-1.0);
    assert_eq!(r.normalized_score().unwrap(), Some(25.0));
    r.scale.direction = EvaluationDirection::LowerBetter;
    assert_eq!(r.normalized_score().unwrap(), Some(75.0));
    r.score = Some(-2.0);
    assert_eq!(r.normalized_score().unwrap(), Some(100.0));
    r.score = Some(2.0);
    assert_eq!(r.normalized_score().unwrap(), Some(0.0));
}

#[test]
fn finite_extreme_scale_does_not_overflow_its_span() {
    let mut r = reference();
    r.scale.minimum = -f64::MAX;
    r.scale.maximum = f64::MAX;
    r.score = Some(0.0);
    assert_eq!(r.normalized_score().unwrap(), Some(50.0));
}

#[test]
fn missing_score_remains_null_but_invalid_scale_is_rejected() {
    let mut r = reference();
    r.score = None;
    r.validate().unwrap();
    assert_eq!(r.normalized_score().unwrap(), None);
    r.scale.maximum = r.scale.minimum;
    assert!(r.validate().is_err());
}

#[test]
fn rejects_out_of_range_nonfinite_and_ambiguous_scales() {
    for value in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        let mut r = reference();
        r.score = Some(value);
        assert!(r.validate().is_err());
    }
    for value in [-1.0, 0.0, f64::NAN, f64::INFINITY] {
        let mut r = reference();
        r.scale.maximum = value;
        assert!(r.validate().is_err());
    }
}

#[test]
fn nullable_fields_are_required_and_unknown_fields_are_rejected() {
    let original = serde_json::to_value(reference()).unwrap();
    for field in ["score", "signature", "suite_hash", "source_version"] {
        let mut value = original.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<EvaluationReference>(value).is_err(),
            "{field}"
        );
    }
    let mut value = original;
    value["popularity_score"] = 100.into();
    assert!(serde_json::from_value::<EvaluationReference>(value).is_err());
}

#[test]
fn invalid_identity_hash_provenance_timestamp_and_version_fail() {
    let original = serde_json::to_value(reference()).unwrap();
    for (field, invalid) in [
        ("id", Value::from("latest")),
        ("variant_id", Value::from("model")),
        ("suite_hash", Value::from("A".repeat(64))),
        ("dataset_hash", Value::from("short")),
        ("source_system", Value::from("")),
        ("source_version", Value::from("")),
        ("signature", Value::from("")),
        ("created_at", Value::from("2026-02-30T02:00:00.000Z")),
        ("issued_at", Value::from("2026-10-30T02:00:00+00:00")),
        ("row_version", Value::from(0)),
    ] {
        let mut value = original.clone();
        value[field] = invalid;
        let r: EvaluationReference = serde_json::from_value(value).unwrap();
        assert!(r.validate().is_err(), "{field}");
    }
}
