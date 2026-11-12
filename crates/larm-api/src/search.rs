//! Rutas de búsqueda de modelos y fuentes (P044).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Extension, Router};
use larm_core::search::ModelSearchService;
use larm_domain::bridge::ApiError;
use larm_domain::error::ErrorCode;
use larm_domain::ports::IdempotencyStore;
use larm_domain::search::Search;

use crate::http::{Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct SearchState {
    pub service: Arc<dyn ModelSearchService>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: SearchState) -> Router {
    Router::new()
        .route("/api/v1/models/search", post(search_models))
        .route("/api/v1/sources/search", post(search_sources))
        .with_state(state)
}

async fn search_models(
    State(state): State<SearchState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    execute(&state, &ctx, &headers, body).await
}

async fn search_sources(
    State(state): State<SearchState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    execute(&state, &ctx, &headers, body).await
}

async fn execute(
    state: &SearchState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    body: Bytes,
) -> Answer {
    let request: Search = crate::http::strict(&body, headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    request
        .validate()
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;

    let service = state.service.clone();
    let request_for_search = request.clone();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        ctx,
        headers,
        &body,
        move || async move {
            let mut result = service.search(&request_for_search).await?;
            if !result.warnings.is_empty() {
                let warnings = serde_json::to_value(&result.warnings).map_err(|error| {
                    ApiError::new(
                        ErrorCode::Internal,
                        "No se pudieron preparar las advertencias de búsqueda",
                        "search_warning_serialize",
                    )
                    .with_technical(error.to_string())
                })?;
                for model in &mut result.page.items {
                    model
                        .metadata
                        .insert("search_warnings".into(), warnings.clone());
                }
            }
            let response = serde_json::to_value(result.page).map_err(|error| {
                ApiError::new(
                    ErrorCode::Internal,
                    "No se pudo preparar la respuesta de búsqueda",
                    "search_response_serialize",
                )
                .with_technical(error.to_string())
            })?;
            Ok((201, response))
        },
    )
    .await
}
