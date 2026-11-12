//! Repositorios de seguridad (P009) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::security::{SqlApprovals, SqlCredentials, SqlIdempotency};
use larm_domain::approval::ApprovalRequest;
use larm_domain::credential::{CredentialReference, SecretType};
use larm_domain::error::ErrorCode;
use larm_domain::ports::{
    ApprovalRepository, CredentialRepository, IdempotencyStore, MutationContext, StoredResponse,
};

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn reference(service: &str, username: &str) -> CredentialReference {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    CredentialReference {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        service: service.into(),
        username: username.into(),
        backend: "WINDOWS_CREDENTIAL_MANAGER".into(),
        secret_type: SecretType::Token,
        scopes: vec!["read".into()],
        expires_at: None,
    }
}

async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn cada_mutacion_escribe_su_evento_en_el_outbox_en_la_misma_transaccion() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlCredentials {
        pool: db.pool.clone(),
    };
    let c = reference("larm/source/hf", "acc");
    let context = ctx();
    let first = repo.insert(&c, &context).await.unwrap();
    let touched = repo.touch(&c.id, &context).await.unwrap();
    assert_eq!(touched.row_version, 2);
    let third = repo.delete(&c.id, 2, &context).await.unwrap();
    assert!(
        third.parse::<i64>().unwrap() > first.parse::<i64>().unwrap(),
        "event_id monotónico"
    );
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT e.event_id, e.event_type, e.actor_id, e.correlation_id, e.resource_id FROM event e \
         JOIN event_outbox o ON o.event_record_id = e.id AND o.event_id = e.event_id ORDER BY e.event_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let types: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(
        types,
        [
            "credential.created",
            "credential.rotated",
            "credential.deleted"
        ]
    );
    assert!(rows
        .iter()
        .all(|r| r.2 == context.actor_id && r.3 == context.correlation_id && r.4 == c.id));
    // una mutación que falla no deja evento
    let before = count(&db.pool, "event").await;
    assert_eq!(
        repo.delete(&c.id, 3, &context).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(count(&db.pool, "event").await, before);
    assert_eq!(count(&db.pool, "event_outbox").await, before);
}

#[tokio::test]
async fn duplicado_stale_y_listado_ordenado() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlCredentials {
        pool: db.pool.clone(),
    };
    let a = reference("larm/source/hf", "acc");
    repo.insert(&a, &ctx()).await.unwrap();
    let dup = reference("larm/source/hf", "acc");
    assert_eq!(
        repo.insert(&dup, &ctx()).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        count(&db.pool, "event").await,
        1,
        "el duplicado no emitió evento"
    );
    assert_eq!(
        repo.delete(&a.id, 5, &ctx()).await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    for i in 0..4 {
        repo.insert(&reference(&format!("larm/source/s{i}"), "u"), &ctx())
            .await
            .unwrap();
    }
    let page1 = repo.list(None, 3).await.unwrap();
    let last = page1.last().unwrap();
    let page2 = repo
        .list(Some((last.created_at.clone(), last.id.clone())), 3)
        .await
        .unwrap();
    assert_eq!(page1.len() + page2.len(), 5);
    assert!(page2.iter().all(|r| !page1.iter().any(|p| p.id == r.id)));
    let principals = repo.by_service_prefix("larm/source/s").await.unwrap();
    assert_eq!(principals.len(), 4);
    let back = repo.get(&a.id).await.unwrap().unwrap();
    assert_eq!(back, a, "roundtrip exacto de la referencia");
}

#[tokio::test]
async fn aprobacion_se_consume_una_sola_vez_aunque_compitan() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = std::sync::Arc::new(SqlApprovals {
        pool: db.pool.clone(),
    });
    let request: ApprovalRequest = serde_json::from_value(serde_json::json!({
        "operation_digest": "c".repeat(64), "resource_ids": ["v1", "v2"], "code_hash": null,
        "permissions": {"delete": true}, "approved": true}))
    .unwrap();
    let record = request
        .grant(
            uuid::Uuid::now_v7().to_string(),
            "operator",
            chrono::Utc::now(),
            chrono::Duration::seconds(900),
        )
        .unwrap();
    let event_id = repo.insert(&record, &ctx()).await.unwrap();
    assert!(event_id.parse::<u64>().is_ok());
    assert_eq!(repo.get(&record.id).await.unwrap().unwrap(), record);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let (repo, id) = (repo.clone(), record.id.clone());
        handles.push(tokio::spawn(async move {
            repo.consume(&id, "2026-10-11T00:00:00.000Z").await.unwrap()
        }));
    }
    let mut won = 0;
    for h in handles {
        won += usize::from(h.await.unwrap());
    }
    assert_eq!(won, 1, "exactamente un consumo gana");
    let consumed = repo.get(&record.id).await.unwrap().unwrap();
    assert_eq!(
        consumed.consumed_at.as_deref(),
        Some("2026-10-11T00:00:00.000Z")
    );
    assert!(repo.get("no-existe").await.unwrap().is_none());
}

#[tokio::test]
async fn idempotencia_guarda_repite_y_caduca() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let store = SqlIdempotency {
        pool: db.pool.clone(),
    };
    let stored = StoredResponse {
        body_hash: "h".into(),
        status: 201,
        response: serde_json::json!({"id": "x"}),
    };
    let future = larm_domain::bridge::timestamp(chrono::Utc::now() + chrono::Duration::days(30));
    store.save("p", "op", "k", &stored, &future).await.unwrap();
    assert_eq!(
        store.lookup("p", "op", "k").await.unwrap(),
        Some(stored.clone())
    );
    assert!(
        store.lookup("otro", "op", "k").await.unwrap().is_none(),
        "por principal"
    );
    assert!(
        store.lookup("p", "otra-op", "k").await.unwrap().is_none(),
        "por operación"
    );
    let past = larm_domain::bridge::timestamp(chrono::Utc::now() - chrono::Duration::seconds(1));
    store
        .save("p", "op", "viejo", &stored, &past)
        .await
        .unwrap();
    assert!(
        store.lookup("p", "op", "viejo").await.unwrap().is_none(),
        "caducada"
    );
}
