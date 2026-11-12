//! P024 HTTP/guard: el servicio controlado sólo registra la delegación.
//! SQLite/IO/replay se prueban en daemon y en el runner nativo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::adapter_updates::AdapterImport;
use larm_domain::auth::{ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::ports::{AdapterUpdater, JobSource, MutationContext, PrincipalAuthenticator};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
struct Accepted {
    target: String,
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
impl AdapterUpdater for Service {
    async fn request_update(
        self: Arc<Self>,
        target: &str,
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
            target: target.into(),
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
    async fn job(&self, p: &Principal, id: &str) -> Result<Option<JobState>, ApiError> {
        Ok(self
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|r| {
                r.receipt.job_id == id
                    && (r.ctx.actor_id == p.id || p.has_api_scope(ApiScope::Admin))
            })
            .map(|r| JobState {
                job_id: id.into(),
                state: JobPhase::Queued,
                resource_type: "adapter_installation".into(),
                resource_id: Some(r.target.clone()),
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
            vec![ApiScope::Read, ApiScope::AdaptersWrite],
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
        let (p, t) = principals
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
        ids.push(p.id);
        tokens.push(t);
    }
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let service = Arc::new(Service::default());
    let router = larm_api::adapters::update_routes(service.clone())
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
    json!({"package_path":"C:/fixture/package.zip","expected_hash":"a".repeat(64),"approval_id":null})
}

#[tokio::test]
async fn update_delega_dto_cerrado_actor_hash_exacto_y_receipt_202() {
    let api = setup().await;
    let id = uuid::Uuid::now_v7().to_string();
    let path = format!("/api/v1/adapters/{id}/update");
    let raw = format!("\n {} \n", body());
    let (status, receipt) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[0]),
        &raw,
        Some("update-key"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let receipt: JobReceipt = serde_json::from_value(receipt).unwrap();
    assert_eq!(receipt.state, JobPhase::Queued);
    let recorded = api.service.calls.lock().unwrap()[0].clone();
    assert_eq!(recorded.target, id);
    assert_eq!(serde_json::to_value(recorded.request).unwrap(), body());
    assert_eq!(recorded.hash, hex::encode(Sha256::digest(raw.as_bytes())));
    assert_eq!(recorded.key, "update-key");
    assert_eq!(recorded.ctx.actor_id, api.ids[0]);
    assert_eq!(recorded.ctx.correlation_id, CID);
    assert_eq!(receipt, recorded.receipt);
}

#[tokio::test]
async fn autorizacion_se_aplica_antes_de_leer_documento_y_no_hay_bypass_mcp() {
    let api = setup().await;
    let path = format!("/api/v1/adapters/{}/update", uuid::Uuid::now_v7());
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(api.tokens[1].as_str()), StatusCode::FORBIDDEN),
        (Some(api.tokens[3].as_str()), StatusCode::FORBIDDEN),
    ] {
        let (status, error) = call(&api, "POST", &path, token, "invalid-json", Some("key")).await;
        assert_eq!(status, expected);
        assert_eq!(error["correlation_id"], CID);
    }
    assert!(api.service.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn esquema_headers_y_uuid_invalidos_no_inician_trabajo() {
    let api = setup().await;
    let path = format!("/api/v1/adapters/{}/update", uuid::Uuid::now_v7());
    let mut missing = body();
    missing.as_object_mut().unwrap().remove("approval_id");
    let mut extra = body();
    extra["trust"] = json!("VERIFIED");
    let mut hash = body();
    hash["expected_hash"] = json!("A".repeat(64));
    let mut approval = body();
    approval["approval_id"] = json!(uuid::Uuid::new_v4());
    for value in [missing, extra, hash, approval] {
        let (status, error) = call(
            &api,
            "POST",
            &path,
            Some(&api.tokens[0]),
            &value.to_string(),
            Some("key"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["correlation_id"], CID);
    }
    for key in [None, Some(""), Some("invalid key")] {
        assert_eq!(
            call(
                &api,
                "POST",
                &path,
                Some(&api.tokens[0]),
                &body().to_string(),
                key
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &api,
            "POST",
            &format!("/api/v1/adapters/{}/update", uuid::Uuid::new_v4()),
            Some(&api.tokens[0]),
            &body().to_string(),
            Some("key")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(api.service.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn jobs_solo_visibles_para_propietario_o_admin_con_scope_read() {
    let api = setup().await;
    let path = format!("/api/v1/adapters/{}/update", uuid::Uuid::now_v7());
    let (_, receipt) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[0]),
        &body().to_string(),
        Some("key"),
    )
    .await;
    let job = receipt["status_url"].as_str().unwrap();
    assert_eq!(
        call(&api, "GET", job, Some(&api.tokens[0]), "", None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&api, "GET", job, Some(&api.tokens[1]), "", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&api, "GET", job, Some(&api.tokens[2]), "", None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&api, "GET", job, None, "", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}
