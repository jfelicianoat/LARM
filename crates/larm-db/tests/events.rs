#![allow(clippy::unwrap_used, clippy::panic)]

use larm_db::{
    events::{self, NewEvent},
    pool::Db,
    testing::temp_migrated,
};
use larm_domain::ports::MutationContext;

fn context() -> MutationContext {
    MutationContext {
        actor_id: "p029-test".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

async fn append_event(db: &Db, event_type: &str) -> String {
    let mut tx = db.pool.begin().await.unwrap();
    let event_id = events::append(
        &mut tx,
        &NewEvent {
            event_type,
            resource_type: "fixture",
            resource_id: "p029-outbox",
            resource_version: 1,
            data: serde_json::json!({"action": event_type}),
        },
        &context(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    event_id
}

async fn expire_event(db: &Db, event_id: &str) {
    sqlx::query("UPDATE event SET occurred_at='2020-01-01T00:00:00.000Z' WHERE event_id=?")
        .bind(event_id.parse::<i64>().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn outbox_es_atomico_con_evento_y_ack_es_idempotente() {
    let (_directory, db) = temp_migrated().await.unwrap();

    let mut rolled_back = db.pool.begin().await.unwrap();
    events::append(
        &mut rolled_back,
        &NewEvent {
            event_type: "fixture.rolled_back",
            resource_type: "fixture",
            resource_id: "p029-outbox",
            resource_version: 1,
            data: serde_json::json!({}),
        },
        &context(),
    )
    .await
    .unwrap();
    rolled_back.rollback().await.unwrap();
    assert!(events::pending(&db.pool, 10).await.unwrap().is_empty());

    let event_id = append_event(&db, "fixture.committed").await;
    let pending = events::pending(&db.pool, 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].event_id.to_string(), event_id);
    assert_eq!(pending[0].event_type, "fixture.committed");

    let numeric_id = event_id.parse::<i64>().unwrap();
    events::mark_dispatched(&db.pool, &[numeric_id])
        .await
        .unwrap();
    events::mark_dispatched(&db.pool, &[numeric_id])
        .await
        .unwrap();
    assert!(events::pending(&db.pool, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn outbox_no_confirmado_sobrevive_al_reinicio_de_sqlite() {
    let (directory, db) = temp_migrated().await.unwrap();
    let event_id = append_event(&db, "fixture.restart_replay").await;
    let database_path = directory.path().join("larm.sqlite");
    db.pool.close().await;

    let restarted = Db::open(&database_path).await.unwrap();
    larm_db::migrate::migrate(&restarted.pool).await.unwrap();
    let replay = events::pending(&restarted.pool, 10).await.unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].event_id.to_string(), event_id);
}

#[tokio::test]
async fn batch_y_limite_de_dispatch_rechazan_entradas_invalidas() {
    let (_directory, db) = temp_migrated().await.unwrap();
    assert_eq!(
        events::pending(&db.pool, 0)
            .await
            .unwrap_err()
            .details
            .reason,
        "event_dispatch_batch_invalid"
    );
    assert!(events::mark_dispatched(&db.pool, &[-1]).await.is_err());
    assert!(events::mark_dispatched(&db.pool, &[1, 1]).await.is_err());
}

#[tokio::test]
async fn replay_devuelve_eventos_ordenados_y_high_watermark_consistente() {
    let (_directory, db) = temp_migrated().await.unwrap();
    let first = append_event(&db, "fixture.first")
        .await
        .parse::<i64>()
        .unwrap();
    let second = append_event(&db, "fixture.second")
        .await
        .parse::<i64>()
        .unwrap();

    let replay = events::replay_window(&db.pool, Some(0), 30, 10)
        .await
        .unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(replay.through_event_id, second);
}

#[tokio::test]
async fn replay_rechaza_cursor_expirado_y_backlog_mayor_al_limite() {
    let (_directory, db) = temp_migrated().await.unwrap();
    let expired_id = append_event(&db, "fixture.expired").await;
    expire_event(&db, &expired_id).await;
    let expired = events::replay_window(&db.pool, Some(0), 30, 10)
        .await
        .unwrap_err();
    assert_eq!(expired.code.as_str(), "EVENT_CURSOR_EXPIRED");
    assert_eq!(expired.http_status(), 410);

    let (_directory, recent_db) = temp_migrated().await.unwrap();
    for i in 0..3 {
        append_event(&recent_db, &format!("fixture.{i}")).await;
    }
    let too_many = events::replay_window(&recent_db.pool, Some(0), 30, 2)
        .await
        .unwrap_err();
    assert_eq!(too_many.code.as_str(), "EVENT_CURSOR_EXPIRED");
    assert_eq!(too_many.details.reason, "event_replay_limit_exceeded");
}

#[tokio::test]
async fn poda_retira_eventos_entregados_y_conserva_outbox_pendiente() {
    let (_directory, db) = temp_migrated().await.unwrap();
    let delivered_id = append_event(&db, "fixture.delivered").await;
    let pending_id = append_event(&db, "fixture.pending").await;
    events::mark_dispatched(&db.pool, &[delivered_id.parse().unwrap()])
        .await
        .unwrap();
    expire_event(&db, &delivered_id).await;
    expire_event(&db, &pending_id).await;

    assert_eq!(events::prune_expired(&db.pool, 30).await.unwrap(), 1);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM event")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    let pending = events::pending(&db.pool, 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].event_id.to_string(), pending_id);
    let expired = events::replay_window(&db.pool, Some(0), 30, 10)
        .await
        .unwrap_err();
    assert_eq!(expired.code.as_str(), "EVENT_CURSOR_EXPIRED");
}

#[tokio::test]
async fn secuencia_durable_expira_cursor_aunque_la_poda_deje_event_vacia() {
    let (_directory, db) = temp_migrated().await.unwrap();
    let event_id = append_event(&db, "fixture.pruned").await;
    let numeric_id = event_id.parse::<i64>().unwrap();
    events::mark_dispatched(&db.pool, &[numeric_id])
        .await
        .unwrap();
    expire_event(&db, &event_id).await;
    assert_eq!(events::prune_expired(&db.pool, 30).await.unwrap(), 1);

    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM event")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(events::last_event_id(&db.pool).await.unwrap(), numeric_id);
    let error = events::replay_window(&db.pool, Some(0), 30, 10)
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "EVENT_CURSOR_EXPIRED");
}
