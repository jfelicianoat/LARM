//! Rutas contractuales P045 para gestionar cuentas de fuentes.

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_core::source_accounts::SourceAccounts;
use larm_domain::error::LarmError;
use larm_domain::source_accounts::{
    CheckSourceLicense, ModelSourceAccount, ModelSourceAccountCreate,
};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct SourceAccountsState {
    pub service: Arc<SourceAccounts>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: SourceAccountsState) -> Router {
    Router::new()
        .route("/api/v1/source-accounts", get(list).post(create))
        .route("/api/v1/source-accounts/{id}", get(read).delete(remove))
        .route("/api/v1/sources/{id}/license", post(check_license))
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> Failure + '_ {
    move |error| Failure::from(error).recorrelate(&ctx.correlation_id)
}

async fn list(
    State(state): State<SourceAccountsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    let page = state
        .cursors
        .page(
            "source-accounts:all",
            cursor.as_deref(),
            limit,
            |account: &ModelSourceAccount| (account.created_at.clone(), account.id.clone()),
            |after, count| state.service.list(after, count),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn create(
    State(state): State<SourceAccountsState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ModelSourceAccountCreate =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let account = state
        .service
        .create(&request, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &account))
}

async fn read(
    State(state): State<SourceAccountsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let account = state.service.get(&id).await.map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &account))
}

async fn remove(
    State(state): State<SourceAccountsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, Failure> {
    crate::http::uuid_param(&id, &headers)?;
    if !body.is_empty() {
        return Err(fail(&ctx)(larm_domain::source_accounts::account_invalid(
            "delete_body",
        )));
    }
    state
        .service
        .delete(&id, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn check_license(
    State(state): State<SourceAccountsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let request: CheckSourceLicense =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let key = crate::http::idempotency_key(&headers)
        .map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let mut digest = sha2::Sha256::new();
    use sha2::Digest as _;
    digest.update(id.as_bytes());
    digest.update([0]);
    digest.update(&body);
    let body_hash = hex::encode(digest.finalize());
    let receipt = state
        .service
        .check_license(&id, &request, key, &body_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &receipt))
}
