//! Witness-lost policy: a system restart (boot session change) is OS-level
//! proof that no process or handle of the previous session survives. REGISTERED
//! intents close their attempt witness-lessly; PREPARING intents are retired
//! with the job left re-dispatchable. Same-session intents retain resources.
use super::*;
use larm_domain::transformations::TransformationState;

const OLD_BOOT: u64 = 1_700_000_000;
const NEW_BOOT: u64 = 1_800_000_000;

async fn simulate_startup_interruption(a: &Admission, id: &str) {
    // recover_interrupted marca el intento interrumpido al arrancar el daemon:
    // misma transición Estado+journal para el oráculo del cierre sin testigo.
    sqlx::query("UPDATE transformation_job SET state='FAILED_RECOVERABLE',error=? WHERE id=?")
        .bind("{\"code\":\"WORKER_INTERRUPTED\"}")
        .bind(id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operation_journal SET phase='FAILED_RECOVERABLE',payload=json_set(payload,'$.state','FAILED_RECOVERABLE','$.error',json('{\"code\":\"WORKER_INTERRUPTED\"}')) WHERE id=? AND kind='transformation.job'")
        .bind(id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn un_reinicio_del_sistema_cierra_el_intento_registrado_sin_testigo() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let c = repo.claim_job(&id).await.unwrap();
    simulate_startup_interruption(&a, &id).await;
    let job = repo
        .finish_interrupted_system_restart(&c, OLD_BOOT, NEW_BOOT)
        .await
        .unwrap();
    assert_eq!(job.state, TransformationState::FailedRecoverable);
    assert_eq!(
        job.metadata["attempt_closed"]["system_restart"],
        json!([OLD_BOOT, NEW_BOOT])
    );
    assert_eq!(a.effects().await, (1, 1, 0, 0, 1, 3));
    let (action,): (String,) = sqlx::query_as(
        "SELECT json_extract(payload,'$.action') FROM event WHERE resource_id=? ORDER BY resource_version DESC LIMIT 1",
    )
    .bind(&id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap();
    assert_eq!(action, "attempt_closed_system_restart");
    // Replay idempotente y pruebas inválidas rechazadas sin escribir.
    let before = snapshot(&a, &id).await;
    let replay = repo
        .finish_interrupted_system_restart(&c, OLD_BOOT, NEW_BOOT)
        .await
        .unwrap();
    assert_eq!(replay.row_version, job.row_version);
    for (previous, current) in [(0, NEW_BOOT), (NEW_BOOT, NEW_BOOT)] {
        assert_eq!(
            repo.finish_interrupted_system_restart(&c, previous, current)
                .await
                .unwrap_err()
                .details
                .reason,
            "transformation_system_restart_proof_invalid"
        );
    }
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn un_reinicio_del_sistema_retira_la_preparacion_y_el_trabajo_vuelve_a_despacharse() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let p = repo.propose_native_preparation(&id).await.unwrap();
    let ready = super::native_owner::Ready::new(&p);
    repo.prepare_native_owner(&p, &ready.native).await.unwrap();
    assert_eq!(ready.native.boot_session, 0);
    // La intención preparada antes de la política no lleva boot: se retiene.
    let before = snapshot(&a, &id).await;
    assert_eq!(
        repo.close_preparation_system_restart(&ready.native, NEW_BOOT)
            .await
            .unwrap_err()
            .details
            .reason,
        "transformation_system_restart_proof_invalid"
    );
    assert_eq!(snapshot(&a, &id).await, before);
    a.f.db.pool.close().await;
}

#[tokio::test]
async fn la_intencion_preparada_con_boot_anterior_se_retira_y_el_trabajo_reclama_de_nuevo() {
    let a = Admission::new().await;
    let id = queued(&a).await;
    let repo = jobs(&a);
    let p = repo.propose_native_preparation(&id).await.unwrap();
    let mut ready = super::native_owner::Ready::new(&p);
    ready.native.boot_session = NEW_BOOT;
    repo.prepare_native_owner(&p, &ready.native).await.unwrap();
    // La reescritura del journal que simula el reinicio del sistema en la
    // aceptación nativa: sólo el boot_session de la intención cambia.
    let (intent_id, payload): (String, String) = sqlx::query_as(
        "SELECT id,payload FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=?",
    )
    .bind(&id)
    .fetch_one(&a.f.db.pool)
    .await
    .unwrap();
    let mut edited: Value = serde_json::from_str(&payload).unwrap();
    edited["owner"]["boot_session"] = json!(OLD_BOOT);
    sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
        .bind(serde_json::to_string(&edited).unwrap())
        .bind(&intent_id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    let mut retired = ready.native.clone();
    retired.boot_session = OLD_BOOT;
    repo.close_preparation_system_restart(&retired, NEW_BOOT)
        .await
        .unwrap();
    // La intención queda CLOSED y el trabajo sigue QUEUED con recursos: el
    // despacho lo reclama de nuevo y la preparación arranca desde cero.
    let intents: Vec<(String,)> = sqlx::query_as(
        "SELECT phase FROM operation_journal WHERE kind='transformation.owner' AND json_extract(payload,'$.owner.fence.job_id')=?",
    )
    .bind(&id)
    .fetch_all(&a.f.db.pool)
    .await
    .unwrap();
    assert_eq!(intents, vec![("CLOSED".to_owned(),)]);
    let claim = repo.claim_job(&id).await.unwrap();
    assert_eq!(claim.job.state, TransformationState::Running);
    assert_eq!(a.effects().await, (1, 1, 1, 4, 1, 3));
    // La misma preparación retirada no se retira dos veces contra otro estado.
    sqlx::query("UPDATE transformation_job SET state='RUNNING' WHERE id=?")
        .bind(&id)
        .execute(&a.f.db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.close_preparation_system_restart(&retired, NEW_BOOT)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    a.f.db.pool.close().await;
}
