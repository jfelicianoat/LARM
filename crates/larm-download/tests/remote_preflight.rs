//! SQLite/service integration with explicit CODE_PASS qualification and space
//! fixtures. The consumer case checks actual local bytes/CAS; it is not P026,
//! a native source worker, or observed volume-capacity evidence.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use larm_domain::{
    acquisition::ModelAcquisitionManifest,
    bridge::{timestamp, ApiError},
    download::{
        DownloadPlanRecord, DownloadPlanRequest, DownloadPriority, DownloadSourceCandidate,
        ExecutePlan,
    },
    estimate::MarginPolicy,
    locations::{LocationState, StorageLocation},
    ports::{
        DownloadActivationIdempotency, DownloadMirrorCatalog, DownloadPlanActivationService,
        DownloadPlanExecutionGate, DownloadPlanPreviewService, DownloadPlanRepository,
        MutationContext, StorageLocationRepository,
    },
    storage::{DriveKind, PerformanceClass, StorageObservation, StorageVolume, VolumeIdentity},
    units::DecimalU64,
};
use larm_download::{
    activation_service::DownloadPlanActivator, preview_service::DownloadPlanPreview,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[path = "../../larm-db/tests/support/acquisition_fixture.rs"]
mod snapshot_fixture;

const VOLUME: &str = "preflight-code-pass-volume";

struct Space;
#[async_trait]
impl larm_domain::ports::StorageProber for Space {
    async fn probe(&self, _: &[String]) -> Result<StorageObservation, ApiError> {
        larm_domain::ports::StorageObservationRefresher::refresh(self, &snapshot_fixture::ctx())
            .await
    }
}
#[async_trait]
impl larm_domain::ports::StorageObservationRefresher for Space {
    async fn refresh(&self, _: &MutationContext) -> Result<StorageObservation, ApiError> {
        Ok(StorageObservation {
            observed_at: timestamp(chrono::Utc::now()),
            unknown: vec![],
            volumes: vec![StorageVolume {
                identity: VolumeIdentity {
                    volume_key: Some(VOLUME.into()),
                    volume_guid: None,
                    unc_path: None,
                    mount_points: vec![],
                    drive_letter: None,
                },
                label: None,
                filesystem: Some("NTFS".into()),
                drive_kind: DriveKind::Fixed,
                bus: None,
                seek_penalty: None,
                performance_class: PerformanceClass::Unknown,
                capacity_bytes: DecimalU64::new(1 << 30),
                free_bytes: DecimalU64::new(1 << 30),
                read_only: false,
                online: true,
                evidence_refs: vec!["fixture:space-code-pass".into()],
            }],
        })
    }
}

struct NoMirrors;
#[async_trait]
impl DownloadMirrorCatalog for NoMirrors {
    async fn eligible_candidates(
        &self,
        _: &str,
        _: &[larm_domain::download::DownloadArtifactEstimate],
    ) -> Result<Vec<DownloadSourceCandidate>, ApiError> {
        Ok(vec![])
    }
}

struct Gate {
    pool: sqlx::SqlitePool,
    disable_once: AtomicBool,
    calls: AtomicUsize,
}

struct ExpiringQualification;
struct ExpiringQualificationGuard;
#[async_trait]
impl larm_domain::ports::AdapterQualification for ExpiringQualification {
    async fn acquire(
        &self,
        _: &larm_domain::adapters::AdapterInstallation,
        _: &larm_domain::conformance::Run,
        _: &MutationContext,
    ) -> Result<Box<dyn larm_domain::ports::AdapterQualificationGuard>, ApiError> {
        Ok(Box::new(ExpiringQualificationGuard))
    }
}
#[async_trait]
impl larm_domain::ports::AdapterQualificationGuard for ExpiringQualificationGuard {
    async fn recheck(&self) -> Result<(), ApiError> {
        Err(ApiError::new(
            "FORBIDDEN",
            "Fixture qualification expired",
            "fixture_qualification_expired",
        ))
    }
}
#[async_trait]
impl DownloadPlanExecutionGate for Gate {
    async fn authorize(
        &self,
        record: &DownloadPlanRecord,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        assert!(record.response.acquisition.is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.disable_once.swap(false, Ordering::SeqCst) {
            sqlx::query("UPDATE model_source SET enabled=0,row_version=row_version+1 WHERE id=?")
                .bind(record.response.source_id.as_deref())
                .execute(&self.pool)
                .await
                .unwrap();
        }
        Ok(())
    }
}

struct Fixture {
    db: larm_db::pool::Db,
    snapshot: ModelAcquisitionManifest,
    location_id: String,
    preview: DownloadPlanPreview,
    activation: DownloadPlanActivator,
    plans: Arc<larm_db::download_plans::SqlDownloadPlans>,
    gate: Arc<Gate>,
    _directory: tempfile::TempDir,
}

struct MemorySource {
    snapshot: ModelAcquisitionManifest,
    closed: Arc<AtomicUsize>,
}
struct MemorySession {
    snapshot: ModelAcquisitionManifest,
    estimate: larm_domain::estimate::StorageEstimate,
    last_plan: std::sync::Mutex<Option<larm_domain::artifacts::Plan>>,
    closed: Arc<AtomicUsize>,
}
#[async_trait]
impl larm_download::consumer::DownloadSourceResolver for MemorySource {
    async fn open(
        &self,
        _: &larm_db::download_jobs::DownloadClaim,
        record: &DownloadPlanRecord,
        _: &DownloadSourceCandidate,
    ) -> Result<
        Box<dyn larm_download::consumer::DownloadSourceSession>,
        larm_domain::error::LarmError,
    > {
        self.snapshot.validate_download_record(record)?;
        Ok(Box::new(MemorySession {
            snapshot: self.snapshot.clone(),
            estimate: record.response.storage_estimate.clone().unwrap(),
            last_plan: std::sync::Mutex::new(None),
            closed: self.closed.clone(),
        }))
    }
}
#[async_trait]
impl larm_download::consumer::DownloadSourceSession for MemorySession {
    fn acquisition(&self) -> Option<&ModelAcquisitionManifest> {
        Some(&self.snapshot)
    }
    async fn download_plan(
        &self,
        request: larm_adapter_sdk::source::DownloadPlanRequest,
    ) -> Result<larm_domain::artifacts::Plan, ApiError> {
        use larm_adapter_sdk::source::{
            AuthResult, RevisionRef, SourceArtifact, SourceDownloadSpec,
        };
        assert_eq!(
            request.acquisition.as_ref().unwrap().revision,
            self.snapshot.source_reference
        );
        assert_eq!(
            request.acquisition.as_ref().unwrap().artifacts[0].artifact_id,
            self.snapshot.artifacts[0].artifact_id
        );
        let revision = &self.snapshot.source_reference;
        let spec = SourceDownloadSpec {
            variant_id: request.variant_id.clone(),
            storage_location_id: request.storage_location_id.clone(),
            revision: RevisionRef {
                source_id: revision.source_id.clone(),
                upstream_namespace: revision.upstream_namespace.clone(),
                upstream_model_id: revision.upstream_model_id.clone(),
                revision_key: revision.revision_key.clone(),
            },
            artifacts: self
                .snapshot
                .artifacts
                .iter()
                .filter(|a| {
                    a.required
                        || request
                            .optional_artifact_ids
                            .as_ref()
                            .is_some_and(|ids| ids.contains(&a.artifact_id))
                })
                .map(|a| SourceArtifact {
                    relative_path: a.relative_path.clone(),
                    size_bytes: a.size_bytes,
                    size_estimate: Some(a.size_estimate.clone()),
                    sha256: a.sha256.clone(),
                    required: a.required,
                    role: a.role.clone(),
                })
                .collect(),
            auth: AuthResult {
                authenticated: false,
                auth_required: false,
                license_required: false,
                reason: "code_pass_public_fixture".into(),
            },
            resumable: false,
            priority: request.priority,
            hard_budget_bytes: request.hard_budget_bytes,
        };
        let plan = spec.bind(
            &request,
            larm_domain::artifacts::Plan {
                plan_id: snapshot_fixture::id(),
                plan_hash: "0".repeat(64),
                operation: "source.download".into(),
                resource_ids: vec![
                    request.variant_id.clone(),
                    request.storage_location_id.clone(),
                ],
                storage_estimate: self.estimate.clone(),
                steps: vec![],
                expires_at: timestamp(chrono::Utc::now() + chrono::Duration::minutes(5)),
                approval_required: false,
            },
            "fixture.source",
        ).inspect_err(|error| {
            eprintln!("CODE_PASS source plan rejected fields={:?}; core_download={:?}; source_download={:?}",
                error.details.resource_ids, self.estimate.download_bytes, spec.download_bytes());
        })?;
        *self.last_plan.lock().unwrap() = Some(plan.clone());
        Ok(plan)
    }
    async fn download_chunk(
        &self,
        request: larm_adapter_sdk::source::SourceDownloadChunkRequest,
    ) -> Result<larm_adapter_sdk::source::SourceDownloadChunk, ApiError> {
        let saved = self.last_plan.lock().unwrap();
        let plan = saved.as_ref().unwrap();
        assert_eq!(
            (&request.plan_id, &request.plan_hash),
            (&plan.plan_id, &plan.plan_hash)
        );
        assert_eq!(
            request.relative_path,
            self.snapshot.artifacts[0].relative_path
        );
        assert_eq!(request.offset.get(), 0);
        assert!(request.max_bytes.get() >= 4);
        Ok(larm_adapter_sdk::source::SourceDownloadChunk {
            offset: request.offset,
            data_base64: "ZGF0YQ==".into(),
            eof: true,
            total_bytes: DecimalU64::new(4),
            validator: None,
        })
    }
    async fn close(&self) -> Result<(), ApiError> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Root(std::path::PathBuf);
#[async_trait]
impl larm_download::consumer::DownloadStorageRootResolver for Root {
    async fn root(&self, _: &str) -> Result<std::path::PathBuf, ApiError> {
        Ok(self.0.clone())
    }
}

impl Fixture {
    fn consumer(&self, closed: Arc<AtomicUsize>) -> larm_download::consumer::DownloadConsumer {
        larm_download::consumer::DownloadConsumer {
            jobs: larm_db::download_jobs::SqlDownloadJobs {
                pool: self.db.pool.clone(),
            },
            plans: (*self.plans).clone(),
            progress: larm_download::transfer::SqliteTransferProgress::new(
                larm_db::download_progress::SqlDownloadProgress {
                    pool: self.db.pool.clone(),
                },
            ),
            sources: Arc::new(MemorySource {
                snapshot: self.snapshot.clone(),
                closed,
            }),
            storage: Arc::new(Root(self._directory.path().to_path_buf())),
            bandwidth_limiter: Arc::new(larm_download::policy::BandwidthLimiter::new(None)),
        }
    }
    async fn new() -> Self {
        Self::new_with_optional(false).await
    }
    async fn new_with_optional(optional: bool) -> Self {
        let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let mut snapshot = snapshot_fixture::seed(&db.pool).await;
        if optional {
            let mut artifact = snapshot.artifacts[0].clone();
            artifact.artifact_id = snapshot_fixture::id();
            artifact.relative_path = "z-not-selected.bin".into();
            artifact.required = false;
            artifact.role = "other".into();
            sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,required,size_estimate) VALUES(?,'t','t',1,'{\"role\":\"other\"}',?,?,0,?)")
                .bind(&artifact.artifact_id).bind(&snapshot.variant_id).bind(&artifact.relative_path)
                .bind(serde_json::to_string(&artifact.size_estimate).unwrap()).execute(&db.pool).await.unwrap();
            snapshot
                .optional_artifact_ids
                .push(artifact.artifact_id.clone());
            snapshot.artifacts.push(artifact);
            snapshot = snapshot.seal().unwrap();
        }
        let acquisition = Arc::new(snapshot_fixture::repo(&db.pool));
        acquisition
            .record(&snapshot, &snapshot_fixture::ctx())
            .await
            .unwrap();
        let locations = Arc::new(larm_db::locations::SqlStorageLocations {
            pool: db.pool.clone(),
        });
        let location_id = snapshot_fixture::id();
        let now = timestamp(chrono::Utc::now());
        let path = directory.path().to_string_lossy().to_string();
        locations
            .insert(
                &StorageLocation {
                    id: location_id.clone(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    row_version: 1,
                    metadata: Default::default(),
                    name: "CODE_PASS destination".into(),
                    root_path: path.clone(),
                    canonical_path: path,
                    volume_guid: None,
                    drive_letter: None,
                    unc_path: None,
                    filesystem: "NTFS".into(),
                    storage_type: "FIXED".into(),
                    capacity_bytes: DecimalU64::new(1 << 30).unwrap(),
                    free_bytes: DecimalU64::new(1 << 30).unwrap(),
                    reserved_bytes: DecimalU64::new(0).unwrap(),
                    priority: 0,
                    auto_placement: true,
                    state: LocationState::Online,
                    last_seen_at: Some(now),
                    volume_key: VOLUME.into(),
                    performance_class: PerformanceClass::Unknown,
                },
                &snapshot_fixture::ctx(),
            )
            .await
            .unwrap();
        let plans = Arc::new(larm_db::download_plans::SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: acquisition.qualification.clone(),
        });
        let variants = Arc::new(larm_db::variants::SqlVariants {
            pool: db.pool.clone(),
        });
        let models = Arc::new(larm_db::models::SqlModels {
            pool: db.pool.clone(),
        });
        let reservations = Arc::new(larm_db::reservations::SqlVolumeReservations {
            pool: db.pool.clone(),
        });
        let gate = Arc::new(Gate {
            pool: db.pool.clone(),
            disable_once: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let policy = larm_storage::estimate::SpacePolicy {
            margin: MarginPolicy {
                minimum_margin_bytes: 16,
                margin_basis_points: 0,
            },
            minimum_free_bytes: 0,
        };
        let preview = DownloadPlanPreview {
            acquisitions: acquisition.clone(),
            gate: gate.clone(),
            variants: variants.clone(),
            models: models.clone(),
            locations: locations.clone(),
            storage: Arc::new(Space),
            reservations: reservations.clone(),
            plans: plans.clone(),
            mirrors: Arc::new(NoMirrors),
            policy,
        };
        let activation = DownloadPlanActivator {
            acquisitions: acquisition,
            plans: plans.clone(),
            activation: plans.clone(),
            variants,
            models,
            locations,
            storage: Arc::new(Space),
            reservations,
            mirrors: Arc::new(NoMirrors),
            gate: gate.clone(),
            policy,
        };
        Self {
            _directory: directory,
            db,
            snapshot,
            location_id,
            preview,
            activation,
            plans,
            gate,
        }
    }

    fn request(&self, destination: bool, budget: bool) -> DownloadPlanRequest {
        DownloadPlanRequest {
            variant_id: self.snapshot.variant_id.clone(),
            storage_location_id: destination.then(|| self.location_id.clone()),
            hard_budget_bytes: budget.then(|| DecimalU64::new(1 << 20).unwrap()),
            source_id: None,
            priority: DownloadPriority::Normal,
            optional_artifact_ids: vec![],
        }
    }

    async fn counts(&self) -> (i64, i64, i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM model_bundle),(SELECT COUNT(*) FROM artifact_location),(SELECT COUNT(*) FROM download_job),(SELECT COUNT(*) FROM volume_reservation),(SELECT COUNT(*) FROM operation_journal WHERE kind='download.plan')")
            .fetch_one(&self.db.pool).await.unwrap()
    }

    async fn events(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM event")
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn remote_comparison_and_budgeted_preparation_need_no_physical_bundle_or_implicit_download() {
    let f = Fixture::new().await;
    let before = f.counts().await;
    let comparison = f
        .preview
        .preview(&f.request(false, false), &snapshot_fixture::ctx())
        .await
        .unwrap();
    assert_eq!(comparison.acquisition, Some(f.snapshot.binding()));
    assert!(comparison.plan_id.is_none() && comparison.selected_storage_location_id.is_none());
    assert_eq!(
        comparison.artifact_estimates[0].size_estimate.confidence,
        larm_domain::model::ArtifactSizeConfidence::Unknown
    );
    assert!(comparison.artifact_estimates[0]
        .size_estimate
        .bytes
        .is_none());
    assert_eq!(f.counts().await, before);
    assert_eq!(f.gate.calls.load(Ordering::SeqCst), 0);
    let unknown = f
        .preview
        .preview(&f.request(true, false), &snapshot_fixture::ctx())
        .await
        .unwrap();
    assert!(unknown.plan_id.is_none() && !unknown.destination_options[0].eligible);
    assert_eq!(f.counts().await, before);
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    assert!(planned.plan_id.is_some() && planned.operator_confirmation_required);
    let record = f
        .plans
        .get_plan(planned.plan_id.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    f.snapshot.validate_download_record(&record).unwrap();
    assert_eq!(f.counts().await, (0, 0, 0, 0, 1));
    let variant = f
        .activation
        .variants
        .get_variant(&f.snapshot.variant_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (variant.lifecycle.as_str(), variant.availability.as_str()),
        ("DISCOVERED", "UNKNOWN")
    );
}

#[tokio::test]
async fn confirmation_is_explicit_and_acceptance_reserves_once_with_snapshot_bound_to_job() {
    let f = Fixture::new().await;
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    let mut execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: false,
    };
    let idempotency = DownloadActivationIdempotency {
        key: "remote-confirmation".into(),
        body_hash: "c".repeat(64),
    };
    let ctx = snapshot_fixture::ctx();
    assert_eq!(
        f.activation
            .activate(&execute, &ctx, &idempotency)
            .await
            .unwrap_err()
            .details
            .reason,
        "operator_confirmation_required"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 1));
    execute.operator_confirmed = true;
    let receipt = f
        .activation
        .activate(&execute, &ctx, &idempotency)
        .await
        .unwrap();
    let before_replay = f.events().await;
    let replay = f
        .activation
        .activate(&execute, &ctx, &idempotency)
        .await
        .unwrap();
    assert_eq!(receipt, replay);
    assert_eq!(f.events().await, before_replay);
    let locks: i64 = sqlx::query_scalar("SELECT count(*) FROM resource_lock WHERE owner_id=?")
        .bind(&execute.plan_id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(locks, 3);
    use larm_domain::ports::ResourceGuard;
    let guard = larm_db::guard::SqlResourceGuard {
        pool: f.db.pool.clone(),
    };
    assert_eq!(
        guard
            .acquire(
                &snapshot_fixture::id(),
                0,
                &[(
                    larm_domain::guard::Resource::new(
                        larm_domain::guard::ResourceKind::Variant,
                        &f.snapshot.variant_id
                    ),
                    larm_domain::guard::LockMode::Shared
                )],
                None
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "resource_locked"
    );
    assert_eq!(f.counts().await, (0, 0, 1, 1, 1));
    let metadata: String = sqlx::query_scalar("SELECT metadata FROM download_job WHERE id=?")
        .bind(&receipt.job_id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(
        metadata["acquisition"],
        serde_json::json!(f.snapshot.binding())
    );
}

#[tokio::test]
async fn source_change_after_service_checks_is_rejected_inside_both_final_writes() {
    let f = Fixture::new().await;
    f.gate.disable_once.store(true, Ordering::SeqCst);
    let before = f.events().await;
    assert_eq!(
        f.preview
            .preview(&f.request(true, true), &snapshot_fixture::ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "acquisition_catalog_mismatch"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0));
    assert_eq!(f.events().await, before);
    sqlx::query("UPDATE model_source SET enabled=1 WHERE id=?")
        .bind(&f.snapshot.source_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    f.gate.disable_once.store(true, Ordering::SeqCst);
    let before = f.events().await;
    let execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let idempotency = DownloadActivationIdempotency {
        key: "remote-race".into(),
        body_hash: "c".repeat(64),
    };
    assert_eq!(
        f.activation
            .activate(&execute, &snapshot_fixture::ctx(), &idempotency)
            .await
            .unwrap_err()
            .details
            .reason,
        "acquisition_catalog_mismatch"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 1));
    assert_eq!(f.events().await, before);
    let receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency WHERE operation='P051_post__downloads'",
    )
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(receipts, 0);
}

#[tokio::test]
async fn qualification_loss_at_commit_rolls_back_planning_and_job_reservation_receipt_outbox() {
    let mut f = Fixture::new().await;
    let expiring = Arc::new(larm_db::download_plans::SqlDownloadPlans {
        pool: f.db.pool.clone(),
        qualification: Some(Arc::new(ExpiringQualification)),
    });
    f.preview.plans = expiring.clone();
    let before = f.events().await;
    assert_eq!(
        f.preview
            .preview(&f.request(true, true), &snapshot_fixture::ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "fixture_qualification_expired"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 0));
    assert_eq!(f.events().await, before);
    f.preview.plans = f.plans.clone();
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    f.activation.activation = expiring;
    let execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let idempotency = DownloadActivationIdempotency {
        key: "qualification-lost".into(),
        body_hash: "c".repeat(64),
    };
    let before = f.events().await;
    assert_eq!(
        f.activation
            .activate(&execute, &snapshot_fixture::ctx(), &idempotency)
            .await
            .unwrap_err()
            .details
            .reason,
        "fixture_qualification_expired"
    );
    assert_eq!(f.counts().await, (0, 0, 0, 0, 1));
    assert_eq!(f.events().await, before);
    let state: String = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE id=? AND kind='download.plan'",
    )
    .bind(&execute.plan_id)
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(state, "READY");
    let receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency WHERE operation='P051_post__downloads'",
    )
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(receipts, 0);
}

#[tokio::test]
async fn late_completion_outbox_fault_reverts_bundle_identity_lifecycle_and_local_facts() {
    let f = Fixture::new().await;
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    let execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let receipt = f
        .activation
        .activate(
            &execute,
            &snapshot_fixture::ctx(),
            &DownloadActivationIdempotency {
                key: "late-physical-failure".into(),
                body_hash: "d".repeat(64),
            },
        )
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_completion_outbox BEFORE INSERT ON event_outbox WHEN EXISTS(SELECT 1 FROM event WHERE id=NEW.event_record_id AND event_type='download.state_changed' AND json_extract(payload,'$.action')='completed') BEGIN SELECT RAISE(ABORT,'CODE_PASS completion outbox fault'); END")
        .execute(&f.db.pool).await.unwrap();
    let closed = Arc::new(AtomicUsize::new(0));
    let result = f.consumer(closed.clone()).run_once().await.unwrap();
    assert!(matches!(
        result,
        larm_download::consumer::DownloadConsumerOutcome::Failed { .. }
    ));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    let (state,bundle_hash,actual_size,bundles,locations,provenance):(String,Option<String>,Option<i64>,i64,i64,i64)=sqlx::query_as("SELECT v.lifecycle,v.bundle_hash,a.actual_size,(SELECT count(*) FROM model_bundle),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM artifact_provenance) FROM model_variant v JOIN model_artifact a ON a.variant_id=v.id WHERE v.id=?")
        .bind(&f.snapshot.variant_id).fetch_one(&f.db.pool).await.unwrap();
    assert_eq!(
        (
            state.as_str(),
            bundle_hash,
            actual_size,
            bundles,
            locations,
            provenance
        ),
        ("DOWNLOADING", None, None, 0, 0, 0)
    );
    let job_state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
        .bind(&receipt.job_id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(job_state, "FAILED");
    let completed_events:i64=sqlx::query_scalar("SELECT count(*) FROM event WHERE json_extract(payload,'$.to') IN ('DOWNLOADED','VERIFIED')").fetch_one(&f.db.pool).await.unwrap();
    assert_eq!(completed_events, 0);
    let journal: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=?")
        .bind(&execute.plan_id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(journal, "FAILED");
    assert_eq!(f.counts().await, (0, 0, 1, 0, 1));
    let locks: i64 = sqlx::query_scalar("SELECT count(*) FROM resource_lock")
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(locks, 0);
}

#[tokio::test]
async fn lost_resource_fence_stops_before_opening_the_source() {
    let f = Fixture::new().await;
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    let execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    f.activation
        .activate(
            &execute,
            &snapshot_fixture::ctx(),
            &DownloadActivationIdempotency {
                key: "lost-fence".into(),
                body_hash: "d".repeat(64),
            },
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM resource_lock WHERE owner_id=?")
        .bind(&execute.plan_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    let closed = Arc::new(AtomicUsize::new(0));
    let outcome = f.consumer(closed.clone()).run_once().await.unwrap();
    assert!(
        matches!(outcome,larm_download::consumer::DownloadConsumerOutcome::Failed {reason,..} if reason=="download_resource_fence_lost")
    );
    assert_eq!(closed.load(Ordering::SeqCst), 0);
    assert_eq!(f.counts().await, (0, 0, 1, 0, 1));
    assert!(!f._directory.path().join("blobs").exists());
}

#[tokio::test]
async fn first_remote_transfer_without_declared_digest_builds_a_bundle_after_local_hashing() {
    use larm_domain::ports::VariantRepository;
    use larm_download::consumer::DownloadConsumerOutcome;
    use sha2::Digest;
    let f = Fixture::new_with_optional(true).await;
    let planned = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    let execute = ExecutePlan {
        plan_id: planned.plan_id.unwrap(),
        plan_hash: planned.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let receipt = f
        .activation
        .activate(
            &execute,
            &snapshot_fixture::ctx(),
            &DownloadActivationIdempotency {
                key: "first-remote-bytes".into(),
                body_hash: "c".repeat(64),
            },
        )
        .await
        .unwrap();
    let closed = Arc::new(AtomicUsize::new(0));
    let consumer = f.consumer(closed.clone());
    let outcome = consumer.run_once().await.unwrap();
    if !matches!(outcome, DownloadConsumerOutcome::Completed { .. }) {
        let error: Option<String> = sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
        eprintln!("CODE_PASS first acquisition error: {error:?}");
    }
    assert_eq!(
        outcome,
        DownloadConsumerOutcome::Completed {
            job_id: receipt.job_id.clone()
        }
    );
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    let (path, digest, trust): (String,String,String) = sqlx::query_as("SELECT l.relative_path,l.sha256,p.trust FROM artifact_location l JOIN artifact_provenance p ON p.artifact_id=l.artifact_id WHERE l.artifact_id=?")
        .bind(&f.snapshot.artifacts[0].artifact_id).fetch_one(&f.db.pool).await.unwrap();
    assert_eq!(
        std::fs::read(f._directory.path().join(&path)).unwrap(),
        b"data"
    );
    assert_eq!(digest, hex::encode(sha2::Sha256::digest(b"data")));
    assert_eq!(trust, "DECLARED");
    let variants = larm_db::variants::SqlVariants {
        pool: f.db.pool.clone(),
    };
    let bundle = variants
        .bundle_of(&f.snapshot.variant_id)
        .await
        .unwrap()
        .expect("first acquisition must reconcile a physical bundle after hashing actual bytes");
    assert_eq!(bundle.state, larm_domain::model::BundleState::Verified);
    let variant = variants
        .get_variant(&f.snapshot.variant_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(variant.lifecycle, "VERIFIED");
    assert_ne!(variant.availability, "AVAILABLE");

    let declared = variants.artifacts_of(&variant.id).await.unwrap();
    assert_eq!(declared[0].sha256, None);
    assert_eq!(declared[0].expected_size, None);
    assert_eq!(declared[0].actual_size.unwrap().get(), 4);
    let physical = variants.bundle_artifacts(&bundle).await.unwrap();
    assert_eq!(physical[0].sha256.as_deref(), Some(digest.as_str()));
    assert_eq!(physical[0].expected_size.unwrap().get(), 4);
    let snapshots = snapshot_fixture::repo(&f.db.pool);
    assert_eq!(
        larm_domain::ports::AcquisitionManifestCatalog::bound_current(
            &snapshots,
            &variant.id,
            &f.snapshot.binding(),
            &snapshot_fixture::ctx()
        )
        .await
        .unwrap(),
        f.snapshot
    );
    let next = f
        .preview
        .preview(&f.request(true, true), &snapshot_fixture::ctx())
        .await
        .unwrap();
    assert_eq!(next.acquisition, Some(f.snapshot.binding()));
    assert_eq!(next.artifact_estimates[0].sha256, None);
    assert_eq!(
        next.artifact_estimates[0].size_estimate.confidence,
        larm_domain::model::ArtifactSizeConfidence::Unknown
    );
    assert_eq!(f.counts().await, (1, 1, 1, 0, 2));
    let locks: i64 = sqlx::query_scalar("SELECT count(*) FROM resource_lock")
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(locks, 0);
    let stages:Vec<String> = sqlx::query_scalar("SELECT json_extract(payload,'$.to') FROM event WHERE resource_type='model_variant' AND resource_id=? AND json_extract(payload,'$.action')='download_lifecycle' ORDER BY event_id")
        .bind(&variant.id).fetch_all(&f.db.pool).await.unwrap();
    assert_eq!(
        stages,
        [
            "SELECTED",
            "DOWNLOAD_PLANNED",
            "DOWNLOADING",
            "DOWNLOADED",
            "VERIFIED"
        ]
    );

    // P042 must use frozen local descriptors, even offline, and independently
    // read bytes again. The mutable location digest cannot redefine expectations.
    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&f.snapshot.source_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    let jobs = Arc::new(larm_db::model_verification::SqlModelVerificationJobs {
        pool: f.db.pool.clone(),
    });
    let worker = larm_models::verification::ModelVerificationWorker::new(
        larm_models::verification::VerificationPorts {
            jobs: jobs.clone(),
            variants: Arc::new(variants.clone()),
            artifact_locations: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: f.db.pool.clone(),
            }),
            storage_locations: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: f.db.pool.clone(),
            }),
            prober: Arc::new(Space),
        },
    );
    use larm_domain::model_verification::ModelVerificationSubmitter;
    for (bytes, expected) in [
        (b"data", "VERIFIED"),
        (b"evil", "CORRUPT"),
        (b"data", "VERIFIED"),
    ] {
        let blob = f._directory.path().join(&path);
        // Simulate damage only to our own temporary CAS blob, whose read-only
        // flag deliberately protects it during normal operation.
        let mut permissions = std::fs::metadata(&blob).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&blob, permissions).unwrap();
        std::fs::write(&blob, bytes).unwrap();
        let mut permissions = std::fs::metadata(&blob).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&blob, permissions).unwrap();
        sqlx::query("UPDATE artifact_location SET sha256=? WHERE artifact_id=?")
            .bind(hex::encode(sha2::Sha256::digest(bytes)))
            .bind(&declared[0].id)
            .execute(&f.db.pool)
            .await
            .unwrap();
        jobs.enqueue_verification(&variant.id, &snapshot_fixture::ctx())
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap());
        let current = variants.get_variant(&variant.id).await.unwrap().unwrap();
        assert_eq!(current.lifecycle, expected);
        assert_ne!(current.availability, "AVAILABLE");
        assert_eq!(
            variants
                .bundle_of(&variant.id)
                .await
                .unwrap()
                .unwrap()
                .state
                .as_str(),
            expected
        );
    }
    let saved: String = sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=?")
        .bind(&execute.plan_id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operation_journal SET payload=json_set(payload,'$.record.request.hard_budget_bytes','999') WHERE id=?")
        .bind(&execute.plan_id).execute(&f.db.pool).await.unwrap();
    let current_bundle = variants.bundle_of(&variant.id).await.unwrap().unwrap();
    assert_eq!(
        variants
            .bundle_artifacts(&current_bundle)
            .await
            .unwrap_err()
            .details
            .reason,
        "download_plan_seal_mismatch"
    );
    sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
        .bind(saved)
        .bind(&execute.plan_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(
        variants.bundle_artifacts(&current_bundle).await.unwrap()[0].sha256,
        Some(digest)
    );
}
