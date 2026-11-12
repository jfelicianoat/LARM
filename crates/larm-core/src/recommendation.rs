//! P092 independent pure composition. Inputs must come from the existing
//! authorized producers; this module performs no import, qualification, IO,
//! promotion or admission. Its internal diagnostics are not a new wire DTO.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use larm_domain::{
    compatibility::CompatibilityRecord,
    estimate::Confidence,
    evaluation::{EvaluationReference, EvaluationStatus},
    locations::{LocationState, StorageLocation},
    model::{CanonicalModel, ModelDistribution, ModelRevision, ModelVariant, Trust},
    runtime::{
        BackendStatus, InstallationState, RuntimeBackend, RuntimeInstallation, RuntimeVersion,
        RuntimeVersionLifecycle,
    },
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    compatibility::score::{
        calculate_technical_fit_with_weights, ComponentBasis, TechnicalFitInput, TechnicalFitResult,
    },
    config::{FitWeights, RecommendationWeights},
};

/// Complete joined candidate; no identities are inferred from names/metadata.
pub struct Candidate<'a> {
    pub model: &'a CanonicalModel,
    pub distribution: &'a ModelDistribution,
    pub revision: &'a ModelRevision,
    pub variant: &'a ModelVariant,
    pub runtime: &'a RuntimeVersion,
    pub installation: &'a RuntimeInstallation,
    pub backend: &'a RuntimeBackend,
    pub storage: &'a StorageLocation,
    pub source_trust: Trust,
    pub compatibility: &'a CompatibilityRecord,
    pub technical: &'a TechnicalFitInput,
    /// Persisted external reference supplied by P116, never arbitrary model metadata.
    pub evaluation: Option<&'a EvaluationReference>,
}

pub struct RecommendationPolicy<'a> {
    pub settings_revision: i64,
    pub technical_weights: &'a FitWeights,
    pub recommendation_weights: &'a RecommendationWeights,
    pub hardware_fingerprint: &'a str,
    pub launch_profile_hash: &'a str,
    /// Both are required to compare functional evaluations within this result.
    pub evaluation_context: Option<(&'a str, &'a str)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateIdentity {
    pub canonical_model_id: String,
    pub distribution_id: String,
    pub source_id: String,
    pub revision_id: String,
    pub variant_id: String,
    pub bundle_hash: Option<String>,
    pub runtime_id: String,
    pub runtime_version_id: String,
    pub installation_id: String,
    pub backend_id: String,
    pub storage_location_id: String,
    pub execution_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RankedCandidate {
    pub identity: CandidateIdentity,
    pub technical_fit: Option<f64>,
    pub coverage_percent: f64,
    pub confidence: Confidence,
    pub headroom_bytes: Option<larm_domain::units::DecimalU64>,
    pub evaluation_score: Option<f64>,
    pub evaluation_reference_id: Option<String>,
    pub recommendation_score: Option<f64>,
    /// Technical eligibility only; not permission to load, execute or promote.
    pub technical_eligible: bool,
    pub reasons: Vec<String>,
    pub equivalent_sources: Vec<CandidateIdentity>,
    #[serde(skip)]
    source_trust: Trust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RankingBasis {
    Technical,
    TechnicalAndEvaluation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecommendationResult {
    pub settings_revision: i64,
    pub technical_weights: FitWeights,
    pub recommendation_weights: RecommendationWeights,
    pub hardware_fingerprint: String,
    pub launch_profile_hash: String,
    pub evaluation_context: Option<(String, String)>,
    pub ranking_basis: RankingBasis,
    pub candidates: Vec<RankedCandidate>,
    pub excluded: Vec<RankedCandidate>,
}

#[derive(Debug, Error)]
#[error("candidato de recomendación inválido: {0}")]
pub struct RecommendationError(pub String);

fn invalid(reason: &str) -> RecommendationError {
    RecommendationError(reason.into())
}

fn validate_join(
    c: &Candidate<'_>,
    policy: &RecommendationPolicy<'_>,
) -> Result<(), RecommendationError> {
    use larm_domain::bridge::is_uuid_v7;
    for validation in [
        c.model.validate(),
        c.distribution.validate(),
        c.revision.validate(),
        c.variant.validate(),
        c.runtime.validate(),
        c.installation.validate(),
        c.backend.validate(),
    ] {
        validation.map_err(|error| RecommendationError(error.to_string()))?;
    }
    if ![
        &c.model.id,
        &c.distribution.id,
        &c.distribution.source_id,
        &c.revision.id,
        &c.variant.id,
        &c.runtime.id,
        &c.runtime.runtime_id,
        &c.installation.id,
        &c.backend.id,
        &c.storage.id,
    ]
    .into_iter()
    .all(|id| is_uuid_v7(id))
    {
        return Err(invalid("candidate_identity"));
    }
    if c.distribution.canonical_model_id != c.model.id
        || c.revision.distribution_id != c.distribution.id
        || c.variant.revision_id != c.revision.id
        || c.installation.runtime_version_id != c.runtime.id
        || c.backend.runtime_installation_id != c.installation.id
        || c.compatibility.variant_id != c.variant.id
        || c.compatibility.runtime_version_id != c.runtime.id
        || c.compatibility.backend_id != c.backend.id
        || c.compatibility.hardware_fingerprint != policy.hardware_fingerprint
        || c.compatibility.launch_profile_hash != policy.launch_profile_hash
        || c.technical.storage_class != c.storage.performance_class
    {
        return Err(invalid("candidate_context_mismatch"));
    }
    if c.variant.bundle_hash.as_ref().is_some_and(|s| !sha256(s))
        || c.technical
            .execution_fingerprint
            .as_ref()
            .is_some_and(|s| !sha256(s))
    {
        return Err(invalid("candidate_hash"));
    }
    Ok(())
}

fn sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn identity(c: &Candidate<'_>) -> CandidateIdentity {
    CandidateIdentity {
        canonical_model_id: c.model.id.clone(),
        distribution_id: c.distribution.id.clone(),
        source_id: c.distribution.source_id.clone(),
        revision_id: c.revision.id.clone(),
        variant_id: c.variant.id.clone(),
        bundle_hash: c.variant.bundle_hash.clone(),
        runtime_id: c.runtime.runtime_id.clone(),
        runtime_version_id: c.runtime.id.clone(),
        installation_id: c.installation.id.clone(),
        backend_id: c.backend.id.clone(),
        storage_location_id: c.storage.id.clone(),
        execution_fingerprint: c.technical.execution_fingerprint.clone(),
    }
}

fn assessment(
    c: &Candidate<'_>,
    p: &RecommendationPolicy<'_>,
) -> Result<RankedCandidate, RecommendationError> {
    validate_join(c, p)?;
    let fit =
        calculate_technical_fit_with_weights(c.compatibility, c.technical, p.technical_weights)
            .map_err(|e| RecommendationError(e.to_string()))?;
    let mut reasons = Vec::new();
    let infrastructure_valid = c.source_trust != Trust::Revoked
        && matches!(
            c.installation.state,
            InstallationState::Validated | InstallationState::Active
        )
        && matches!(
            c.runtime.lifecycle,
            RuntimeVersionLifecycle::Validated
                | RuntimeVersionLifecycle::Default
                | RuntimeVersionLifecycle::Retained
        )
        && c.backend.status == BackendStatus::Available
        && !c.backend.evidence_refs.is_empty()
        && matches!(
            c.storage.state,
            LocationState::Online
                | LocationState::ReadOnly
                | LocationState::LowSpace
                | LocationState::Degraded
        )
        && !c.storage.volume_key.is_empty()
        && !c
            .storage
            .volume_key
            .starts_with(larm_domain::locations::UNKNOWN_VOLUME_PREFIX)
        && c.variant.bundle_hash.is_some()
        && matches!(
            c.variant.lifecycle.as_str(),
            "VERIFIED" | "PREPARED" | "TECHNICALLY_VALIDATED" | "AVAILABLE" | "RUNNING"
        );
    if !infrastructure_valid {
        reasons.push("CANDIDATE_NOT_CURRENTLY_OPERABLE".into());
    }
    if let Some(reason) = fit.not_applicable_reason {
        reasons.push(reason.into());
    }
    if !fit.coverage_gate_passed {
        reasons.push("INSUFFICIENT_TECHNICAL_COVERAGE".into());
    }
    let technical_fit = infrastructure_valid.then_some(fit.score).flatten();
    let (evaluation_score, evaluation_reference_id) = evaluation(c, p, &mut reasons)?;
    let weights = p.recommendation_weights;
    let denominator = u64::from(weights.technical) + u64::from(weights.evaluation);
    let recommendation_score = match (technical_fit, evaluation_score) {
        (Some(technical), Some(evaluation)) if denominator > 0 => Some(
            (technical * f64::from(weights.technical) + evaluation * f64::from(weights.evaluation))
                / denominator as f64,
        ),
        _ => None,
    };
    if denominator == 0 {
        reasons.push("NO_POSITIVE_RECOMMENDATION_WEIGHT".into());
    }
    reasons.extend(c.compatibility.limitations.iter().cloned());
    Ok(RankedCandidate {
        identity: identity(c),
        technical_fit,
        coverage_percent: fit.coverage_percent,
        confidence: fit.confidence,
        headroom_bytes: headroom(&fit).and_then(larm_domain::units::DecimalU64::new),
        evaluation_score,
        evaluation_reference_id,
        recommendation_score,
        technical_eligible: technical_fit.is_some() && fit.coverage_gate_passed && denominator > 0,
        reasons,
        equivalent_sources: Vec::new(),
        source_trust: c.source_trust,
    })
}

fn headroom(fit: &TechnicalFitResult) -> Option<u64> {
    fit.breakdown.iter().find_map(|part| match part.basis {
        ComponentBasis::Memory { headroom_bytes, .. } => headroom_bytes,
        _ => None,
    })
}

fn evaluation(
    c: &Candidate<'_>,
    p: &RecommendationPolicy<'_>,
    reasons: &mut Vec<String>,
) -> Result<(Option<f64>, Option<String>), RecommendationError> {
    let Some(reference) = c.evaluation else {
        reasons.push("NO EVALUADO: sin evaluación funcional".into());
        return Ok((None, None));
    };
    reference
        .validate()
        .map_err(|e| RecommendationError(e.to_string()))?;
    let compatible = p.evaluation_context.is_some_and(|(suite, dataset)| {
        suite == reference.suite_hash && dataset == reference.dataset_hash
    }) && reference.variant_id == c.variant.id
        && c.technical.execution_fingerprint.as_deref()
            == Some(reference.execution_fingerprint.as_str())
        && reference.status == EvaluationStatus::Pass;
    if !compatible {
        reasons.push("NO EVALUADO: referencia no comparable o no vigente".into());
        return Ok((None, None));
    }
    let score = reference
        .normalized_score()
        .map_err(|e| RecommendationError(e.to_string()))?;
    if score.is_none() {
        reasons.push("NO EVALUADO: referencia sin puntuación".into());
    }
    Ok((score, score.map(|_| reference.id.clone())))
}

fn trust_rank(trust: Trust) -> u8 {
    match trust {
        Trust::Verified => 0,
        Trust::Declared => 1,
        Trust::Untrusted => 2,
        Trust::Revoked => 3,
    }
}

fn equivalent(a: &RankedCandidate, b: &RankedCandidate) -> bool {
    let x = &a.identity;
    let y = &b.identity;
    x.bundle_hash.is_some()
        && x.bundle_hash == y.bundle_hash
        && x.canonical_model_id == y.canonical_model_id
        && x.runtime_version_id == y.runtime_version_id
        && x.installation_id == y.installation_id
        && x.backend_id == y.backend_id
        && x.storage_location_id == y.storage_location_id
        && x.execution_fingerprint.is_some()
        && x.execution_fingerprint == y.execution_fingerprint
        && a.technical_fit == b.technical_fit
        && a.coverage_percent == b.coverage_percent
        && a.confidence == b.confidence
        && a.headroom_bytes == b.headroom_bytes
        && a.evaluation_score == b.evaluation_score
        && a.recommendation_score == b.recommendation_score
        && a.reasons == b.reasons
}

fn descending(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn tie(a: &RankedCandidate, b: &RankedCandidate) -> Ordering {
    descending(a.technical_fit, b.technical_fit)
        .then_with(|| b.headroom_bytes.cmp(&a.headroom_bytes))
        .then_with(|| a.identity.variant_id.cmp(&b.identity.variant_id))
        .then_with(|| {
            a.identity
                .runtime_version_id
                .cmp(&b.identity.runtime_version_id)
        })
        .then_with(|| a.identity.backend_id.cmp(&b.identity.backend_id))
        .then_with(|| {
            a.identity
                .storage_location_id
                .cmp(&b.identity.storage_location_id)
        })
        .then_with(|| a.identity.distribution_id.cmp(&b.identity.distribution_id))
}

/// Rank the complete candidate set before top-N. A mixed evaluation set uses
/// technical ordering throughout, never comparing a mixed score to a raw fit.
/// Equivalent source choices are resolved before ranking; all alternative IDs
/// remain visible. This does not authorize a mirror, download or execution.
pub fn recommend(
    candidates: &[Candidate<'_>],
    policy: &RecommendationPolicy<'_>,
    top_n: usize,
) -> Result<RecommendationResult, RecommendationError> {
    if policy.settings_revision < 1
        || !sha256(policy.hardware_fingerprint)
        || !sha256(policy.launch_profile_hash)
        || policy
            .evaluation_context
            .is_some_and(|(suite, dataset)| !sha256(suite) || !sha256(dataset))
    {
        return Err(invalid("recommendation_policy"));
    }
    let mut identities = BTreeSet::new();
    for c in candidates {
        if !identities.insert((&c.variant.id, &c.runtime.id, &c.backend.id, &c.storage.id)) {
            return Err(invalid("duplicate_candidate_tuple"));
        }
    }
    let mut all = candidates
        .iter()
        .map(|c| assessment(c, policy))
        .collect::<Result<Vec<_>, _>>()?;
    all.sort_by(|a, b| {
        trust_rank(a.source_trust)
            .cmp(&trust_rank(b.source_trust))
            .then_with(|| a.identity.source_id.cmp(&b.identity.source_id))
            .then_with(|| tie(a, b))
    });
    let (eligible, mut excluded): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|c| c.technical_eligible);
    let mut ranked: Vec<RankedCandidate> = Vec::new();
    for candidate in eligible {
        if let Some(previous) = ranked.iter_mut().find(|p| equivalent(p, &candidate)) {
            if previous.identity != candidate.identity
                && !previous.equivalent_sources.contains(&candidate.identity)
            {
                previous.equivalent_sources.push(candidate.identity);
            }
        } else {
            ranked.push(candidate);
        }
    }
    let basis = if !ranked.is_empty() && ranked.iter().all(|c| c.recommendation_score.is_some()) {
        RankingBasis::TechnicalAndEvaluation
    } else {
        RankingBasis::Technical
    };
    ranked.sort_by(|a, b| {
        (if basis == RankingBasis::TechnicalAndEvaluation {
            descending(a.recommendation_score, b.recommendation_score)
        } else {
            Ordering::Equal
        })
        .then_with(|| tie(a, b))
    });
    ranked.truncate(top_n);
    excluded.sort_by(tie);
    Ok(RecommendationResult {
        settings_revision: policy.settings_revision,
        technical_weights: policy.technical_weights.clone(),
        recommendation_weights: policy.recommendation_weights.clone(),
        hardware_fingerprint: policy.hardware_fingerprint.into(),
        launch_profile_hash: policy.launch_profile_hash.into(),
        evaluation_context: policy
            .evaluation_context
            .map(|(suite, dataset)| (suite.into(), dataset.into())),
        ranking_basis: basis,
        candidates: ranked,
        excluded,
    })
}
