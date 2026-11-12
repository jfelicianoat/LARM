//! Resultado durable de comparación consumible por los gates de promoción.
use crate::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCompareAxis {
    RuntimeVersion,
    ModelRevision,
    Backend,
    LaunchProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BenchmarkComparisonOutcome {
    Pass,
    Fail,
    Inconclusive,
    Incomparable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkComparison {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub left_benchmark_id: String,
    pub right_benchmark_id: String,
    pub axis: BenchmarkCompareAxis,
    pub outcome: BenchmarkComparisonOutcome,
    #[serde(deserialize_with = "Option::deserialize")]
    pub left_runtime_version_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub right_runtime_version_id: Option<String>,
    pub left_fingerprint: String,
    pub right_fingerprint: String,
    pub differing_components: Vec<String>,
    pub limitations: Vec<String>,
    pub evidence_refs: Vec<String>,
}

impl BenchmarkComparison {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.id)
            || !is_uuid_v7(&self.left_benchmark_id)
            || !is_uuid_v7(&self.right_benchmark_id)
            || self.left_benchmark_id == self.right_benchmark_id
            || self.row_version < 1
            || !valid_fingerprint(&self.left_fingerprint)
            || !valid_fingerprint(&self.right_fingerprint)
            || !valid_timestamp(&self.created_at)
            || !valid_timestamp(&self.updated_at)
            || self.differing_components.iter().any(String::is_empty)
            || self.limitations.iter().any(String::is_empty)
            || self.evidence_refs.iter().any(String::is_empty)
        {
            return Err(invalid("benchmark_comparison_invalid"));
        }
        if let Some(id) = &self.left_runtime_version_id {
            if !is_uuid_v7(id) {
                return Err(invalid("benchmark_comparison_runtime_version_invalid"));
            }
        }
        if let Some(id) = &self.right_runtime_version_id {
            if !is_uuid_v7(id) {
                return Err(invalid("benchmark_comparison_runtime_version_invalid"));
            }
        }
        if self.axis == BenchmarkCompareAxis::RuntimeVersion
            && (self.left_runtime_version_id.is_none()
                || self.right_runtime_version_id.is_none()
                || self.left_runtime_version_id == self.right_runtime_version_id)
        {
            return Err(invalid("benchmark_comparison_runtime_axis_invalid"));
        }
        Ok(())
    }

    #[must_use]
    pub fn promotion_passes_for(&self, candidate_id: &str, baseline_id: &str) -> bool {
        self.axis == BenchmarkCompareAxis::RuntimeVersion
            && self.outcome == BenchmarkComparisonOutcome::Pass
            && matches!(
                (
                    self.left_runtime_version_id.as_deref(),
                    self.right_runtime_version_id.as_deref()
                ),
                (Some(left), Some(right))
                    if (left == candidate_id && right == baseline_id)
                        || (right == candidate_id && left == baseline_id)
            )
    }
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .is_some_and(|parsed| timestamp(parsed.with_timezone(&chrono::Utc)) == value)
}

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Resultado de comparación no válido",
        reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison() -> BenchmarkComparison {
        BenchmarkComparison {
            id: "0192aaaa-0000-7000-8000-000000000001".into(),
            created_at: "2026-10-25T20:00:00.000Z".into(),
            updated_at: "2026-10-25T20:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            left_benchmark_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            right_benchmark_id: "0192aaaa-0000-7000-8000-000000000003".into(),
            axis: BenchmarkCompareAxis::RuntimeVersion,
            outcome: BenchmarkComparisonOutcome::Pass,
            left_runtime_version_id: Some("0192aaaa-0000-7000-8000-000000000004".into()),
            right_runtime_version_id: Some("0192aaaa-0000-7000-8000-000000000005".into()),
            left_fingerprint: "a".repeat(64),
            right_fingerprint: "b".repeat(64),
            differing_components: vec!["runtime_version".into()],
            limitations: vec![],
            evidence_refs: vec!["benchmark:0192aaaa-0000-7000-8000-000000000002".into()],
        }
    }

    #[test]
    fn promotion_requires_pass_and_exact_candidate_baseline_pair() {
        let value = comparison();
        assert!(value.promotion_passes_for(
            "0192aaaa-0000-7000-8000-000000000005",
            "0192aaaa-0000-7000-8000-000000000004"
        ));
        let mut inconclusive = value.clone();
        inconclusive.outcome = BenchmarkComparisonOutcome::Inconclusive;
        assert!(!inconclusive.promotion_passes_for(
            "0192aaaa-0000-7000-8000-000000000005",
            "0192aaaa-0000-7000-8000-000000000004"
        ));
        assert!(!value.promotion_passes_for(
            "0192aaaa-0000-7000-8000-000000000005",
            "0192aaaa-0000-7000-8000-000000000099"
        ));
    }

    #[test]
    fn unknown_or_malformed_comparison_fails_closed() {
        let mut value = comparison();
        value.left_fingerprint = "unknown".into();
        assert_eq!(
            value
                .validate()
                .err()
                .as_ref()
                .map(|error| error.details.reason.as_str()),
            Some("benchmark_comparison_invalid")
        );
        let mut value = comparison();
        value.right_runtime_version_id = None;
        assert_eq!(
            value
                .validate()
                .err()
                .as_ref()
                .map(|error| error.details.reason.as_str()),
            Some("benchmark_comparison_runtime_axis_invalid")
        );
    }
}
