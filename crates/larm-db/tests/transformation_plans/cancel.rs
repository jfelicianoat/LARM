//! SQL cancellation: durable CANCELLING signal, queued release and
//! witness-closed attempt closure. No native process or daemon acceptance.
use super::*;
use larm_domain::bridge::JobPhase;
use larm_domain::transformations::TransformationState;

async fn cancel(a: &Admission, id: &str, actor: &str) -> Result<TransformationJob, LarmError> {
    jobs(a)
        .request_cancellation(
            id,
            actor,
            &MutationContext {
                actor_id: actor.to_string(),
                correlation_id: super::id(),
            },
        )
        .await
}

async fn outbox(a: &Admission, id: &str) -> (i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM event WHERE resource_id=? AND resource_type='transformation_job'),\
         (SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?))",
    )
    .bind(id)
    .bind(id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn queued_cancellation_is_durable_then_releases_everything_and_stays_terminal() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 1));
    let job = cancel(&a, &id, "plan-owner").await.unwrap();
    assert_eq!(job.state, TransformationState::Cancelling);
    let (events, outbox) = outbox(&a, &id).await;
    assert!(
        events >= 2 && events == outbox,
        "eventos sin outbox: {events}/{outbox}"
    );
    let replay = cancel(&a, &id, "plan-owner").await.unwrap();
    assert_eq!(replay.state, TransformationState::Cancelling);
    assert_eq!(replay.row_version, job.row_version, "el replay no muta");
    let closed = jobs(&a).release_queued_cancellation(&id).await.unwrap();
    assert_eq!(closed.state, TransformationState::Cancelled);
    assert_eq!(a.effects().await, (1, 1, 0, 0, 1, 3));
    assert_eq!(
        cancel(&a, &id, "plan-owner").await.unwrap_err().code,
        ErrorCode::InvalidStateTransition,
        "los terminales no se cancelan"
    );
    let again = jobs(&a).release_queued_cancellation(&id).await.unwrap();
    assert_eq!(again.state, TransformationState::Cancelled);
    assert_eq!(again.row_version, closed.row_version);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn foreign_unknown_or_mismatched_actors_stay_hidden_on_cancel() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let before = snapshot(&a, &id).await;
    let foreign = cancel(&a, &id, "otro-actor").await.unwrap_err();
    assert_eq!(foreign.code, ErrorCode::NotFound);
    assert_eq!(foreign.details.reason, "transformation_job_missing");
    let missing = cancel(&a, &crate::id(), "plan-owner").await.unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    // El actor de la señal debe coincidir con la credencial del guard.
    let mismatched = jobs(&a)
        .request_cancellation(
            &id,
            "plan-owner",
            &MutationContext {
                actor_id: "otro-actor".into(),
                correlation_id: crate::id(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        mismatched.details.reason,
        "transformation_cancel_context_invalid"
    );
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn cancelling_blocks_the_claim_until_the_queued_release_or_attempt_closure() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    cancel(&a, &id, "plan-owner").await.unwrap();
    assert!(jobs(&a).claim_next().await.unwrap().is_none());
    assert!(jobs(&a).claim_job(&id).await.is_err());
    assert_eq!(
        jobs(&a)
            .release_queued_cancellation(&id)
            .await
            .unwrap()
            .state,
        TransformationState::Cancelled
    );
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn claimed_cancellation_closes_cancelled_only_with_closed_witness() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    assert_eq!(c.job.state, TransformationState::Running);
    let job = cancel(&a, &id, "plan-owner").await.unwrap();
    assert_eq!(job.state, TransformationState::Cancelling);
    // Un intento reclamado no se cierra por la vía de cola.
    assert!(jobs(&a).release_queued_cancellation(&id).await.is_err());
    let g = Guard::new(&c);
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).finish_cancelled_attempt(&c, &g).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    g.closed.store(true, Ordering::SeqCst);
    let closed = jobs(&a).finish_cancelled_attempt(&c, &g).await.unwrap();
    assert_eq!(closed.state, TransformationState::Cancelled);
    assert_eq!(
        closed.metadata["attempt_closed"]["state"],
        json!("CANCELLED")
    );
    assert_eq!(a.effects().await, (1, 1, 0, 0, 1, 4));
    // Replay idempotente del cierre con el mismo testigo.
    let again = jobs(&a).finish_cancelled_attempt(&c, &g).await.unwrap();
    assert_eq!(again.row_version, closed.row_version);
    assert_eq!(again.state, TransformationState::Cancelled);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn wrong_claim_binding_cannot_cancel_the_attempt() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    cancel(&a, &id, "plan-owner").await.unwrap();
    let mut g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    g.fence.claim_id = super::id();
    let before = snapshot(&a, &id).await;
    assert_eq!(
        jobs(&a)
            .finish_cancelled_attempt(&c, &g)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn failed_attempts_are_not_cancellable_but_replay_keeps_their_closure() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let c = jobs(&a).claim_job(&id).await.unwrap();
    let g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    fail(&a, &c, &g, true).await.unwrap();
    let failed = snapshot(&a, &id).await;
    assert_eq!(
        cancel(&a, &id, "plan-owner").await.unwrap_err().code,
        ErrorCode::InvalidStateTransition
    );
    assert_eq!(snapshot(&a, &id).await, failed);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn cancel_during_preparation_closes_the_unclaimed_intent_and_keeps_the_signal() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
    let ready = super::native_owner::Ready::new(&p);
    jobs(&a)
        .prepare_native_owner(&p, &ready.native)
        .await
        .unwrap();
    let job = cancel(&a, &id, "plan-owner").await.unwrap();
    assert_eq!(job.state, TransformationState::Cancelling);
    let closed = jobs(&a).release_queued_cancellation(&id).await.unwrap();
    assert_eq!(closed.state, TransformationState::Cancelled);
    let intents: Vec<(String, String)> = sqlx::query_as(
        "SELECT phase,json_extract(payload,'$.owner.fence.job_id') FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=?",
    )
    .bind(&id)
    .fetch_all(&a.f.db.pool)
    .await
    .unwrap();
    assert_eq!(intents, vec![("CLOSED".into(), id.clone())]);
    // El consumidor en preparación ya no puede reclamar: el trabajo es
    // terminal y el testigo se retira sin autoridad nueva.
    assert!(jobs(&a).claim_job(&id).await.is_err());
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn service_cancel_job_returns_receipt_and_releases_queued_jobs() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let receipt = larm_domain::ports::TransformationService::cancel_job(
        &jobs(&a),
        &id,
        "plan-owner",
        &MutationContext {
            actor_id: "plan-owner".into(),
            correlation_id: super::id(),
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt.job_id, id);
    assert_eq!(receipt.status_url, format!("/api/v1/jobs/{id}"));
    assert_eq!(receipt.state, JobPhase::Cancelled);
    let state: String = sqlx::query_scalar("SELECT state FROM transformation_job WHERE id=?")
        .bind(&id)
        .fetch_one(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(state, "CANCELLED");
    a.f.db.pool.close().await;
}
