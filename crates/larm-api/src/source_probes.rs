//! P045 `POST /sources/{id}/probe`: aceptación durable autenticada.

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use larm_domain::error::LarmError;
use larm_domain::ports::SourceProber;
use std::sync::Arc;

pub fn routes(service: Arc<dyn SourceProber>) -> Router {
    Router::new()
        .route("/api/v1/sources/{id}/probe", post(probe))
        .with_state(service)
}

async fn probe(
    State(service): State<Arc<dyn SourceProber>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    use sha2::{Digest, Sha256};
    let fail = |error: LarmError| Failure::from(error).recorrelate(&ctx.correlation_id);
    crate::http::uuid_param(&id, &headers)?;
    let request: larm_domain::hardware::Probe =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    larm_domain::source_probes::validate_request(&id, &request).map_err(fail)?;
    let key = crate::http::idempotency_key(&headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let receipt = service
        .request_source_probe(
            &id,
            &request,
            &hex::encode(Sha256::digest(&body)),
            key,
            &ctx.mutation(),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}
