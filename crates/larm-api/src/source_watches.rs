//! Rutas P050 para CRUD de suscripciones a fuentes.

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::error::ApiError;
use larm_domain::ports::SourceWatchRepository;
use larm_domain::watchers::{SourceWatch, SourceWatchCreate};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct SourceWatchesState {
    pub repository: Arc<dyn SourceWatchRepository>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: SourceWatchesState) -> Router {
    Router::new()
        .route("/api/v1/source-watches", get(list).post(create))
        .route(
            "/api/v1/source-watches/{id}",
            get(read).patch(update).delete(remove),
        )
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |error| Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn version(headers: &HeaderMap, ctx: &AuthContext) -> Result<i64, Failure> {
    i64::try_from(
        crate::http::if_match(headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?,
    )
    .map_err(|_| {
        fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })
}

async fn list(
    State(state): State<SourceWatchesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    let page = state
        .cursors
        .page(
            "source-watches",
            cursor.as_deref(),
            limit,
            |watch: &SourceWatch| (watch.created_at.clone(), watch.id.clone()),
            |after, count| state.repository.page_source_watches(after, count),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn create(
    State(state): State<SourceWatchesState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: SourceWatchCreate =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let body_hash = hex::encode(Sha256::digest(&body));
    let watch = state
        .repository
        .create_source_watch(&request, key, &body_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &watch))
}

async fn read(
    State(state): State<SourceWatchesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let watch = state
        .repository
        .source_watch(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| {
            fail(&ctx)(ApiError::new(
                "NOT_FOUND",
                "Vigilancia desconocida",
                "not_found",
            ))
        })?;
    Ok(json(StatusCode::OK, &watch))
}

async fn update(
    State(state): State<SourceWatchesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let if_match = version(&headers, &ctx)?;
    let request: SourceWatchCreate =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail(&ctx))?;
    let watch = state
        .repository
        .update_source_watch(&id, if_match, &request, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &watch))
}

async fn remove(
    State(state): State<SourceWatchesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let if_match = version(&headers, &ctx)?;
    state
        .repository
        .delete_source_watch(&id, if_match, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
