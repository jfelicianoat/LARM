//! Persistence integration only. P026 observations and the physical guard are
//! deliberate fixtures; these cases do not qualify a worker or convert a model.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::{pool::Db, transformation_plans::SqlTransformationPlans};
use larm_domain::{
    adapters::{AdapterInstallation, AdapterState, AdapterTrust},
    approval::ApprovalRequest,
    conformance::{hash, BindingHashes, Observation, Outcome, Proof, Run},
    conformance_profiles::ConformanceProfile,
    error::{ErrorCode, LarmError},
    estimate::{Confidence, StorageEstimate},
    ports::{
        AdapterConformanceRepository, AdapterInstallationRepository, AdapterQualification,
        AdapterQualificationGuard, ApprovalRepository, ConformanceProfileRepository,
        MutationContext,
    },
    transformations::{
        seal_bound_plan, seal_plan, InputArtifact, PlanDraft, PlanStep, SealedTransformationPlan,
        TransformationExecutionBinding, TransformationRequest,
    },
    units::DecimalU64,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[path = "transformation_plans/admission.rs"]
mod admission;

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn owner() -> MutationContext {
    MutationContext {
        actor_id: "plan-owner".into(),
        correlation_id: id(),
    }
}
fn backend() -> MutationContext {
    MutationContext {
        actor_id: "profile-backend".into(),
        correlation_id: id(),
    }
}

#[derive(Default)]
struct FixtureGate {
    reject: Arc<AtomicBool>,
    acquisitions: AtomicUsize,
}
struct FixtureGuard(Arc<AtomicBool>);
#[async_trait::async_trait]
impl AdapterQualificationGuard for FixtureGuard {
    async fn recheck(&self) -> Result<(), LarmError> {
        if self.0.load(Ordering::SeqCst) {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "Fixture changed",
                "fixture_guard_changed",
            ));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl AdapterQualification for FixtureGate {
    async fn acquire(
        &self,
        a: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<Box<dyn AdapterQualificationGuard>, LarmError> {
        assert_eq!(a.family, "transformation");
        assert_eq!(run.installation_id, a.id);
        assert_eq!(ctx.actor_id, backend().actor_id);
        self.acquisitions.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FixtureGuard(self.reject.clone())))
    }
}

struct Fixture {
    db: Db,
    repo: SqlTransformationPlans,
    gate: Arc<FixtureGate>,
    binding: TransformationExecutionBinding,
    request: TransformationRequest,
    input: Vec<InputArtifact>,
    _directory: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let now = chrono::Utc::now();
        let at = larm_domain::bridge::timestamp(now);
        let a = AdapterInstallation {
            id: id(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: Default::default(),
            adapter_id: "fixture.transformation".into(),
            version: "1.0.0".into(),
            family: "transformation".into(),
            package_hash: "a".repeat(64),
            publisher: "persistence fixture".into(),
            trust: AdapterTrust::Untrusted,
            state: AdapterState::Installed,
            manifest: json!({"id":"fixture.transformation","version":"1.0.0","family":"transformation","publisher":"persistence fixture","package_sha256":"a".repeat(64),"permissions":{},"capabilities":[]}),
        };
        let installations = larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone(),
        };
        installations.insert(&a, &backend()).await.unwrap();
        let approval=ApprovalRequest {
            operation_digest:"b".repeat(64),resource_ids:vec![a.id.clone()],code_hash:Some(a.package_hash.clone()),
            permissions:json!({"publisher":a.publisher,"permissions":{},"expires_at":larm_domain::bridge::timestamp(now+chrono::Duration::hours(1)),"secret_refs":[]}).as_object().unwrap().clone(),approved:true,
        }.grant(id(),backend().actor_id,now,chrono::Duration::minutes(15)).unwrap();
        larm_db::security::SqlApprovals {
            pool: db.pool.clone(),
        }
        .insert(&approval, &backend())
        .await
        .unwrap();
        let profile = ConformanceProfile {
            id: id(),
            installation_id: a.id.clone(),
            actor_id: backend().actor_id,
            manifest_sha256: hash(&a.manifest).unwrap(),
            configuration: json!({"scope":"CODE_PASS repository fixture, no worker execution"}),
            approval,
            created_at: at,
        };
        larm_db::conformance_profiles::SqlConformanceProfiles {
            pool: db.pool.clone(),
        }
        .store_conformance_profile(&profile, None, &backend())
        .await
        .unwrap();
        let run = Run::new(
            &a,
            BindingHashes {
                suite_sha256: "b".repeat(64),
                capability_profile_sha256: "c".repeat(64),
                execution_context_sha256: "d".repeat(64),
            },
            &backend().actor_id,
        )
        .unwrap();
        let observations = run
            .required_checks()
            .into_iter()
            .map(|check| Observation {
                check_id: check.clone(),
                passed: true,
                evidence_sha256: hash(&json!({"fixture":check,"run":run.id})).unwrap(),
            })
            .collect();
        let outcome = Outcome::Passed {
            proof: Proof::evaluate(&run, observations).unwrap(),
        };
        larm_db::conformance::SqlAdapterConformance {
            pool: db.pool.clone(),
        }
        .commit_conformance(&run, &outcome, &backend())
        .await
        .unwrap();
        let binding = TransformationExecutionBinding {
            adapter_installation_id: a.id,
            conformance_run_id: run.id.clone(),
            conformance_run_sha256: run.digest().unwrap(),
            profile_sha256: hash(&profile).unwrap(),
            worker_plan_id: id(),
            worker_plan_hash: "e".repeat(64),
        };
        let request = TransformationRequest {
            input_variant_id: id(),
            adapter_id: "fixture.transformation".into(),
            tool_version: "1.0.0".into(),
            target_format: "BASE64".into(),
            parameters: json!({}),
            storage_location_id: id(),
            hard_budget_bytes: None,
        };
        let input = vec![InputArtifact {
            artifact_id: id(),
            relative_path: "input.bin".into(),
            size_bytes: DecimalU64::new(4).unwrap(),
            sha256: "f".repeat(64),
        }];
        let gate = Arc::new(FixtureGate::default());
        let repo = SqlTransformationPlans {
            pool: db.pool.clone(),
            qualification: Some(gate.clone()),
        };
        Self {
            db,
            repo,
            gate,
            binding,
            request,
            input,
            _directory: directory,
        }
    }
    fn draft(&self) -> PlanDraft {
        PlanDraft {
            operation: "transformation.execute".into(),
            resource_ids: vec![
                self.request.input_variant_id.clone(),
                self.request.storage_location_id.clone(),
            ],
            storage_estimate: StorageEstimate {
                download_bytes: None,
                final_artifact_bytes: DecimalU64::new(8),
                installed_bytes: None,
                derived_cache_bytes: None,
                temporary_peak_bytes: DecimalU64::new(8),
                existing_bytes: DecimalU64::new(4),
                peak_extra_bytes: DecimalU64::new(16),
                required_free_bytes: DecimalU64::new((1 << 30) + 16),
                upper_bound_bytes: DecimalU64::new(16),
                confidence: Confidence::Estimated,
                reason: "repository fixture only".into(),
                evidence_refs: vec!["fixture:storage".into()],
                per_volume: vec![],
            },
            steps: vec![PlanStep {
                phase: "BASE64".into(),
                adapter_id: Some(self.request.adapter_id.clone()),
                request: json!({}),
            }],
            expires_at: larm_domain::bridge::timestamp(
                chrono::Utc::now() + chrono::Duration::minutes(15),
            ),
            approval_required: false,
        }
    }
    fn plan(&self) -> SealedTransformationPlan {
        seal_bound_plan(
            id(),
            self.request.clone(),
            self.input.clone(),
            self.draft(),
            self.binding.clone(),
        )
        .unwrap()
    }
    async fn counts(&self) -> (i64, i64, i64, i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='transformation.plan'),(SELECT count(*) FROM event WHERE resource_type='transformation_plan'),(SELECT count(*) FROM event_outbox o JOIN event e ON e.id=o.event_record_id WHERE e.resource_type='transformation_plan'),(SELECT count(*) FROM transformation_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)")
            .fetch_one(&self.db.pool).await.unwrap()
    }
}

#[tokio::test]
async fn preview_is_durable_owned_and_idempotent_without_jobs_reservations_or_locks() {
    let f = Fixture::new().await;
    let p = f.plan();
    f.repo.store_plan(&p, &owner()).await.unwrap();
    f.repo.store_plan(&p, &owner()).await.unwrap();
    assert_eq!(f.counts().await, (1, 1, 1, 0, 0, 0));
    assert_eq!(f.gate.acquisitions.load(Ordering::SeqCst), 2);
    let mut outsider = owner();
    outsider.actor_id = "another-owner".into();
    assert!(f
        .repo
        .plan(&p.response.plan_id, &outsider)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        f.repo.plan(&p.response.plan_id, &owner()).await.unwrap(),
        Some(p.clone())
    );
    let path: String =
        sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
    f.db.pool.close().await;
    let reopened = Db::open(std::path::Path::new(&path)).await.unwrap();
    let repo = SqlTransformationPlans {
        pool: reopened.pool.clone(),
        qualification: None,
    };
    assert_eq!(
        repo.plan(&p.response.plan_id, &owner()).await.unwrap(),
        Some(p)
    );
    reopened.pool.close().await;
}

#[tokio::test]
async fn event_failure_rolls_back_the_plan_and_all_its_effects() {
    let f = Fixture::new().await;
    sqlx::query("CREATE TRIGGER reject_plan_event BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned fixture fault'); END").execute(&f.db.pool).await.unwrap();
    assert!(f.repo.store_plan(&f.plan(), &owner()).await.is_err());
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn changed_physical_guard_rolls_back_and_is_rechecked_on_noop() {
    let f = Fixture::new().await;
    let p = f.plan();
    f.gate.reject.store(true, Ordering::SeqCst);
    assert_eq!(
        f.repo
            .store_plan(&p, &owner())
            .await
            .unwrap_err()
            .details
            .reason,
        "fixture_guard_changed"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0, 0));
    f.gate.reject.store(false, Ordering::SeqCst);
    f.repo.store_plan(&p, &owner()).await.unwrap();
    f.gate.reject.store(true, Ordering::SeqCst);
    assert!(f.repo.store_plan(&p, &owner()).await.is_err());
    assert_eq!(f.counts().await, (1, 1, 1, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn same_id_cannot_replace_contents_or_owner_even_with_valid_new_seal() {
    let f = Fixture::new().await;
    let p = f.plan();
    f.repo.store_plan(&p, &owner()).await.unwrap();
    let mut request = f.request.clone();
    request.parameters = json!({"different":true});
    let changed = seal_bound_plan(
        &p.response.plan_id,
        request,
        f.input.clone(),
        f.draft(),
        f.binding.clone(),
    )
    .unwrap();
    assert_eq!(
        f.repo
            .store_plan(&changed, &owner())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut outsider = owner();
    outsider.actor_id = "another-owner".into();
    assert_eq!(
        f.repo.store_plan(&p, &outsider).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        f.repo.plan(&p.response.plan_id, &owner()).await.unwrap(),
        Some(p)
    );
    assert_eq!(f.counts().await, (1, 1, 1, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn concurrent_identical_stores_publish_one_plan_and_one_event() {
    let f = Fixture::new().await;
    let p = f.plan();
    let a = owner();
    let b = owner();
    let (first, second) = tokio::join!(f.repo.store_plan(&p, &a), f.repo.store_plan(&p, &b));
    first.unwrap();
    second.unwrap();
    assert_eq!(f.counts().await, (1, 1, 1, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn no_physical_gate_and_stale_qualification_or_profile_cannot_store() {
    let f = Fixture::new().await;
    let unqualified = SqlTransformationPlans {
        pool: f.db.pool.clone(),
        qualification: None,
    };
    let mut request = f.request.clone();
    request.adapter_id = "another.adapter".into();
    let wrong =
        seal_bound_plan(id(), request, f.input.clone(), f.draft(), f.binding.clone()).unwrap();
    assert_eq!(
        f.repo
            .store_plan(&wrong, &owner())
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_adapter_identity_mismatch"
    );
    assert_eq!(
        unqualified
            .store_plan(&f.plan(), &owner())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_physical_gate_unavailable"
    );
    for field in ["run", "profile"] {
        let mut binding = f.binding.clone();
        if field == "run" {
            binding.conformance_run_sha256 = "0".repeat(64);
        } else {
            binding.profile_sha256 = "0".repeat(64);
        }
        let p =
            seal_bound_plan(id(), f.request.clone(), f.input.clone(), f.draft(), binding).unwrap();
        assert_eq!(
            f.repo.store_plan(&p, &owner()).await.unwrap_err().code,
            ErrorCode::StaleRevision
        );
    }
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&f.binding.adapter_installation_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.repo.store_plan(&f.plan(), &owner()).await.is_err());
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn unbound_expired_or_tampered_plans_create_no_persistent_state() {
    let f = Fixture::new().await;
    let legacy = seal_plan(id(), f.request.clone(), f.input.clone(), f.draft()).unwrap();
    assert_eq!(
        f.repo
            .store_plan(&legacy, &owner())
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_execution_binding_missing"
    );
    let mut draft = f.draft();
    draft.expires_at = "2000-01-01T00:00:00.000Z".into();
    let expired = seal_bound_plan(
        id(),
        f.request.clone(),
        f.input.clone(),
        draft,
        f.binding.clone(),
    )
    .unwrap();
    assert_eq!(
        f.repo
            .store_plan(&expired, &owner())
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_plan_expired"
    );
    let mut tampered = f.plan();
    tampered.request.tool_version = "other".into();
    assert_eq!(
        f.repo
            .store_plan(&tampered, &owner())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityFailed
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0, 0));
    f.db.pool.close().await;
}

#[tokio::test]
async fn stored_payload_with_changed_binding_or_unknown_version_is_not_a_valid_plan() {
    let f = Fixture::new().await;
    let p = f.plan();
    f.repo.store_plan(&p, &owner()).await.unwrap();
    for modification in [
        "json_set(payload,'$.schema_version',2)",
        "json_set(payload,'$.plan.execution_binding.worker_plan_hash','bad')",
    ] {
        let original: String =
            sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=?")
                .bind(&p.response.plan_id)
                .fetch_one(&f.db.pool)
                .await
                .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE operation_journal SET payload={modification} WHERE id=?"
        )))
        .bind(&p.response.plan_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
        assert!(f.repo.plan(&p.response.plan_id, &owner()).await.is_err());
        sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
            .bind(original)
            .bind(&p.response.plan_id)
            .execute(&f.db.pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE operation_journal SET resource_ids='[]' WHERE id=?")
        .bind(&p.response.plan_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.repo.plan(&p.response.plan_id, &owner()).await.is_err());
    assert!(f.repo.store_plan(&p, &owner()).await.is_err());
    f.db.pool.close().await;
}
