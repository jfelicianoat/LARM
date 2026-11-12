//! P026 HTTP/guard: el servicio controlado sólo registra la delegación.
//! SQLite/IO/replay se prueban en daemon y en el runner nativo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt, JobState};
use larm_domain::hardware::Probe;
use larm_domain::ports::{AdapterProber, JobSource, MutationContext, PrincipalAuthenticator};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone)]
struct Accepted {
    target: String,
    request: Probe,
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
impl AdapterProber for Service {
    async fn request_probe(
        self: Arc<Self>,
        target: &str,
        request: &Probe,
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
    let router = larm_api::adapters::probe_routes(service.clone())
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
async fn probe_202_delega_solo_referencias_actor_hash_exacto_y_correlacion() {
    let api = setup().await;
    let id = uuid::Uuid::now_v7().to_string();
    let path = format!("/api/v1/adapters/{id}/probe");
    let raw = format!(" \n{}\n", body());
    let (status, response) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[0]),
        &raw,
        Some("probe-key"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let calls = api.service.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let r = &calls[0];
    assert_eq!(r.target, id);
    assert_eq!(r.hash, hex::encode(Sha256::digest(raw.as_bytes())));
    assert_eq!(r.ctx.actor_id, api.ids[0]);
    assert_eq!(r.ctx.correlation_id, CID);
    assert_eq!(r.key, "probe-key");
    assert_eq!(r.request.resource_id, None);
    assert_eq!(response, serde_json::to_value(&r.receipt).unwrap());
}
#[tokio::test]
async fn probe_rechaza_dto_extra_ausente_invalido_veredictos_y_perfil_ajeno() {
    let api = setup().await;
    let path = format!("/api/v1/adapters/{}/probe", uuid::Uuid::now_v7());
    for raw in [
        "{}".to_string(),
        r#"{"resource_id":null,"resource_id":null,"profile_id":null}"#.to_string(),
        "null".into(),
        json!({"resource_id":null}).to_string(),
        json!({"resource_id":null,"profile_id":null,"passed":true}).to_string(),
        json!({"resource_id":"bad","profile_id":null}).to_string(),
        json!({"resource_id":null,"profile_id":""}).to_string(),
        json!({"resource_id":null,"profile_id":2}).to_string(),
    ] {
        assert_eq!(
            call(&api, "POST", &path, Some(&api.tokens[0]), &raw, Some("dto"))
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{raw}"
        );
    }
    assert_eq!(
        call(
            &api,
            "POST",
            &path,
            Some(&api.tokens[0]),
            &json!({"resource_id":null,"profile_id":"unregistered"}).to_string(),
            Some("profile")
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
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
    assert!(api.service.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn probe_exige_scope_antes_de_leer_body_y_jobs_respetan_propietario_admin() {
    let api = setup().await;
    let path = format!("/api/v1/adapters/{}/probe", uuid::Uuid::now_v7());
    assert_eq!(
        call(&api, "POST", &path, None, "not json", Some("auth"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &api,
            "POST",
            &path,
            Some(&api.tokens[1]),
            "not json",
            Some("auth")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(api.service.calls.lock().unwrap().is_empty());
    let (_, response) = call(
        &api,
        "POST",
        &path,
        Some(&api.tokens[0]),
        &body().to_string(),
        Some("owner"),
    )
    .await;
    let job = format!("/api/v1/jobs/{}", response["job_id"].as_str().unwrap());
    assert_eq!(
        call(&api, "GET", &job, Some(&api.tokens[0]), "", None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&api, "GET", &job, Some(&api.tokens[1]), "", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&api, "GET", &job, Some(&api.tokens[2]), "", None)
            .await
            .0,
        StatusCode::OK
    );
}
