//! Consulta paginada de la cola durable de descargas (P051).

use std::{collections::BTreeMap, sync::Arc};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::bridge::ApiError;
use larm_domain::download::{
    DownloadCommand, DownloadControlIdempotency, DownloadJob, DownloadPriorityUpdate, DownloadStop,
};
use larm_domain::ports::DownloadQueueService;
use sha2::{Digest, Sha256};

use crate::{http::Answer, security::AuthContext};

#[derive(Clone)]
pub struct DownloadsState {
    pub service: Arc<dyn DownloadQueueService>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: DownloadsState) -> Router {
    Router::new()
        .route("/api/v1/downloads", get(list_jobs))
        .route("/api/v1/downloads/{id}/priority", patch(update_priority))
        .route("/api/v1/downloads/{id}/pause", post(pause))
        .route("/api/v1/downloads/{id}/resume", post(resume))
        .route("/api/v1/downloads/{id}/cancel", post(cancel))
        .with_state(state)
}

async fn pause(
    State(state): State<DownloadsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    control(state, ctx, id, headers, body, DownloadCommand::Pause).await
}
async fn resume(
    State(state): State<DownloadsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    control(state, ctx, id, headers, body, DownloadCommand::Resume).await
}
async fn cancel(
    State(state): State<DownloadsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    control(state, ctx, id, headers, body, DownloadCommand::Cancel).await
}

async fn control(
    state: DownloadsState,
    ctx: AuthContext,
    id: String,
    headers: HeaderMap,
    body: Bytes,
    command: DownloadCommand,
) -> Answer {
    let fail = |e: ApiError| crate::http::Failure::from(e).recorrelate(&ctx.correlation_id);
    crate::http::uuid_param(&id, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let request: DownloadStop =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if request.reason.is_empty()
        || request
            .approval_id
            .as_deref()
            .is_some_and(|id| !larm_domain::bridge::is_uuid_v7(id))
    {
        return Err(fail(ApiError::new(
            "VALIDATION_FAILED",
            "La orden de descarga no es válida",
            "download_control_invalid",
        )));
    }
    let key = crate::http::idempotency_key(&headers)
        .map_err(|f| f.recorrelate(&ctx.correlation_id))?
        .to_string();
    let idempotency = DownloadControlIdempotency {
        key,
        body_hash: hex::encode(Sha256::digest(&body)),
    };
    // El repositorio confirma el recibo con la mutación y lo vincula al ID de
    // ruta. Un caché separado podría repetir un recibo de otro recurso.
    let receipt = state
        .service
        .control(&id, command, &request, &ctx.mutation(), &idempotency)
        .await
        .map_err(fail)?;
    Ok(crate::http::json(StatusCode::ACCEPTED, &receipt))
}

async fn list_jobs(
    State(state): State<DownloadsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let fail = |error: ApiError| crate::http::Failure::from(error).recorrelate(&ctx.correlation_id);
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail)?;
    let filter = larm_domain::canonical::sha256_hex(&serde_json::json!({
        "list": "downloads",
        "actor_id": ctx.principal.id,
    }))
    .map_err(|_| {
        fail(ApiError::new(
            "VALIDATION_FAILED",
            "No se pudo preparar la consulta",
            "download_list_invalid",
        ))
    })?;
    let service = state.service.clone();
    let mutation = ctx.mutation();
    let page = state
        .cursors
        .page(
            &filter,
            cursor.as_deref(),
            limit,
            |job: &larm_domain::download::DownloadJob| (job.created_at.clone(), job.id.clone()),
            |after, count| async move { service.list_jobs(after, count, &mutation).await },
        )
        .await
        .map_err(fail)?;
    Ok(crate::http::json(axum::http::StatusCode::OK, &page))
}

async fn update_priority(
    State(state): State<DownloadsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected = crate::http::if_match(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected = i64::try_from(expected).map_err(|_| {
        crate::http::Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match debe ser la row_version",
            "if_match_invalid",
        ))
        .recorrelate(&ctx.correlation_id)
    })?;
    let request: DownloadPriorityUpdate =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let job = state
        .service
        .update_priority(&id, expected, request.priority, &ctx.mutation())
        .await
        .map_err(|error| crate::http::Failure::from(error).recorrelate(&ctx.correlation_id))?;
    Ok(download_json(StatusCode::OK, &job))
}

fn download_json(status: StatusCode, job: &DownloadJob) -> Response {
    let mut response = crate::http::json(status, job);
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", job.row_version)) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response.into_response()
}
