//! SQL progress relay: pure observation of the active attempt exposed by the
//! JobSource. No row_version, journal or event side effects, no aggregation.
use super::*;
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::ports::JobSource;

fn observer(actor: &str) -> Principal {
    Principal::new(
        actor,
        "progreso",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap()
}

fn progress_snapshot(completed: u64) -> Value {
    json!({
        "request_id": crate::id(),
        "phase": "TRANSFORM_ENCODING",
        "completed_bytes": completed,
        "total_bytes": 16777224_u64,
        "message_code": "reference_block_durable",
        "observed_at": "2026-11-06T20:00:00.000Z",
    })
}

#[tokio::test]
async fn el_progreso_del_intento_activo_se_expone_sin_efectos_colaterales() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    // En cola no se observa: el intento no está activo.
    assert_eq!(
        repo.observe_progress(&id, 1, &progress_snapshot(1))
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_progress_attempt_inactive"
    );
    let c = repo.claim_job(&id).await.unwrap();
    let before = snapshot(&a, &id).await;
    repo.observe_progress(&id, c.job.attempt, &progress_snapshot(1024))
        .await
        .unwrap();
    let job = repo
        .job(&observer("plan-owner"), &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.state, larm_domain::bridge::JobPhase::Running);
    let worker = &job.progress["worker"];
    assert_eq!(worker["phase"], "TRANSFORM_ENCODING");
    assert_eq!(worker["completed_bytes"], json!(1024));
    assert_eq!(worker["total_bytes"], json!(16777224_u64));
    // Observación pura: ni revisión ni journal ni eventos; los recursos siguen.
    let after = snapshot(&a, &id).await;
    assert_eq!(after["job"][1], before["job"][1], "row_version inmutable");
    assert_eq!(
        after["effects"], before["effects"],
        "sin eventos ni outbox nuevos"
    );
    // La siguiente observación sustituye a la anterior (última observada).
    repo.observe_progress(&id, c.job.attempt, &progress_snapshot(2048))
        .await
        .unwrap();
    let job = repo
        .job(&observer("plan-owner"), &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.progress["worker"]["completed_bytes"], json!(2048));
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn un_intento_viejo_o_terminado_no_observa_progreso() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let c = repo.claim_job(&id).await.unwrap();
    let g = Guard::new(&c);
    g.closed.store(true, Ordering::SeqCst);
    fail(&a, &c, &g, true).await.unwrap();
    // Intento terminado: ni el propio intento ni uno nuevo escriben.
    assert_eq!(
        repo.observe_progress(&id, c.job.attempt, &progress_snapshot(1))
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_progress_attempt_inactive"
    );
    assert_eq!(
        repo.observe_progress(&id, c.job.attempt + 1, &progress_snapshot(1))
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_progress_attempt_inactive"
    );
    let job = repo
        .job(&observer("plan-owner"), &id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.progress.get("worker").is_none(),
        "los terminales no exponen progreso"
    );
    // Payload no objeto y job desconocido se rechazan sin escribir.
    assert_eq!(
        repo.observe_progress(&id, 1, &json!("no-objeto"))
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_progress_invalid"
    );
    // Un id con formato válido pero desconocido no revela existencia.
    assert_eq!(
        repo.observe_progress(&crate::id(), 1, &progress_snapshot(1))
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_progress_attempt_inactive"
    );
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn el_intento_reclamado_de_otro_actor_no_ve_el_progreso_ajeno() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let c = repo.claim_job(&id).await.unwrap();
    repo.observe_progress(&id, c.job.attempt, &progress_snapshot(512))
        .await
        .unwrap();
    let owner_view = repo
        .job(&observer("plan-owner"), &id)
        .await
        .unwrap()
        .unwrap();
    assert!(owner_view.progress["worker"].is_object());
    let foreign_view = repo.job(&observer("profile-backend"), &id).await.unwrap();
    assert!(foreign_view.is_none(), "el job ajeno queda oculto");
    a.f.db.pool.close().await;
}
