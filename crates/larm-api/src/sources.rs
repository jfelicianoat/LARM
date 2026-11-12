//! P043: CRUD de configuración. Guard de scopes por el catálogo de operaciones.
use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_core::sources::SourceCatalog;
use larm_domain::error::LarmError;
use larm_domain::sources::{ModelSource, ModelSourceCreate, SourceFilter};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct SourcesState {
    pub catalog: Arc<SourceCatalog>,
    pub cursors: Arc<CursorSigner>,
}
pub fn routes(state: SourcesState) -> Router {
    Router::new()
        .route("/api/v1/sources", get(list).post(create))
        .route(
            "/api/v1/sources/{id}",
            get(read).patch(update).delete(remove),
        )
        .with_state(state)
}
fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> Failure + '_ {
    move |e| Failure::from(e).recorrelate(&ctx.correlation_id)
}
fn version(headers: &HeaderMap, ctx: &AuthContext) -> Result<i64, Failure> {
    i64::try_from(crate::http::if_match(headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?)
        .map_err(|_| {
            fail(ctx)(LarmError::new(
                "VALIDATION_FAILED",
                "If-Match fuera de rango",
                "if_match_invalid",
            ))
        })
}
async fn list(
    State(state): State<SourcesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(&params, &["q", "capability"]).map_err(fail(&ctx))?;
    let filter = SourceFilter {
        q: params.get("q").cloned(),
        capability: params.get("capability").cloned(),
    };
    let digest = larm_domain::canonical::sha256_hex(
        &serde_json::json!({"list":"sources","q":filter.q,"capability":filter.capability}),
    )
    .map_err(|_| fail(&ctx)(larm_domain::sources::source_invalid("filters")))?;
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |s: &ModelSource| (s.created_at.clone(), s.id.clone()),
            |after, n| state.catalog.repo.page_sources(after, n, &filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}
async fn create(
    State(state): State<SourcesState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ModelSourceCreate =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let key =
        crate::http::idempotency_key(&headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let source = state
        .catalog
        .create(
            &request,
            key,
            &hex::encode(Sha256::digest(&body)),
            &ctx.mutation(),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &source))
}
async fn read(
    State(state): State<SourcesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let source = state
        .catalog
        .repo
        .source(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| {
            fail(&ctx)(LarmError::new(
                "NOT_FOUND",
                "Fuente desconocida",
                "not_found",
            ))
        })?;
    Ok(json(StatusCode::OK, &source))
}
async fn update(
    State(state): State<SourcesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let version = version(&headers, &ctx)?;
    let request: ModelSourceCreate =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let source = state
        .catalog
        .update(&id, version, &request, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &source))
}
async fn remove(
    State(state): State<SourcesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, Failure> {
    crate::http::uuid_param(&id, &headers)?;
    let version = version(&headers, &ctx)?;
    if !body.is_empty() {
        return Err(fail(&ctx)(larm_domain::sources::source_invalid(
            "delete_body",
        )));
    }
    state
        .catalog
        .repo
        .delete_source(&id, version, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
