//! Casos de uso sobre SQLite real; instalación inicial es una fixture CODE_PASS.
#![allow(clippy::unwrap_used)]
use larm_domain::{ports::MutationContext, runtime::*, runtime_ports::*};
use larm_runtime::catalog::RuntimeCatalog;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn politica_persistida_filtra_descubrimiento_y_no_el_inventario() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let ctx = MutationContext {
        actor_id: "p030-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let id = uuid::Uuid::now_v7().to_string();
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{\"fixture_only\":true}','fixture.catalog','1.0.0','runtime',?,'fixture','UNTRUSTED',?,'INSTALLED')")
        .bind(&id).bind(&now).bind(&now).bind("a".repeat(64)).bind(json!({"capabilities":["vendor.future"]}).to_string()).execute(&db.pool).await.unwrap();
    let adapter = repo
        .register_runtime_adapter(&id, "fixture.catalog", &ctx)
        .await
        .unwrap();
    let request = RuntimeCreate {
        name: "fixture runtime".into(),
        adapter_id: adapter.id,
        capabilities: vec!["vendor.future".into()],
        default_version_id: None,
        channel_policy: RuntimeChannelPolicy::default(),
        retention_policy: Default::default(),
    };
    let catalog = RuntimeCatalog { repo: repo.clone() };
    let runtime = catalog
        .create(&request, "runtime-catalog-fixture", &"a".repeat(64), &ctx)
        .await
        .unwrap();
    let names = [
        "stable",
        "prerelease",
        "nightly",
        "commit",
        "custom",
        "experimental",
        "unlisted/preview",
    ]
    .map(str::to_owned);
    for channel in &names[..5] {
        let version = RuntimeVersion {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
            row_version: 1,
            metadata: Default::default(),
            runtime_id: runtime.id.clone(),
            version_string: "b7142-local".into(),
            commit: None,
            build: None,
            channel: serde_json::from_value(json!(channel)).unwrap(),
            released_at: None,
            artifact_hash: None,
            promotion: RuntimePromotion::None,
            lifecycle: Default::default(),
        };
        repo.record_version(&version, &ctx).await.unwrap();
    }
    assert_eq!(
        catalog
            .discovery_channels(&runtime.id, &names)
            .await
            .unwrap()
            .iter()
            .map(|c| c.channel)
            .collect::<Vec<_>>(),
        vec![RuntimeChannel::Stable]
    );
    let mut revision = 1;
    for expected in [
        RuntimeChannel::Prerelease,
        RuntimeChannel::Nightly,
        RuntimeChannel::Commit,
        RuntimeChannel::Custom,
    ] {
        let changed = catalog
            .change_policy(
                &runtime.id,
                revision,
                &RuntimeChannelPolicy {
                    allowed_channels: vec![expected],
                },
                &ctx,
            )
            .await
            .unwrap();
        revision = changed.row_version;
        let observed = catalog
            .discovery_channels(&runtime.id, &names)
            .await
            .unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].channel, expected);
        let filter = RuntimeFilter {
            runtime_id: Some(runtime.id.clone()),
            ..Default::default()
        };
        assert_eq!(repo.versions(None, 201, &filter).await.unwrap().len(), 5);
    }
    assert!(catalog
        .discovery_channels(&uuid::Uuid::now_v7().to_string(), &names)
        .await
        .is_err());
}
