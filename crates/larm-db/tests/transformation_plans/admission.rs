//! Transactional repository tests with explicit observation doubles. No native
//! worker, physical-file verification or REAL_PASS is claimed by this fixture.
use super::*;
use larm_domain::{
    bridge::{timestamp, JobPhase},
    estimate::{MarginPolicy, VolumeEstimate},
    ports::VolumeDemand,
    transformations::{
        ExecuteTransformationPlan, TransformationActivationIdempotency, TransformationAdmission,
        TransformationInputGuard, TransformationInputIdentity, TransformationInputLocation,
        TransformationSpaceObserver, TransformationSpaceSnapshot,
    },
};

const VOLUME: &str = "fixture-volume";
const REQUIRED: u64 = (1 << 30) + 16;

#[path = "dispatch.rs"]
mod dispatch;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "restart.rs"]
mod restart;

struct Input {
    identity: TransformationInputIdentity,
    artifacts: Vec<InputArtifact>,
    locations: Vec<TransformationInputLocation>,
    calls: AtomicUsize,
    reject_at: AtomicUsize,
}
#[async_trait::async_trait]
impl TransformationInputGuard for Input {
    fn identity(&self) -> &TransformationInputIdentity {
        &self.identity
    }
    fn artifacts(&self) -> &[InputArtifact] {
        &self.artifacts
    }
    fn locations(&self) -> &[TransformationInputLocation] {
        &self.locations
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call >= self.reject_at.load(Ordering::SeqCst) {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "Fixture changed",
                "input_fixture_changed",
            ));
        }
        Ok(())
    }
}

struct Space {
    free: u64,
    age_ms: i64,
    policy: MarginPolicy,
    minimum: u64,
    calls: AtomicUsize,
}
impl Default for Space {
    fn default() -> Self {
        Self {
            free: REQUIRED * 2 + 11,
            age_ms: 0,
            policy: MarginPolicy::default(),
            minimum: 11,
            calls: AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl TransformationSpaceObserver for Space {
    async fn observe(&self) -> Result<TransformationSpaceSnapshot, LarmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(TransformationSpaceSnapshot {
            observed_at: chrono::Utc::now() - chrono::Duration::milliseconds(self.age_ms),
            demands: vec![VolumeDemand {
                volume_key: VOLUME.into(),
                required_bytes: REQUIRED,
                os_free: self.free,
                policy_minimum: self.minimum,
            }],
            margin_policy: self.policy,
            minimum_free_bytes: 0,
        })
    }
}

struct Admission {
    f: Fixture,
    input: Input,
    space: Space,
    plan: SealedTransformationPlan,
    req: ExecuteTransformationPlan,
    key: TransformationActivationIdempotency,
}
impl Admission {
    async fn new() -> Self {
        let f = Fixture::new().await;
        let revision = id();
        let bundle = id();
        let source_root = id();
        let at = timestamp(chrono::Utc::now());
        let mut tx = f.db.pool.begin().await.unwrap();
        // An unrelated source fixture establishes the catalogue FK chain. Its
        // installation is never presented as transformation qualification.
        for sql in [
            "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Fixture','[]','[]')",
            "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-install','t','t',1,'{}','fixture.source','1.0.0','source','fixture','fixture','UNTRUSTED','{}','INSTALLED')",
            "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter','t','t',1,'{}','source-install','LOCAL','[]')",
            "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source','t','t',1,'{}','Fixture','source-adapter','file:///fixture',0,1,'NONE','{}')",
            "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','cm','source','fixture','model')",
        ] { sqlx::query(sql).execute(&mut *tx).await.unwrap(); }
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}','distribution','r1',?,'CURRENT')")
            .bind(&revision).bind(&at).bind(&at).bind("c".repeat(64)).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available,bundle_hash) VALUES(?,?,?,1,'{}',?,'VERIFIED','VERIFIED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0,?)")
            .bind(&f.request.input_variant_id).bind(&at).bind(&at).bind(&revision).bind("c".repeat(64)).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
            .bind(&bundle).bind(&at).bind(&at).bind(&f.request.input_variant_id).bind(json!([f.input[0].artifact_id]).to_string()).bind("c".repeat(64)).execute(&mut *tx).await.unwrap();
        for (root, path, minimum) in [
            (&source_root, "C:/fixture/input", 7_i64),
            (&f.request.storage_location_id, "C:/fixture/output", 11_i64),
        ] {
            sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,?,?,1,'{}','Fixture',?,?,'NTFS','LOCAL',9999999999,9999999999,?,0,1,'ONLINE',?,'SSD')")
                .bind(root).bind(&at).bind(&at).bind(path).bind(path).bind(minimum).bind(VOLUME).execute(&mut *tx).await.unwrap();
        }
        let location = TransformationInputLocation {
            artifact_id: f.input[0].artifact_id.clone(),
            artifact_location_id: id(),
            storage_location_id: source_root,
            canonical_storage_path: "C:/fixture/input".into(),
            relative_path: "blobs/sha256/ff/input".into(),
            volume_key: VOLUME.into(),
        };
        // Observation double only. Production resolves immutable catalogue /
        // acquisition descriptors and hashes pinned bytes before admission.
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,actual_size,sha256,required) VALUES(?,?,?,1,'{}',?,'input.bin',NULL,4,NULL,1)")
            .bind(&location.artifact_id).bind(&at).bind(&at).bind(&f.request.input_variant_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership,verified_at) VALUES(?,?,?,1,'{}',?,?,?,4,?,'ONLINE','LARM_MANAGED',?)")
            .bind(&location.artifact_location_id).bind(&at).bind(&at).bind(&location.artifact_id).bind(&location.storage_location_id).bind(&location.relative_path).bind(&f.input[0].sha256).bind(&at).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let input = Input {
            identity: TransformationInputIdentity {
                variant_id: f.request.input_variant_id.clone(),
                revision_id: revision,
                variant_bundle_hash: "c".repeat(64),
                bundle_id: bundle,
                bundle_row_version: 1,
                bundle_manifest_hash: "c".repeat(64),
            },
            artifacts: f.input.clone(),
            locations: vec![location],
            calls: AtomicUsize::new(0),
            reject_at: AtomicUsize::new(usize::MAX),
        };
        let mut draft = f.draft();
        draft.storage_estimate.per_volume = vec![VolumeEstimate {
            volume_key: VOLUME.into(),
            peak_extra_bytes: DecimalU64::new(16).unwrap(),
            margin_bytes: DecimalU64::new(1 << 30).unwrap(),
            other_reservations_bytes: DecimalU64::new(0).unwrap(),
            usable_free_bytes: DecimalU64::new(REQUIRED * 2).unwrap(),
            eligible: true,
            reason: "fixture only".into(),
        }];
        let plan = seal_bound_plan(
            id(),
            f.request.clone(),
            f.input.clone(),
            draft,
            f.binding.clone(),
        )
        .unwrap();
        f.repo.store_plan(&plan, &owner()).await.unwrap();
        let req = ExecuteTransformationPlan {
            plan_id: plan.response.plan_id.clone(),
            plan_hash: plan.response.plan_hash.clone(),
            approval_id: None,
        };
        let key = TransformationActivationIdempotency {
            key: "initial-confirmation".into(),
            body_hash: hash(&req).unwrap(),
        };
        Self {
            f,
            input,
            space: Space::default(),
            plan,
            req,
            key,
        }
    }
    async fn activate(&self) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
        self.f
            .repo
            .activate_plan(
                &self.req,
                &owner(),
                &self.key,
                TransformationAdmission {
                    input: &self.input,
                    space: &self.space,
                },
            )
            .await
    }
    async fn effects(&self) -> (i64, i64, i64, i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM operation_journal WHERE kind='transformation.job'),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock),(SELECT count(*) FROM idempotency),(SELECT count(*) FROM event WHERE resource_type='transformation_job')")
            .fetch_one(&self.f.db.pool).await.unwrap()
    }
    async fn assert_unconsumed(&self) {
        assert_eq!(self.effects().await, (0, 0, 0, 0, 0, 0));
        let (phase, activation): (String, bool) = sqlx::query_as("SELECT phase,json_type(payload,'$.activation') IS NOT NULL FROM operation_journal WHERE id=?")
            .bind(&self.req.plan_id).fetch_one(&self.f.db.pool).await.unwrap();
        assert_eq!((phase.as_str(), activation), ("PLANNED", false));
    }
}

#[tokio::test]
async fn initial_confirmation_commits_one_queued_job_and_deduplicates_shared_volume() {
    let a = Admission::new().await;
    let receipt = a.activate().await.unwrap();
    assert_eq!(receipt.state, JobPhase::Queued);
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    let (state, attempt, output, input, adapter): (String,i64,Option<String>,String,String) = sqlx::query_as("SELECT state,attempt,output_variant_id,input_variant_id,adapter_id FROM transformation_job WHERE id=?")
        .bind(&receipt.job_id).fetch_one(&a.f.db.pool).await.unwrap();
    assert_eq!((state.as_str(), attempt, output), ("QUEUED", 0, None));
    assert_eq!(input, a.f.request.input_variant_id);
    assert_eq!(adapter, a.f.binding.adapter_installation_id);
    let reserve: i64 =
        sqlx::query_scalar("SELECT remaining_bytes FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&a.f.db.pool)
            .await
            .unwrap();
    assert_eq!(reserve, REQUIRED as i64);
    let locks: Vec<(String, String)> =
        sqlx::query_as("SELECT resource_id,mode FROM resource_lock ORDER BY resource_id")
            .fetch_all(&a.f.db.pool)
            .await
            .unwrap();
    assert_eq!(locks.iter().filter(|(_, m)| m == "EXCLUSIVE").count(), 3);
    assert!(locks.contains(&(format!("volume:{VOLUME}"), "SHARED".into())));
    let (phase, job): (String,String)=sqlx::query_as("SELECT phase,json_extract(payload,'$.activation.job_id') FROM operation_journal WHERE id=?").bind(&a.req.plan_id).fetch_one(&a.f.db.pool).await.unwrap();
    assert_eq!(
        (phase.as_str(), job.as_str()),
        ("QUEUED", receipt.job_id.as_str())
    );
    assert_eq!(a.input.calls.load(Ordering::SeqCst), 2);
    assert_eq!(a.space.calls.load(Ordering::SeqCst), 1);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn replay_uses_receipt_without_qualifying_measuring_or_accepting_a_changed_request() {
    let mut a = Admission::new().await;
    let receipt = a.activate().await.unwrap();
    let acquisitions = a.f.gate.acquisitions.load(Ordering::SeqCst);
    a.input.reject_at.store(0, Ordering::SeqCst);
    a.f.gate.reject.store(true, Ordering::SeqCst);
    assert_eq!(a.activate().await.unwrap(), receipt);
    assert_eq!(a.f.gate.acquisitions.load(Ordering::SeqCst), acquisitions);
    assert_eq!(a.space.calls.load(Ordering::SeqCst), 1);
    // The raw HTTP body digest cannot stand in for canonical request equality.
    a.req.approval_id = Some(id());
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    a.req.approval_id = None;
    a.key.body_hash = "0".repeat(64);
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn failure_after_writes_rolls_back_the_entire_confirmation() {
    for failure in [
        "disk",
        "stale",
        "future",
        "policy",
        "margin",
        "last-input",
        "outbox",
    ] {
        let mut a = Admission::new().await;
        match failure {
            "disk" => a.space.free = REQUIRED + 10,
            "stale" => a.space.age_ms = 5001,
            "future" => a.space.age_ms = -10000,
            "policy" => a.space.minimum = 0,
            "margin" => a.space.policy.minimum_margin_bytes = 1,
            "last-input" => a.input.reject_at.store(2, Ordering::SeqCst),
            "outbox" => {
                sqlx::query("CREATE TRIGGER deny_transform_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture outbox unavailable'); END").execute(&a.f.db.pool).await.unwrap();
            }
            _ => unreachable!(),
        }
        assert!(a.activate().await.is_err(), "{failure}");
        a.assert_unconsumed().await;
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn catalogue_drift_does_not_queue_or_reserve() {
    for sql in [
        "UPDATE model_bundle SET row_version=row_version+1",
        "UPDATE model_bundle SET required_artifact_ids='[]',optional_artifact_ids='[]'",
        "UPDATE model_variant SET lifecycle='CORRUPT'",
        "UPDATE model_variant SET availability='OFFLINE'",
        "UPDATE artifact_location SET sha256=NULL",
        "UPDATE artifact_location SET file_size=5",
        "UPDATE artifact_location SET state='COPYING'",
        "UPDATE artifact_location SET relative_path='another.bin'",
        "UPDATE storage_location SET canonical_path='C:/changed' WHERE reserved_bytes=7",
        "UPDATE storage_location SET state='READ_ONLY' WHERE reserved_bytes=11",
        "UPDATE storage_location SET volume_key='unknown:fixture' WHERE reserved_bytes=11",
    ] {
        let a = Admission::new().await;
        sqlx::query(sql).execute(&a.f.db.pool).await.unwrap();
        assert!(a.activate().await.is_err(), "{sql}");
        a.assert_unconsumed().await;
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn shared_artifact_requires_its_explicit_reference_at_confirmation() {
    let a = Admission::new().await;
    let origin = id();
    sqlx::query("INSERT INTO model_variant SELECT update_available,status,?,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits FROM model_variant WHERE id=?")
        .bind(&origin).bind(&a.f.request.input_variant_id).execute(&a.f.db.pool).await.unwrap();
    sqlx::query("UPDATE model_artifact SET variant_id=? WHERE id=?")
        .bind(origin)
        .bind(&a.input.artifacts[0].artifact_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    a.assert_unconsumed().await;
    sqlx::query("INSERT INTO artifact_reference(artifact_id,variant_id) VALUES(?,?)")
        .bind(&a.input.artifacts[0].artifact_id)
        .bind(&a.f.request.input_variant_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(a.activate().await.unwrap().state, JobPhase::Queued);
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn concurrent_confirmation_replays_one_job_but_a_second_key_cannot_reuse_the_plan() {
    let a = Admission::new().await;
    let (one, two) = tokio::join!(a.activate(), a.activate());
    assert_eq!(one.unwrap(), two.unwrap());
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    let mut key = a.key.clone();
    key.key = "another-confirmation".into();
    assert_eq!(
        a.f.repo
            .activate_plan(
                &a.req,
                &owner(),
                &key,
                TransformationAdmission {
                    input: &a.input,
                    space: &a.space
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn existing_reader_blocks_exclusive_transformation_without_losing_its_lock() {
    let a = Admission::new().await;
    let resource = format!("artifact:{}", a.input.artifacts[0].artifact_id);
    sqlx::query("INSERT INTO resource_lock(resource_id,owner_id,generation,mode) VALUES(?,'existing-reader',3,'SHARED')").bind(&resource).execute(&a.f.db.pool).await.unwrap();
    assert_eq!(a.activate().await.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(a.effects().await, (0, 0, 0, 1, 0, 0));
    let lock: (String, i64, String) =
        sqlx::query_as("SELECT owner_id,generation,mode FROM resource_lock")
            .fetch_one(&a.f.db.pool)
            .await
            .unwrap();
    assert_eq!(lock, ("existing-reader".into(), 3, "SHARED".into()));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn approval_is_bound_consumed_once_and_restored_when_reserving_fails() {
    let mut a = Admission::new().await;
    let mut draft = a.f.draft();
    draft.storage_estimate = a.plan.response.storage_estimate.clone();
    draft.approval_required = true;
    a.plan = seal_bound_plan(
        id(),
        a.f.request.clone(),
        a.f.input.clone(),
        draft,
        a.f.binding.clone(),
    )
    .unwrap();
    a.f.repo.store_plan(&a.plan, &owner()).await.unwrap();
    a.req.plan_id = a.plan.response.plan_id.clone();
    a.req.plan_hash = a.plan.response.plan_hash.clone();
    a.key.body_hash = hash(&a.req).unwrap();
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::ApprovalRequired
    );
    a.assert_unconsumed().await;
    let approval = ApprovalRequest {
        operation_digest: a.req.plan_hash.clone(),
        resource_ids: a.plan.response.resource_ids.clone(),
        code_hash: Some("a".repeat(64)),
        permissions: Default::default(),
        approved: true,
    }
    .grant(
        id(),
        owner().actor_id,
        chrono::Utc::now(),
        chrono::Duration::minutes(15),
    )
    .unwrap();
    larm_db::security::SqlApprovals {
        pool: a.f.db.pool.clone(),
    }
    .insert(&approval, &owner())
    .await
    .unwrap();
    a.req.approval_id = Some(approval.id.clone());
    a.key.body_hash = hash(&a.req).unwrap();
    a.space.free = REQUIRED;
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::StorageInsufficient
    );
    a.assert_unconsumed().await;
    let (phase, consumed): (String, bool) = sqlx::query_as("SELECT phase,json_extract(payload,'$.consumed_at') IS NOT NULL FROM operation_journal WHERE id=?")
        .bind(&approval.id).fetch_one(&a.f.db.pool).await.unwrap();
    assert_eq!((phase.as_str(), consumed), ("APPROVED", false));
    a.space.free = REQUIRED + 11;
    let receipt = a.activate().await.unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=?")
        .bind(&approval.id)
        .fetch_one(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(phase, "CONSUMED");
    assert_eq!(a.activate().await.unwrap(), receipt);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn wrong_owner_changed_seal_and_revoked_qualification_cannot_activate() {
    let mut a = Admission::new().await;
    let mut stranger = owner();
    stranger.actor_id = "another-owner".into();
    assert_eq!(
        a.f.repo
            .activate_plan(
                &a.req,
                &stranger,
                &a.key,
                TransformationAdmission {
                    input: &a.input,
                    space: &a.space
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    a.req.plan_hash = "0".repeat(64);
    a.key.body_hash = hash(&a.req).unwrap();
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    a.req.plan_hash = a.plan.response.plan_hash.clone();
    a.key.body_hash = hash(&a.req).unwrap();
    a.f.gate.reject.store(true, Ordering::SeqCst);
    assert_eq!(
        a.activate().await.unwrap_err().code,
        ErrorCode::IntegrityFailed
    );
    a.assert_unconsumed().await;
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn concurrent_independent_inputs_cannot_overbook_one_physical_volume() {
    let mut a = Admission::new().await;
    a.space.free = REQUIRED + 11;
    let mut input = Input {
        identity: a.input.identity.clone(),
        artifacts: a.input.artifacts.clone(),
        locations: a.input.locations.clone(),
        calls: AtomicUsize::new(0),
        reject_at: AtomicUsize::new(usize::MAX),
    };
    input.identity.variant_id = id();
    input.identity.bundle_id = id();
    input.artifacts[0].artifact_id = id();
    input.locations[0].artifact_id = input.artifacts[0].artifact_id.clone();
    input.locations[0].artifact_location_id = id();
    let mut tx = a.f.db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO model_variant SELECT update_available,status,?,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits FROM model_variant WHERE id=?")
        .bind(&input.identity.variant_id).bind(&a.input.identity.variant_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_bundle SELECT ?,created_at,updated_at,row_version,metadata,?,?,optional_artifact_ids,manifest_hash,state FROM model_bundle WHERE id=?")
        .bind(&input.identity.bundle_id).bind(&input.identity.variant_id).bind(json!([input.artifacts[0].artifact_id]).to_string()).bind(&a.input.identity.bundle_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,expected_size,actual_size,sha256,source_url,required,size_estimate) SELECT ?,created_at,updated_at,row_version,metadata,?,filename,format,quantization,expected_size,actual_size,sha256,source_url,required,size_estimate FROM model_artifact WHERE id=?")
        .bind(&input.artifacts[0].artifact_id).bind(&input.identity.variant_id).bind(&a.input.artifacts[0].artifact_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO artifact_location SELECT ownership,?,created_at,updated_at,row_version,metadata,?,storage_location_id,relative_path,file_size,sha256,state,verified_at FROM artifact_location WHERE id=?")
        .bind(&input.locations[0].artifact_location_id).bind(&input.artifacts[0].artifact_id).bind(&a.input.locations[0].artifact_location_id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let mut request = a.f.request.clone();
    request.input_variant_id = input.identity.variant_id.clone();
    let mut draft = a.f.draft();
    draft.resource_ids[0] = request.input_variant_id.clone();
    draft.storage_estimate = a.plan.response.storage_estimate.clone();
    let plan = seal_bound_plan(
        id(),
        request,
        input.artifacts.clone(),
        draft,
        a.f.binding.clone(),
    )
    .unwrap();
    a.f.repo.store_plan(&plan, &owner()).await.unwrap();
    let req = ExecuteTransformationPlan {
        plan_id: plan.response.plan_id.clone(),
        plan_hash: plan.response.plan_hash.clone(),
        approval_id: None,
    };
    let key = TransformationActivationIdempotency {
        key: "other-input".into(),
        body_hash: hash(&req).unwrap(),
    };
    let second_owner = owner();
    let (one, two) = tokio::join!(
        a.activate(),
        a.f.repo.activate_plan(
            &req,
            &second_owner,
            &key,
            TransformationAdmission {
                input: &input,
                space: &a.space
            }
        )
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert_eq!(
        one.err().or_else(|| two.err()).unwrap().code,
        ErrorCode::StorageInsufficient
    );
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    let phases: Vec<String> = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE kind='transformation.plan' ORDER BY phase",
    )
    .fetch_all(&a.f.db.pool)
    .await
    .unwrap();
    assert_eq!(phases, vec!["PLANNED", "QUEUED"]);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn suppressed_mandatory_writes_abort_admission_instead_of_acknowledging_a_partial_job() {
    for trigger in [
        "CREATE TRIGGER suppress_job BEFORE INSERT ON transformation_job BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER suppress_journal BEFORE INSERT ON operation_journal WHEN NEW.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER suppress_key BEFORE INSERT ON idempotency BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER suppress_locks BEFORE INSERT ON resource_lock BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER suppress_reserve BEFORE INSERT ON volume_reservation BEGIN SELECT RAISE(IGNORE); END",
    ] {
        let a=Admission::new().await;
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();
        assert!(a.activate().await.is_err(),"{trigger}");
        a.assert_unconsumed().await;
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn unknown_size_requires_a_budget_and_the_budget_must_cover_the_peak() {
    for (budget, code) in [
        (None, Some(ErrorCode::UnknownSize)),
        (Some(15), Some(ErrorCode::StorageInsufficient)),
        (Some(16), None),
    ] {
        let mut a = Admission::new().await;
        let mut request = a.f.request.clone();
        request.hard_budget_bytes = budget.and_then(DecimalU64::new);
        let mut draft = a.f.draft();
        draft.storage_estimate = a.plan.response.storage_estimate.clone();
        draft.storage_estimate.confidence = Confidence::Unknown;
        // With an explicit budget the preflight materializes its bound; without
        // one this scenario stays unbounded. Bounded UNKNOWN with no budget is
        // covered separately by dispatch.
        draft.storage_estimate.upper_bound_bytes = if budget.is_some() {
            DecimalU64::new(16)
        } else {
            None
        };
        a.plan =
            seal_bound_plan(id(), request, a.f.input.clone(), draft, a.f.binding.clone()).unwrap();
        a.f.repo.store_plan(&a.plan, &owner()).await.unwrap();
        a.req.plan_id = a.plan.response.plan_id.clone();
        a.req.plan_hash = a.plan.response.plan_hash.clone();
        a.key.body_hash = hash(&a.req).unwrap();
        let result = a.activate().await;
        if let Some(code) = code {
            assert_eq!(result.unwrap_err().code, code);
            a.assert_unconsumed().await;
        } else {
            assert!(result.is_ok());
            assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
        }
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn accepted_receipt_survives_reopening_without_a_live_worker_or_current_input() {
    let a = Admission::new().await;
    let receipt = a.activate().await.unwrap();
    let path: String =
        sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
            .fetch_one(&a.f.db.pool)
            .await
            .unwrap();
    a.f.db.pool.close().await;
    let reopened = Db::open(std::path::Path::new(&path)).await.unwrap();
    let repo = SqlTransformationPlans {
        pool: reopened.pool.clone(),
        qualification: None,
    };
    a.input.reject_at.store(0, Ordering::SeqCst);
    assert_eq!(
        repo.activate_plan(
            &a.req,
            &owner(),
            &a.key,
            TransformationAdmission {
                input: &a.input,
                space: &a.space
            }
        )
        .await
        .unwrap(),
        receipt
    );
    assert_eq!(a.space.calls.load(Ordering::SeqCst), 1);
    reopened.pool.close().await;
}
