//! Ruta protegida P141 para guardar credenciales Basic de una fuente.

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use larm_domain::error::LarmError;
use larm_domain::ports::SourceBasicCredentialWriter;
use larm_domain::source_credentials::BasicCredentialWrite;
use std::sync::Arc;

pub fn routes(service: Arc<dyn SourceBasicCredentialWriter>) -> Router {
    Router::new()
        .route("/api/v1/sources/{id}/basic-credential", post(write_basic))
        .with_state(service)
}

async fn write_basic(
    State(service): State<Arc<dyn SourceBasicCredentialWriter>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let fail = |error: LarmError| Failure::from(error).recorrelate(&ctx.correlation_id);
    crate::http::uuid_param(&id, &headers)?;
    let request: BasicCredentialWrite =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail)?;
    if request.source_id != id {
        return Err(fail(LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "La fuente del cuerpo no coincide con la ruta",
            "basic_credential_source_mismatch",
        )));
    }
    let reference = service
        .write_basic_credential(&id, &request, &ctx.mutation())
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::CREATED, &reference))
}
