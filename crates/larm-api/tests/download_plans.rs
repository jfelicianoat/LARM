#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::download_plans::{routes, DownloadPlansState};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt};
use larm_domain::download::{DownloadPlanRequest, DownloadPlanResponse, ExecutePlan};
use larm_domain::ports::{
    DownloadActivationIdempotency, DownloadPlanActivationService, DownloadPlanPreviewService,
    MutationContext, PrincipalAuthenticator,
};
use tower::ServiceExt;

const VARIANT: &str = "0192f000-0000-7000-8000-000000000031";

#[derive(Default)]
struct Preview(AtomicUsize);

#[async_trait]
impl DownloadPlanPreviewService for Preview {
    async fn preview(
        &self,
        request: &DownloadPlanRequest,
        _: &MutationContext,
    ) -> Result<DownloadPlanResponse, ApiError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(DownloadPlanResponse {
            acquisition: None,
            preview_id: "0192f000-0000-7000-8000-000000000099".into(),
            preview_hash: "a".repeat(64),
            variant_id: request.variant_id.clone(),
            optional_artifact_ids: request.optional_artifact_ids.clone(),
            artifact_estimates: vec![],
            destination_options: vec![],
            selected_storage_location_id: None,
            plan_id: None,
            plan_hash: None,
            storage_estimate: None,
            source_id: None,
            source_candidates: vec![],
            hard_budget_bytes: None,
            priority: request.priority,
            expires_at: "2026-10-23T12:00:00.000Z".into(),
            operator_confirmation_required: true,
        })
    }
}

#[derive(Default)]
struct Activation(AtomicUsize);

#[async_trait]
impl DownloadPlanActivationService for Activation {
    async fn activate(
        &self,
        request: &ExecutePlan,
        _: &MutationContext,
        _: &DownloadActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(JobReceipt {
            job_id: request.plan_id.clone(),
            status_url: format!("/api/v1/jobs/{}", request.plan_id),
            state: JobPhase::Queued,
        })
    }
}

async fn setup() -> (axum::Router, String, Arc<Preview>, Arc<Activation>) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "test-setup".into(),
        correlation_id: "0192f000-0000-7000-8000-000000000001".into(),
    };
    let (_, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "descargas",
            vec![ApiScope::Read, ApiScope::JobsWrite],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let preview = Arc::new(Preview::default());
    let activation = Arc::new(Activation::default());
    let app = routes(DownloadPlansState {
        preview: preview.clone(),
        activation: activation.clone(),
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    (app, token, preview, activation)
}

async fn post(
    app: &axum::Router,
    token: &str,
    key: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/download-plans")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
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

async fn post_download(
    app: &axum::Router,
    token: &str,
    key: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/downloads")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
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
async fn preview_replays_idempotently_and_optional_ids_can_be_omitted() {
    let (app, token, preview, _) = setup().await;
    let body = format!(
        r#"{{"variant_id":"{VARIANT}","storage_location_id":null,"hard_budget_bytes":null,"source_id":null,"priority":"NORMAL"}}"#
    );
    let (status, first) = post(&app, &token, "preview-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["plan_id"], serde_json::Value::Null);
    assert_eq!(first["operator_confirmation_required"], true);
    let (status, replay) = post(&app, &token, "preview-1", &body).await;
    assert_eq!((status, replay), (StatusCode::CREATED, first));
    assert_eq!(preview.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn request_with_duplicate_optional_ids_fails_before_preflight() {
    let (app, token, preview, _) = setup().await;
    let artifact = "0192f000-0000-7000-8000-000000000051";
    let body = format!(
        r#"{{"variant_id":"{VARIANT}","storage_location_id":null,"hard_budget_bytes":null,"source_id":null,"priority":"NORMAL","optional_artifact_ids":["{artifact}","{artifact}"]}}"#
    );
    let (status, error) = post(&app, &token, "preview-duplicate", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(
        error["details"]["reason"],
        "optional_artifact_selection_duplicate"
    );
    assert_eq!(preview.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn create_download_requires_explicit_confirmation_and_replays_receipt() {
    let (app, token, _, activation) = setup().await;
    let id = "0192f000-0000-7000-8000-000000000111";
    let hash = "a".repeat(64);
    let unconfirmed =
        format!(r#"{{"plan_id":"{id}","plan_hash":"{hash}","operator_confirmed":false}}"#);
    let (status, error) = post_download(&app, &token, "confirm-no", &unconfirmed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(activation.0.load(Ordering::SeqCst), 0);

    let confirmed =
        format!(r#"{{"plan_id":"{id}","plan_hash":"{hash}","operator_confirmed":true}}"#);
    let (status, first) = post_download(&app, &token, "confirm-yes", &confirmed).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["state"], "QUEUED");
    let (status, replay) = post_download(&app, &token, "confirm-yes", &confirmed).await;
    assert_eq!((status, replay), (StatusCode::ACCEPTED, first));
    assert_eq!(activation.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn create_download_rejects_unknown_fields_from_closed_contract() {
    let (app, token, _, activation) = setup().await;
    let id = "0192f000-0000-7000-8000-000000000111";
    let hash = "a".repeat(64);
    let body = format!(
        r#"{{"plan_id":"{id}","plan_hash":"{hash}","operator_confirmed":true,"unexpected":true}}"#
    );
    let (status, error) = post_download(&app, &token, "confirm-unknown", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(activation.0.load(Ordering::SeqCst), 0);
}
