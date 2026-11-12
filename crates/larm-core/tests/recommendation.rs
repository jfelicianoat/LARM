//! Synthetic evidence only: these tests do not qualify a runtime or import.
#![allow(clippy::unwrap_used)]
use larm_core::{
    compatibility::score::{
        BackendEvidence, EffectiveBackend, Estimate, RuntimeMaturity, TechnicalFitInput, Workload,
    },
    config::{FitWeights, RecommendationWeights},
    recommendation::{recommend, Candidate, RankingBasis, RecommendationPolicy},
};
use larm_domain::{
    compatibility::{
        CompatibilityRecord, CompatibilityStatus, GateStatus, HardGate, REQUIRED_HARD_GATE_NAMES,
    },
    estimate::Confidence,
    evaluation::{EvaluationReference, EvaluationStatus},
    locations::StorageLocation,
    model::{CanonicalModel, ModelDistribution, ModelRevision, ModelVariant, Trust},
    runtime::{RuntimeBackend, RuntimeInstallation, RuntimeVersion},
    storage::PerformanceClass,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

fn id(n: u32) -> String {
    format!("0192aaaa-0000-7000-8000-{n:012x}")
}
fn entity<T: DeserializeOwned>(n: u32, fields: Value) -> T {
    let mut value = json!({"id":id(n),"created_at":"2026-10-30T02:00:00.000Z","updated_at":"2026-10-30T02:00:00.000Z","row_version":1,"metadata":{}});
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

struct Graph {
    model: CanonicalModel,
    distribution: ModelDistribution,
    revision: ModelRevision,
    variant: ModelVariant,
    runtime: RuntimeVersion,
    installation: RuntimeInstallation,
    backend: RuntimeBackend,
    storage: StorageLocation,
    compatibility: CompatibilityRecord,
    technical: TechnicalFitInput,
    evaluation: Option<EvaluationReference>,
    trust: Trust,
}

impl Graph {
    fn new(n: u32, memory: u64) -> Self {
        let hash = format!("{n:064x}");
        Self {
            model: entity(
                1,
                json!({"name":"Same name","upstream_identity":null,"capabilities":["text.generate"],"possible_duplicates":[]}),
            ),
            distribution: entity(
                10 + n,
                json!({"canonical_model_id":id(1),"source_id":id(30+n),"upstream_namespace":"unit","upstream_model_id":"same-name","license_id":null}),
            ),
            revision: entity(
                20 + n,
                json!({"distribution_id":id(10+n),"revision_key":format!("content:{hash}"),"upstream_commit":null,"manifest_hash":hash,"state":"CURRENT"}),
            ),
            variant: entity(
                40 + n,
                json!({"revision_id":id(20+n),"format":"GGUF","quantization":"Q4_K_M","architecture":"unit","bundle_hash":hash,"lifecycle":"VERIFIED","availability":"UNKNOWN","ownership":"LARM_MANAGED","promotion":"NONE","system_role":"GENERAL","capabilities":["text.generate"],"model_limits":{"parameter_count":null,"layers":null,"kv_heads":null,"head_dim":null,"context_limit":null,"kv_bits":null,"model_max_memory_bytes":null,"architecture_metadata_hash":null},"system1_state":null,"update_available":false,"status":"VERIFIED"}),
            ),
            runtime: entity(
                70,
                json!({"runtime_id":id(71),"version_string":"unit","commit":null,"build":null,"channel":"stable","released_at":null,"artifact_hash":null,"promotion":"NONE","lifecycle":"VALIDATED"}),
            ),
            installation: entity(
                72,
                json!({"runtime_version_id":id(70),"storage_location_id":id(74),"relative_path":"runtime","strategy":"PREBUILT_BINARY","external_host_id":null,"state":"VALIDATED","ownership":"LARM_MANAGED","lock_hash":null,"stage":"VALIDATED","registration_mode":"MANAGED"}),
            ),
            backend: entity(
                73,
                json!({"runtime_installation_id":id(72),"backend_type":"cpu","device_id":null,"driver_version":null,"status":"AVAILABLE","evidence_refs":["unit:backend"],"limits":{"max_instances":1,"multi_model_support":false,"parallel_requests":1,"gpu_sharing":false}}),
            ),
            storage: entity(
                74,
                json!({"name":"unit storage","root_path":"C:\\unit","canonical_path":"C:\\unit","volume_guid":null,"drive_letter":"C:","unc_path":null,"filesystem":"NTFS","storage_type":"LOCAL","capacity_bytes":"1000","free_bytes":"900","reserved_bytes":"0","priority":1,"auto_placement":false,"state":"ONLINE","last_seen_at":null,"volume_key":"unit-volume","performance_class":"NVME"}),
            ),
            compatibility: entity(
                80 + n,
                json!({"variant_id":id(40+n),"runtime_version_id":id(70),"backend_id":id(73),"hardware_fingerprint":"a".repeat(64),"launch_profile_hash":"b".repeat(64),"status":"COMPATIBLE","hard_gates":REQUIRED_HARD_GATE_NAMES.iter().map(|name|HardGate{name:(*name).into(),status:GateStatus::Pass,reason:"unit evidence".into(),evidence_refs:vec!["unit:gate".into()]}).collect::<Vec<_>>(),"limitations":[],"technical_fit":null,"evidence_refs":["unit:compatibility"]}),
            ),
            technical: TechnicalFitInput {
                execution_fingerprint: Some("f".repeat(64)),
                estimated_required_memory_bytes: Some(Estimate {
                    value: memory,
                    confidence: Confidence::Exact,
                }),
                available_memory_budget_bytes: Some(Estimate {
                    value: 100,
                    confidence: Confidence::Exact,
                }),
                effective_backend: Some(BackendEvidence {
                    backend: EffectiveBackend::Cpu,
                    effective: true,
                    evidence_refs: vec!["unit:backend".into()],
                }),
                maturity: RuntimeMaturity::Unqualified,
                workload: Workload::LanguageModel,
                context: None,
                storage_class: PerformanceClass::Nvme,
                local_benchmark: None,
            },
            evaluation: None,
            trust: Trust::Declared,
        }
    }
    fn candidate(&self) -> Candidate<'_> {
        Candidate {
            model: &self.model,
            distribution: &self.distribution,
            revision: &self.revision,
            variant: &self.variant,
            runtime: &self.runtime,
            installation: &self.installation,
            backend: &self.backend,
            storage: &self.storage,
            source_trust: self.trust,
            compatibility: &self.compatibility,
            technical: &self.technical,
            evaluation: self.evaluation.as_ref(),
        }
    }
    fn same_artifacts_as(&mut self, other: &Self) {
        self.variant.bundle_hash = other.variant.bundle_hash.clone();
        self.revision.manifest_hash = other.revision.manifest_hash.clone();
        self.revision.revision_key = other.revision.revision_key.clone();
    }
    fn evaluated(mut self, value: f64) -> Self {
        let mut reference: EvaluationReference = serde_json::from_str(include_str!(
            "../../../tests/fixtures/evaluation-reference.json"
        ))
        .unwrap();
        reference.variant_id = self.variant.id.clone();
        reference.score = Some(value);
        self.evaluation = Some(reference);
        self
    }
}

struct Policy {
    fit: FitWeights,
    mix: RecommendationWeights,
    hw: String,
    profile: String,
    suite: String,
    dataset: String,
}
impl Policy {
    fn new() -> Self {
        Self {
            fit: FitWeights {
                memory: 100,
                backend: 0,
                maturity: 0,
                speed: 0,
                storage: 0,
                context: 0,
                stability: 0,
            },
            mix: RecommendationWeights {
                technical: 60,
                evaluation: 40,
            },
            hw: "a".repeat(64),
            profile: "b".repeat(64),
            suite: "a".repeat(64),
            dataset: "b".repeat(64),
        }
    }
    fn get(&self) -> RecommendationPolicy<'_> {
        RecommendationPolicy {
            settings_revision: 4,
            technical_weights: &self.fit,
            recommendation_weights: &self.mix,
            hardware_fingerprint: &self.hw,
            launch_profile_hash: &self.profile,
            evaluation_context: Some((&self.suite, &self.dataset)),
        }
    }
}

#[test]
fn technical_only_is_explicit_and_popularity_never_becomes_evaluation() {
    let mut a = Graph::new(1, 10);
    a.variant
        .metadata
        .insert("evaluation_score".into(), json!(100));
    a.model.metadata.insert("downloads".into(), json!(9999999));
    let b = Graph::new(2, 40);
    let p = Policy::new();
    let result = recommend(&[b.candidate(), a.candidate()], &p.get(), 1).unwrap();
    assert_eq!(result.ranking_basis, RankingBasis::Technical);
    let best = &result.candidates[0];
    assert_eq!(best.identity.variant_id, a.variant.id);
    assert_eq!(best.technical_fit, Some(90.0));
    assert_eq!(best.evaluation_score, None);
    assert_eq!(best.recommendation_score, None);
    assert!(best.reasons.iter().any(|s| s.contains("NO EVALUADO")));
    assert_eq!(best.identity.distribution_id, a.distribution.id);
    assert_eq!(best.identity.runtime_version_id, a.runtime.id);
    assert_eq!(best.identity.storage_location_id, a.storage.id);
}

#[test]
fn comparable_external_evaluation_changes_recommendation_without_changing_fit() {
    let a = Graph::new(1, 10).evaluated(0.0);
    let b = Graph::new(2, 40).evaluated(1.0);
    let p = Policy::new();
    let result = recommend(&[a.candidate(), b.candidate()], &p.get(), 2).unwrap();
    assert_eq!(result.ranking_basis, RankingBasis::TechnicalAndEvaluation);
    assert_eq!(result.candidates[0].identity.variant_id, b.variant.id);
    assert_eq!(result.candidates[0].recommendation_score, Some(76.0));
    assert_eq!(result.candidates[0].technical_fit, Some(60.0));
    assert_eq!(result.candidates[1].recommendation_score, Some(54.0));
}

#[test]
fn mixed_evaluation_coverage_orders_every_candidate_by_technical_fit() {
    let a = Graph::new(1, 10);
    let b = Graph::new(2, 40).evaluated(1.0);
    let p = Policy::new();
    let result = recommend(&[b.candidate(), a.candidate()], &p.get(), 2).unwrap();
    assert_eq!(result.ranking_basis, RankingBasis::Technical);
    assert_eq!(result.candidates[0].identity.variant_id, a.variant.id);
    assert_eq!(result.candidates[1].evaluation_score, Some(100.0));
}

#[test]
fn evaluation_requires_matching_variant_suite_dataset_fingerprint_and_current_pass() {
    let p = Policy::new();
    for field in [
        "variant",
        "suite",
        "dataset",
        "fingerprint",
        "expired",
        "inconclusive",
        "fail",
        "blocked",
        "null",
    ] {
        let mut a = Graph::new(1, 10).evaluated(1.0);
        let r = a.evaluation.as_mut().unwrap();
        match field {
            "variant" => r.variant_id = id(999),
            "suite" => r.suite_hash = "c".repeat(64),
            "dataset" => r.dataset_hash = "c".repeat(64),
            "fingerprint" => r.execution_fingerprint = "c".repeat(64),
            "expired" => r.status = EvaluationStatus::Expired,
            "inconclusive" => r.status = EvaluationStatus::Inconclusive,
            "fail" => r.status = EvaluationStatus::Fail,
            "blocked" => r.status = EvaluationStatus::Blocked,
            _ => r.score = None,
        }
        let result = recommend(&[a.candidate()], &p.get(), 1).unwrap();
        assert_eq!(result.candidates[0].evaluation_score, None, "{field}");
    }
    let a = Graph::new(1, 10).evaluated(1.0);
    let mut policy = p.get();
    policy.evaluation_context = None;
    assert_eq!(
        recommend(&[a.candidate()], &policy, 1).unwrap().candidates[0].evaluation_score,
        None
    );
}

#[test]
fn configured_objective_changes_fit_and_extreme_mix_weights_do_not_overflow() {
    let a = Graph::new(1, 10).evaluated(1.0);
    let mut b = Graph::new(2, 40).evaluated(1.0);
    b.technical.effective_backend.as_mut().unwrap().backend = EffectiveBackend::Gpu;
    let mut p = Policy::new();
    p.fit.memory = 0;
    p.fit.backend = 100;
    p.mix.technical = u32::MAX;
    p.mix.evaluation = u32::MAX;
    let result = recommend(&[a.candidate(), b.candidate()], &p.get(), 1).unwrap();
    assert_eq!(result.candidates[0].identity.variant_id, b.variant.id);
    assert_eq!(result.candidates[0].technical_fit, Some(80.0));
    assert_eq!(result.candidates[0].recommendation_score, Some(90.0));
}

#[test]
fn same_verified_artifacts_prefer_trusted_source_and_keep_alternative_ids() {
    let a = Graph::new(1, 10);
    let mut b = Graph::new(2, 10);
    b.same_artifacts_as(&a);
    b.trust = Trust::Verified;
    let p = Policy::new();
    let result = recommend(&[a.candidate(), b.candidate()], &p.get(), 2).unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].identity.variant_id, b.variant.id);
    assert_eq!(
        result.candidates[0].equivalent_sources[0].variant_id,
        a.variant.id
    );
}

#[test]
fn equal_names_or_different_execution_context_do_not_merge_candidates() {
    let a = Graph::new(1, 10);
    let mut b = Graph::new(2, 10);
    let p = Policy::new();
    assert_eq!(
        recommend(&[a.candidate(), b.candidate()], &p.get(), 2)
            .unwrap()
            .candidates
            .len(),
        2
    );
    b.same_artifacts_as(&a);
    b.technical.execution_fingerprint = Some("c".repeat(64));
    assert_eq!(
        recommend(&[a.candidate(), b.candidate()], &p.get(), 2)
            .unwrap()
            .candidates
            .len(),
        2
    );
}

#[test]
fn headroom_and_identity_ties_are_deterministic_before_top_n() {
    let a = Graph::new(1, 40);
    let mut b = Graph::new(2, 10);
    let mut p = Policy::new();
    p.fit.memory = 0;
    p.fit.backend = 100;
    assert_eq!(
        recommend(&[a.candidate(), b.candidate()], &p.get(), 1)
            .unwrap()
            .candidates[0]
            .identity
            .variant_id,
        b.variant.id
    );
    b.technical.estimated_required_memory_bytes = a.technical.estimated_required_memory_bytes;
    assert_eq!(
        recommend(&[b.candidate(), a.candidate()], &p.get(), 1)
            .unwrap()
            .candidates[0]
            .identity
            .variant_id,
        a.variant.id
    );
}

#[test]
fn insufficient_coverage_and_unknown_gates_never_enter_recommendations() {
    let mut a = Graph::new(1, 10);
    let mut p = Policy::new();
    p.fit.memory = 30;
    p.fit.speed = 70;
    let result = recommend(&[a.candidate()], &p.get(), 1).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.excluded[0].coverage_percent, 30.0);
    a.compatibility.status = CompatibilityStatus::Unknown;
    a.compatibility.hard_gates[0].status = GateStatus::Unknown;
    let result = recommend(&[a.candidate()], &Policy::new().get(), 1).unwrap();
    assert_eq!(result.excluded[0].technical_fit, None);
}

#[test]
fn stale_join_and_invalid_evaluation_are_errors_not_partial_success() {
    let p = Policy::new();
    let mut a = Graph::new(1, 10);
    a.revision.distribution_id = id(999);
    assert!(recommend(&[a.candidate()], &p.get(), 1).is_err());
    let mut a = Graph::new(1, 10).evaluated(1.0);
    a.evaluation.as_mut().unwrap().scale.maximum = 0.0;
    assert!(recommend(&[a.candidate()], &p.get(), 1).is_err());
    let mut policy = p.get();
    policy.hardware_fingerprint = &p.dataset;
    assert!(recommend(&[Graph::new(1, 10).candidate()], &policy, 1).is_err());
}

#[test]
fn zero_weights_and_unavailable_backend_preserve_diagnostics() {
    let a = Graph::new(1, 10);
    let mut p = Policy::new();
    p.mix.technical = 0;
    p.mix.evaluation = 0;
    let result = recommend(&[a.candidate()], &p.get(), 10).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.excluded[0].recommendation_score, None);
    let mut a = Graph::new(1, 10);
    a.backend.evidence_refs.clear();
    let result = recommend(&[a.candidate()], &Policy::new().get(), 10).unwrap();
    assert_eq!(result.excluded[0].technical_fit, None);
}

#[test]
fn input_permutations_have_identical_serialization_and_zero_limit_is_empty() {
    let a = Graph::new(1, 10);
    let b = Graph::new(2, 20);
    let c = Graph::new(3, 30);
    let p = Policy::new();
    let first = recommend(&[a.candidate(), b.candidate(), c.candidate()], &p.get(), 3).unwrap();
    let second = recommend(&[c.candidate(), a.candidate(), b.candidate()], &p.get(), 3).unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    assert!(recommend(&[a.candidate()], &p.get(), 0)
        .unwrap()
        .candidates
        .is_empty());
}

#[test]
fn repeated_tuple_is_rejected_even_if_its_assessment_differs() {
    let a = Graph::new(1, 10);
    let mut b = Graph::new(1, 20);
    b.trust = Trust::Verified;
    let p = Policy::new();
    assert!(recommend(&[a.candidate(), b.candidate()], &p.get(), 10).is_err());
    assert!(recommend(&[b.candidate(), a.candidate()], &p.get(), 10).is_err());
}

#[test]
fn revoked_corrupt_broken_or_offline_candidates_have_no_technical_score() {
    let p = Policy::new();
    for field in [
        "source",
        "model",
        "runtime",
        "installation",
        "backend",
        "storage",
        "volume",
        "bundle",
    ] {
        let mut a = Graph::new(1, 10);
        match field {
            "source" => a.trust = Trust::Revoked,
            "model" => {
                a.variant.lifecycle = "CORRUPT".into();
                a.variant.status = a.variant.derived_status().into();
            }
            "runtime" => {
                a.runtime.lifecycle = larm_domain::runtime::RuntimeVersionLifecycle::Broken
            }
            "installation" => {
                a.installation.state = larm_domain::runtime::InstallationState::Retired;
                a.installation.stage = larm_domain::runtime::InstallationStage::Retired;
            }
            "backend" => a.backend.status = larm_domain::runtime::BackendStatus::Unknown,
            "storage" => a.storage.state = larm_domain::locations::LocationState::Offline,
            "volume" => a.storage.volume_key = "unknown:unit".into(),
            _ => a.variant.bundle_hash = None,
        }
        let result = recommend(&[a.candidate()], &p.get(), 10).unwrap();
        assert!(result.candidates.is_empty(), "{field}");
        assert_eq!(result.excluded[0].technical_fit, None, "{field}");
    }
}

#[test]
fn malformed_domain_records_are_rejected_before_ranking() {
    let p = Policy::new();
    for field in [
        "role",
        "name",
        "upstream",
        "revision",
        "runtime_version",
        "backend_limits",
        "stage",
    ] {
        let mut a = Graph::new(1, 10);
        match field {
            "role" => a.variant.system_role = "NONE".into(),
            "name" => a.model.name.clear(),
            "upstream" => a.distribution.upstream_namespace.clear(),
            "revision" => a.revision.manifest_hash = "invalid".into(),
            "runtime_version" => a.runtime.version_string.clear(),
            "backend_limits" => a.backend.limits.max_instances = 0,
            _ => a.installation.stage = larm_domain::runtime::InstallationStage::Retired,
        }
        assert!(
            recommend(&[a.candidate()], &p.get(), 10).is_err(),
            "{field}"
        );
    }
}

#[test]
fn read_only_model_storage_does_not_require_write_placement_permission() {
    let mut a = Graph::new(1, 10);
    a.storage.state = larm_domain::locations::LocationState::ReadOnly;
    assert_eq!(
        recommend(&[a.candidate()], &Policy::new().get(), 1)
            .unwrap()
            .candidates
            .len(),
        1
    );
}

#[test]
fn equal_scores_break_on_runtime_version_after_variant_identity() {
    let a = Graph::new(1, 10);
    let mut b = Graph::new(1, 10);
    b.runtime.id = id(95);
    b.installation.runtime_version_id = id(95);
    b.compatibility.runtime_version_id = id(95);
    let p = Policy::new();
    let result = recommend(&[b.candidate(), a.candidate()], &p.get(), 2).unwrap();
    assert_eq!(
        result.candidates[0].identity.runtime_version_id,
        a.runtime.id
    );
}

#[test]
fn known_zero_headroom_orders_before_unknown_without_imputing_zero() {
    let a = Graph::new(1, 100);
    let mut b = Graph::new(2, 10);
    b.technical.available_memory_budget_bytes = None;
    let mut p = Policy::new();
    p.fit.memory = 0;
    p.fit.backend = 100;
    let result = recommend(&[b.candidate(), a.candidate()], &p.get(), 2).unwrap();
    assert_eq!(result.candidates[0].identity.variant_id, a.variant.id);
    assert_eq!(result.candidates[0].headroom_bytes.unwrap().get(), 0);
    assert_eq!(result.candidates[1].headroom_bytes, None);
}
