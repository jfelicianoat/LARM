//! `adapter_installation` (P023) sobre SQLite real con las migraciones del
//! contrato: alta, lectura, búsqueda por versión, CHECKs y evento
//! `adapter.updated` en la misma transacción.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::adapters::SqlAdapterInstallations;
use larm_domain::adapters::{AdapterFilter, AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::ports::{
    AdapterCatalogRepository, AdapterInstallationRepository, MutationContext,
};
use serde_json::json;

fn installation(id: &str, version: &str) -> AdapterInstallation {
    AdapterInstallation {
        id: id.into(),
        created_at: "2026-10-15T10:00:00.000Z".into(),
        updated_at: "2026-10-15T10:00:00.000Z".into(),
        row_version: 1,
        metadata: json!({"kind": "DECLARATIVE", "signature_state": "CUSTOM"})
            .as_object()
            .cloned()
            .unwrap(),
        adapter_id: "acme.fake-runtime".into(),
        version: version.into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "operador local".into(),
        trust: AdapterTrust::Untrusted,
        manifest: json!({"id": "acme.fake-runtime", "version": version}),
        state: AdapterState::Installed,
    }
}

#[tokio::test]
async fn paginas_estables_y_filtros_sobre_sqlite_real() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterInstallations {
        pool: db.pool.clone(),
    };
    let ctx = MutationContext {
        actor_id: "test".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let a = installation(&uuid::Uuid::now_v7().to_string(), "1.0");
    let mut b = installation(&uuid::Uuid::now_v7().to_string(), "2.0");
    b.state = AdapterState::Failed;
    repo.insert(&a, &ctx).await.unwrap();
    repo.insert(&b, &ctx).await.unwrap();
    let page = repo.page(None, 1, &AdapterFilter::default()).await.unwrap();
    assert_eq!(page, vec![a.clone()]);
    let next_page = repo
        .page(
            Some((a.created_at.clone(), a.id.clone())),
            2,
            &AdapterFilter::default(),
        )
        .await
        .unwrap();
    assert_eq!(next_page.len(), 2);
    assert_eq!(next_page[0], b.clone());
    assert_eq!(next_page[1].adapter_id, "larm.builtin.local-folder");
    assert_eq!(
        repo.page(
            None,
            20,
            &AdapterFilter {
                q: Some("2.0".into()),
                state: Some(AdapterState::Failed)
            }
        )
        .await
        .unwrap(),
        vec![b]
    );
    assert!(repo
        .page(
            None,
            20,
            &AdapterFilter {
                q: Some("%".into()),
                state: None
            }
        )
        .await
        .unwrap()
        .is_empty());
    assert!(repo
        .page(
            None,
            20,
            &AdapterFilter {
                q: Some("' OR 1=1 --".into()),
                state: None
            }
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn alta_lectura_y_evento_en_la_misma_transaccion() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterInstallations {
        pool: db.pool.clone(),
    };
    let ctx = MutationContext {
        actor_id: "user:operador".into(),
        correlation_id: "0192aaaa-0000-7000-8000-000000000001".into(),
    };
    let a = installation("0192aaaa-0000-7000-8000-0000000000a1", "1.0.0");
    repo.insert(&a, &ctx).await.unwrap();
    assert_eq!(repo.get(&a.id).await.unwrap(), Some(a.clone()));
    assert_eq!(
        repo.find("acme.fake-runtime", "1.0.0").await.unwrap(),
        Some(a.clone())
    );
    assert_eq!(repo.find("acme.fake-runtime", "2.0.0").await.unwrap(), None);
    let (event_type, resource, data): (String, String, String) = sqlx::query_as(
        "SELECT event_type, resource_id, payload FROM event WHERE resource_type = 'adapter_installation'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (event_type.as_str(), resource.as_str()),
        ("adapter.updated", a.id.as_str())
    );
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(data["action"], "created");
    assert_eq!(data["trust"], "UNTRUSTED");

    // Un CHECK roto anula también el evento.
    let mut bad = installation("0192aaaa-0000-7000-8000-0000000000a2", "3.0.0");
    bad.family = "plugin".into();
    assert!(repo.insert(&bad, &ctx).await.is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id = ?")
        .bind(&bad.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(repo.get(&bad.id).await.unwrap(), None);
}
