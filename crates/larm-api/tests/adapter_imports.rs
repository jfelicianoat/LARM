//! P028 HTTP: importación con autorización, DTO cerrado y receipt durable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::adapter_updates::AdapterImport;
use larm_domain::auth::{ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::ports::{AdapterImporter, JobSource, MutationContext, PrincipalAuthenticator};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
struct Accepted {
    request: AdapterImport,
    hash: String,
    key: String,
    ctx: MutationContext,
    receipt: JobReceipt,
}

#[derive(Default)]
struct Service {
    calls: Mutex<Vec<Accepted>>,
}

#[async_trait]
impl AdapterImporter for Service {
    async fn request_import(
        self: Arc<Self>,
        request: &AdapterImport,
        hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        let receipt = JobReceipt {
            job_id: uuid::Uuid::now_v7().to_string(),
            status_url: String::new(),
            state: JobPhase::Queued,
        };
        let receipt = JobReceipt {
            status_url: format!("/api/v1/jobs/{}", receipt.job_id),
            ..receipt
        };
        self.calls.lock().unwrap().push(Accepted {
            request: request.clone(),
            hash: hash.into(),
            key: key.into(),
            ctx: ctx.clone(),
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }
}

#[async_trait]
impl JobSource for Service {
    async fn job(&self, _principal: &Principal, _id: &str) -> Result<Option<JobState>, ApiError> {
        Ok(None)
    }
}

struct Api {
    router: axum::Router,
    service: Arc<Service>,
    writer: String,
    reader: String,
    actor: String,
}

async fn setup() -> Api {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let mut enrolled = Vec::new();
    for (name, scopes) in [
        ("writer", vec![ApiScope::Read, ApiScope::AdaptersWrite]),
        ("reader", vec![ApiScope::Read]),
    ] {
        let (principal, token) = principals
            .enroll(
                PrincipalKind::Operator,
                name,
                scopes,
                Vec::<McpPermission>::new(),
                None,
                &MutationContext {
                    actor_id: "fixture".into(),
                    correlation_id: uuid::Uuid::now_v7().to_string(),
                },
            )
            .await
            .unwrap();
        enrolled.push((principal.id, token));
    }
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let service = Arc::new(Service::default());
    let router = larm_api::adapters::import_routes(service.clone()).layer(
        axum::middleware::from_fn_with_state(auth, larm_api::security::guard),
    );
    Api {
        router,
        service,
        actor: enrolled[0].0.clone(),
        writer: enrolled[0].1.clone(),
        reader: enrolled[1].1.clone(),
    }
}

const CID: &str = "0192aaaa-0000-7000-8000-000000000077";

async fn call(
    api: &Api,
    token: Option<&str>,
    body: &str,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/adapters/import")
        .header("x-correlation-id", CID)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let response = api
        .router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn body() -> Value {
    json!({"package_path":"C:/fixture/package.zip","expected_hash":"a".repeat(64),"approval_id":null})
}

#[tokio::test]
async fn import_exige_scope_schema_y_idempotency_y_delega_hash_exacto() {
    let api = setup().await;
    let raw = format!("\n {} \n", body());
    assert_eq!(
        call(&api, None, &raw, Some("key")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&api, Some(&api.reader), "not-json", Some("key"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let mut missing_nullable = body();
    missing_nullable
        .as_object_mut()
        .unwrap()
        .remove("approval_id");
    assert_ne!(
        call(
            &api,
            Some(&api.writer),
            &missing_nullable.to_string(),
            Some("key")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let mut extra = body();
    extra["trust"] = json!("VERIFIED");
    assert_ne!(
        call(&api, Some(&api.writer), &extra.to_string(), Some("key"))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_ne!(
        call(&api, Some(&api.writer), &raw, None).await.0,
        StatusCode::ACCEPTED
    );
    assert!(api.service.calls.lock().unwrap().is_empty());

    let (status, value) = call(&api, Some(&api.writer), &raw, Some("import-key")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    let receipt: JobReceipt = serde_json::from_value(value).unwrap();
    let recorded = api.service.calls.lock().unwrap()[0].clone();
    assert_eq!(serde_json::to_value(recorded.request).unwrap(), body());
    assert_eq!(recorded.hash, hex::encode(Sha256::digest(raw.as_bytes())));
    assert_eq!(recorded.key, "import-key");
    assert_eq!(recorded.ctx.actor_id, api.actor);
    assert_eq!(recorded.ctx.correlation_id, CID);
    assert_eq!(receipt, recorded.receipt);
}
