#![allow(clippy::panic)] // en tests un fallo inesperado debe abortar el caso

use super::*;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use larm_core::bridge::Principal;
use larm_core::bridge::{CreateLeaseOutcome, EndpointCredential, GatewayResponse};
use larm_domain::bridge::{BrokerCatalog, JobState, Page, RuntimeLease};
use larm_domain::correlation::CORRELATION_HEADER;
use serde_json::{json, Value};
use std::sync::Mutex;
use tower::ServiceExt;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl BrokerBridge for Recorder {
    async fn catalog(&self) -> Result<BrokerCatalog, ApiError> {
        Ok(BrokerCatalog {
            contract_version: "1.0".into(),
            items: vec![],
            snapshot_at: "2026-10-10T00:00:00.000Z".into(),
        })
    }
    async fn launch_profiles(&self) -> Result<Page<Value>, ApiError> {
        Ok(Page {
            items: vec![],
            next_cursor: None,
            snapshot_at: "2026-10-10T00:00:00.000Z".into(),
        })
    }
    async fn create_lease(
        &self,
        p: &Principal,
        r: LeaseRequest,
        key: &str,
    ) -> Result<CreateLeaseOutcome, ApiError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("create:{}:{key}:{}", p.name, r.request_id));
        if r.ttl_seconds == 200 {
            return Ok(CreateLeaseOutcome {
                status: 202,
                body: json!({"job_id": "j", "status_url": "/x", "state": "RUNNING"}),
            });
        }
        Ok(CreateLeaseOutcome {
            status: 201,
            body: json!({"id": "lease"}),
        })
    }
    async fn get_lease(&self, _: &Principal, id: &str) -> Result<RuntimeLease, ApiError> {
        Err(ApiError::new("NOT_FOUND", "no", "not_found").with_resource(id))
    }
    async fn list_leases(&self, _: &Principal) -> Result<Page<RuntimeLease>, ApiError> {
        Ok(Page {
            items: vec![],
            next_cursor: None,
            snapshot_at: "2026-10-10T00:00:00.000Z".into(),
        })
    }
    async fn renew_lease(
        &self,
        _: &Principal,
        id: &str,
        rev: u64,
        key: &str,
        r: Renew,
    ) -> Result<RuntimeLease, ApiError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("renew:{id}:{rev}:{key}:{}", r.ttl_seconds));
        Err(ApiError::new("STALE_REVISION", "vieja", "stale"))
    }
    async fn release_lease(&self, _: &Principal, id: &str, rev: u64) -> Result<(), ApiError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("release:{id}:{rev}"));
        Ok(())
    }
    async fn get_job(&self, _: &Principal, _: &str) -> Result<JobState, ApiError> {
        Err(ApiError::new("NOT_FOUND", "no", "not_found"))
    }
    async fn gateway(
        &self,
        lease: &str,
        bearer: Option<&str>,
        path: &str,
        body: Value,
    ) -> Result<GatewayResponse, ApiError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "gateway:{lease}:{}:{path}:{}",
                bearer.unwrap_or("-"),
                body["model"]
            ));
        Ok(GatewayResponse {
            status: 200,
            content_type: Some("application/json".into()),
            body: br#"{"ok":true}"#.to_vec(),
        })
    }
    async fn endpoint_credential(
        &self,
        _: &Principal,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<EndpointCredential, ApiError> {
        Err(ApiError::new("FORBIDDEN", "no", "no"))
    }
}

struct Tokens;
#[async_trait::async_trait]
impl PrincipalAuthenticator for Tokens {
    async fn authenticate(&self, bearer: &str) -> Result<Option<Principal>, ApiError> {
        Ok(match bearer {
            "broker" => Some(
                larm_domain::auth::Principal::new(
                    "0192aaaa-0000-7000-8000-000000000006",
                    "ai-broker",
                    larm_domain::auth::PrincipalKind::Broker,
                    [
                        larm_domain::auth::ApiScope::Read,
                        larm_domain::auth::ApiScope::LeasesManage,
                    ],
                    [],
                )
                .unwrap_or_else(|e| panic!("{e}")),
            ),
            "reader" => Some(
                larm_domain::auth::Principal::new(
                    "0192aaaa-0000-7000-8000-000000000007",
                    "lector",
                    larm_domain::auth::PrincipalKind::Operator,
                    [larm_domain::auth::ApiScope::Read],
                    [],
                )
                .unwrap_or_else(|e| panic!("{e}")),
            ),
            _ => None,
        })
    }
}

fn app() -> (Router, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    (
        router(ApiState {
            bridge: recorder.clone(),
            auth: Arc::new(Tokens),
        }),
        recorder,
    )
}

async fn call(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.oneshot(request).await.unwrap_or_else(|e| panic!("{e}"));
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, body)
}

fn lease_body(ttl: u64) -> Value {
    json!({
        "variant_id": uuid::Uuid::now_v7().to_string(), "capability": "text.generate",
        "launch_profile_id": uuid::Uuid::now_v7().to_string(),
        "constraints": {"runtime_version_id": null, "backend_id": null, "execution_fingerprint": null,
                        "allow_backend_fallback": false, "max_memory_bytes": null},
        "ttl_seconds": ttl, "owner_id": "ai-broker", "request_id": "t:i", "resource_requirements": null
    })
}

fn post(uri: &str, token: Option<&str>, headers: &[(&str, &str)], body: &Value) -> Request<Body> {
    let mut builder = Request::post(uri).header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|e| panic!("{e}"))
}

#[tokio::test]
async fn the_catalog_needs_a_principal() {
    // health/live y health/ready son de la aplicación (P005): ver tests/app.rs.
    let (app, _) = app();
    let (status, _, body) = call(
        app.clone(),
        Request::get("/api/v1/broker/catalog")
            .body(Body::empty())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTH_REQUIRED");
    assert_eq!(body["details"]["evidence_refs"], json!([]));
    let request = Request::get("/api/v1/broker/catalog")
        .header("authorization", "Bearer reader")
        .body(Body::empty())
        .unwrap_or_default();
    let (status, _, body) = call(app, request).await;
    assert_eq!(
        (status, body["contract_version"].clone()),
        (StatusCode::OK, json!("1.0"))
    );
}

#[tokio::test]
async fn leases_need_scope_idempotency_key_and_strict_body() {
    let (app, recorder) = app();
    let (status, _, body) = call(
        app.clone(),
        post(
            "/api/v1/leases",
            Some("reader"),
            &[("idempotency-key", "k")],
            &lease_body(120),
        ),
    )
    .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::FORBIDDEN, json!("FORBIDDEN"))
    );
    let (status, _, body) = call(
        app.clone(),
        post("/api/v1/leases", Some("broker"), &[], &lease_body(120)),
    )
    .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("VALIDATION_FAILED"))
    );
    let mut extra = lease_body(120);
    extra["sorpresa"] = json!(1);
    let (status, _, body) = call(
        app.clone(),
        post(
            "/api/v1/leases",
            Some("broker"),
            &[("idempotency-key", "k")],
            &extra,
        ),
    )
    .await;
    assert_eq!(
        (status, body["details"]["reason"].clone()),
        (StatusCode::BAD_REQUEST, json!("schema_violation"))
    );
    let (status, _, _) = call(
        app.clone(),
        post(
            "/api/v1/leases",
            Some("broker"),
            &[("idempotency-key", "a b")],
            &lease_body(120),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = call(
        app.clone(),
        post(
            "/api/v1/leases",
            Some("broker"),
            &[("idempotency-key", "task:inv")],
            &lease_body(120),
        ),
    )
    .await;
    assert_eq!(
        (status, body["id"].clone()),
        (StatusCode::CREATED, json!("lease"))
    );
    let (status, _, _) = call(
        app,
        post(
            "/api/v1/leases",
            Some("broker"),
            &[("idempotency-key", "slow")],
            &lease_body(200),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let calls = recorder
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert_eq!(
        calls,
        vec!["create:ai-broker:task:inv:t:i", "create:ai-broker:slow:t:i"]
    );
}

#[tokio::test]
async fn renew_and_release_require_if_match_and_valid_ids() {
    let (app, recorder) = app();
    let id = uuid::Uuid::now_v7().to_string();
    let renew = json!({"generation": 3, "ttl_seconds": 60});
    let (status, _, body) = call(
        app.clone(),
        post(
            &format!("/api/v1/leases/{id}/renew"),
            Some("broker"),
            &[("idempotency-key", "r")],
            &renew,
        ),
    )
    .await;
    assert_eq!(
        (status.as_u16(), body["code"].clone()),
        (428, json!("PRECONDITION_REQUIRED"))
    );
    let headers = [("idempotency-key", "r"), ("if-match", "\"7\"")];
    let (status, _, body) = call(
        app.clone(),
        post(
            &format!("/api/v1/leases/{id}/renew"),
            Some("broker"),
            &headers,
            &renew,
        ),
    )
    .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("STALE_REVISION"))
    );
    let (status, _, _) = call(
        app.clone(),
        post(
            "/api/v1/leases/no-es-uuid/renew",
            Some("broker"),
            &headers,
            &renew,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let release = Request::delete(format!("/api/v1/leases/{id}"))
        .header("authorization", "Bearer broker")
        .header("if-match", "8")
        .body(Body::empty())
        .unwrap_or_default();
    let (status, _, _) = call(app.clone(), release).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let missing = Request::delete(format!("/api/v1/leases/{id}"))
        .header("authorization", "Bearer broker")
        .body(Body::empty())
        .unwrap_or_default();
    assert_eq!(call(app, missing).await.0.as_u16(), 428);
    let calls = recorder
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert_eq!(
        calls,
        vec![format!("renew:{id}:7:r:60"), format!("release:{id}:8")]
    );
}

#[tokio::test]
async fn correlation_id_is_echoed_and_bodies_are_bounded() {
    let (app, _) = app();
    let correlation = uuid::Uuid::now_v7().to_string();
    let request = Request::get(format!("/api/v1/leases/{}", uuid::Uuid::now_v7()))
        .header("authorization", "Bearer broker")
        .header(CORRELATION_HEADER, correlation.as_str())
        .body(Body::empty())
        .unwrap_or_default();
    let (status, headers, body) = call(app.clone(), request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["correlation_id"], correlation);
    assert_eq!(
        headers
            .get(CORRELATION_HEADER)
            .and_then(|v| v.to_str().ok()),
        Some(correlation.as_str())
    );
    let huge = json!({"x": "a".repeat(REQUEST_BODY_BYTES)});
    let (status, _, _) = call(
        app,
        post(
            "/api/v1/leases",
            Some("broker"),
            &[("idempotency-key", "k")],
            &huge,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn gateway_passes_the_endpoint_token_not_the_principal() {
    let (app, recorder) = app();
    let lease = uuid::Uuid::now_v7().to_string();
    let request = post(
        &format!("/api/v1/gateway/{lease}/v1/chat/completions"),
        Some("endpoint-token"),
        &[],
        &json!({"model": "x"}),
    );
    let (status, _, body) = call(app, request).await;
    assert_eq!((status, body["ok"].clone()), (StatusCode::OK, json!(true)));
    let calls = recorder
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert_eq!(
        calls,
        vec![format!(
            "gateway:{lease}:endpoint-token:chat/completions:\"x\""
        )]
    );
}
