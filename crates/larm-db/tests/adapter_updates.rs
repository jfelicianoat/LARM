//! P024: contratos durables del journal sobre SQLite real. No ejecuta ZIP ni
//! acredita P026; el coordinador físico aún debe comprobar bytes/quiescencia.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::adapter_updates::SqlAdapterUpdates;
use larm_db::adapters::SqlAdapterInstallations;
use larm_db::guard::SqlResourceGuard;
use larm_domain::adapter_updates::{
    AdapterImport, UpdateApproval, UpdatePlan, UpdateStep as Step, UPDATE_KIND, UPDATE_OPERATION,
};
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::bridge::JobPhase;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterInstallationRepository, AdapterUpdateRepository, ApprovalRepository, JobSource,
    MutationContext, ResourceGuard, VolumeDemand,
};
use larm_domain::units::DecimalU64;
use serde_json::json;
use sqlx::SqlitePool;

const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn ctx(actor: &str) -> MutationContext {
    MutationContext {
        actor_id: actor.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn request() -> AdapterImport {
    AdapterImport {
        package_path: "C:/private-fixtures/test.zip".into(),
        expected_hash: HASH.into(),
        approval_id: None,
    }
}
fn candidate(version: &str) -> AdapterInstallation {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: "acme.update-fixture".into(),
        version: version.into(),
        family: "runtime".into(),
        package_hash: HASH.into(),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        manifest: json!({"fixture_only":"journal; not a qualified plugin"}),
        state: AdapterState::Installed,
    }
}
fn plan() -> UpdatePlan {
    UpdatePlan {
        candidate: candidate("2"),
        workspace_path: "C:/private-fixtures/data/adapters/.staging/job".into(),
        destination_path: "C:/private-fixtures/data/adapters/acme.update-fixture/2".into(),
        volume_key: "fixture-volume".into(),
        archive_bytes: DecimalU64::new(1024).unwrap(),
        unpacked_bytes: DecimalU64::new(2048).unwrap(),
        required_bytes: DecimalU64::new(3072 + (1 << 30)).unwrap(),
        approval: None,
    }
}
fn demand(p: &UpdatePlan) -> VolumeDemand {
    VolumeDemand {
        volume_key: p.volume_key.clone(),
        required_bytes: p.required_bytes.get(),
        os_free: 10 << 30,
        policy_minimum: 0,
    }
}
async fn setup() -> (
    tempfile::TempDir,
    larm_db::pool::Db,
    SqlAdapterUpdates,
    AdapterInstallation,
) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let target = candidate("1");
    SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&target, &ctx("setup"))
    .await
    .unwrap();
    let repo = SqlAdapterUpdates {
        pool: db.pool.clone(),
    };
    (dir, db, repo, target)
}
async fn snapshot(pool: &SqlitePool) -> serde_json::Value {
    // Oráculo SQL independiente de los getters del repositorio: incluye
    // payloads/revisiones para detectar escrituras parciales, no sólo filas.
    let mut result = serde_json::Map::new();
    for (key, query) in [
        ("journal","SELECT json_group_array(json_object('id',id,'phase',phase,'payload',payload,'hash',plan_hash)) FROM (SELECT * FROM operation_journal ORDER BY id)"),
        ("idempotency","SELECT json_group_array(json_object('principal',principal_id,'key',key,'response',response_json)) FROM (SELECT * FROM idempotency ORDER BY principal_id,operation,key)"),
        ("adapters","SELECT json_group_array(json_object('id',id,'state',state,'revision',row_version)) FROM (SELECT * FROM adapter_installation ORDER BY id)"),
        ("events","SELECT json_group_array(json_object('id',event_id,'payload',payload)) FROM (SELECT * FROM event ORDER BY event_id)"),
        ("outbox","SELECT json_group_array(event_id) FROM (SELECT * FROM event_outbox ORDER BY event_id)"),
        ("reservations","SELECT json_group_array(json_object('job',job_id,'volume',volume_key,'total',total_bytes,'remaining',remaining_bytes)) FROM (SELECT * FROM volume_reservation ORDER BY id)"),
        ("locks","SELECT json_group_array(json_object('owner',owner_id,'resource',resource_id,'generation',generation,'mode',mode)) FROM (SELECT * FROM resource_lock ORDER BY resource_id,owner_id,generation)"),
    ] {
        let raw: String = sqlx::query_scalar(query).fetch_one(pool).await.unwrap();
        result.insert(key.into(), serde_json::from_str(&raw).unwrap());
    }
    serde_json::Value::Object(result)
}
async fn queued(repo: &SqlAdapterUpdates, target: &AdapterInstallation, key: &str) -> String {
    repo.enqueue_update(&target.id, &request(), HASH, key, &ctx("actor"))
        .await
        .unwrap()
        .0
        .job_id
}
async fn prepared(
    repo: &SqlAdapterUpdates,
    target: &AdapterInstallation,
    p: &UpdatePlan,
    key: &str,
) -> String {
    let id = queued(repo, target, key).await;
    repo.advance_update(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    repo.prepare_update(&id, p, &demand(p), chrono::Utc::now())
        .await
        .unwrap();
    id
}
async fn publishing(repo: &SqlAdapterUpdates, id: &str) {
    for (from, to) in [
        (Step::Reserved, Step::Extracting),
        (Step::Extracting, Step::Staged),
        (Step::Staged, Step::Publishing),
    ] {
        repo.advance_update(id, from, to).await.unwrap();
    }
}
async fn trigger(pool: &SqlitePool, target: &str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER inject_update_fault BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'P024 fixture failure'); END;")))
        .execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}
async fn untrigger(pool: &SqlitePool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql("DROP TRIGGER inject_update_fault;")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn aceptacion_concurrente_idempotente_aisla_actor_destino_y_cuerpo() {
    let (_dir, db, repo, target) = setup().await;
    let (req, ca, cb) = (request(), ctx("actor"), ctx("actor"));
    let (a, b) = tokio::join!(
        repo.enqueue_update(&target.id, &req, HASH, "same-key", &ca),
        repo.enqueue_update(&target.id, &req, HASH, "same-key", &cb)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.0, b.0);
    assert_ne!(a.1, b.1);
    let before = snapshot(&db.pool).await;
    assert_eq!(before["journal"].as_array().unwrap().len(), 1);
    assert_eq!(before["idempotency"].as_array().unwrap().len(), 1);
    let mut other_request = request();
    other_request.expected_hash = "b".repeat(64);
    assert_eq!(
        repo.enqueue_update(&target.id, &other_request, HASH, "same-key", &ctx("actor"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        repo.enqueue_update(&target.id, &req, &"b".repeat(64), "same-key", &ctx("actor"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        repo.enqueue_update(
            &uuid::Uuid::now_v7().to_string(),
            &req,
            HASH,
            "same-key",
            &ctx("actor")
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(snapshot(&db.pool).await, before);
    let other_actor = repo
        .enqueue_update(&target.id, &req, HASH, "same-key", &ctx("other"))
        .await
        .unwrap();
    assert!(other_actor.1);
    assert_ne!(other_actor.0.job_id, a.0.job_id);
}

#[tokio::test]
async fn rechazo_y_fallo_de_aceptacion_no_dejan_job_ni_replay_parcial() {
    let (_dir, db, repo, target) = setup().await;
    let before = snapshot(&db.pool).await;
    for table in ["operation_journal", "idempotency", "event_outbox"] {
        trigger(&db.pool, table).await;
        assert!(repo
            .enqueue_update(&target.id, &request(), HASH, "key", &ctx("actor"))
            .await
            .is_err());
        assert_eq!(snapshot(&db.pool).await, before);
        untrigger(&db.pool).await;
    }
    for key in ["".to_string(), "x".repeat(241)] {
        assert_eq!(
            repo.enqueue_update(&target.id, &request(), HASH, &key, &ctx("actor"))
                .await
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );
    }
    sqlx::query("UPDATE adapter_installation SET state='RETIRED' WHERE id=?")
        .bind(&target.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.enqueue_update(&target.id, &request(), HASH, "key", &ctx("actor"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidStateTransition
    );
    assert_eq!(snapshot(&db.pool).await, before);
}

#[tokio::test]
async fn reserva_locks_plan_y_outbox_se_confirman_o_revierten_juntos() {
    let (_dir, db, repo, target) = setup().await;
    let id = queued(&repo, &target, "key").await;
    repo.advance_update(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let p = plan();
    let before = snapshot(&db.pool).await;
    let mut short = demand(&p);
    short.os_free = p.required_bytes.get() - 1;
    assert_eq!(
        repo.prepare_update(&id, &p, &short, chrono::Utc::now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageInsufficient
    );
    assert_eq!(snapshot(&db.pool).await, before);
    assert_eq!(
        repo.prepare_update(
            &id,
            &p,
            &demand(&p),
            chrono::Utc::now() - chrono::Duration::seconds(6)
        )
        .await
        .unwrap_err()
        .details
        .reason,
        "free_snapshot_stale"
    );
    assert_eq!(snapshot(&db.pool).await, before);
    trigger(&db.pool, "event_outbox").await;
    assert!(repo
        .prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
        .await
        .is_err());
    assert_eq!(snapshot(&db.pool).await, before);
    untrigger(&db.pool).await;
    repo.prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
        .await
        .unwrap();
    let observed = snapshot(&db.pool).await;
    assert_eq!(observed["locks"].as_array().unwrap().len(), 2);
    assert_eq!(observed["reservations"].as_array().unwrap().len(), 1);
    assert_eq!(observed["reservations"][0]["total"], p.required_bytes.get());
    assert_eq!(repo.update_job(&id).await.unwrap().unwrap().plan, Some(p));
}

#[tokio::test]
async fn conflictos_de_lock_plan_invalido_y_target_stale_conservan_estado() {
    let (_dir, db, repo, target) = setup().await;
    let id = queued(&repo, &target, "key").await;
    repo.advance_update(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let p = plan();
    let mut invalid_plan = p.clone();
    invalid_plan.candidate.state = AdapterState::Validated;
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.prepare_update(&id, &invalid_plan, &demand(&p), chrono::Utc::now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ValidationFailed
    );
    assert_eq!(snapshot(&db.pool).await, before);
    let guard = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    guard.acquire("other", 1, &p.locks(), None).await.unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(snapshot(&db.pool).await, before);
    guard.release("other", 1, None).await.unwrap();
    sqlx::query("UPDATE adapter_installation SET row_version=row_version+1 WHERE id=?")
        .bind(&target.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    assert_eq!(snapshot(&db.pool).await, before);
}

#[tokio::test]
async fn aprobacion_consumida_con_la_reserva_y_reuso_rechazado_sin_locks_huerfanos() {
    let (_dir, db, repo, target) = setup().await;
    let approval_id = uuid::Uuid::now_v7().to_string();
    let approval_request = larm_domain::approval::ApprovalRequest {
        operation_digest: HASH.into(),
        resource_ids: vec!["adapter:fixture".into()],
        code_hash: Some(HASH.into()),
        permissions: serde_json::Map::new(),
        approved: true,
    };
    let record = approval_request
        .grant(
            &approval_id,
            "actor",
            chrono::Utc::now(),
            chrono::Duration::seconds(900),
        )
        .unwrap();
    let approvals = larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    };
    approvals.insert(&record, &ctx("actor")).await.unwrap();
    let mut req = request();
    req.approval_id = Some(approval_id.clone());
    let id = repo
        .enqueue_update(&target.id, &req, HASH, "key", &ctx("actor"))
        .await
        .unwrap()
        .0
        .job_id;
    repo.advance_update(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let mut p = plan();
    p.approval = Some(UpdateApproval {
        id: approval_id.clone(),
        operation_digest: HASH.into(),
        resource_ids: vec!["adapter:fixture".into()],
        code_hash: HASH.into(),
    });
    let before = snapshot(&db.pool).await;
    trigger(&db.pool, "event_outbox").await;
    assert!(repo
        .prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
        .await
        .is_err());
    assert_eq!(snapshot(&db.pool).await, before);
    assert_eq!(
        approvals
            .get(&approval_id)
            .await
            .unwrap()
            .unwrap()
            .consumed_at,
        None
    );
    untrigger(&db.pool).await;
    repo.prepare_update(&id, &p, &demand(&p), chrono::Utc::now())
        .await
        .unwrap();
    assert!(approvals
        .get(&approval_id)
        .await
        .unwrap()
        .unwrap()
        .consumed_at
        .is_some());
    repo.fail_update(
        &id,
        &LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Fixture terminada",
            "operation_cancelled",
        ),
    )
    .await
    .unwrap();
    let other = repo
        .enqueue_update(&target.id, &req, HASH, "key2", &ctx("actor"))
        .await
        .unwrap()
        .0
        .job_id;
    repo.advance_update(&other, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.prepare_update(&other, &p, &demand(&p), chrono::Utc::now())
            .await
            .unwrap_err()
            .details
            .reason,
        "approval_consumed"
    );
    assert_eq!(snapshot(&db.pool).await, before);
}

#[tokio::test]
async fn completar_es_atomico_fenced_idempotente_y_nunca_promueve() {
    let (_dir, db, repo, target) = setup().await;
    let p = plan();
    let id = prepared(&repo, &target, &p, "key").await;
    let before = snapshot(&db.pool).await;
    assert!(repo
        .advance_update(&id, Step::Reserved, Step::Publishing)
        .await
        .is_err());
    assert!(repo.complete_update(&id).await.is_err());
    assert_eq!(snapshot(&db.pool).await, before);
    publishing(&repo, &id).await;
    let before = snapshot(&db.pool).await;
    trigger(&db.pool, "event_outbox").await;
    assert!(repo.complete_update(&id).await.is_err());
    assert_eq!(snapshot(&db.pool).await, before);
    untrigger(&db.pool).await;
    sqlx::query("DELETE FROM resource_lock WHERE owner_id=?")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.complete_update(&id).await.unwrap_err().details.reason,
        "fence_lost"
    );
    assert_eq!(snapshot(&db.pool).await, before);
    SqlResourceGuard {
        pool: db.pool.clone(),
    }
    .acquire(&id, 1, &p.locks(), None)
    .await
    .unwrap();
    repo.complete_update(&id).await.unwrap();
    let after = snapshot(&db.pool).await;
    assert_eq!(after["locks"], json!([]));
    assert_eq!(after["reservations"], json!([]));
    let installed = SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .get(&p.candidate.id)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(installed, p.candidate);
    let selections: i64 = sqlx::query_scalar("SELECT count(*) FROM adapter_selection")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(selections, 0);
    repo.complete_update(&id).await.unwrap();
    assert_eq!(snapshot(&db.pool).await, after);
    let (receipt, new) = repo
        .enqueue_update(&target.id, &request(), HASH, "key", &ctx("actor"))
        .await
        .unwrap();
    assert!(!new);
    assert_eq!(receipt.state, JobPhase::Queued);
    assert_eq!(receipt.job_id, id);
    assert_eq!(
        repo.update_job(&id)
            .await
            .unwrap()
            .unwrap()
            .public_state()
            .state,
        JobPhase::Completed
    );
    assert!(repo
        .fail_update(
            &id,
            &LarmError::new(ErrorCode::Internal, "fixture", "fixture")
        )
        .await
        .is_err());
}

#[tokio::test]
async fn fallo_terminal_revierte_si_outbox_falla_y_libera_solo_los_recursos_del_job() {
    let (_dir, db, repo, target) = setup().await;
    let p = plan();
    let id = prepared(&repo, &target, &p, "key").await;
    let mut other = plan();
    other.candidate.version = "3".into();
    let other_id = prepared(&repo, &target, &other, "other-key").await;
    let error = LarmError::new(ErrorCode::Timeout, "Fixture IO detenida", "package_timeout");
    let before = snapshot(&db.pool).await;
    trigger(&db.pool, "event_outbox").await;
    assert!(repo.fail_update(&id, &error).await.is_err());
    assert_eq!(snapshot(&db.pool).await, before);
    untrigger(&db.pool).await;
    repo.fail_update(&id, &error).await.unwrap();
    let after = snapshot(&db.pool).await;
    assert_eq!(after["locks"].as_array().unwrap().len(), 2);
    assert_eq!(after["reservations"].as_array().unwrap().len(), 1);
    assert_eq!(after["reservations"][0]["job"], other_id);
    let job = repo.update_job(&id).await.unwrap().unwrap();
    assert_eq!(job.step, Step::Failed);
    assert_eq!(job.error.unwrap().code, ErrorCode::Timeout);
    repo.fail_update(&id, &error).await.unwrap();
    assert_eq!(snapshot(&db.pool).await, after);
}

#[tokio::test]
async fn reinicio_replay_y_visibilidad_no_caducan_io_ni_filtran_paths() {
    let (dir, db, repo, target) = setup().await;
    let p = plan();
    let id = prepared(&repo, &target, &p, "key").await;
    repo.advance_update(&id, Step::Reserved, Step::Extracting)
        .await
        .unwrap();
    let queued_id = queued(&repo, &target, "queued").await;
    drop(repo);
    db.pool.close().await;
    let db = larm_db::pool::Db::open(&dir.path().join("larm.sqlite"))
        .await
        .unwrap();
    let repo = SqlAdapterUpdates {
        pool: db.pool.clone(),
    };
    let pending = repo.pending_updates().await.unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].job_id, id);
    assert_eq!(pending[1].job_id, queued_id);
    assert_eq!(pending[0].step, Step::Extracting);
    assert_eq!(pending[0].plan, Some(p));
    let before = snapshot(&db.pool).await;
    let principal = |id: &str, scope| {
        Principal::new(id, "fixture", PrincipalKind::Operator, [scope], []).unwrap()
    };
    assert!(repo
        .job(&principal("other", ApiScope::Read), &id)
        .await
        .unwrap()
        .is_none());
    let visible = repo
        .job(&principal("actor", ApiScope::Read), &id)
        .await
        .unwrap()
        .unwrap();
    assert!(repo
        .job(&principal("admin", ApiScope::Admin), &id)
        .await
        .unwrap()
        .is_some());
    let public = serde_json::to_string(&visible).unwrap();
    assert!(
        !public.contains("private-fixtures")
            && !public.contains("workspace_path")
            && !public.contains("package_path")
    );
    assert_eq!(snapshot(&db.pool).await, before);
    assert!(
        !repo
            .enqueue_update(&target.id, &request(), HASH, "key", &ctx("actor"))
            .await
            .unwrap()
            .1
    );
    assert_eq!(snapshot(&db.pool).await, before);
    db.pool.close().await;
}

#[tokio::test]
async fn journal_alterado_y_reserva_perdida_se_rechazan_sin_finalizar() {
    let (_dir, db, repo, target) = setup().await;
    let p = plan();
    let id = prepared(&repo, &target, &p, "key").await;
    publishing(&repo, &id).await;
    sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.complete_update(&id).await.unwrap_err().details.reason,
        "adapter_update_reservation_missing"
    );
    assert_eq!(snapshot(&db.pool).await, before);
    sqlx::query("UPDATE operation_journal SET payload=json_set(payload,'$.request.expected_hash',?) WHERE id=? AND kind=?")
        .bind("b".repeat(64)).bind(&id).bind(UPDATE_KIND).execute(&db.pool).await.unwrap();
    let before = snapshot(&db.pool).await;
    assert_eq!(
        repo.update_job(&id).await.unwrap_err().code,
        ErrorCode::IntegrityFailed
    );
    assert_eq!(
        repo.fail_update(
            &id,
            &LarmError::new(ErrorCode::Internal, "fixture", "fixture")
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IntegrityFailed
    );
    assert_eq!(snapshot(&db.pool).await, before);
}

#[tokio::test]
async fn replay_retenido_30_dias_y_caducado_inicia_otro_job_sin_borrar_historia() {
    let (_dir, db, repo, target) = setup().await;
    let id = queued(&repo, &target, "key").await;
    let expiry: String = sqlx::query_scalar("SELECT expires_at FROM idempotency WHERE operation=?")
        .bind(UPDATE_OPERATION)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let expiry = chrono::DateTime::parse_from_rfc3339(&expiry)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!((expiry - chrono::Utc::now()).num_seconds() >= 30 * 86400 - 5);
    sqlx::query("UPDATE idempotency SET expires_at='2000-01-01T00:00:00.000Z' WHERE operation=?")
        .bind(UPDATE_OPERATION)
        .execute(&db.pool)
        .await
        .unwrap();
    let other = queued(&repo, &target, "key").await;
    assert_ne!(id, other);
    assert_eq!(repo.pending_updates().await.unwrap().len(), 2);
    let idem: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency WHERE operation=?")
        .bind(UPDATE_OPERATION)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(idem, 1);
}
