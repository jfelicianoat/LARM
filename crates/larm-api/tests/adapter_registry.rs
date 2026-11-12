#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use larm_api::adapters::{registry_routes, AdapterRegistryState};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::adapter_registry::{
    AdapterRegistryManager, RegistryCheckpoint, RegistryStatus, RegistrySyncReceipt,
};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::ports::{MutationContext, PrincipalAuthenticator};
use serde_json::Value;
use tower::ServiceExt;

#[derive(Default)]
struct RegistryManager {
    status_calls: AtomicUsize,
    sync_calls: AtomicUsize,
}

#[async_trait]
impl AdapterRegistryManager for RegistryManager {
    async fn status(&self) -> Result<RegistryStatus, ApiError> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        Ok(RegistryStatus {
            enabled: true,
            registry_url: Some("https://registry.example.test/index.json".into()),
            checkpoint: Some(RegistryCheckpoint {
                registry_id: "registry-test".into(),
                registry_url: "https://registry.example.test/index.json".into(),
                sequence: 7,
                payload_sha256: "a".repeat(64),
                issued_at: "2026-10-24T10:00:00Z".into(),
                expires_at: "2026-11-24T10:00:00Z".into(),
                accepted_at: "2026-10-24T10:05:00Z".into(),
                envelope: serde_json::json!({"payload":{"packages":[]}}),
            }),
            revocations: vec![],
        })
    }

    async fn sync(&self, _ctx: &MutationContext) -> Result<RegistrySyncReceipt, ApiError> {
        let sequence = self.sync_calls.fetch_add(1, Ordering::SeqCst) + 8;
        Ok(RegistrySyncReceipt {
            registry_id: "registry-test".into(),
            sequence: sequence as u64,
            payload_sha256: "b".repeat(64),
            accepted_at: "2026-10-24T10:10:00Z".into(),
            expires_at: "2026-11-24T10:00:00Z".into(),
            package_count: 3,
            revocation_count: 2,
            replay: false,
        })
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    manager: Arc<RegistryManager>,
}

async fn setup() -> Api {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "test-operator".into(),
        correlation_id: "test-correlation".into(),
    };
    let mut tokens = Vec::new();
    for (name, scopes) in [
        ("reader", vec![ApiScope::Read]),
        ("writer", vec![ApiScope::Read, ApiScope::AdaptersWrite]),
    ] {
        let (_, token) = principals
            .enroll(PrincipalKind::Operator, name, scopes, vec![], None, &ctx)
            .await
            .unwrap();
        tokens.push(token);
    }

    let manager = Arc::new(RegistryManager::default());
    let manager_port: Arc<dyn AdapterRegistryManager> = manager.clone();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let app = registry_routes(AdapterRegistryState {
        manager: manager_port,
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        writer: tokens.pop().unwrap(),
        reader: tokens.pop().unwrap(),
        manager,
    }
}

async fn send(api: &Api, request: Request<Body>) -> (StatusCode, Value) {
    let response = api.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn registry_routes_enforce_scopes_closed_body_and_idempotent_sync() {
    let api = setup().await;

    let (status, body) = send(
        &api,
        Request::get("/api/v1/adapters/registry")
            .header("Authorization", format!("Bearer {}", api.reader))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["checkpoint"]["sequence"], 7);
    assert_eq!(api.manager.status_calls.load(Ordering::SeqCst), 1);

    let (status, _) = send(
        &api,
        Request::post("/api/v1/adapters/registry/sync")
            .header("Authorization", format!("Bearer {}", api.reader))
            .header("Idempotency-Key", "registry-sync-reader")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(api.manager.sync_calls.load(Ordering::SeqCst), 0);

    let (status, _) = send(
        &api,
        Request::post("/api/v1/adapters/registry/sync")
            .header("Authorization", format!("Bearer {}", api.writer))
            .header("Idempotency-Key", "registry-sync-invalid-body")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"registry_url":"https://attacker.invalid/"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert!(!status.is_success());
    assert_eq!(api.manager.sync_calls.load(Ordering::SeqCst), 0);

    let request = || {
        Request::post("/api/v1/adapters/registry/sync")
            .header("Authorization", format!("Bearer {}", api.writer))
            .header("Idempotency-Key", "registry-sync-once")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    };
    let (first_status, first_body) = send(&api, request()).await;
    let (replay_status, replay_body) = send(&api, request()).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(first_body, replay_body);
    assert_eq!(first_body["sequence"], 8);
    assert_eq!(api.manager.sync_calls.load(Ordering::SeqCst), 1);
}
