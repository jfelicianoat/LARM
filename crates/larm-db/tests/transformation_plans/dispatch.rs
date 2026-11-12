//! SQL doubles isolate delivery/replay/rollback; they grant no native proof.
use super::*;
use larm_db::transformations::SqlTransformationJobs;
async fn send(a: &Admission) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
    a.f.repo
        .admit_for_dispatch(
            &a.req,
            &owner(),
            &a.key,
            TransformationAdmission {
                input: &a.input,
                space: &a.space,
            },
            None,
        )
        .await
}
fn jobs(a: &Admission) -> SqlTransformationJobs {
    SqlTransformationJobs {
        pool: a.f.db.pool.clone(),
    }
}
async fn hints(a: &Admission) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM operation_journal WHERE kind='transformation.dispatch'",
    )
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn durable_dispatch_and_replay_do_not_reacquire_revoked_resources() {
    let a = Admission::new().await;
    assert!(a
        .f
        .repo
        .admission_replay(&a.req, &owner(), &a.key, None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(a.input.calls.load(Ordering::SeqCst), 0);
    assert_eq!(a.space.calls.load(Ordering::SeqCst), 0);
    let receipt = send(&a).await.unwrap();
    assert_eq!(hints(&a).await, 1);
    assert_eq!(
        jobs(&a).dispatch_candidates(64).await.unwrap(),
        vec![receipt.job_id.clone()]
    );
    let before = a.effects().await;
    let qualifications = a.f.gate.acquisitions.load(Ordering::SeqCst);
    a.f.gate.reject.store(true, Ordering::SeqCst);
    a.input.reject_at.store(0, Ordering::SeqCst);
    let replay =
        a.f.repo
            .admission_replay(&a.req, &owner(), &a.key, None)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(replay, receipt);
    assert_eq!(a.effects().await, before);
    assert_eq!(hints(&a).await, 1);
    assert_eq!(a.f.gate.acquisitions.load(Ordering::SeqCst), qualifications);
    assert_eq!(send(&a).await.unwrap(), receipt);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn direct_engine_jobs_are_not_dispatch_hints_and_delivery_is_not_a_claim() {
    let a = Admission::new().await;
    let receipt = a.activate().await.unwrap();
    assert!(jobs(&a).dispatch_candidates(1).await.unwrap().is_empty());
    assert_eq!(hints(&a).await, 0);
    assert!(jobs(&a).dispatch_candidates(0).await.is_err());
    assert!(jobs(&a).dispatch_candidates(65).await.is_err());
    let claimed = jobs(&a).claim_job(&receipt.job_id).await.unwrap();
    assert_eq!(claimed.job.attempt, 1);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn missing_or_altered_delivery_rolls_back_plan_job_events_and_resources() {
    for trigger in [
        "CREATE TRIGGER deny_dispatch BEFORE INSERT ON operation_journal WHEN NEW.kind='transformation.dispatch' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER deny_dispatch AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.dispatch' BEGIN DELETE FROM operation_journal WHERE id=NEW.id; END",
        "CREATE TRIGGER deny_dispatch AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.dispatch' BEGIN UPDATE operation_journal SET payload=json_set(payload,'$.actor_id','stranger') WHERE id=NEW.id; END",
        "CREATE TRIGGER deny_dispatch AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.dispatch' BEGIN UPDATE operation_journal SET resource_ids='[]' WHERE id=NEW.id; END",
        "CREATE TRIGGER deny_dispatch AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.dispatch' BEGIN UPDATE operation_journal SET plan_hash='wrong' WHERE id=NEW.id; END",
    ] {
        let a=Admission::new().await;sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();
        assert!(send(&a).await.is_err(),"{trigger}");a.assert_unconsumed().await;assert_eq!(hints(&a).await,0);
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn corrupted_delivery_context_never_dispatches_a_job() {
    for field in ["schema_version", "actor_id", "correlation_id", "attempt"] {
        let a = Admission::new().await;
        send(&a).await.unwrap();
        let sql=format!("UPDATE operation_journal SET payload=json_set(payload,'$.{field}',?) WHERE kind='transformation.dispatch'");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind("invalid")
            .execute(&a.f.db.pool)
            .await
            .unwrap();
        let before = a.effects().await;
        assert!(jobs(&a).dispatch_candidates(1).await.is_err());
        assert_eq!(a.effects().await, before);
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn bounded_unknown_without_hard_budget_is_admitted_unbounded_unknown_is_not() {
    for bound in [true, false] {
        let mut a = Admission::new().await;
        let mut draft = a.f.draft();
        draft.storage_estimate = a.plan.response.storage_estimate.clone();
        draft.storage_estimate.confidence = Confidence::Unknown;
        if !bound {
            draft.storage_estimate.upper_bound_bytes = None;
        }
        let mut request = a.f.request.clone();
        request.hard_budget_bytes = None;
        let plan =
            seal_bound_plan(id(), request, a.f.input.clone(), draft, a.f.binding.clone()).unwrap();
        a.f.repo.store_plan(&plan, &owner()).await.unwrap();
        a.req.plan_id = plan.response.plan_id.clone();
        a.req.plan_hash = plan.response.plan_hash.clone();
        a.key.body_hash = hash(&a.req).unwrap();
        a.plan = plan;
        if bound {
            send(&a).await.unwrap();
            assert_eq!(hints(&a).await, 1);
        } else {
            assert_eq!(send(&a).await.unwrap_err().code, ErrorCode::UnknownSize);
            a.assert_unconsumed().await;
        }
        a.f.db.pool.close().await;
    }
}
