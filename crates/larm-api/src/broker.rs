//! Rutas del puente Broker–LARM (`api-operations.json`: P100, P101 y jobs).
//!
//! La API no tiene lógica de negocio: autentica, valida la forma de la
//! petición (cabeceras, ids, JSON estricto) y delega en [`BrokerBridge`].

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use larm_core::bridge::{ApiScope, BrokerBridge, PrincipalAuthenticator};
use larm_domain::bridge::{ApiError, LeaseRequest, Renew};

pub use crate::http::REQUEST_BODY_BYTES;
use crate::http::{
    bearer, correlated, idempotency_key, if_match, json, principal, strict, uuid_param, Answer,
};

/// Estado compartido de las rutas.
#[derive(Clone)]
pub struct ApiState {
    pub bridge: Arc<dyn BrokerBridge>,
    pub auth: Arc<dyn PrincipalAuthenticator>,
}

/// Jobs del puente para `GET /api/v1/jobs/{id}` (P007).
pub struct BridgeJobs(pub Arc<dyn BrokerBridge>);

#[async_trait::async_trait]
impl larm_domain::ports::JobSource for BridgeJobs {
    async fn job(
        &self,
        principal: &larm_domain::auth::Principal,
        job_id: &str,
    ) -> Result<Option<larm_domain::bridge::JobState>, ApiError> {
        match self.0.get_job(principal, job_id).await {
            Ok(job) => Ok(Some(job)),
            Err(e) if e.code == "NOT_FOUND" || e.code == "FORBIDDEN" => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Router del puente.
pub fn router(state: ApiState) -> Router {
    admin_routes(state.clone()).merge(gateway_routes(state))
}

/// Rutas administrativas del puente (detrás del guard en la aplicación).
pub fn admin_routes(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/broker/catalog", get(catalog))
        .route("/api/v1/leases", post(create_lease).get(list_leases))
        .route("/api/v1/leases/{id}", get(get_lease).delete(release_lease))
        .route("/api/v1/leases/{id}/renew", post(renew_lease))
        .layer(DefaultBodyLimit::max(REQUEST_BODY_BYTES))
        .with_state(state)
}

/// Gateway de inferencia de las leases: lo autoriza el token de endpoint.
pub fn gateway_routes(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/gateway/{lease_id}/v1/{*path}", post(gateway))
        .layer(DefaultBodyLimit::max(REQUEST_BODY_BYTES))
        .with_state(state)
}

async fn catalog(State(state): State<ApiState>, headers: HeaderMap) -> Answer {
    principal(state.auth.as_ref(), &headers, ApiScope::Read).await?;
    let catalog = state
        .bridge
        .catalog()
        .await
        .map_err(|e| correlated(e, &headers))?;
    Ok(json(StatusCode::OK, &catalog))
}

async fn create_lease(State(state): State<ApiState>, headers: HeaderMap, body: Bytes) -> Answer {
    let principal = principal(state.auth.as_ref(), &headers, ApiScope::LeasesManage).await?;
    let key = idempotency_key(&headers)?;
    let request: LeaseRequest = strict(&body, &headers)?;
    if request.variant_id.is_none() && request.capability.is_none() {
        let error = ApiError::new(
            "VALIDATION_FAILED",
            "variant_id o capability obligatorio",
            "target_missing",
        );
        return Err(correlated(error, &headers).into());
    }
    let outcome = state
        .bridge
        .create_lease(&principal, request, key)
        .await
        .map_err(|e| correlated(e, &headers))?;
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::CREATED);
    Ok(json(status, &outcome.body))
}

async fn list_leases(State(state): State<ApiState>, headers: HeaderMap) -> Answer {
    let principal = principal(state.auth.as_ref(), &headers, ApiScope::LeasesManage).await?;
    let page = state
        .bridge
        .list_leases(&principal)
        .await
        .map_err(|e| correlated(e, &headers))?;
    Ok(json(StatusCode::OK, &page))
}

async fn get_lease(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    let principal = principal(state.auth.as_ref(), &headers, ApiScope::LeasesManage).await?;
    uuid_param(&id, &headers)?;
    let lease = state
        .bridge
        .get_lease(&principal, &id)
        .await
        .map_err(|e| correlated(e, &headers))?;
    Ok(json(StatusCode::OK, &lease))
}

async fn renew_lease(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let principal = principal(state.auth.as_ref(), &headers, ApiScope::LeasesManage).await?;
    uuid_param(&id, &headers)?;
    let revision = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let renew: Renew = strict(&body, &headers)?;
    let lease = state
        .bridge
        .renew_lease(&principal, &id, revision, key, renew)
        .await
        .map_err(|e| correlated(e, &headers))?;
    Ok(json(StatusCode::CREATED, &lease))
}

async fn release_lease(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    let principal = principal(state.auth.as_ref(), &headers, ApiScope::LeasesManage).await?;
    uuid_param(&id, &headers)?;
    let revision = if_match(&headers)?;
    state
        .bridge
        .release_lease(&principal, &id, revision)
        .await
        .map_err(|e| correlated(e, &headers))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn gateway(
    State(state): State<ApiState>,
    Path((lease_id, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    uuid_param(&lease_id, &headers)?;
    let payload: serde_json::Value = strict(&body, &headers)?;
    let served = state
        .bridge
        .gateway(&lease_id, bearer(&headers), &path, payload)
        .await
        .map_err(|e| correlated(e, &headers))?;
    let status = StatusCode::from_u16(served.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, served.body).into_response();
    let content_type = served.content_type.as_deref().unwrap_or("application/json");
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers_mut().insert("content-type", value);
    }
    Ok(response)
}

#[cfg(test)]
mod tests;
