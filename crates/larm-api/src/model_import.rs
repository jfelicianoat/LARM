//! Inicio de jobs de importación local P098.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use larm_domain::model_import::{ModelImportRequest, ModelImportSubmitter};
use sha2::{Digest, Sha256};

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct ModelImportState {
    pub submitter: Arc<dyn ModelImportSubmitter>,
}

pub fn routes(state: ModelImportState) -> Router {
    Router::new()
        .route("/api/v1/models/import", post(start_import))
        .with_state(state)
}

async fn start_import(
    State(state): State<ModelImportState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ModelImportRequest = crate::http::strict(&body, &headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    request
        .validate()
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    let idempotency_key = crate::http::idempotency_key(&headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let body_hash = hex::encode(Sha256::digest(&body));
    let receipt = state
        .submitter
        .enqueue_import(&request, idempotency_key, &body_hash, &ctx.mutation())
        .await
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}
