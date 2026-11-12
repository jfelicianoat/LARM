//! Repository/recovery tests. Qualification, physical observations and closure
//! are explicit doubles, not proof of a native tool or daemon restart.
use super::*;
use larm_domain::{
    ports::{ResourceGuard, VolumeReservationRepository},
    transformations::TransformationRestart,
};

async fn failed_attempt(a: &Admission, release: bool) -> TransformationRestart {
    let receipt = a.activate().await.unwrap();
    // Simulate the first worker claim. Use the actual recovery repository to
    // preserve its state/error/journal/event behavior after an interruption.
    sqlx::query("UPDATE transformation_job SET state='RUNNING',attempt=1,row_version=2 WHERE id=?")
        .bind(&receipt.job_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operation_journal SET phase='RUNNING',payload=json_set(payload,'$.state','RUNNING','$.attempt',1) WHERE id=?")
        .bind(&receipt.job_id).execute(&a.f.db.pool).await.unwrap();
    let jobs = larm_db::transformations::SqlTransformationJobs {
        pool: a.f.db.pool.clone(),
    };
    assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
    if release {
        // The double models a supervisor that has already drained its IO.
        // Recovery alone is deliberately not claimed to prove worker closure.
        larm_db::guard::SqlResourceGuard {
            pool: a.f.db.pool.clone(),
        }
        .release(&receipt.job_id, 0, None)
        .await
        .unwrap();
        larm_db::reservations::SqlVolumeReservations {
            pool: a.f.db.pool.clone(),
        }
        .release(&receipt.job_id)
        .await
        .unwrap();
    }
    TransformationRestart {
        job_id: receipt.job_id,
        if_match: 3,
    }
}

pub(super) async fn confirmation(
    a: &Admission,
) -> (
    ExecuteTransformationPlan,
    TransformationActivationIdempotency,
) {
    let mut binding = a.f.binding.clone();
    binding.worker_plan_id = id();
    binding.worker_plan_hash = "9".repeat(64);
    let mut draft = a.f.draft();
    draft.storage_estimate = a.plan.response.storage_estimate.clone();
    let plan =
        seal_bound_plan(id(), a.f.request.clone(), a.f.input.clone(), draft, binding).unwrap();
    a.f.repo.store_plan(&plan, &owner()).await.unwrap();
    let req = ExecuteTransformationPlan {
        plan_id: plan.response.plan_id,
        plan_hash: plan.response.plan_hash,
        approval_id: None,
    };
    let key = TransformationActivationIdempotency {
        key: "restart-one".into(),
        body_hash: hash(&req).unwrap(),
    };
    (req, key)
}

async fn row(a: &Admission, id: &str) -> (String, i64, i64, String, Option<String>, String) {
    sqlx::query_as("SELECT state,row_version,attempt,metadata,error,plan_hash FROM transformation_job WHERE id=?")
        .bind(id).fetch_one(&a.f.db.pool).await.unwrap()
}
async fn restart(
    a: &Admission,
    r: &TransformationRestart,
    req: &ExecuteTransformationPlan,
    key: &TransformationActivationIdempotency,
) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
    a.f.repo
        .restart_plan(
            r,
            req,
            &owner(),
            key,
            TransformationAdmission {
                input: &a.input,
                space: &a.space,
            },
        )
        .await
}
async fn unconsumed(a: &Admission, req: &ExecuteTransformationPlan) {
    let (phase,activation):(String,bool)=sqlx::query_as("SELECT phase,json_type(payload,'$.activation') IS NOT NULL FROM operation_journal WHERE id=?")
        .bind(&req.plan_id).fetch_one(&a.f.db.pool).await.unwrap();
    assert_eq!((phase.as_str(), activation), ("PLANNED", false));
}

#[tokio::test]
async fn restart_preserves_same_job_original_snapshot_error_and_previous_attempt_history() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let before = row(&a, &r.job_id).await;
    let (req, key) = confirmation(&a).await;
    let ctx = owner();
    let receipt =
        a.f.repo
            .restart_plan(
                &r,
                &req,
                &ctx,
                &key,
                TransformationAdmission {
                    input: &a.input,
                    space: &a.space,
                },
            )
            .await
            .unwrap();
    assert_eq!(receipt.job_id, r.job_id);
    assert_eq!(receipt.state, JobPhase::Queued);
    let after = row(&a, &r.job_id).await;
    assert_eq!(
        (&after.0, after.1, after.2, &after.4, &after.5),
        (&"QUEUED".into(), 4, 2, &None, &req.plan_hash)
    );
    let previous: serde_json::Value = serde_json::from_str(&before.3).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&after.3).unwrap();
    let history = &metadata["attempt_history"];
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["attempt"], 1);
    assert_eq!(history[0]["state"], "FAILED_RECOVERABLE");
    assert_eq!(
        history[0]["error"],
        serde_json::from_str::<serde_json::Value>(before.4.as_deref().unwrap()).unwrap()
    );
    assert_eq!(history[0]["plan_snapshot"], previous["plan_snapshot"]);
    assert_eq!(
        history[0]["execution_binding"],
        previous["execution_binding"]
    );
    assert_eq!(history[0]["request"], previous["request"]);
    assert_eq!(history[0]["resource_locks"], previous["resource_locks"]);
    assert_eq!(
        history[0]["input_location_ids"],
        previous["input_location_ids"]
    );
    assert_eq!(
        metadata["input_fingerprint_sha256"],
        previous["input_fingerprint_sha256"]
    );
    assert_eq!(metadata["input_artifacts"], previous["input_artifacts"]);
    assert_eq!(
        metadata["initial_activation_digest"],
        previous["initial_activation_digest"]
    );
    assert_ne!(
        metadata["planned_output_variant_id"],
        previous["planned_output_variant_id"]
    );
    assert_eq!(metadata["guard_generation"], 1);
    assert_eq!(metadata["correlation_id"], ctx.correlation_id);
    let event:(i64,String,String)=sqlx::query_as("SELECT resource_version,correlation_id,payload FROM event WHERE resource_id=? AND json_extract(payload,'$.action')='restarted'")
        .bind(&r.job_id).fetch_one(&a.f.db.pool).await.unwrap();
    assert_eq!((event.0, event.1), (4, ctx.correlation_id));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&event.2).unwrap()["attempt"],
        2
    );
    assert_eq!(a.effects().await, (1, 1, 1, 4, 2, 3));
    // The old confirmation still replays even after a restart changed the job.
    assert_eq!(a.activate().await.unwrap().job_id, r.job_id);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn restart_replay_is_bound_to_job_body_and_exact_revision_even_after_job_advances() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let (req, key) = confirmation(&a).await;
    let receipt = restart(&a, &r, &req, &key).await.unwrap();
    sqlx::query("UPDATE transformation_job SET row_version=row_version+1 WHERE id=?")
        .bind(&r.job_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    let before = row(&a, &r.job_id).await;
    let observations = a.space.calls.load(Ordering::SeqCst);
    a.input.reject_at.store(0, Ordering::SeqCst);
    a.f.gate.reject.store(true, Ordering::SeqCst);
    assert_eq!(restart(&a, &r, &req, &key).await.unwrap(), receipt);
    assert_eq!(a.space.calls.load(Ordering::SeqCst), observations);
    let mut wrong = r.clone();
    wrong.if_match += 1;
    assert_eq!(
        restart(&a, &wrong, &req, &key).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    wrong = r.clone();
    wrong.job_id = id();
    assert_eq!(
        restart(&a, &wrong, &req, &key).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut changed = req.clone();
    changed.approval_id = Some(id());
    assert_eq!(
        restart(&a, &r, &changed, &key).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut changed = key.clone();
    changed.body_hash = "0".repeat(64);
    assert_eq!(
        restart(&a, &r, &req, &changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(row(&a, &r.job_id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn recovery_does_not_authorize_stealing_previous_attempt_resources() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, false).await;
    let (req, key) = confirmation(&a).await;
    let before = row(&a, &r.job_id).await;
    let effects = a.effects().await;
    let error = restart(&a, &r, &req, &key).await.unwrap_err();
    assert_eq!(
        error.details.reason,
        "transformation_previous_attempt_resources_pending"
    );
    assert_eq!(row(&a, &r.job_id).await, before);
    assert_eq!(a.effects().await, effects);
    unconsumed(&a, &req).await;
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn obsolete_revision_wrong_owner_and_terminal_state_leave_failed_attempt_unchanged() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let (req, key) = confirmation(&a).await;
    let before = row(&a, &r.job_id).await;
    let mut stale = r.clone();
    stale.if_match = 2;
    assert_eq!(
        restart(&a, &stale, &req, &key).await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    let mut other = owner();
    other.actor_id = "other-owner".into();
    assert_eq!(
        a.f.repo
            .restart_plan(
                &r,
                &req,
                &other,
                &key,
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
    assert_eq!(row(&a, &r.job_id).await, before);
    for state in ["COMPLETED", "CANCELLED", "QUEUED", "RUNNING", "PAUSED"] {
        sqlx::query("UPDATE transformation_job SET state=? WHERE id=?")
            .bind(state)
            .bind(&r.job_id)
            .execute(&a.f.db.pool)
            .await
            .unwrap();
        assert_eq!(
            restart(&a, &r, &req, &key).await.unwrap_err().code,
            ErrorCode::InvalidStateTransition,
            "{state}"
        );
        unconsumed(&a, &req).await;
    }
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn restart_storage_or_persistence_failure_rolls_back_history_attempt_and_new_plan() {
    for failure in [
        "disk",
        "input",
        "outbox",
        "job",
        "journal",
        "plan",
        "locks",
        "reservation",
        "idempotency",
    ] {
        let mut a = Admission::new().await;
        let r = failed_attempt(&a, true).await;
        let (req, key) = confirmation(&a).await;
        let before = row(&a, &r.job_id).await;
        let effects = a.effects().await;
        match failure {
            "disk" => a.space.free = 0,
            "input" => a.input.reject_at.store(0, Ordering::SeqCst),
            "outbox" => {
                sqlx::query("CREATE TRIGGER deny_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture'); END").execute(&a.f.db.pool).await.unwrap();
            }
            "job" => {
                sqlx::query("CREATE TRIGGER deny_job BEFORE UPDATE ON transformation_job BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            "journal" => {
                sqlx::query("CREATE TRIGGER deny_journal BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            "plan" => {
                sqlx::query("CREATE TRIGGER deny_plan BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.plan' BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            "locks" => {
                sqlx::query("CREATE TRIGGER deny_locks BEFORE INSERT ON resource_lock BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            "reservation" => {
                sqlx::query("CREATE TRIGGER deny_reserve BEFORE INSERT ON volume_reservation BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            "idempotency" => {
                sqlx::query("CREATE TRIGGER deny_key BEFORE INSERT ON idempotency BEGIN SELECT RAISE(IGNORE); END").execute(&a.f.db.pool).await.unwrap();
            }
            _ => unreachable!(),
        }
        assert!(restart(&a, &r, &req, &key).await.is_err(), "{failure}");
        assert_eq!(row(&a, &r.job_id).await, before, "{failure}");
        assert_eq!(a.effects().await, effects, "{failure}");
        unconsumed(&a, &req).await;
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn concurrent_restarts_accept_once_and_exact_replay_keeps_one_attempt() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let (req, key) = confirmation(&a).await;
    let (first, second) = tokio::join!(restart(&a, &r, &req, &key), restart(&a, &r, &req, &key));
    assert_eq!(first.unwrap(), second.unwrap());
    let after = row(&a, &r.job_id).await;
    assert_eq!((after.1, after.2), (4, 2));
    assert_eq!(a.effects().await, (1, 1, 1, 4, 2, 3));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn new_plan_cannot_replace_the_original_input_even_if_its_new_snapshot_is_consistent() {
    let mut a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let before = row(&a, &r.job_id).await;
    let mut changed = a.f.input.clone();
    changed[0].sha256 = "0".repeat(64);
    a.input.artifacts = changed.clone();
    let mut draft = a.f.draft();
    draft.storage_estimate = a.plan.response.storage_estimate.clone();
    let plan = seal_bound_plan(
        id(),
        a.f.request.clone(),
        changed,
        draft,
        a.f.binding.clone(),
    )
    .unwrap();
    a.f.repo.store_plan(&plan, &owner()).await.unwrap();
    let req = ExecuteTransformationPlan {
        plan_id: plan.response.plan_id,
        plan_hash: plan.response.plan_hash,
        approval_id: None,
    };
    let key = TransformationActivationIdempotency {
        key: "changed-input".into(),
        body_hash: hash(&req).unwrap(),
    };
    assert_eq!(
        restart(&a, &r, &req, &key).await.unwrap_err().code,
        ErrorCode::IntegrityFailed
    );
    assert_eq!(row(&a, &r.job_id).await, before);
    unconsumed(&a, &req).await;
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn replay_binds_revisions_above_json_exact_integer_range_without_rounding() {
    let a = Admission::new().await;
    let mut r = failed_attempt(&a, true).await;
    let (req, key) = confirmation(&a).await;
    r.if_match = 9_007_199_254_740_993;
    sqlx::query("UPDATE transformation_job SET row_version=? WHERE id=?")
        .bind(r.if_match as i64)
        .bind(&r.job_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    restart(&a, &r, &req, &key).await.unwrap();
    let mut changed = r.clone();
    changed.if_match -= 1;
    assert_eq!(
        restart(&a, &changed, &req, &key).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert!(restart(&a, &r, &req, &key).await.is_ok());
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn earlier_restart_receipt_and_full_history_survive_a_later_restart() {
    let a = Admission::new().await;
    let r = failed_attempt(&a, true).await;
    let (req, key) = confirmation(&a).await;
    let first = restart(&a, &r, &req, &key).await.unwrap();
    sqlx::query(
        "UPDATE transformation_job SET state='RUNNING',row_version=row_version+1 WHERE id=?",
    )
    .bind(&r.job_id)
    .execute(&a.f.db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE operation_journal SET phase='RUNNING',payload=json_set(payload,'$.state','RUNNING') WHERE id=?").bind(&r.job_id).execute(&a.f.db.pool).await.unwrap();
    larm_db::transformations::SqlTransformationJobs {
        pool: a.f.db.pool.clone(),
    }
    .recover_interrupted()
    .await
    .unwrap();
    larm_db::guard::SqlResourceGuard {
        pool: a.f.db.pool.clone(),
    }
    .release(&r.job_id, 1, None)
    .await
    .unwrap();
    larm_db::reservations::SqlVolumeReservations {
        pool: a.f.db.pool.clone(),
    }
    .release(&r.job_id)
    .await
    .unwrap();
    let (next_req, mut next_key) = confirmation(&a).await;
    next_key.key = "restart-two".into();
    let next = TransformationRestart {
        job_id: r.job_id.clone(),
        if_match: 6,
    };
    restart(&a, &next, &next_req, &next_key).await.unwrap();
    let after = row(&a, &r.job_id).await;
    assert_eq!((after.1, after.2), (7, 3));
    let metadata: serde_json::Value = serde_json::from_str(&after.3).unwrap();
    assert_eq!(metadata["attempt_history"].as_array().unwrap().len(), 2);
    assert_eq!(metadata["attempt_history"][0]["plan_hash"], a.req.plan_hash);
    assert_eq!(metadata["attempt_history"][1]["plan_hash"], req.plan_hash);
    assert_eq!(metadata["guard_generation"], 2);
    assert_eq!(restart(&a, &r, &req, &key).await.unwrap(), first);
    assert_eq!(a.activate().await.unwrap().job_id, r.job_id);
    assert_eq!(a.effects().await, (1, 1, 1, 4, 3, 5));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn missing_journal_or_invalid_generation_never_resets_fencing_or_consumes_a_plan() {
    for modification in [
        "UPDATE operation_journal SET phase='RUNNING' WHERE kind='transformation.job'",
        "UPDATE operation_journal SET payload=json_set(payload,'$.actor_id','other') WHERE kind='transformation.job'",
        "DELETE FROM operation_journal WHERE kind='transformation.job'",
        "UPDATE transformation_job SET metadata=json_set(metadata,'$.guard_generation','invalid')",
        "UPDATE transformation_job SET metadata=json_set(metadata,'$.guard_generation',-1)",
        "UPDATE transformation_job SET metadata=json_set(metadata,'$.guard_generation',9223372036854775807)",
    ] {
        let a=Admission::new().await; let r=failed_attempt(&a,true).await; let (req,key)=confirmation(&a).await;
        sqlx::query(modification).execute(&a.f.db.pool).await.unwrap();
        let before=row(&a,&r.job_id).await; let effects=a.effects().await;
        assert!(restart(&a,&r,&req,&key).await.is_err(),"{modification}");
        assert_eq!(row(&a,&r.job_id).await,before); assert_eq!(a.effects().await,effects);
        unconsumed(&a,&req).await; a.f.db.pool.close().await;
    }
}
