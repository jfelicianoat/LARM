//! SQL ownership tests with explicit ready/closure observation doubles.
//! This module does not claim native process or H07 acceptance.
use super::*;
use larm_domain::transformation_execution::{
    TransformationClaimProposal, TransformationNativeOwner, TransformationReadyOwner,
};
#[path = "native_intent.rs"]
mod intent;

pub(super) struct Ready {
    guard: Guard,
    pub(super) native: TransformationNativeOwner,
    calls: AtomicUsize,
    fail_at: usize,
}
impl Ready {
    pub(super) fn new(p: &TransformationClaimProposal) -> Self {
        Self {
            guard: Guard::new(&p.claim),
            native: TransformationNativeOwner {
                schema_version: "1.0".into(),
                session_id: super::super::id(),
                public_key: "a".repeat(64),
                queued_row_version: p.queued_row_version,
                fence: p.claim.fence.clone(),
                adapter_id: p.claim.plan.request.adapter_id.clone(),
                binding: p.claim.plan.execution_binding.clone().unwrap(),
                boot_session: 0,
            },
            calls: AtomicUsize::new(0),
            fail_at: usize::MAX,
        }
    }
}
#[async_trait::async_trait]
impl TransformationAttemptGuard for Ready {
    fn fence(&self) -> &TransformationAttemptFence {
        &self.guard.fence
    }
    fn binding(&self) -> &TransformationExecutionBinding {
        &self.guard.binding
    }
    fn native_owner(&self) -> Option<&TransformationNativeOwner> {
        Some(&self.native)
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.guard.recheck().await
    }
    async fn require_closed(&self) -> Result<(), LarmError> {
        self.guard.require_closed().await
    }
}
#[async_trait::async_trait]
impl TransformationReadyOwner for Ready {
    async fn require_ready(&self) -> Result<(), LarmError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_at {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "Ready fixture lost",
                "ready_fixture_lost",
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn propuesta_no_muta_y_claim_registra_owner_job_journal_y_evento_juntos() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let before = snapshot(&a, &id).await;
    let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
    assert_eq!(snapshot(&a, &id).await, before);
    assert_eq!(p.claim.job.attempt, 1);
    assert_eq!(p.claim.job.state, TransformationState::Running);
    let ready = Ready::new(&p);
    jobs(&a)
        .prepare_native_owner(&p, &ready.native)
        .await
        .unwrap();
    jobs(&a)
        .check_pending_native_owner(&ready.native, &p.claim.context)
        .await
        .unwrap();
    let registered = jobs(&a).claim_with_native_owner(&p, &ready).await.unwrap();
    assert_eq!(ready.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        registered.claim.job.metadata["native_owner"],
        json!(ready.native)
    );
    assert_eq!(registered.owner, ready.native);
    assert_eq!(jobs(&a).owned_claim(&id).await.unwrap().owner, ready.native);
    assert!(jobs(&a)
        .check_pending_native_owner(&ready.native, &p.claim.context)
        .await
        .is_err());
    jobs(&a)
        .check_registered_native_owner(&ready.native, &p.claim.context)
        .await
        .unwrap();
    jobs(&a)
        .check_active_claim(&registered.claim, &ready)
        .await
        .unwrap();
    let legacy = Guard::new(&registered.claim);
    legacy.closed.store(true, Ordering::SeqCst);
    assert_eq!(
        jobs(&a)
            .finish_failed_attempt(&registered.claim, &legacy, &failure(), true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn disponibilidad_perdida_antes_o_despues_de_escribir_revierte_todo() {
    for fail_at in [1, 2] {
        let a = Admission::new().await;
        let id = queued(&a).await;
        let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
        let mut ready = Ready::new(&p);
        jobs(&a)
            .prepare_native_owner(&p, &ready.native)
            .await
            .unwrap();
        ready.fail_at = fail_at;
        let before = snapshot(&a, &id).await;
        assert!(jobs(&a).claim_with_native_owner(&p, &ready).await.is_err());
        assert_eq!(snapshot(&a, &id).await, before);
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn propuesta_o_owner_de_otro_claim_no_reclaman_y_no_hay_claim_doble() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
    let other = jobs(&a).propose_native_preparation(&id).await.unwrap();
    assert_ne!(p.claim.fence.claim_id, other.claim.fence.claim_id);
    let ready = Ready::new(&p);
    jobs(&a)
        .prepare_native_owner(&p, &ready.native)
        .await
        .unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a)
        .claim_with_native_owner(&other, &ready)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    jobs(&a).claim_with_native_owner(&p, &ready).await.unwrap();
    let once = snapshot(&a, &id).await;
    assert!(jobs(&a).claim_with_native_owner(&p, &ready).await.is_err());
    assert_eq!(snapshot(&a, &id).await, once);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn escrituras_omitidas_no_dejan_owner_o_claim_parcial() {
    for trigger in [
        "CREATE TRIGGER fault BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault AFTER UPDATE ON transformation_job BEGIN UPDATE transformation_job SET metadata=json_remove(metadata,'$.native_owner') WHERE id=NEW.id; END",
    ] {
        let a=Admission::new().await;let id=queued(&a).await;let p=jobs(&a).propose_native_preparation(&id).await.unwrap();let ready=Ready::new(&p);
        jobs(&a).prepare_native_owner(&p,&ready.native).await.unwrap();
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(jobs(&a).claim_with_native_owner(&p,&ready).await.is_err());assert_eq!(snapshot(&a,&id).await,before);
        a.f.db.pool.close().await;
    }
}

#[tokio::test]
async fn una_mitad_de_owner_corrupta_no_libera_recursos() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
    let ready = Ready::new(&p);
    jobs(&a)
        .prepare_native_owner(&p, &ready.native)
        .await
        .unwrap();
    let registered = jobs(&a).claim_with_native_owner(&p, &ready).await.unwrap();
    ready.guard.closed.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE transformation_job SET metadata=json_set(metadata,'$.native_owner.public_key',?) WHERE id=?")
        .bind("b".repeat(64)).bind(&id).execute(&a.f.db.pool).await.unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).owned_claim(&id).await.is_err());
    assert!(jobs(&a)
        .finish_failed_attempt(&registered.claim, &ready, &failure(), true)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn cierre_y_reinicio_conservan_owner_historico_y_exigen_nuevo_registro() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let p = jobs(&a).propose_native_preparation(&id).await.unwrap();
    let ready = Ready::new(&p);
    jobs(&a)
        .prepare_native_owner(&p, &ready.native)
        .await
        .unwrap();
    let c = jobs(&a).claim_with_native_owner(&p, &ready).await.unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a)
        .finish_failed_attempt(&c.claim, &ready, &failure(), true)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    ready.guard.closed.store(true, Ordering::SeqCst);
    let failed = jobs(&a)
        .finish_failed_attempt(&c.claim, &ready, &failure(), true)
        .await
        .unwrap();
    let (req, key) = super::super::restart::confirmation(&a).await;
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
    let next = jobs(&a).propose_native_preparation(&id).await.unwrap();
    assert_eq!(next.claim.job.attempt, 2);
    assert_eq!(next.claim.fence.generation, 1);
    assert!(next.claim.job.metadata.get("native_owner").is_none());
    assert_eq!(
        next.claim.job.metadata["attempt_history"][0]["native_owner"],
        json!(ready.native)
    );
    let fresh = Ready::new(&next);
    jobs(&a)
        .prepare_native_owner(&next, &fresh.native)
        .await
        .unwrap();
    assert_ne!(fresh.native.session_id, ready.native.session_id);
    jobs(&a)
        .claim_with_native_owner(&next, &fresh)
        .await
        .unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a)
        .finish_failed_attempt(&c.claim, &ready, &failure(), true)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}
