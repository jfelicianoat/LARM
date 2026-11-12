//! Real SQL admission/claims/closure. Guards below are explicit observation
//! doubles, not authority from a native process or a daemon-restart acceptance.
use super::*;
use larm_db::transformations::SqlTransformationJobs;
use larm_domain::{
    transformation_execution::{
        TransformationAttemptFence, TransformationAttemptGuard, TransformationClaim,
    },
    transformations::{TransformationJob, TransformationRestart, TransformationState},
};
use serde_json::Value;
#[path = "cancel.rs"]
mod cancel;
#[path = "native_owner.rs"]
mod native_owner;
#[path = "progress.rs"]
mod progress;
#[path = "witness_lost.rs"]
mod witness_lost;

struct Guard {
    fence: TransformationAttemptFence,
    binding: TransformationExecutionBinding,
    closed: AtomicBool,
    rechecks: AtomicUsize,
    closes: AtomicUsize,
    fail_recheck: usize,
    fail_close: usize,
}
impl Guard {
    fn new(claim: &TransformationClaim) -> Self {
        Self {
            fence: claim.fence.clone(),
            binding: claim.plan.execution_binding.clone().unwrap(),
            closed: AtomicBool::new(false),
            rechecks: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            fail_recheck: usize::MAX,
            fail_close: usize::MAX,
        }
    }
}
#[async_trait::async_trait]
impl TransformationAttemptGuard for Guard {
    fn fence(&self) -> &TransformationAttemptFence {
        &self.fence
    }
    fn binding(&self) -> &TransformationExecutionBinding {
        &self.binding
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        if self.rechecks.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_recheck {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "fixture changed",
                "fixture_qualification_changed",
            ));
        }
        Ok(())
    }
    async fn require_closed(&self) -> Result<(), LarmError> {
        if !self.closed.load(Ordering::SeqCst)
            || self.closes.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_close
        {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "fixture IO live",
                "fixture_io_not_closed",
            ));
        }
        Ok(())
    }
}
fn jobs(a: &Admission) -> SqlTransformationJobs {
    SqlTransformationJobs {
        pool: a.f.db.pool.clone(),
    }
}
fn failure() -> LarmError {
    LarmError::new(
        ErrorCode::AdapterCrashed,
        "Worker failed: Authorization: Bearer synthetic-credential-123456",
        "fixture_worker_crashed",
    )
    .retryable(true)
}
async fn queued(a: &Admission) -> String {
    a.activate().await.unwrap().job_id
}
async fn snapshot(a: &Admission, id: &str) -> Value {
    let job: (String, i64, i64, String, Option<String>) = sqlx::query_as(
        "SELECT state,row_version,attempt,metadata,error FROM transformation_job WHERE id=?",
    )
    .bind(id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap();
    let journal: (String, String, String, String) = sqlx::query_as(
        "SELECT phase,plan_hash,resource_ids,payload FROM operation_journal WHERE id=?",
    )
    .bind(id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap();
    let locks:Vec<(String,String,i64)>=sqlx::query_as("SELECT resource_id,owner_id,generation FROM resource_lock ORDER BY resource_id,owner_id,generation").fetch_all(&a.f.db.pool).await.unwrap();
    let reserves: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id,job_id,total_bytes,remaining_bytes FROM volume_reservation ORDER BY id",
    )
    .fetch_all(&a.f.db.pool)
    .await
    .unwrap();
    let intents:Vec<(String,String,String,String)>=sqlx::query_as("SELECT id,phase,resource_ids,payload FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=? ORDER BY id")
        .bind(id).fetch_all(&a.f.db.pool).await.unwrap();
    json!({"job":job,"journal":journal,"locks":locks,"reserves":reserves,"native_intents":intents,"effects":a.effects().await})
}
async fn fail(
    a: &Admission,
    c: &TransformationClaim,
    g: &Guard,
    recoverable: bool,
) -> Result<TransformationJob, LarmError> {
    jobs(a)
        .finish_failed_attempt(c, g, &failure(), recoverable)
        .await
}

#[tokio::test]
async fn competing_claims_have_one_winner_and_initial_attempt_is_one() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let (one, two) = tokio::join!(repo.claim_next(), repo.claim_next());
    let claims = [one.unwrap(), two.unwrap()];
    assert_eq!(claims.iter().filter(|c| c.is_some()).count(), 1);
    let claim = claims.into_iter().flatten().next().unwrap();
    assert_eq!(claim.job.id, id);
    assert_eq!(claim.job.attempt, 1);
    assert_eq!(claim.fence.attempt, 1);
    assert_eq!(claim.fence.generation, 0);
    assert_eq!(claim.job.state, TransformationState::Running);
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 2));
    assert!(repo.claim_next().await.unwrap().is_none());
    assert!(repo.claim_job(&id).await.is_err());
    let g = Guard::new(&claim);
    let verified = repo.mark_verifying(&claim, &g).await.unwrap();
    assert_eq!(verified.state, TransformationState::Verifying);
    let before = snapshot(&a, &id).await;
    assert_eq!(
        repo.mark_verifying(&claim, &g).await.unwrap().row_version,
        verified.row_version
    );
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn malformed_legacy_or_changed_admission_never_partially_claims() {
    for change in [
        "UPDATE transformation_job SET metadata=json_remove(metadata,'$.input_identity')",
        "UPDATE operation_journal SET payload=json_set(payload,'$.attempt',9) WHERE kind='transformation.job'",
        "UPDATE operation_journal SET payload=json_set(payload,'$.actor_id','foreign') WHERE kind='transformation.job'",
        "UPDATE operation_journal SET resource_ids='[]' WHERE kind='transformation.job'",
        "UPDATE resource_lock SET generation=7",
        "DELETE FROM resource_lock WHERE resource_id LIKE 'artifact:%'",
        "DELETE FROM volume_reservation",
        "UPDATE volume_reservation SET remaining_bytes=0",
        "UPDATE transformation_job SET row_version=9223372036854775807",
    ] {
        let a=Admission::new().await;let id=queued(&a).await;
        sqlx::query(change).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(jobs(&a).claim_next().await.is_err(),"{change}");
        assert_eq!(snapshot(&a,&id).await,before,"{change}");a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn claim_mandatory_writes_and_outbox_rollback_together() {
    for trigger in [
        "CREATE TRIGGER fault BEFORE UPDATE ON transformation_job BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(IGNORE); END",
    ] {
        let a=Admission::new().await;let id=queued(&a).await;
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(jobs(&a).claim_next().await.is_err());assert_eq!(snapshot(&a,&id).await,before);
        sqlx::query("DROP TRIGGER fault").execute(&a.f.db.pool).await.unwrap();
        assert_eq!(jobs(&a).claim_next().await.unwrap().unwrap().job.attempt,1);
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn an_open_owner_or_a_lost_second_observation_cannot_release_resources() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let mut g = Guard::new(&c);
    let before = snapshot(&a, &id).await;
    assert!(fail(&a, &c, &g, true).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    g.closed.store(true, Ordering::SeqCst);
    g.fail_close = 2;
    assert!(fail(&a, &c, &g, true).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn verification_rechecks_qualification_inside_its_transaction() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let mut g = Guard::new(&c);
    g.fail_recheck = 2;
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).mark_verifying(&c, &g).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn an_unexpected_output_pointer_cannot_authorize_more_io_or_resource_release() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE transformation_job SET output_variant_id=input_variant_id WHERE id=?")
        .bind(&id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).check_active_claim(&c, &g).await.is_err());
    assert!(fail(&a, &c, &g, true).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn wrong_claim_or_session_binding_cannot_advance_or_close() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let before = snapshot(&a, &id).await;
    for fault in 0..4 {
        let mut g = Guard::new(&c);
        g.closed.store(true, Ordering::SeqCst);
        match fault {
            0 => g.fence.claim_id = super::id(),
            1 => g.fence.generation += 1,
            2 => g.binding.profile_sha256 = "f".repeat(64),
            _ => g.binding.worker_plan_id = super::id(),
        }
        assert_eq!(
            jobs(&a).mark_verifying(&c, &g).await.unwrap_err().code,
            ErrorCode::StaleRevision
        );
        assert_eq!(
            fail(&a, &c, &g, true).await.unwrap_err().code,
            ErrorCode::StaleRevision
        );
        assert_eq!(snapshot(&a, &id).await, before);
    }
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn closed_failure_preserves_first_redacted_cause_and_replays_without_extra_events() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    let result = fail(&a, &c, &g, false).await.unwrap();
    assert_eq!(result.state, TransformationState::Failed);
    assert_eq!(result.attempt, 1);
    assert!(result.output_variant_id.is_none());
    assert!(!result
        .error
        .as_ref()
        .unwrap()
        .to_string()
        .contains("synthetic-credential"));
    assert_eq!(result.error.as_ref().unwrap()["code"], "ADAPTER_CRASHED");
    assert_eq!(a.effects().await, (1, 1, 0, 0, 1, 3));
    let before = snapshot(&a, &id).await;
    let other = LarmError::new(ErrorCode::Timeout, "late failure", "late_failure");
    let replay = jobs(&a)
        .finish_failed_attempt(&c, &g, &other, true)
        .await
        .unwrap();
    assert_eq!(replay.error, result.error);
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn failed_closure_rolls_back_deletes_state_journal_and_outbox() {
    for trigger in [
        "CREATE TRIGGER fault BEFORE DELETE ON resource_lock BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE DELETE ON volume_reservation BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END",
    ] {
        let a=Admission::new().await;let id=queued(&a).await;let c=jobs(&a).claim_job(&id).await.unwrap();
        let g=Guard::new(&c);g.closed.store(true,Ordering::SeqCst);
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(fail(&a,&c,&g,true).await.is_err());assert_eq!(snapshot(&a,&id).await,before);
        sqlx::query("DROP TRIGGER fault").execute(&a.f.db.pool).await.unwrap();
        assert!(fail(&a,&c,&g,true).await.is_ok());a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn restart_claim_keeps_the_accepted_attempt_and_rejects_old_callbacks() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let old = jobs(&a).claim_job(&id).await.unwrap();
    let old_guard = Guard::new(&old);
    old_guard.closed.store(true, Ordering::SeqCst);
    let failed = fail(&a, &old, &old_guard, true).await.unwrap();
    let (req, key) = super::restart::confirmation(&a).await;
    a.f.repo
        .restart_plan(
            &TransformationRestart {
                job_id: id.clone(),
                if_match: failed.row_version as u64,
            },
            &req,
            &owner(),
            &key,
            TransformationAdmission {
                input: &a.input,
                space: &a.space,
            },
        )
        .await
        .unwrap();
    let next = jobs(&a).claim_job(&id).await.unwrap();
    assert_eq!(next.job.attempt, 2);
    assert_eq!(next.fence.generation, 1);
    assert_ne!(old.fence.claim_id, next.fence.claim_id);
    let history = next.job.metadata["attempt_history"].as_array().unwrap();
    assert_eq!(history[0]["attempt_execution"], json!(old.fence));
    assert!(history[0]["attempt_closed"].is_object());
    let before = snapshot(&a, &id).await;
    assert_eq!(
        fail(&a, &old, &old_guard, true).await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    assert_eq!(snapshot(&a, &id).await, before);
    assert_eq!(a.effects().await.3, 4);
    let g = Guard::new(&next);
    g.closed.store(true, Ordering::SeqCst);
    fail(&a, &next, &g, true).await.unwrap();
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn startup_reconciliation_keeps_resources_until_a_closed_owner_reconciles_them() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    assert_eq!(jobs(&a).recover_interrupted().await.unwrap(), 1);
    let g = Guard::new(&c);
    let before = snapshot(&a, &id).await;
    assert!(fail(&a, &c, &g, false).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    let original: Option<String> =
        sqlx::query_scalar("SELECT error FROM transformation_job WHERE id=?")
            .bind(&id)
            .fetch_one(&a.f.db.pool)
            .await
            .unwrap();
    g.closed.store(true, Ordering::SeqCst);
    let closed = fail(&a, &c, &g, false).await.unwrap();
    assert_eq!(closed.state, TransformationState::FailedRecoverable);
    assert_eq!(
        closed.error,
        original
            .as_deref()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
    );
    assert_eq!(a.effects().await.2, 0);
    assert_eq!(a.effects().await.3, 0);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn a_claim_and_its_nonce_survive_sqlite_reopen_without_reclaiming_or_incrementing() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    a.f.db.pool.close().await;
    let db = Db::open(&a.f._directory.path().join("larm.sqlite"))
        .await
        .unwrap();
    let repo = SqlTransformationJobs {
        pool: db.pool.clone(),
    };
    assert!(repo.claim_next().await.unwrap().is_none());
    assert!(repo.claim_job(&id).await.is_err());
    let g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    let result = repo
        .finish_failed_attempt(&c, &g, &failure(), true)
        .await
        .unwrap();
    assert_eq!(result.attempt, 1);
    assert_eq!(result.metadata["attempt_execution"], json!(c.fence));
    db.pool.close().await;
}
