//! Existing evaluation_reference contract. Validation does not authenticate an
//! import, verify a signature, or confer certification (P116).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvaluationDirection {
    HigherBetter,
    LowerBetter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvaluationStatus {
    Pass,
    Fail,
    Inconclusive,
    Blocked,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationScale {
    pub minimum: f64,
    pub maximum: f64,
    pub direction: EvaluationDirection,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationReference {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub source_system: String,
    pub external_id: String,
    pub source_version: String,
    pub experiment_id: String,
    pub suite_hash: String,
    pub dataset_hash: String,
    pub variant_id: String,
    pub execution_fingerprint: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub score: Option<f64>,
    pub scale: EvaluationScale,
    pub status: EvaluationStatus,
    pub artifact_refs: Vec<String>,
    pub issued_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("referencia de evaluación inválida: {0}")]
pub struct EvaluationError(pub &'static str);

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn timestamp(value: &str) -> bool {
    value.len() == 24
        && chrono::DateTime::parse_from_rfc3339(value)
            .is_ok_and(|at| crate::bridge::timestamp(at.with_timezone(&chrono::Utc)) == value)
}

impl EvaluationReference {
    pub fn validate(&self) -> Result<(), EvaluationError> {
        if !crate::bridge::is_uuid_v7(&self.id) || !crate::bridge::is_uuid_v7(&self.variant_id) {
            return Err(EvaluationError("identity"));
        }
        if ![&self.created_at, &self.updated_at, &self.issued_at]
            .into_iter()
            .all(|s| timestamp(s))
            || self.row_version < 1
        {
            return Err(EvaluationError("record_version_or_timestamp"));
        }
        if [
            &self.source_system,
            &self.external_id,
            &self.source_version,
            &self.experiment_id,
        ]
        .into_iter()
        .any(|s| s.is_empty())
            || self.signature.as_ref().is_some_and(String::is_empty)
        {
            return Err(EvaluationError("provenance"));
        }
        if ![
            &self.suite_hash,
            &self.dataset_hash,
            &self.execution_fingerprint,
        ]
        .into_iter()
        .all(|s| hash(s))
        {
            return Err(EvaluationError("hash"));
        }
        self.normalized_score().map(|_| ())
    }

    /// Contract 05: normalize an explicit scale, preserving an absent score.
    /// PASS/comparability and trusted import are separate caller obligations.
    pub fn normalized_score(&self) -> Result<Option<f64>, EvaluationError> {
        let scale = &self.scale;
        if !scale.minimum.is_finite()
            || !scale.maximum.is_finite()
            || scale.maximum <= scale.minimum
            || scale.unit.is_empty()
        {
            return Err(EvaluationError("scale"));
        }
        let Some(value) = self.score else {
            return Ok(None);
        };
        if !value.is_finite() || value < scale.minimum || value > scale.maximum {
            return Err(EvaluationError("score_out_of_scale"));
        }
        let span = scale.maximum - scale.minimum;
        let ratio = if span.is_finite() {
            (value - scale.minimum) / span
        } else {
            // Finite endpoints can have a span larger than f64::MAX.
            (value / 2.0 - scale.minimum / 2.0) / (scale.maximum / 2.0 - scale.minimum / 2.0)
        };
        let ratio = ratio.clamp(0.0, 1.0);
        Ok(Some(
            100.0
                * match scale.direction {
                    EvaluationDirection::HigherBetter => ratio,
                    EvaluationDirection::LowerBetter => 1.0 - ratio,
                },
        ))
    }
}
