//! P045 API: autorización, DTO estricto, digest y aceptación durable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::hardware::Probe;
use larm_domain::ports::{JobSource, MutationContext, PrincipalAuthenticator, SourceProber};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
struct Accepted {
    target: String,
    request: Probe,
    body_hash: String,
    key: String,
    ctx: MutationContext,
    receipt: JobReceipt,
}

#[derive(Default)]
struct Service {
    calls: Mutex<Vec<Accepted>>,
}

#[async_trait]
impl SourceProber for Service {
    async fn request_source_probe(
        self: Arc<Self>,
        target: &str,
        request: &Probe,
        body_hash: &str,
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
            target: target.into(),
            request: request.clone(),
            body_hash: body_hash.into(),
            key: key.into(),
            ctx: ctx.clone(),
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }
}

#[async_trait]
impl JobSource for Service {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, ApiError> {
        Ok(self
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| {
                call.receipt.job_id == id
                    && (call.ctx.actor_id == principal.id
                        || principal.has_api_scope(ApiScope::Admin))
            })
            .map(|call| JobState {
                job_id: id.into(),
                state: JobPhase::Queued,
                resource_type: "model_source".into(),
                resource_id: Some(call.target.clone()),
                progress: serde_json::Map::new(),
                result: None,
                error: None,
                evidence_refs: vec![],
            }))
    }
}

struct Api {
    router: axum::Router,
    service: Arc<Service>,
    tokens: Vec<String>,
    ids: Vec<String>,
}

async fn setup() -> Api {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let mut tokens = vec![];
    let mut ids = vec![];
    for (name, kind, scopes, mcp) in [
        (
            "writer",
            PrincipalKind::Operator,
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![],
        ),
        (
            "reader",
            PrincipalKind::Operator,
            vec![ApiScope::Read],
            vec![],
        ),
        (
            "admin",
            PrincipalKind::Operator,
            vec![ApiScope::Admin],
            vec![],
        ),
        (
            "mcp",
            PrincipalKind::Mcp,
            vec![ApiScope::Admin],
            vec![McpPermission::Admin],
        ),
    ] {
        let (principal, token) = principals
            .enroll(
                kind,
                name,
                scopes,
                mcp,
                None,
                &MutationContext {
                    actor_id: "fixture".into(),
                    correlation_id: uuid::Uuid::now_v7().to_string(),
                },
            )
            .await
            .unwrap();
        ids.push(principal.id);
        tokens.push(token);
    }
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let service = Arc::new(Service::default());
    let router = larm_api::source_probes::routes(service.clone())
        .merge(larm_api::jobs::routes(Arc::new(vec![service.clone()])))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            larm_api::security::guard,
        ));
    Api {
        router,
        service,
        tokens,
        ids,
    }
}

const CID: &str = "0192aaaa-0000-7000-8000-000000000077";

async fn call(
    api: &Api,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
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
    json!({"resource_id":null,"profile_id":null})
}

#[tokio::test]
async fn probe_202_pasa_solo_referencias_hash_y_actor() {
    let api = setup().await;
    let id = uuid::Uuid::now_v7().to_string();
    let raw = format!(" \n{}\n", body());
    let (status, receipt) = call(
        &api,
        "POST",
        &format!("/api/v1/sources/{id}/probe"),
        Some(&api.tokens[0]),
        &raw,
        Some("source-probe-key"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let calls = api.service.calls.lock().unwrap();
    let accepted = &calls[0];
    assert_eq!(accepted.target, id);
    assert_eq!(
        accepted.body_hash,
        hex::encode(Sha256::digest(raw.as_bytes()))
    );
    assert_eq!(accepted.ctx.actor_id, api.ids[0]);
    assert_eq!(accepted.ctx.correlation_id, CID);
    assert_eq!(accepted.key, "source-probe-key");
    assert_eq!(accepted.request.resource_id, None);
    assert_eq!(receipt, serde_json::to_value(&accepted.receipt).unwrap());
}

#[tokio::test]
async fn probe_aplica_scope_dto_y_clave_de_idempotencia() {
    let api = setup().await;
    let path = format!("/api/v1/sources/{}/probe", uuid::Uuid::now_v7());
    let (status, _) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[1]),
        &body().to_string(),
        Some("reader"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for (raw, expected) in [
        ("{}", StatusCode::BAD_REQUEST),
        (
            r#"{"resource_id":null,"profile_id":null,"secret":"no"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"resource_id":null,"profile_id":"unknown"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, _) = call(&api, "POST", &path, Some(&api.tokens[0]), raw, Some("dto")).await;
        assert_eq!(status, expected, "{raw}");
    }
    assert_eq!(
        call(
            &api,
            "POST",
            &path,
            Some(&api.tokens[0]),
            &body().to_string(),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, _) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[0]),
        &body().to_string(),
        Some("valid"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}
