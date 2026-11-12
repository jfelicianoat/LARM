//! TechnicalFit P091. Pure, contract-backed scoring for eligible candidates.
//!
//! This module deliberately returns score diagnostics without changing the
//! published CompatibilityRecord schema. Persisting a score still requires
//! an existing authorized producer and a compatible benchmark fingerprint.

use crate::config::FitWeights;
use larm_domain::{
    compatibility::{
        CompatibilityRecord, CompatibilityStatus, CompatibilityValidationError, GateStatus,
        REQUIRED_HARD_GATE_NAMES,
    },
    estimate::Confidence,
    storage::PerformanceClass,
};
use thiserror::Error;

const REQUIRED_BENCHMARK_SAMPLES: usize = 5;
const MAX_STABILITY_SAMPLES: usize = 20;

/// Normative score dimensions and their fixed contract weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitComponent {
    Memory,
    Backend,
    Maturity,
    Speed,
    Storage,
    Context,
    Stability,
}

impl FitComponent {
    fn configured_weight(self, weights: &FitWeights) -> u32 {
        match self {
            Self::Memory => weights.memory,
            Self::Backend => weights.backend,
            Self::Maturity => weights.maturity,
            Self::Speed => weights.speed,
            Self::Storage => weights.storage,
            Self::Context => weights.context,
            Self::Stability => weights.stability,
        }
    }

    #[must_use]
    pub const fn weight(self) -> u32 {
        match self {
            Self::Memory => 30,
            Self::Backend => 20,
            Self::Maturity => 10,
            Self::Speed => 15,
            Self::Storage => 10,
            Self::Context => 10,
            Self::Stability => 5,
        }
    }
}

/// The backend class is scored only when the selected backend is effective
/// and carries evidence. A detected device by itself is not enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveBackend {
    Cpu,
    Gpu,
    Npu,
    Hybrid,
}

impl EffectiveBackend {
    const fn score(self) -> f64 {
        match self {
            Self::Cpu => 40.0,
            Self::Gpu | Self::Npu => 80.0,
            Self::Hybrid => 70.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMaturity {
    /// Stable release and its adapter contract suite passed.
    StableContractSuitePassed,
    Experimental,
    /// Includes a stable release with no qualifying contract suite.
    Unqualified,
}

/// A bounded value taken from an estimate, with the confidence already
/// established by its producer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate<T> {
    pub value: T,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendEvidence {
    pub backend: EffectiveBackend,
    pub effective: bool,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextEvidence {
    pub effective: Estimate<f64>,
    pub required: Estimate<f64>,
}

/// A benchmark repetition. `throughput` is input units processed per second:
/// tokens/s, items/s, images/s, operations/s, or real-time input seconds per
/// processing second for ASR/TTS/video. Failed runs remain for Stability but
/// their metric values are ignored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BenchmarkRun {
    pub succeeded: bool,
    pub peak_memory_bytes: Option<u64>,
    pub throughput: Option<f64>,
    pub storage_mbps: Option<f64>,
}

/// Local observations tied to a complete execution fingerprint. Runs are
/// ordered oldest to newest; stability consumes the most recent twenty.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalBenchmark {
    pub execution_fingerprint: String,
    pub runs: Vec<BenchmarkRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    LanguageModel,
    Embedding,
    Asr,
    ImageGeneration,
    TextToSpeech,
    Tensor,
    VideoRealtime,
}

impl Workload {
    const fn target_throughput(self) -> f64 {
        match self {
            Self::LanguageModel => 20.0,
            Self::Embedding => 100.0,
            Self::Asr | Self::TextToSpeech | Self::VideoRealtime => 1.0,
            Self::ImageGeneration => 1.0 / 30.0,
            Self::Tensor => 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TechnicalFitInput {
    /// SHA-256 of the candidate's complete execution fingerprint.
    pub execution_fingerprint: Option<String>,
    pub estimated_required_memory_bytes: Option<Estimate<u64>>,
    pub available_memory_budget_bytes: Option<Estimate<u64>>,
    pub effective_backend: Option<BackendEvidence>,
    pub maturity: RuntimeMaturity,
    pub workload: Workload,
    pub context: Option<ContextEvidence>,
    pub storage_class: PerformanceClass,
    pub local_benchmark: Option<LocalBenchmark>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSource {
    Contract,
    Estimate,
    LocalBenchmark,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComponentBasis {
    Memory {
        required_bytes: Option<u64>,
        budget_bytes: Option<u64>,
        headroom_bytes: Option<u64>,
    },
    Backend {
        effective_backend: Option<EffectiveBackend>,
        evidence_refs: Vec<String>,
    },
    Maturity {
        maturity: RuntimeMaturity,
    },
    Speed {
        observed_per_second: Option<f64>,
        target_per_second: f64,
    },
    Storage {
        observed_mbps: Option<f64>,
        target_mbps: f64,
        performance_class: PerformanceClass,
    },
    Context {
        effective: Option<f64>,
        required: Option<f64>,
    },
    Stability {
        failed_runs: Option<usize>,
        total_runs: Option<usize>,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentBreakdown {
    pub component: FitComponent,
    /// `None` means the input is unknown and its weight is removed.
    pub score: Option<f64>,
    pub weight: u32,
    pub source: ValueSource,
    pub confidence: Confidence,
    pub basis: ComponentBasis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TechnicalFitResult {
    /// `None` means not applicable (incompatible/unknown gates) or no known
    /// score components. The caller must show the reason and breakdown.
    pub score: Option<f64>,
    pub coverage_percent: f64,
    pub confidence: Confidence,
    /// The normative 60% coverage gate only; other recommendation gates may
    /// still apply in a separate recommendation workflow.
    pub coverage_gate_passed: bool,
    pub not_applicable_reason: Option<&'static str>,
    pub breakdown: Vec<ComponentBreakdown>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TechnicalFitError {
    #[error("registro de compatibilidad inválido: {0}")]
    InvalidCompatibility(#[from] CompatibilityValidationError),
    #[error("execution_fingerprint debe ser SHA-256 hexadecimal en minúsculas")]
    InvalidFingerprint,
    #[error("benchmark contiene una medida negativa o no finita")]
    InvalidBenchmarkMetric,
    #[error("bytes del estimate o benchmark superan el máximo contractual")]
    InvalidMemoryBytes,
    #[error("contexto efectivo/requerido debe ser finito y no negativo")]
    InvalidContextMetric,
}

/// Calculates the contract TechnicalFitScore, never a quality/evaluation
/// score. Missing dimensions are excluded from the denominator, with coverage
/// shown separately; under 60% cannot be auto-recommended.
pub fn calculate_technical_fit(
    compatibility: &CompatibilityRecord,
    input: &TechnicalFitInput,
) -> Result<TechnicalFitResult, TechnicalFitError> {
    calculate_weighted(compatibility, input, None)
}

/// Uses the existing Settings weight representation. The composition owner
/// must supply the effective, versioned benchmark settings and retain their
/// revision with the assessment; this pure function does not persist policy.
/// Default callers continue to use the exact contract weights above.
pub fn calculate_technical_fit_with_weights(
    compatibility: &CompatibilityRecord,
    input: &TechnicalFitInput,
    weights: &FitWeights,
) -> Result<TechnicalFitResult, TechnicalFitError> {
    calculate_weighted(compatibility, input, Some(weights))
}

fn calculate_weighted(
    compatibility: &CompatibilityRecord,
    input: &TechnicalFitInput,
    weights: Option<&FitWeights>,
) -> Result<TechnicalFitResult, TechnicalFitError> {
    compatibility.validate()?;

    let ineligible_reason = match compatibility.status {
        CompatibilityStatus::Incompatible => Some("INCOMPATIBLE"),
        CompatibilityStatus::Unknown => Some("COMPATIBILITY_UNKNOWN"),
        CompatibilityStatus::Compatible | CompatibilityStatus::CompatibleWithLimitations => None,
        _ => Some("CANDIDATE_NOT_TECHNICALLY_VALID"),
    }
    .or_else(|| {
        compatibility
            .hard_gates
            .iter()
            .any(|gate| gate.status != GateStatus::Pass)
            .then_some("HARD_GATES_NOT_ALL_PASS")
    })
    .or_else(|| {
        REQUIRED_HARD_GATE_NAMES
            .iter()
            .any(|required| {
                !compatibility
                    .hard_gates
                    .iter()
                    .any(|gate| gate.name == *required && gate.status == GateStatus::Pass)
            })
            .then_some("REQUIRED_HARD_GATE_NOT_PASS")
    });
    if let Some(reason) = ineligible_reason {
        return Ok(not_applicable(reason, input.storage_class, weights));
    }

    if let Some(fingerprint) = input.execution_fingerprint.as_deref() {
        validate_fingerprint(fingerprint)?;
    }
    for estimate in [
        input.estimated_required_memory_bytes,
        input.available_memory_budget_bytes,
    ]
    .into_iter()
    .flatten()
    {
        if estimate.value > i64::MAX as u64 {
            return Err(TechnicalFitError::InvalidMemoryBytes);
        }
    }
    if let Some(benchmark) = &input.local_benchmark {
        validate_fingerprint(&benchmark.execution_fingerprint)?;
        for run in &benchmark.runs {
            if run
                .peak_memory_bytes
                .is_some_and(|bytes| bytes > i64::MAX as u64)
            {
                return Err(TechnicalFitError::InvalidMemoryBytes);
            }
            for value in [run.throughput, run.storage_mbps].into_iter().flatten() {
                if !value.is_finite() || value < 0.0 {
                    return Err(TechnicalFitError::InvalidBenchmarkMetric);
                }
            }
        }
    }
    if let Some(context) = input.context {
        if !context.effective.value.is_finite()
            || !context.required.value.is_finite()
            || context.effective.value < 0.0
            || context.required.value < 0.0
        {
            return Err(TechnicalFitError::InvalidContextMetric);
        }
    }

    let benchmark = input.local_benchmark.as_ref().filter(|benchmark| {
        input.execution_fingerprint.as_deref() == Some(benchmark.execution_fingerprint.as_str())
    });
    let mut breakdown = vec![
        memory_component(input, benchmark),
        backend_component(input.effective_backend.as_ref()),
        maturity_component(input.maturity),
        speed_component(input.workload, benchmark),
        storage_component(input.storage_class, benchmark),
        context_component(input.context),
        stability_component(benchmark),
    ];
    apply_weights(&mut breakdown, weights);

    // Seven u32 settings weights can exceed u32 in aggregate. Widen before
    // summing; even the percentage cross-products below fit exactly in u64.
    let total_weight: u64 = breakdown.iter().map(|part| u64::from(part.weight)).sum();
    let covered_weight: u64 = breakdown
        .iter()
        .filter(|component| component.score.is_some())
        .map(|component| u64::from(component.weight))
        .sum();
    let weighted_score = if covered_weight == 0 {
        None
    } else {
        Some(
            breakdown
                .iter()
                .filter_map(|component| {
                    component
                        .score
                        .map(|score| score * f64::from(component.weight))
                })
                .sum::<f64>()
                / covered_weight as f64,
        )
    };
    let confidence = breakdown
        .iter()
        .filter(|component| component.score.is_some() && component.weight > 0)
        .map(|component| component.confidence)
        .max()
        .unwrap_or(Confidence::Unknown);
    let coverage_percent = if total_weight == 0 {
        0.0
    } else {
        100.0 * covered_weight as f64 / total_weight as f64
    };

    Ok(TechnicalFitResult {
        score: weighted_score,
        coverage_percent,
        confidence,
        coverage_gate_passed: weighted_score.is_some() && covered_weight * 100 >= total_weight * 60,
        not_applicable_reason: weighted_score.is_none().then_some(if total_weight == 0 {
            "NO_POSITIVE_SCORING_WEIGHT"
        } else {
            "NO_KNOWN_COMPONENTS"
        }),
        breakdown,
    })
}

fn apply_weights(parts: &mut [ComponentBreakdown], weights: Option<&FitWeights>) {
    if let Some(weights) = weights {
        for part in parts {
            part.weight = part.component.configured_weight(weights);
        }
    }
}

fn not_applicable(
    reason: &'static str,
    storage_class: PerformanceClass,
    weights: Option<&FitWeights>,
) -> TechnicalFitResult {
    let mut breakdown = all_unknown(storage_class);
    apply_weights(&mut breakdown, weights);
    TechnicalFitResult {
        score: None,
        coverage_percent: 0.0,
        confidence: Confidence::Unknown,
        coverage_gate_passed: false,
        not_applicable_reason: Some(reason),
        breakdown,
    }
}

fn all_unknown(storage_class: PerformanceClass) -> Vec<ComponentBreakdown> {
    [
        FitComponent::Memory,
        FitComponent::Backend,
        FitComponent::Maturity,
        FitComponent::Speed,
        FitComponent::Storage,
        FitComponent::Context,
        FitComponent::Stability,
    ]
    .into_iter()
    .map(|component| {
        component_result(
            component,
            None,
            ValueSource::Unknown,
            Confidence::Unknown,
            ComponentBasis::Unknown,
        )
    })
    .map(|mut result| {
        if result.component == FitComponent::Storage {
            result.basis = ComponentBasis::Storage {
                observed_mbps: None,
                target_mbps: 200.0,
                performance_class: storage_class,
            };
        }
        result
    })
    .collect()
}

fn component_result(
    component: FitComponent,
    score: Option<f64>,
    source: ValueSource,
    confidence: Confidence,
    basis: ComponentBasis,
) -> ComponentBreakdown {
    ComponentBreakdown {
        component,
        score,
        weight: component.weight(),
        source,
        confidence,
        basis,
    }
}

fn memory_component(
    input: &TechnicalFitInput,
    benchmark: Option<&LocalBenchmark>,
) -> ComponentBreakdown {
    let measured_peak = benchmark.and_then(|benchmark| {
        let values: Vec<u64> = benchmark
            .runs
            .iter()
            .filter(|run| run.succeeded)
            .filter_map(|run| run.peak_memory_bytes)
            .collect();
        (values.len() >= REQUIRED_BENCHMARK_SAMPLES)
            .then(|| values.into_iter().max())
            .flatten()
    });
    let (required, source, confidence) = match measured_peak {
        Some(bytes) => (Some(bytes), ValueSource::LocalBenchmark, Confidence::Exact),
        None => input
            .estimated_required_memory_bytes
            .map(|value| (Some(value.value), ValueSource::Estimate, value.confidence))
            .unwrap_or((None, ValueSource::Unknown, Confidence::Unknown)),
    };
    let budget = input.available_memory_budget_bytes;
    let budget_bytes = budget.map(|value| value.value);
    let headroom_bytes = required
        .zip(budget_bytes)
        .map(|(required, budget)| budget.saturating_sub(required));
    let score = required
        .zip(budget_bytes)
        .filter(|(_, budget)| *budget > 0)
        .map(|(required, budget)| {
            (100.0 * (1.0 - required as f64 / budget as f64)).clamp(0.0, 100.0)
        });
    let confidence = budget
        .map(|value| value.confidence.max(confidence))
        .unwrap_or(Confidence::Unknown);
    let effective_source = if score.is_none() {
        ValueSource::Unknown
    } else {
        source
    };
    component_result(
        FitComponent::Memory,
        score,
        effective_source,
        if score.is_some() {
            confidence
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Memory {
            required_bytes: required,
            budget_bytes,
            headroom_bytes,
        },
    )
}

fn backend_component(value: Option<&BackendEvidence>) -> ComponentBreakdown {
    let effective = value.filter(|backend| {
        backend.effective
            && !backend.evidence_refs.is_empty()
            && backend
                .evidence_refs
                .iter()
                .all(|reference| !reference.trim().is_empty())
    });
    component_result(
        FitComponent::Backend,
        effective.map(|backend| backend.backend.score()),
        if effective.is_some() {
            ValueSource::Contract
        } else {
            ValueSource::Unknown
        },
        if effective.is_some() {
            Confidence::Exact
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Backend {
            effective_backend: effective.map(|backend| backend.backend),
            evidence_refs: effective
                .map(|backend| backend.evidence_refs.clone())
                .unwrap_or_default(),
        },
    )
}

fn maturity_component(value: RuntimeMaturity) -> ComponentBreakdown {
    let score = match value {
        RuntimeMaturity::StableContractSuitePassed => Some(100.0),
        RuntimeMaturity::Experimental => Some(50.0),
        RuntimeMaturity::Unqualified => None,
    };
    component_result(
        FitComponent::Maturity,
        score,
        if score.is_some() {
            ValueSource::Contract
        } else {
            ValueSource::Unknown
        },
        if score.is_some() {
            Confidence::Exact
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Maturity { maturity: value },
    )
}

fn speed_component(workload: Workload, benchmark: Option<&LocalBenchmark>) -> ComponentBreakdown {
    let speed = measured_median(benchmark, |run| run.throughput);
    let target = workload.target_throughput();
    let score = speed.map(|value| (100.0 * value / target).min(100.0));
    component_result(
        FitComponent::Speed,
        score,
        if score.is_some() {
            ValueSource::LocalBenchmark
        } else {
            ValueSource::Unknown
        },
        if score.is_some() {
            Confidence::Exact
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Speed {
            observed_per_second: speed,
            target_per_second: target,
        },
    )
}

fn storage_component(
    storage_class: PerformanceClass,
    benchmark: Option<&LocalBenchmark>,
) -> ComponentBreakdown {
    let mbps = measured_median(benchmark, |run| run.storage_mbps);
    let score = mbps.map(|value| (100.0 * value / 200.0).min(100.0));
    component_result(
        FitComponent::Storage,
        score,
        if score.is_some() {
            ValueSource::LocalBenchmark
        } else {
            ValueSource::Unknown
        },
        if score.is_some() {
            Confidence::Exact
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Storage {
            observed_mbps: mbps,
            target_mbps: 200.0,
            performance_class: storage_class,
        },
    )
}

fn context_component(value: Option<ContextEvidence>) -> ComponentBreakdown {
    let score = value.and_then(|context| {
        (context.required.value > 0.0)
            .then(|| (100.0 * context.effective.value / context.required.value).min(100.0))
    });
    let confidence = value
        .map(|context| {
            context
                .effective
                .confidence
                .max(context.required.confidence)
        })
        .unwrap_or(Confidence::Unknown);
    component_result(
        FitComponent::Context,
        score,
        if score.is_some() {
            ValueSource::Estimate
        } else {
            ValueSource::Unknown
        },
        confidence,
        ComponentBasis::Context {
            effective: value.map(|context| context.effective.value),
            required: value.map(|context| context.required.value),
        },
    )
}

fn stability_component(benchmark: Option<&LocalBenchmark>) -> ComponentBreakdown {
    let stats = benchmark.and_then(|benchmark| {
        let recent: Vec<&BenchmarkRun> = benchmark
            .runs
            .iter()
            .rev()
            .take(MAX_STABILITY_SAMPLES)
            .collect();
        let failures = recent.iter().filter(|run| !run.succeeded).count();
        (recent.len() >= REQUIRED_BENCHMARK_SAMPLES).then_some((failures, recent.len()))
    });
    let score = stats.map(|(failures, total)| 100.0 * (1.0 - failures as f64 / total as f64));
    component_result(
        FitComponent::Stability,
        score,
        if score.is_some() {
            ValueSource::LocalBenchmark
        } else {
            ValueSource::Unknown
        },
        if score.is_some() {
            Confidence::Exact
        } else {
            Confidence::Unknown
        },
        ComponentBasis::Stability {
            failed_runs: stats.map(|(failures, _)| failures),
            total_runs: stats.map(|(_, total)| total),
        },
    )
}

fn measured_median(
    benchmark: Option<&LocalBenchmark>,
    metric: impl Fn(&BenchmarkRun) -> Option<f64>,
) -> Option<f64> {
    let mut values: Vec<f64> = benchmark?
        .runs
        .iter()
        .filter(|run| run.succeeded)
        .filter_map(metric)
        .collect();
    if values.len() < REQUIRED_BENCHMARK_SAMPLES {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        values[middle - 1] + (values[middle] - values[middle - 1]) / 2.0
    } else {
        values[middle]
    })
}

fn validate_fingerprint(value: &str) -> Result<(), TechnicalFitError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(TechnicalFitError::InvalidFingerprint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use larm_domain::compatibility::{HardGate, REQUIRED_HARD_GATE_NAMES};
    use serde_json::Map;

    const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn compatible_record() -> CompatibilityRecord {
        CompatibilityRecord {
            id: "01952f8a-3a00-7000-8000-000000000001".to_owned(),
            created_at: "2026-10-22T12:00:00.000Z".to_owned(),
            updated_at: "2026-10-22T12:00:00.000Z".to_owned(),
            row_version: 1,
            metadata: Map::new(),
            variant_id: "01952f8a-3a00-7000-8000-000000000002".to_owned(),
            runtime_version_id: "01952f8a-3a00-7000-8000-000000000003".to_owned(),
            backend_id: "01952f8a-3a00-7000-8000-000000000004".to_owned(),
            hardware_fingerprint: FINGERPRINT.to_owned(),
            launch_profile_hash: FINGERPRINT.to_owned(),
            status: CompatibilityStatus::Compatible,
            hard_gates: REQUIRED_HARD_GATE_NAMES
                .iter()
                .map(|name| HardGate {
                    name: (*name).to_owned(),
                    status: GateStatus::Pass,
                    reason: "fixture evidence passed".to_owned(),
                    evidence_refs: vec!["fixture://p091/gate".to_owned()],
                })
                .collect(),
            limitations: Vec::new(),
            technical_fit: None,
            evidence_refs: vec!["fixture://p091/compatibility".to_owned()],
        }
    }

    fn input() -> TechnicalFitInput {
        TechnicalFitInput {
            execution_fingerprint: Some(FINGERPRINT.to_owned()),
            estimated_required_memory_bytes: Some(Estimate {
                value: 4_000,
                confidence: Confidence::Estimated,
            }),
            available_memory_budget_bytes: Some(Estimate {
                value: 10_000,
                confidence: Confidence::High,
            }),
            effective_backend: Some(BackendEvidence {
                backend: EffectiveBackend::Gpu,
                effective: true,
                evidence_refs: vec!["fixture://p091/backend".to_owned()],
            }),
            maturity: RuntimeMaturity::StableContractSuitePassed,
            workload: Workload::LanguageModel,
            context: Some(ContextEvidence {
                effective: Estimate {
                    value: 8_000.0,
                    confidence: Confidence::High,
                },
                required: Estimate {
                    value: 10_000.0,
                    confidence: Confidence::High,
                },
            }),
            storage_class: PerformanceClass::Nvme,
            local_benchmark: Some(LocalBenchmark {
                execution_fingerprint: FINGERPRINT.to_owned(),
                runs: (0..5)
                    .map(|_| BenchmarkRun {
                        succeeded: true,
                        peak_memory_bytes: Some(6_000),
                        throughput: Some(10.0),
                        storage_mbps: Some(100.0),
                    })
                    .collect(),
            }),
        }
    }

    #[test]
    fn score_is_deterministic_and_uses_exact_contract_weights() {
        let record = compatible_record();
        let first = calculate_technical_fit(&record, &input()).unwrap();
        let second = calculate_technical_fit(&record, &input()).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.coverage_percent, 100.0);
        assert!(first.coverage_gate_passed);
        assert_eq!(first.score, Some(63.5));
        assert_eq!(
            first.breakdown.iter().map(|part| part.weight).sum::<u32>(),
            100
        );
    }

    fn zero_weights() -> FitWeights {
        FitWeights {
            memory: 0,
            backend: 0,
            maturity: 0,
            speed: 0,
            storage: 0,
            context: 0,
            stability: 0,
        }
    }

    #[test]
    fn configured_defaults_preserve_the_contract_result() {
        let defaults: serde_json::Value =
            serde_json::from_str(include_str!("../../../../docs/contracts/defaults.json")).unwrap();
        let weights: FitWeights =
            serde_json::from_value(defaults["benchmarks"]["technical_fit_weights"].clone())
                .unwrap();
        assert_eq!(
            calculate_technical_fit_with_weights(&compatible_record(), &input(), &weights).unwrap(),
            calculate_technical_fit(&compatible_record(), &input()).unwrap()
        );
    }

    #[test]
    fn configured_objective_changes_the_score_without_imputing_other_dimensions() {
        let mut weights = zero_weights();
        weights.memory = 100;
        let memory =
            calculate_technical_fit_with_weights(&compatible_record(), &input(), &weights).unwrap();
        weights.memory = 0;
        weights.backend = 100;
        let backend =
            calculate_technical_fit_with_weights(&compatible_record(), &input(), &weights).unwrap();
        assert_eq!(memory.score, Some(40.0));
        assert_eq!(backend.score, Some(80.0));
        assert_eq!(backend.coverage_percent, 100.0);
        assert!(backend.coverage_gate_passed);
        assert_eq!(
            backend
                .breakdown
                .iter()
                .map(|part| part.weight)
                .collect::<Vec<_>>(),
            vec![0, 100, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn configured_coverage_uses_the_full_policy_and_keeps_the_sixty_percent_gate() {
        let mut data = input();
        data.local_benchmark = None;
        let mut weights = zero_weights();
        weights.memory = 3;
        weights.speed = 2; // Unmeasured, retains its share of missing coverage.
        let enough =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        assert_eq!(enough.score, Some(60.0));
        assert_eq!(enough.coverage_percent, 60.0);
        assert!(enough.coverage_gate_passed);
        weights.memory = 2;
        weights.speed = 3;
        let insufficient =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        assert_eq!(insufficient.score, Some(60.0));
        assert_eq!(insufficient.coverage_percent, 40.0);
        assert!(!insufficient.coverage_gate_passed);
    }

    #[test]
    fn disabled_estimates_do_not_lower_the_confidence_of_the_weighted_result() {
        let mut data = input();
        data.local_benchmark = None; // Estimated memory is present but disabled.
        let mut weights = zero_weights();
        weights.backend = 7;
        let result =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        assert_eq!(result.score, Some(80.0));
        assert_eq!(result.confidence, Confidence::Exact);
        assert_eq!(result.breakdown[0].confidence, Confidence::Estimated);
    }

    #[test]
    fn effective_settings_roundtrip_supplies_weights_without_a_new_public_shape() {
        let mut settings: crate::config::Settings =
            serde_json::from_str(include_str!("../../../../docs/contracts/defaults.json")).unwrap();
        settings.benchmarks.technical_fit_weights = zero_weights();
        settings.benchmarks.technical_fit_weights.backend = 7;
        settings.validate().unwrap();
        let roundtrip: crate::config::Settings =
            serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
        let result = calculate_technical_fit_with_weights(
            &compatible_record(),
            &input(),
            &roundtrip.benchmarks.technical_fit_weights,
        )
        .unwrap();
        assert_eq!(result.score, Some(80.0));
        assert_eq!(result.coverage_percent, 100.0);
    }

    #[test]
    fn configured_speed_does_not_reuse_a_different_execution_fingerprint() {
        let mut data = input();
        data.local_benchmark.as_mut().unwrap().execution_fingerprint = "b".repeat(64);
        let mut weights = zero_weights();
        weights.speed = 100;
        let result =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        assert_eq!(result.score, None);
        assert_eq!(result.coverage_percent, 0.0);
        assert_eq!(result.confidence, Confidence::Unknown);
        assert!(!result.coverage_gate_passed);
    }

    #[test]
    fn zero_policy_cannot_create_a_score_or_recommendation() {
        let result =
            calculate_technical_fit_with_weights(&compatible_record(), &input(), &zero_weights())
                .unwrap();
        assert_eq!(result.score, None);
        assert_eq!(result.coverage_percent, 0.0);
        assert_eq!(result.confidence, Confidence::Unknown);
        assert!(!result.coverage_gate_passed);
        assert_eq!(
            result.not_applicable_reason,
            Some("NO_POSITIVE_SCORING_WEIGHT")
        );
    }

    #[test]
    fn maximum_settings_weights_do_not_overflow_the_denominator() {
        let weights = FitWeights {
            memory: u32::MAX,
            backend: u32::MAX,
            maturity: u32::MAX,
            speed: u32::MAX,
            storage: u32::MAX,
            context: u32::MAX,
            stability: u32::MAX,
        };
        let result =
            calculate_technical_fit_with_weights(&compatible_record(), &input(), &weights).unwrap();
        assert!((result.score.unwrap() - 500.0 / 7.0).abs() < 1e-9);
        assert_eq!(result.coverage_percent, 100.0);
        assert!(result.coverage_gate_passed);
    }

    #[test]
    fn weights_never_bypass_a_failed_hard_gate() {
        let mut record = compatible_record();
        record.status = CompatibilityStatus::Incompatible;
        record.hard_gates[0].status = GateStatus::Fail;
        let mut weights = zero_weights();
        weights.backend = 100;
        let result = calculate_technical_fit_with_weights(&record, &input(), &weights).unwrap();
        assert_eq!(result.score, None);
        assert!(!result.coverage_gate_passed);
        assert_eq!(result.breakdown[1].weight, 100);
    }

    #[test]
    fn scaling_policy_weights_does_not_change_score_or_coverage() {
        let mut data = input();
        data.local_benchmark = None;
        let mut weights = zero_weights();
        weights.memory = 3;
        weights.speed = 2;
        let first =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        weights.memory *= 2;
        weights.speed *= 2;
        let scaled =
            calculate_technical_fit_with_weights(&compatible_record(), &data, &weights).unwrap();
        assert_eq!(
            (
                first.score,
                first.coverage_percent,
                first.confidence,
                first.coverage_gate_passed
            ),
            (
                scaled.score,
                scaled.coverage_percent,
                scaled.confidence,
                scaled.coverage_gate_passed
            )
        );
    }

    #[test]
    fn matching_local_benchmark_overrides_memory_estimate() {
        let record = compatible_record();
        let result = calculate_technical_fit(&record, &input()).unwrap();
        let memory = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Memory)
            .unwrap();

        assert_eq!(memory.score, Some(40.0));
        assert_eq!(memory.source, ValueSource::LocalBenchmark);
        assert_eq!(memory.confidence, Confidence::High);
        assert_eq!(
            memory.basis,
            ComponentBasis::Memory {
                required_bytes: Some(6_000),
                budget_bytes: Some(10_000),
                headroom_bytes: Some(4_000),
            }
        );
        let speed = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Speed)
            .unwrap();
        assert_eq!(speed.score, Some(50.0));
        assert_eq!(speed.source, ValueSource::LocalBenchmark);
        let storage = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Storage)
            .unwrap();
        assert_eq!(storage.score, Some(50.0));
        assert_eq!(
            storage.basis,
            ComponentBasis::Storage {
                observed_mbps: Some(100.0),
                target_mbps: 200.0,
                performance_class: PerformanceClass::Nvme,
            }
        );
        let stability = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Stability)
            .unwrap();
        assert_eq!(stability.score, Some(100.0));
        assert_eq!(
            stability.basis,
            ComponentBasis::Stability {
                failed_runs: Some(0),
                total_runs: Some(5),
            }
        );

        let mut mismatched = input();
        mismatched.execution_fingerprint =
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned());
        let result = calculate_technical_fit(&record, &mismatched).unwrap();
        let memory = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Memory)
            .unwrap();
        assert_eq!(memory.score, Some(60.0));
        assert_eq!(memory.source, ValueSource::Estimate);
        let speed = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Speed)
            .unwrap();
        assert_eq!(speed.score, None);
        assert_eq!(speed.source, ValueSource::Unknown);
    }

    #[test]
    fn incompatible_candidate_has_no_score() {
        let mut record = compatible_record();
        record.status = CompatibilityStatus::Incompatible;
        record.hard_gates[0].status = GateStatus::Fail;
        record.hard_gates[0].reason = "unsupported format".to_owned();
        record.hard_gates[0].evidence_refs = vec!["fixture://p091/fail".to_owned()];

        let result = calculate_technical_fit(&record, &input()).unwrap();
        assert_eq!(result.score, None);
        assert_eq!(result.coverage_percent, 0.0);
        assert!(!result.coverage_gate_passed);
        assert_eq!(result.not_applicable_reason, Some("INCOMPATIBLE"));
    }

    #[test]
    fn unknown_components_are_removed_from_denominator_and_coverage_is_visible() {
        let record = compatible_record();
        let mut sparse = input();
        sparse.local_benchmark = None;
        sparse.execution_fingerprint = None;
        sparse.effective_backend = None;
        sparse.maturity = RuntimeMaturity::Unqualified;
        sparse.context = None;
        let result = calculate_technical_fit(&record, &sparse).unwrap();

        assert_eq!(result.coverage_percent, 30.0);
        assert!(!result.coverage_gate_passed);
        assert_eq!(result.score, Some(60.0));
        assert_eq!(result.confidence, Confidence::Estimated);
    }

    #[test]
    fn backend_presence_without_effective_evidence_is_unknown() {
        let record = compatible_record();
        let mut no_evidence = input();
        no_evidence.effective_backend = Some(BackendEvidence {
            backend: EffectiveBackend::Gpu,
            effective: false,
            evidence_refs: vec!["fixture://p091/backend".to_owned()],
        });
        let result = calculate_technical_fit(&record, &no_evidence).unwrap();
        let backend = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Backend)
            .unwrap();
        assert_eq!(backend.score, None);
        assert_eq!(backend.source, ValueSource::Unknown);
    }

    #[test]
    fn public_compatibility_shape_remains_closed_and_scalar_only() {
        let result = calculate_technical_fit(&compatible_record(), &input()).unwrap();
        let mut public_record = compatible_record();
        public_record.technical_fit = result.score;
        let value = serde_json::to_value(public_record).unwrap();
        assert!(value.get("technical_fit").is_some());
        assert_eq!(value.get("technical_fit").unwrap().as_f64(), result.score);
        assert!(value.get("breakdown").is_none());
        assert!(value.get("confidence").is_none());
        assert!(result.score.is_some());
    }

    #[test]
    fn unknown_compatibility_and_invalid_schema_cannot_be_scored() {
        let mut unknown = compatible_record();
        unknown.status = CompatibilityStatus::Unknown;
        unknown.hard_gates[0].status = GateStatus::Unknown;
        unknown.hard_gates[0].evidence_refs.clear();
        let result = calculate_technical_fit(&unknown, &input()).unwrap();
        assert_eq!(result.score, None);
        assert_eq!(result.not_applicable_reason, Some("COMPATIBILITY_UNKNOWN"));

        let mut invalid = compatible_record();
        invalid.technical_fit = Some(101.0);
        assert!(matches!(
            calculate_technical_fit(&invalid, &input()),
            Err(TechnicalFitError::InvalidCompatibility(
                CompatibilityValidationError::InvalidTechnicalFit
            ))
        ));
    }

    #[test]
    fn benchmark_with_fewer_than_five_measurements_does_not_override_estimate() {
        let record = compatible_record();
        let mut insufficient = input();
        insufficient.local_benchmark.as_mut().unwrap().runs.pop();
        let result = calculate_technical_fit(&record, &insufficient).unwrap();
        let memory = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Memory)
            .unwrap();
        let speed = result
            .breakdown
            .iter()
            .find(|part| part.component == FitComponent::Speed)
            .unwrap();
        assert_eq!(memory.score, Some(60.0));
        assert_eq!(memory.source, ValueSource::Estimate);
        assert_eq!(speed.score, None);
    }

    #[test]
    fn fixed_weights_match_the_versioned_defaults() {
        let defaults: serde_json::Value =
            serde_json::from_str(include_str!("../../../../docs/contracts/defaults.json")).unwrap();
        let weights = defaults
            .pointer("/benchmarks/technical_fit_weights")
            .unwrap();
        for (name, component) in [
            ("memory", FitComponent::Memory),
            ("backend", FitComponent::Backend),
            ("maturity", FitComponent::Maturity),
            ("speed", FitComponent::Speed),
            ("storage", FitComponent::Storage),
            ("context", FitComponent::Context),
            ("stability", FitComponent::Stability),
        ] {
            assert_eq!(weights[name].as_u64(), Some(u64::from(component.weight())));
        }
    }
}
