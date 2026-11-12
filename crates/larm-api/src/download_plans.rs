//! Preflight HTTP de planes de descarga (P052, contrato 3.1.0).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Extension, Router};
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::download::{DownloadPlanRequest, ExecutePlan};
use larm_domain::ports::{
    DownloadActivationIdempotency, DownloadPlanActivationService, DownloadPlanPreviewService,
    IdempotencyStore,
};
use sha2::{Digest, Sha256};

use crate::http::{Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct DownloadPlansState {
    pub preview: Arc<dyn DownloadPlanPreviewService>,
    pub activation: Arc<dyn DownloadPlanActivationService>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: DownloadPlansState) -> Router {
    Router::new()
        .route("/api/v1/download-plans", post(create_preview))
        .route("/api/v1/downloads", post(create_download))
        .with_state(state)
}

async fn create_preview(
    State(state): State<DownloadPlansState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: DownloadPlanRequest =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    validate(&request).map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    let preview = state.preview.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let response = preview.preview(&request, &mutation).await?;
            Ok((201, serde_json::to_value(response).unwrap_or_default()))
        },
    )
    .await
}

async fn create_download(
    State(state): State<DownloadPlansState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ExecutePlan =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if !request.operator_confirmed
        || !is_uuid_v7(&request.plan_id)
        || !larm_domain::adapter_updates::is_sha256(&request.plan_hash)
    {
        return Err(Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "Confirma explícitamente un plan válido para continuar",
            "download_activation_invalid",
        ))
        .recorrelate(&ctx.correlation_id));
    }
    let key = crate::http::idempotency_key(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?
        .to_string();
    let body_hash = hex::encode(Sha256::digest(&body));
    let activator = state.activation.clone();
    let mutation = ctx.mutation();
    let idempotency = DownloadActivationIdempotency { key, body_hash };
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let receipt = activator
                .activate(&request, &mutation, &idempotency)
                .await?;
            Ok((202, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}

fn validate(request: &DownloadPlanRequest) -> Result<(), ApiError> {
    if !is_uuid_v7(&request.variant_id)
        || request
            .storage_location_id
            .as_deref()
            .is_some_and(|id| !is_uuid_v7(id))
        || request
            .source_id
            .as_deref()
            .is_some_and(|id| !is_uuid_v7(id))
        || request.optional_artifact_ids.len() > 4096
        || request
            .optional_artifact_ids
            .iter()
            .any(|id| !is_uuid_v7(id))
    {
        return Err(ApiError::new(
            "VALIDATION_FAILED",
            "Identidad de la petición de descarga no válida",
            "download_plan_request_invalid",
        ));
    }
    let mut ids = request.optional_artifact_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != request.optional_artifact_ids.len() {
        return Err(ApiError::new(
            "VALIDATION_FAILED",
            "La selección opcional contiene artefactos repetidos",
            "optional_artifact_selection_duplicate",
        ));
    }
    Ok(())
}
