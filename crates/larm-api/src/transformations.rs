//! Planificación y consulta de jobs de transformación (P093).

use std::{collections::BTreeMap, sync::Arc};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::ports::{IdempotencyStore, TransformationService};
use larm_domain::transformations::{
    ExecuteTransformationPlan, TransformationActivationIdempotency, TransformationJob,
    TransformationRequest, TransformationRestart,
};
use sha2::{Digest, Sha256};

use crate::{
    http::{Answer, Failure},
    security::AuthContext,
};

#[derive(Clone)]
pub struct TransformationsState {
    pub service: Arc<dyn TransformationService>,
    pub cursors: Arc<CursorSigner>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: TransformationsState) -> Router {
    Router::new()
        .route("/api/v1/transform-plans", post(create_plan))
        .route("/api/v1/transformations", get(list_jobs).post(execute_plan))
        .route("/api/v1/transformations/{id}/restart", post(restart_plan))
        .with_state(state)
}

fn valid_confirmation(request: &ExecuteTransformationPlan) -> bool {
    is_uuid_v7(&request.plan_id)
        && larm_domain::adapter_updates::is_sha256(&request.plan_hash)
        && request.approval_id.as_deref().is_none_or(is_uuid_v7)
}

async fn restart_plan(
    State(state): State<TransformationsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let fail = |error| Failure::from(error).recorrelate(&ctx.correlation_id);
    let request: ExecuteTransformationPlan = crate::http::strict(&body, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let if_match = crate::http::if_match(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    if !is_uuid_v7(&id)
        || if_match == 0
        || if_match > i64::MAX as u64
        || !valid_confirmation(&request)
    {
        return Err(fail(ApiError::new(
            "VALIDATION_FAILED",
            "El reinicio debe identificar un trabajo, revisión y plan válidos",
            "transformation_restart_invalid",
        )));
    }
    let key = crate::http::idempotency_key(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let restart = TransformationRestart {
        job_id: id,
        if_match,
    };
    let idempotency = TransformationActivationIdempotency {
        key: key.to_owned(),
        body_hash: hex::encode(Sha256::digest(&body)),
    };
    // The repository owns the atomic replay. A generic cached response keyed
    // only by body would miss the target job and If-Match binding.
    let receipt = state
        .service
        .restart_plan(&restart, &request, &ctx.mutation(), &idempotency)
        .await
        .map_err(fail)?;
    Ok(crate::http::json(StatusCode::ACCEPTED, &receipt))
}

async fn create_plan(
    State(state): State<TransformationsState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: TransformationRequest = crate::http::strict(&body, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(|_| {
        Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "La petición de transformación no cumple el contrato",
            "transformation_request_invalid",
        ))
        .recorrelate(&ctx.correlation_id)
    })?;
    let service = state.service.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let response = service.create_plan(&request, &mutation).await?;
            Ok((201, serde_json::to_value(response).unwrap_or_default()))
        },
    )
    .await
}

async fn execute_plan(
    State(state): State<TransformationsState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ExecuteTransformationPlan = crate::http::strict(&body, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    if !valid_confirmation(&request) {
        return Err(Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "La confirmación debe identificar un plan válido",
            "transformation_activation_invalid",
        ))
        .recorrelate(&ctx.correlation_id));
    }
    let key = crate::http::idempotency_key(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?
        .to_string();
    let idempotency = TransformationActivationIdempotency {
        key,
        body_hash: hex::encode(Sha256::digest(&body)),
    };
    let service = state.service.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let receipt = service
                .execute_plan(&request, &mutation, &idempotency)
                .await?;
            Ok((202, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}

async fn list_jobs(
    State(state): State<TransformationsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let fail = |error| Failure::from(error).recorrelate(&ctx.correlation_id);
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail)?;
    let filter = larm_domain::canonical::sha256_hex(&serde_json::json!({
        "list": "transformations",
        "actor_id": ctx.principal.id,
    }))
    .map_err(|_| {
        fail(ApiError::new(
            "VALIDATION_FAILED",
            "No se pudo preparar la consulta",
            "transformation_list_invalid",
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
            |job: &TransformationJob| (job.created_at.clone(), job.id.clone()),
            |after, count| async move { service.list_jobs(after, count, &mutation).await },
        )
        .await
        .map_err(fail)?;
    Ok(crate::http::json(StatusCode::OK, &page))
}
