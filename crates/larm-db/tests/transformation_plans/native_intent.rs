//! Persistence faults with explicit observation doubles, not native authority.
use super::*;
async fn candidate(a: &Admission) -> (String, TransformationClaimProposal, Ready) {
    let id = queued(a).await;
    let p = jobs(a).propose_native_preparation(&id).await.unwrap();
    let r = Ready::new(&p);
    (id, p, r)
}
async fn phase(a: &Admission, owner: &TransformationNativeOwner) -> String {
    sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE id=? AND kind='transformation.owner'",
    )
    .bind(&owner.session_id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn intencion_detecta_evento_o_outbox_borrado_o_alterado_despues_de_insertar() {
    for trigger in [
        "CREATE TRIGGER fault AFTER INSERT ON event_outbox BEGIN DELETE FROM event_outbox WHERE event_id=NEW.event_id; END",
        "CREATE TRIGGER fault AFTER INSERT ON event_outbox BEGIN UPDATE event SET payload='{}' WHERE event_id=NEW.event_id; END",
        "CREATE TRIGGER fault AFTER INSERT ON event_outbox BEGIN UPDATE event SET actor_id='other-actor' WHERE event_id=NEW.event_id; END",
        "CREATE TRIGGER fault AFTER INSERT ON event_outbox BEGIN UPDATE event SET resource_version=resource_version+1 WHERE event_id=NEW.event_id; END",
    ] {
        let a=Admission::new().await;let(id,p,r)=candidate(&a).await;
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(jobs(&a).prepare_native_owner(&p,&r.native).await.is_err(),"una escritura contada no basta: evento y outbox deben sobrevivir intactos");
        assert_eq!(snapshot(&a,&id).await,before);a.f.db.pool.close().await;
    }
}
#[tokio::test]
async fn intencion_precede_handshake_y_no_concede_claim_ni_cierre() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a)
        .check_pending_native_owner(&r.native, &p.claim.context)
        .await
        .is_err());
    assert!(jobs(&a).claim_with_native_owner(&p, &r).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    let prepared = snapshot(&a, &id).await;
    assert_eq!(prepared["job"][0], "QUEUED");
    assert_eq!(prepared["job"][1], 2);
    assert_eq!(prepared["job"][2], 0);
    assert_eq!(phase(&a, &r.native).await, "PREPARING");
    assert!(jobs(&a).claim_job(&id).await.is_err());
    assert!(jobs(&a).claim_closed_preparation(&p, &r).await.is_err());
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    assert_eq!(snapshot(&a, &id).await, prepared);
    let all = jobs(&a).native_preparations(None, 1).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].owner, r.native);
    assert!(!all[0].registered);
    assert!(jobs(&a)
        .native_preparations(Some(&r.native.session_id), 1)
        .await
        .unwrap()
        .is_empty());
    assert!(jobs(&a).native_preparations(Some("bad"), 1).await.is_err());
    let registered = jobs(&a).claim_with_native_owner(&p, &r).await.unwrap();
    assert_eq!(registered.claim.job.row_version, 3);
    assert_eq!(phase(&a, &r.native).await, "REGISTERED");
    assert!(jobs(&a).native_preparations(None, 1).await.unwrap()[0].registered);
    a.f.db.pool.close().await;
}
#[tokio::test]
async fn fallos_de_intencion_cas_journal_y_outbox_revierten_todo() {
    for trigger in [
        "CREATE TRIGGER fault BEFORE INSERT ON operation_journal WHEN NEW.kind='transformation.owner' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE UPDATE ON transformation_job BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.job' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fault AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.owner' BEGIN UPDATE operation_journal SET phase='CLOSED' WHERE id=NEW.id; END",
        "CREATE TRIGGER fault AFTER INSERT ON operation_journal WHEN NEW.kind='transformation.owner' BEGIN UPDATE operation_journal SET payload=json_set(payload,'$.owner.public_key','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb') WHERE id=NEW.id; END",
    ] {
        let a=Admission::new().await;let(id,p,r)=candidate(&a).await;
        sqlx::query(trigger).execute(&a.f.db.pool).await.unwrap();let before=snapshot(&a,&id).await;
        assert!(jobs(&a).prepare_native_owner(&p,&r.native).await.is_err());assert_eq!(snapshot(&a,&id).await,before);
        a.f.db.pool.close().await;
    }
}
#[tokio::test]
async fn dos_intenciones_compiten_con_un_unico_cas_y_no_se_sustituyen() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    let other = jobs(&a).propose_native_preparation(&id).await.unwrap();
    let second = Ready::new(&other);
    let repo = jobs(&a);
    let (one, two) = tokio::join!(
        repo.prepare_native_owner(&p, &r.native),
        repo.prepare_native_owner(&other, &second.native)
    );
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(repo.native_preparations(None, 128).await.unwrap().len(), 1);
    assert!(repo.propose_native_preparation(&id).await.is_err());
    let snapshot = snapshot(&a, &id).await;
    assert_eq!(snapshot["job"][1], 2);
    assert_eq!(snapshot["native_intents"].as_array().unwrap().len(), 1);
    a.f.db.pool.close().await;
}
#[tokio::test]
async fn omitir_transicion_de_intencion_revierte_claim_y_cierre() {
    for target in ["REGISTERED", "CLOSED"] {
        let a = Admission::new().await;
        let (id, p, r) = candidate(&a).await;
        jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
        let registered = if target == "CLOSED" {
            Some(jobs(&a).claim_with_native_owner(&p, &r).await.unwrap())
        } else {
            None
        };
        let trigger=format!("CREATE TRIGGER fault BEFORE UPDATE ON operation_journal WHEN OLD.kind='transformation.owner' AND NEW.phase='{target}' BEGIN SELECT RAISE(IGNORE); END");
        sqlx::query(sqlx::AssertSqlSafe(trigger))
            .execute(&a.f.db.pool)
            .await
            .unwrap();
        let before = snapshot(&a, &id).await;
        if let Some(c) = registered {
            r.guard.closed.store(true, Ordering::SeqCst);
            assert!(jobs(&a)
                .finish_failed_attempt(&c.claim, &r, &failure(), true)
                .await
                .is_err());
        } else {
            assert!(jobs(&a).claim_with_native_owner(&p, &r).await.is_err());
        }
        assert_eq!(snapshot(&a, &id).await, before);
        a.f.db.pool.close().await;
    }
}
#[tokio::test]
async fn recuperacion_adopta_preparacion_solo_con_testigo_de_cierre() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).claim_closed_preparation(&p, &r).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    r.guard.closed.store(true, Ordering::SeqCst);
    let c = jobs(&a).claim_closed_preparation(&p, &r).await.unwrap();
    let failed = jobs(&a)
        .finish_failed_attempt(&c.claim, &r, &failure(), true)
        .await
        .unwrap();
    assert_eq!(failed.state, TransformationState::FailedRecoverable);
    assert_eq!(failed.attempt, 1);
    assert_eq!(failed.row_version, 4);
    assert_eq!(phase(&a, &r.native).await, "CLOSED");
    assert!(jobs(&a)
        .native_preparations(None, 128)
        .await
        .unwrap()
        .is_empty());
    let after = snapshot(&a, &id).await;
    assert!(after["locks"].as_array().unwrap().is_empty());
    assert!(after["reserves"].as_array().unwrap().is_empty());
    a.f.db.pool.close().await;
}
#[tokio::test]
async fn propietario_registrado_anterior_sin_intencion_sigue_requiriendo_cierre_vivo() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    let c = jobs(&a).claim_with_native_owner(&p, &r).await.unwrap();
    sqlx::query("DELETE FROM operation_journal WHERE id=? AND kind='transformation.owner'")
        .bind(&r.native.session_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(jobs(&a).recover_interrupted().await.unwrap(), 1);
    let page = jobs(&a).native_preparations(None, 128).await.unwrap();
    assert_eq!(page.len(), 1);
    assert!(page[0].registered);
    assert_eq!(page[0].owner, r.native);
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a)
        .finish_failed_attempt(&c.claim, &r, &failure(), true)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    r.guard.closed.store(true, Ordering::SeqCst);
    let closed = jobs(&a)
        .finish_failed_attempt(&c.claim, &r, &failure(), true)
        .await
        .unwrap();
    assert_eq!(closed.error.as_ref().unwrap()["code"], "WORKER_INTERRUPTED");
    assert!(jobs(&a)
        .native_preparations(None, 128)
        .await
        .unwrap()
        .is_empty());
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn testigo_cerrado_se_puede_retirar_desde_historial_sin_liberar_el_intento_nuevo() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    let c = jobs(&a).claim_with_native_owner(&p, &r).await.unwrap();
    r.guard.closed.store(true, Ordering::SeqCst);
    let failed = jobs(&a)
        .finish_failed_attempt(&c.claim, &r, &failure(), true)
        .await
        .unwrap();
    let before = snapshot(&a, &id).await;
    let closed = jobs(&a).native_retirements(None, 1).await.unwrap();
    assert_eq!(closed, vec![r.native.clone()]);
    assert_eq!(snapshot(&a, &id).await, before);
    // Old v2 records can lack the side intent. Their archived owner remains
    // discoverable for housekeeping, never as permission to release resources.
    sqlx::query("DELETE FROM operation_journal WHERE id=? AND kind='transformation.owner'")
        .bind(&r.native.session_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    let (req, key) = super::super::super::restart::confirmation(&a).await;
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
    let fresh = Ready::new(&next);
    jobs(&a)
        .prepare_native_owner(&next, &fresh.native)
        .await
        .unwrap();
    let before = snapshot(&a, &id).await;
    assert_eq!(
        jobs(&a).native_retirements(None, 128).await.unwrap(),
        vec![r.native.clone()]
    );
    assert_eq!(snapshot(&a, &id).await, before);
    assert_eq!(before["locks"].as_array().unwrap().len(), 4);
    assert_eq!(before["reserves"].as_array().unwrap().len(), 1);
    assert!(jobs(&a)
        .finish_failed_attempt(&c.claim, &r, &failure(), true)
        .await
        .is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}
#[tokio::test]
async fn fase_closed_sin_cierre_correspondiente_no_identifica_un_testigo_retirado() {
    let a = Admission::new().await;
    let (id, p, r) = candidate(&a).await;
    jobs(&a).prepare_native_owner(&p, &r.native).await.unwrap();
    sqlx::query(
        "UPDATE operation_journal SET phase='CLOSED' WHERE id=? AND kind='transformation.owner'",
    )
    .bind(&r.native.session_id)
    .execute(&a.f.db.pool)
    .await
    .unwrap();
    let before = snapshot(&a, &id).await;
    assert!(jobs(&a).native_retirements(None, 128).await.is_err());
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}
