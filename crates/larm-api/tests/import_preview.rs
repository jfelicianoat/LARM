#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::import_preview::{routes, ModelImportPreviewState, ModelImportPreviewer};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::ports::{MutationContext, PrincipalAuthenticator};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Default)]
struct Previewer {
    calls: AtomicUsize,
    paths: Mutex<Vec<String>>,
}

#[async_trait]
impl ModelImportPreviewer for Previewer {
    async fn preview(&self, source_path: &str) -> Result<Value, ApiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.paths.lock().push(source_path.to_string());
        Ok(json!({
            "files_seen": 1,
            "recognized_files": 1,
            "bytes_seen": "4",
            "recognized_bytes": "4",
            "variants": [],
            "ignored": [],
            "skipped": []
        }))
    }
}

async fn setup() -> (axum::Router, String, String, Arc<Previewer>) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let context = MutationContext {
        actor_id: "import-preview-test".into(),
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
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let previewer = Arc::new(Previewer::default());
    let app = routes(ModelImportPreviewState {
        previewer: previewer.clone(),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    (app, reader, writer, previewer)
}

async fn post(app: &axum::Router, token: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/models/import/preview")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
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
async fn preview_es_read_only_y_no_exige_clave_de_idempotencia() {
    let (app, _, writer, previewer) = setup().await;
    let (status, result) = post(&app, &writer, json!({"source_path":"C:\\Modelos\\fixture"})).await;

    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["bytes_seen"], "4");
    assert_eq!(previewer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(previewer.paths.lock().as_slice(), ["C:\\Modelos\\fixture"]);
}

#[tokio::test]
async fn preview_rechaza_lectores_cuerpos_invalidos_y_rutas_vacias_antes_del_scanner() {
    let (app, reader, writer, previewer) = setup().await;

    let (status, denied) = post(&app, &reader, json!({"source_path":"C:\\Modelos"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["details"]["reason"], "scope_missing");

    let (status, invalid) = post(
        &app,
        &writer,
        json!({"source_path":"C:\\Modelos", "extra":true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_eq!(invalid["details"]["reason"], "schema_violation");

    let (status, empty) = post(&app, &writer, json!({"source_path":"  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{empty}");
    assert_eq!(empty["details"]["reason"], "import_source_path_empty");
    assert_eq!(previewer.calls.load(Ordering::SeqCst), 0);
}
