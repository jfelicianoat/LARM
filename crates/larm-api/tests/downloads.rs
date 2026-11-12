#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::downloads::{routes, DownloadsState};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::download::{DownloadJob, DownloadPriority, DownloadState};
use larm_domain::ports::{DownloadQueueService, MutationContext};
use serde_json::{Map, Value};
use tower::ServiceExt;

const FIRST: &str = "0192f000-0000-7000-8000-00000000e201";
const SECOND: &str = "0192f000-0000-7000-8000-00000000e202";
const VARIANT: &str = "0192f000-0000-7000-8000-00000000e203";
const LOCATION: &str = "0192f000-0000-7000-8000-00000000e204";
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Default)]
struct MockQueue {
    actors: Mutex<Vec<String>>,
    priority_updates: Mutex<Vec<(String, i64, DownloadPriority, String)>>,
}

fn job(id: &str, created_at: &str) -> DownloadJob {
    DownloadJob {
        id: id.into(),
        created_at: created_at.into(),
        updated_at: created_at.into(),
        row_version: 1,
        metadata: Map::new(),
        variant_id: VARIANT.into(),
        storage_location_id: LOCATION.into(),
        plan_hash: HASH.into(),
        state: DownloadState::Queued,
        attempt: 0,
        progress: Map::from_iter([("phase".into(), Value::String("QUEUED".into()))]),
        error: None,
        priority: DownloadPriority::Normal,
    }
}

#[async_trait]
impl DownloadQueueService for MockQueue {
    async fn control(
        &self,
        job_id: &str,
        _command: larm_domain::download::DownloadCommand,
        _request: &larm_domain::download::DownloadStop,
        ctx: &MutationContext,
        _idempotency: &larm_domain::download::DownloadControlIdempotency,
    ) -> Result<larm_domain::bridge::JobReceipt, ApiError> {
        self.actors.lock().unwrap().push(ctx.actor_id.clone());
        Ok(larm_domain::bridge::JobReceipt {
            job_id: job_id.into(),
            status_url: format!("/api/v1/jobs/{job_id}"),
            state: larm_domain::bridge::JobPhase::Running,
        })
    }
    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<DownloadJob>, ApiError> {
        self.actors.lock().unwrap().push(ctx.actor_id.clone());
        let mut jobs = vec![
            job(FIRST, "2026-10-24T10:00:00.000Z"),
            job(SECOND, "2026-10-24T10:01:00.000Z"),
        ];
        if let Some((date, id)) = after {
            jobs.retain(|item| item.created_at > date || (item.created_at == date && item.id > id));
        }
        jobs.truncate(limit as usize);
        Ok(jobs)
    }

    async fn update_priority(
        &self,
        job_id: &str,
        expected_row_version: i64,
        priority: DownloadPriority,
        ctx: &MutationContext,
    ) -> Result<DownloadJob, ApiError> {
        self.priority_updates.lock().unwrap().push((
            job_id.into(),
            expected_row_version,
            priority,
            ctx.actor_id.clone(),
        ));
        let mut updated = job(job_id, "2026-10-24T10:00:00.000Z");
        updated.row_version = expected_row_version + 1;
        updated.priority = priority;
        Ok(updated)
    }
}

async fn setup() -> (axum::Router, String, Arc<MockQueue>) {
    setup_scopes(vec![ApiScope::Read, ApiScope::JobsWrite]).await
}

async fn setup_scopes(scopes: Vec<ApiScope>) -> (axum::Router, String, Arc<MockQueue>) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let context = MutationContext {
        actor_id: "0192f000-0000-7000-8000-00000000e205".into(),
        correlation_id: "0192f000-0000-7000-8000-00000000e206".into(),
    };
    let (_, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "descargas",
            scopes,
            vec![],
            None,
            &context,
        )
        .await
        .unwrap();
    let auth = principals as Arc<dyn larm_domain::ports::PrincipalAuthenticator>;
    let queue = Arc::new(MockQueue::default());
    let app = routes(DownloadsState {
        service: queue.clone(),
        cursors: Arc::new(CursorSigner::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    (app, token, queue)
}

#[tokio::test]
async fn controles_exigen_scope_clave_y_stop_estricto_y_devuelven_202() {
    let (app, token, queue) = setup().await;
    for action in ["pause", "resume", "cancel"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/downloads/{FIRST}/{action}"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("idempotency-key", format!("control-{action}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"reason":"operator_request","approval_id":null}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let value: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["job_id"], FIRST);
        assert_eq!(value["status_url"], format!("/api/v1/jobs/{FIRST}"));
    }
    assert_eq!(queue.actors.lock().unwrap().len(), 3);
    for (key, body) in [
        (None, r#"{"reason":"operator_request","approval_id":null}"#),
        (Some("invalid"), r#"{"reason":"operator_request"}"#),
        (Some("invalid"), r#"{"reason":"","approval_id":null}"#),
        (Some("invalid"), r#"{"reason":"x","approval_id":"bad"}"#),
        (
            Some("invalid"),
            r#"{"reason":"x","approval_id":null,"force":true}"#,
        ),
    ] {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/downloads/{FIRST}/cancel"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(queue.actors.lock().unwrap().len(), 3);
    let (app, token, queue) = setup_scopes(vec![ApiScope::Read]).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/downloads/{FIRST}/pause"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "denied")
                .body(Body::from(
                    r#"{"reason":"operator_request","approval_id":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(queue.actors.lock().unwrap().is_empty());
}

async fn get(app: &axum::Router, token: &str, path: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn lista_descargas_con_cursor_firmado_y_contexto_del_actor() {
    let (app, token, queue) = setup().await;
    let (status, first_page) = get(&app, &token, "/api/v1/downloads?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_page["items"][0]["id"], FIRST);
    let cursor = first_page["next_cursor"]
        .as_str()
        .expect("siguiente cursor");

    let (status, second_page) = get(
        &app,
        &token,
        &format!("/api/v1/downloads?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_page["items"][0]["id"], SECOND);
    assert_eq!(second_page["next_cursor"], Value::Null);
    let actors = queue.actors.lock().unwrap();
    assert_eq!(actors.len(), 2);
    assert_eq!(actors[0], actors[1]);
}

#[tokio::test]
async fn actualiza_prioridad_con_if_match_y_devuelve_etag_y_contexto() {
    let (app, token, queue) = setup().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/downloads/{FIRST}/priority"))
                .header("authorization", format!("Bearer {token}"))
                .header("if-match", "\"1\"")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"priority":"HIGH"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("etag").unwrap(), "\"2\"");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["id"], FIRST);
    assert_eq!(body["priority"], "HIGH");
    assert_eq!(body["row_version"], 2);
    let updates = queue.priority_updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, FIRST);
    assert_eq!(updates[0].1, 1);
    assert_eq!(updates[0].2, DownloadPriority::High);
    assert!(!updates[0].3.is_empty());
}
