#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::model_import::{routes, ModelImportState};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt};
use larm_domain::model_import::{ModelImportRequest, ModelImportSubmitter};
use larm_domain::ports::MutationContext;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Default)]
struct Submitter {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelImportRequest>>,
    metadata: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl ModelImportSubmitter for Submitter {
    async fn enqueue_import(
        &self,
        request: &ModelImportRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().push(request.clone());
        self.metadata.lock().push((
            idempotency_key.to_string(),
            body_hash.to_string(),
            ctx.actor_id.clone(),
        ));
        Ok(JobReceipt {
            job_id: "0192f000-0000-7000-8000-000000000099".into(),
            status_url: "/api/v1/jobs/0192f000-0000-7000-8000-000000000099".into(),
            state: JobPhase::Queued,
        })
    }
}

async fn setup() -> (axum::Router, String, String, Arc<Submitter>) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let context = MutationContext {
        actor_id: "model-import-test".into(),
        correlation_id: "0192f000-0000-7000-8000-000000000001".into(),
    };
    let (_, reader) = principals
        .enroll(
            PrincipalKind::Operator,
            "reader",
            vec![ApiScope::Read],
            vec![],
            None,
            &context,
        )
        .await
        .unwrap();
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "model-writer",
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![],
            None,
            &context,
        )
        .await
        .unwrap();
    let auth_trait: Arc<dyn larm_domain::ports::PrincipalAuthenticator> = principals;
    let submitter = Arc::new(Submitter::default());
    let app = routes(ModelImportState {
        submitter: submitter.clone(),
    })
    .layer(axum::middleware::from_fn_with_state(auth_trait, guard));
    (app, reader, writer, submitter)
}

fn request_body() -> Value {
    json!({
        "source_path": "C:\\Modelos\\qwen",
        "storage_location_id": "0192f000-0000-7000-8000-00000000c001",
        "mode": "ADOPT",
        "format": null,
        "license_id": "apache-2.0"
    })
}

async fn post(
    app: &axum::Router,
    token: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/models/import")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
async fn importacion_requiere_scope_y_clave_y_devuelve_202_job_receipt() {
    let (app, reader, writer, submitter) = setup().await;
    let (status, denied) = post(&app, &reader, Some("key-1"), request_body()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["details"]["reason"], "scope_missing");

    let (status, missing_key) = post(&app, &writer, None, request_body()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_key}");
    assert_eq!(missing_key["details"]["reason"], "idempotency_key_invalid");
    assert_eq!(submitter.calls.load(Ordering::SeqCst), 0);

    let (status, receipt) = post(&app, &writer, Some("model-import-1"), request_body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{receipt}");
    assert_eq!(receipt["state"], "QUEUED");
    assert_eq!(submitter.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        submitter.requests.lock()[0].license_id.as_deref(),
        Some("apache-2.0")
    );
    assert_eq!(submitter.metadata.lock()[0].0, "model-import-1");
}

#[tokio::test]
async fn importacion_rechaza_solicitudes_incompletas_o_invalidas_antes_de_encolar() {
    let (app, _, writer, submitter) = setup().await;
    let mut missing_nullable = request_body();
    missing_nullable.as_object_mut().unwrap().remove("format");
    let (status, invalid) = post(&app, &writer, Some("key-1"), missing_nullable).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");

    let mut extra = request_body();
    extra["extra"] = Value::Bool(true);
    let (status, invalid) = post(&app, &writer, Some("key-1"), extra).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");

    let mut invalid_location = request_body();
    invalid_location["storage_location_id"] = Value::String("bad".into());
    let (status, invalid) = post(&app, &writer, Some("key-1"), invalid_location).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_eq!(invalid["details"]["reason"], "import_location_id_invalid");
    assert_eq!(submitter.calls.load(Ordering::SeqCst), 0);
}
