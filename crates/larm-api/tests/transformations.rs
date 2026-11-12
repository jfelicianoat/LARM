#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::security::guard;
use larm_api::transformations::{routes, TransformationsState};
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt};
use larm_domain::estimate::{Confidence, StorageEstimate, VolumeEstimate};
use larm_domain::ports::{MutationContext, TransformationService};
use larm_domain::transformations::{
    seal_plan, ExecuteTransformationPlan, InputArtifact, PlanDraft, PlanStep,
    SealedTransformationPlan, TransformationActivationIdempotency, TransformationJob,
    TransformationPlanResponse, TransformationRequest,
};
use larm_domain::units::DecimalU64;
use tower::ServiceExt;

const VARIANT: &str = "0192f000-0000-7000-8000-000000000031";
const DESTINATION: &str = "0192f000-0000-7000-8000-000000000032";
const ADAPTER_ID: &str = "0192f000-0000-7000-8000-000000000033";
const ARTIFACT: &str = "0192f000-0000-7000-8000-000000000034";
const PLAN: &str = "0192f000-0000-7000-8000-000000000035";
const JOB: &str = "0192f000-0000-7000-8000-000000000036";
const CORRELATION: &str = "0192f000-0000-7000-8000-000000000037";
const NOW: &str = "2026-10-23T12:00:00.000Z";
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn context() -> MutationContext {
    MutationContext {
        actor_id: "0192f000-0000-7000-8000-000000000038".into(),
        correlation_id: CORRELATION.into(),
    }
}

fn sealed_plan() -> SealedTransformationPlan {
    let request = TransformationRequest {
        input_variant_id: VARIANT.into(),
        adapter_id: "test.transformer".into(),
        tool_version: "1.0.0".into(),
        target_format: "GGUF".into(),
        parameters: serde_json::json!({"quantization":"Q4_K_M"}),
        storage_location_id: DESTINATION.into(),
        hard_budget_bytes: Some(DecimalU64::new(10_000).unwrap()),
    };
    let artifacts = vec![InputArtifact {
        artifact_id: ARTIFACT.into(),
        relative_path: "model.safetensors".into(),
        size_bytes: DecimalU64::new(500).unwrap(),
        sha256: HASH.into(),
    }];
    let estimate = StorageEstimate {
        download_bytes: None,
        final_artifact_bytes: DecimalU64::new(400),
        installed_bytes: None,
        derived_cache_bytes: None,
        temporary_peak_bytes: DecimalU64::new(500),
        existing_bytes: DecimalU64::new(500),
        peak_extra_bytes: DecimalU64::new(900),
        required_free_bytes: DecimalU64::new(1000),
        upper_bound_bytes: DecimalU64::new(900),
        confidence: Confidence::Estimated,
        reason: "test fixture".into(),
        evidence_refs: vec!["fixture:transformation-api".into()],
        per_volume: vec![VolumeEstimate {
            volume_key: "test-volume".into(),
            peak_extra_bytes: DecimalU64::new(900).unwrap(),
            margin_bytes: DecimalU64::new(100).unwrap(),
            other_reservations_bytes: DecimalU64::new(0).unwrap(),
            usable_free_bytes: DecimalU64::new(5000).unwrap(),
            eligible: true,
            reason: "fits".into(),
        }],
    };
    seal_plan(
        PLAN,
        request,
        artifacts,
        PlanDraft {
            operation: "transform".into(),
            resource_ids: vec![VARIANT.into(), DESTINATION.into()],
            storage_estimate: estimate,
            steps: vec![PlanStep {
                phase: "convert".into(),
                adapter_id: Some("test.transformer".into()),
                request: serde_json::json!({"tool_version":"1.0.0"}),
            }],
            expires_at: "2026-10-23T12:15:00.000Z".into(),
            approval_required: false,
        },
    )
    .unwrap()
}

#[derive(Default)]
struct MockService {
    plans: AtomicUsize,
    executions: AtomicUsize,
    lists: AtomicUsize,
    restarts: AtomicUsize,
    cancels: AtomicUsize,
}

#[async_trait]
impl TransformationService for MockService {
    async fn restart_plan(
        &self,
        restart: &larm_domain::transformations::TransformationRestart,
        request: &ExecuteTransformationPlan,
        _: &MutationContext,
        _: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        assert_eq!(restart.job_id, JOB);
        assert_eq!(restart.if_match, 4);
        assert_eq!(request.plan_id, PLAN);
        assert_eq!(request.plan_hash, HASH);
        assert_eq!(request.approval_id, None);
        Ok(JobReceipt {
            job_id: JOB.into(),
            status_url: format!("/api/v1/jobs/{JOB}"),
            state: JobPhase::Queued,
        })
    }
    async fn create_plan(
        &self,
        _: &TransformationRequest,
        _: &MutationContext,
    ) -> Result<TransformationPlanResponse, ApiError> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        Ok(sealed_plan().response)
    }

    async fn execute_plan(
        &self,
        _: &ExecuteTransformationPlan,
        _: &MutationContext,
        _: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, ApiError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(JobReceipt {
            job_id: JOB.into(),
            status_url: format!("/api/v1/jobs/{JOB}"),
            state: JobPhase::Queued,
        })
    }

    async fn list_jobs(
        &self,
        _: Option<(String, String)>,
        _: u32,
        _: &MutationContext,
    ) -> Result<Vec<TransformationJob>, ApiError> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let job =
            TransformationJob::from_plan(JOB, ADAPTER_ID, &sealed_plan(), NOW, &context()).unwrap();
        Ok(vec![job])
    }

    async fn cancel_job(
        &self,
        job_id: &str,
        _: &str,
        _: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        if job_id != JOB {
            return Err(ApiError::new(
                "NOT_FOUND",
                "Trabajo desconocido",
                "transformation_job_missing",
            ));
        }
        Ok(JobReceipt {
            job_id: JOB.into(),
            status_url: format!("/api/v1/jobs/{JOB}"),
            state: JobPhase::Cancelled,
        })
    }
}

async fn setup() -> (axum::Router, String, Arc<MockService>) {
    setup_scopes(vec![ApiScope::Read, ApiScope::JobsWrite]).await
}

async fn setup_scopes(scopes: Vec<ApiScope>) -> (axum::Router, String, Arc<MockService>) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let (_, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "transformaciones",
            scopes,
            vec![],
            None,
            &context(),
        )
        .await
        .unwrap();
    let auth = principals as Arc<dyn larm_domain::ports::PrincipalAuthenticator>;
    let service = Arc::new(MockService::default());
    let app = routes(TransformationsState {
        service: service.clone(),
        cursors: Arc::new(CursorSigner::default()),
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    (app, token, service)
}

async fn request(
    app: &axum::Router,
    token: &str,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn restart_request(
    app: &axum::Router,
    token: &str,
    id: &str,
    version: Option<&str>,
    key: Option<&str>,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/transformations/{id}/restart"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(version) = version {
        builder = builder.header("if-match", version);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn restart_body() -> String {
    serde_json::json!({"plan_id":PLAN,"plan_hash":HASH,"approval_id":null}).to_string()
}

#[tokio::test]
async fn restart_route_forwards_explicit_job_revision_and_confirmed_plan() {
    let (app, token, service) = setup().await;
    let response = restart_request(
        &app,
        &token,
        JOB,
        Some("\"4\""),
        Some("restart-1"),
        &restart_body(),
    )
    .await;
    assert_eq!(response.0, StatusCode::ACCEPTED);
    assert_eq!(response.1["job_id"], JOB);
    assert_eq!(response.1["state"], "QUEUED");
    assert_eq!(service.restarts.load(Ordering::SeqCst), 1);
    let api = larm_api::openapi::document("test", &larm_api::openapi::is_implemented).unwrap();
    let operation = &api["paths"]["/api/v1/transformations/{id}/restart"]["post"];
    assert_eq!(
        operation["operationId"],
        "P093_post__transformations_id_restart"
    );
    assert_eq!(operation["x-larm-implemented"], true);
    assert!(operation["responses"]["202"].is_object());
    assert!(operation["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "If-Match" && p["required"] == true));
}

#[tokio::test]
async fn restart_missing_or_invalid_headers_and_noncontractual_body_never_reach_service() {
    let (app, token, service) = setup().await;
    let valid = restart_body();
    let cases=[
        (JOB,None,Some("key"),valid.clone(),StatusCode::PRECONDITION_REQUIRED),
        (JOB,Some("bad"),Some("key"),valid.clone(),StatusCode::BAD_REQUEST),
        (JOB,Some("0"),Some("key"),valid.clone(),StatusCode::BAD_REQUEST),
        (JOB,Some("9223372036854775808"),Some("key"),valid.clone(),StatusCode::BAD_REQUEST),
        (JOB,Some("4"),None,valid.clone(),StatusCode::BAD_REQUEST),
        ("not-a-uuid",Some("4"),Some("key"),valid,StatusCode::BAD_REQUEST),
        (JOB,Some("4"),Some("key"),serde_json::json!({"plan_id":PLAN,"plan_hash":HASH}).to_string(),StatusCode::BAD_REQUEST),
        (JOB,Some("4"),Some("key"),serde_json::json!({"plan_id":PLAN,"plan_hash":HASH,"approval_id":null,"resume_offset":9}).to_string(),StatusCode::BAD_REQUEST),
    ];
    for (job, revision, key, body, status) in cases {
        assert_eq!(
            restart_request(&app, &token, job, revision, key, &body)
                .await
                .0,
            status
        );
    }
    assert_eq!(service.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn restart_requires_jobs_write_and_an_authenticated_actor() {
    let (app, token, service) = setup_scopes(vec![ApiScope::Read]).await;
    assert_eq!(
        restart_request(&app, &token, JOB, Some("4"), Some("key"), &restart_body())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        restart_request(
            &app,
            "invalid",
            JOB,
            Some("4"),
            Some("key"),
            &restart_body()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(service.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn planifica_y_repite_la_respuesta_idempotente() {
    let (app, token, service) = setup().await;
    let body = serde_json::json!({
        "input_variant_id": VARIANT,
        "adapter_id": "test.transformer",
        "tool_version": "1.0.0",
        "target_format": "GGUF",
        "parameters": {"quantization":"Q4_K_M"},
        "storage_location_id": DESTINATION,
        "hard_budget_bytes": "10000"
    })
    .to_string();
    let first = request(
        &app,
        &token,
        "POST",
        "/api/v1/transform-plans",
        Some("plan-1"),
        &body,
    )
    .await;
    let replay = request(
        &app,
        &token,
        "POST",
        "/api/v1/transform-plans",
        Some("plan-1"),
        &body,
    )
    .await;
    assert_eq!(first.0, StatusCode::CREATED);
    assert_eq!(first, replay);
    assert_eq!(first.1["plan_id"], PLAN);
    assert_eq!(service.plans.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ejecutar_exige_plan_valido_y_es_idempotente() {
    let (app, token, service) = setup().await;
    let body = serde_json::json!({
        "plan_id": PLAN,
        "plan_hash": sealed_plan().response.plan_hash,
        "approval_id": null
    })
    .to_string();
    let first = request(
        &app,
        &token,
        "POST",
        "/api/v1/transformations",
        Some("execute-1"),
        &body,
    )
    .await;
    let replay = request(
        &app,
        &token,
        "POST",
        "/api/v1/transformations",
        Some("execute-1"),
        &body,
    )
    .await;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(first, replay);
    assert_eq!(first.1["job_id"], JOB);
    assert_eq!(service.executions.load(Ordering::SeqCst), 1);

    let invalid = serde_json::json!({
        "plan_id": VARIANT,
        "plan_hash": "not-a-hash",
        "approval_id": null
    })
    .to_string();
    let rejected = request(
        &app,
        &token,
        "POST",
        "/api/v1/transformations",
        Some("execute-2"),
        &invalid,
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
    assert_eq!(service.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lista_jobs_con_cursor_y_rechaza_campos_no_contractuales() {
    let (app, token, service) = setup().await;
    let list = request(
        &app,
        &token,
        "GET",
        "/api/v1/transformations?limit=1",
        None,
        "",
    )
    .await;
    assert_eq!(list.0, StatusCode::OK);
    assert_eq!(list.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(list.1["next_cursor"], serde_json::Value::Null);
    assert_eq!(service.lists.load(Ordering::SeqCst), 1);

    let body = serde_json::json!({
        "input_variant_id": VARIANT,
        "adapter_id": "test.transformer",
        "tool_version": "1.0.0",
        "target_format": "GGUF",
        "parameters": {},
        "storage_location_id": DESTINATION,
        "hard_budget_bytes": null,
        "unexpected": true
    })
    .to_string();
    let rejected = request(
        &app,
        &token,
        "POST",
        "/api/v1/transform-plans",
        Some("invalid-plan"),
        &body,
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
    assert_eq!(service.plans.load(Ordering::SeqCst), 0);
}
