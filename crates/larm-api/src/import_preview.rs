//! Preview read-only del escaneo local de importación (P098, contrato 3.2.0).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use larm_domain::bridge::ApiError;
use serde::Deserialize;

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

/// Implementación de exploración proporcionada por el daemon.
#[async_trait]
pub trait ModelImportPreviewer: Send + Sync {
    async fn preview(&self, source_path: &str) -> Result<serde_json::Value, ApiError>;
}

#[derive(Clone)]
pub struct ModelImportPreviewState {
    pub previewer: Arc<dyn ModelImportPreviewer>,
}

pub fn routes(state: ModelImportPreviewState) -> Router {
    Router::new()
        .route("/api/v1/models/import/preview", post(preview))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPreviewRequest {
    source_path: String,
}

async fn preview(
    State(state): State<ModelImportPreviewState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ImportPreviewRequest =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if request.source_path.trim().is_empty() {
        return Err(Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "Indica la carpeta que quieres revisar",
            "import_source_path_empty",
        ))
        .recorrelate(&ctx.correlation_id));
    }
    let result = state
        .previewer
        .preview(&request.source_path)
        .await
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    Ok(json(StatusCode::OK, &result))
}
