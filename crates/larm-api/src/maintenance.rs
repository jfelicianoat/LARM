//! Mantenimiento del almacenamiento (P018), detrás del guard:
//!
//! - `GET /storage-locations/{id}/analysis` (`read`): `StorageAnalysis`;
//! - `POST /storage-locations/{id}/cleanup-plan` (`storage:write`, `Probe`):
//!   201 `Plan` con aprobación obligatoria;
//! - `POST /storage-locations/{id}/cleanup` (`storage:write`, `ExecutePlan`):
//!   202 `JobReceipt`; sin `approval_id` válido, `APPROVAL_REQUIRED`;
//! - `POST /models/{id}/delete-plan` (`models:write`, `Probe`,
//!   `Idempotency-Key`): 201 `Plan`; `{id}` es la variante;
//! - `DELETE /models/{id}?approval_id&plan_hash` (`models:write`,
//!   `If-Match`): 204.
//!
//! La aprobación se consume ligada al `plan_hash` y a los recursos del plan
//! antes de tocar nada.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::security::Approvals;
use larm_domain::approval::ApprovalUse;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::hardware::Probe;
use larm_domain::ports::{IdempotencyStore, StorageMaintenance};

use crate::http::{if_match, json, strict, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct MaintenanceState {
    pub service: Arc<dyn StorageMaintenance>,
    pub approvals: Arc<Approvals>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: MaintenanceState) -> Router {
    Router::new()
        .route("/api/v1/storage-locations/{id}/analysis", get(analysis))
        .route(
            "/api/v1/storage-locations/{id}/cleanup-plan",
            post(cleanup_plan),
        )
        .route("/api/v1/storage-locations/{id}/cleanup", post(cleanup))
        .route("/api/v1/models/{id}/delete-plan", post(delete_plan))
        .route("/api/v1/models/{id}", axum::routing::delete(delete_model))
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |e| Failure::from(e).recorrelate(&ctx.correlation_id)
}

fn id_param(id: &str, ctx: &AuthContext) -> Result<(), Failure> {
    if is_uuid_v7(id) {
        Ok(())
    } else {
        Err(fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "Identificador no es UUIDv7",
            "path_id_invalid",
        )))
    }
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `Probe` con `resource_id` nulo o igual al de la ruta y sin `profile_id`.
fn probe(body: &Bytes, id: &str) -> Result<(), ApiError> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        ApiError::new(
            "VALIDATION_FAILED",
            "El cuerpo no es JSON",
            "schema_violation",
        )
        .with_original("larm-api", "SERDE", &e.to_string())
    })?;
    let request = Probe::from_json(&value).map_err(|e| {
        ApiError::new(
            "VALIDATION_FAILED",
            "El cuerpo no cumple el schema Probe",
            "schema_violation",
        )
        .with_original("larm-api", "SERDE", &e)
    })?;
    if request.profile_id.is_some() || request.resource_id.as_deref().is_some_and(|r| r != id) {
        return Err(ApiError::new(
            "VALIDATION_FAILED",
            "resource_id debe ser null o el id de la ruta, y profile_id null",
            "probe_not_applicable",
        ));
    }
    Ok(())
}

async fn consume(
    approvals: &Approvals,
    ctx: &AuthContext,
    approval_id: Option<&str>,
    plan: &larm_domain::artifacts::Plan,
) -> Result<(), ApiError> {
    let Some(approval_id) = approval_id else {
        return Err(ApiError::new(
            "APPROVAL_REQUIRED",
            "Esta operación borra datos: hace falta una aprobación del plan",
            "approval_missing",
        ));
    };
    approvals
        .consume(
            approval_id,
            &ApprovalUse {
                actor_id: &ctx.principal.id,
                operation_digest: &plan.plan_hash,
                resource_ids: &plan.resource_ids,
                code_hash: None,
            },
        )
        .await
        .map(|_| ())
}

async fn analysis(
    State(state): State<MaintenanceState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let a = state.service.analysis(&id).await.map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &a))
}

async fn cleanup_plan(
    State(state): State<MaintenanceState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    probe(&body, &id).map_err(fail(&ctx))?;
    let plan = state
        .service
        .cleanup_plan(&id, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &plan))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutePlan {
    plan_id: String,
    plan_hash: String,
    approval_id: Option<String>,
}

async fn cleanup(
    State(state): State<MaintenanceState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let request: ExecutePlan =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if !is_uuid_v7(&request.plan_id)
        || !is_sha256(&request.plan_hash)
        || request
            .approval_id
            .as_deref()
            .is_some_and(|a| !is_uuid_v7(a))
    {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "El cuerpo no cumple el schema ExecutePlan",
            "schema_violation",
        )));
    }
    let plan = state
        .service
        .planned_cleanup(&id, &request.plan_id, &request.plan_hash)
        .await
        .map_err(fail(&ctx))?;
    consume(
        &state.approvals,
        &ctx,
        request.approval_id.as_deref(),
        &plan,
    )
    .await
    .map_err(fail(&ctx))?;
    let (receipt, task) = state
        .service
        .cleanup(&id, &request.plan_id, &request.plan_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    tokio::spawn(task);
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

async fn delete_plan(
    State(state): State<MaintenanceState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    probe(&body, &id).map_err(fail(&ctx))?;
    let (service, mutation) = (state.service.clone(), ctx.mutation());
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let plan = service.delete_plan(&id, &mutation).await?;
            Ok((201, serde_json::to_value(plan).unwrap_or_default()))
        },
    )
    .await
}

async fn delete_model(
    State(state): State<MaintenanceState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Answer {
    id_param(&id, &ctx)?;
    let version = if_match(&headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let version = i64::try_from(version).map_err(|_| {
        fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })?;
    if let Some(unknown) = query
        .keys()
        .find(|k| !matches!(k.as_str(), "approval_id" | "plan_hash"))
    {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            format!("Parámetro no previsto: {unknown}"),
            "query_invalid",
        )));
    }
    let plan_hash = query
        .get("plan_hash")
        .filter(|h| is_sha256(h))
        .ok_or_else(|| {
            fail(&ctx)(ApiError::new(
                "VALIDATION_FAILED",
                "Hace falta el plan_hash del delete-plan",
                "plan_hash_required",
            ))
        })?;
    if query.get("approval_id").is_some_and(|a| !is_uuid_v7(a)) {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "approval_id no es UUIDv7",
            "schema_violation",
        )));
    }
    let plan = state
        .service
        .planned_delete(&id, plan_hash)
        .await
        .map_err(fail(&ctx))?;
    consume(
        &state.approvals,
        &ctx,
        query.get("approval_id").map(String::as_str),
        &plan,
    )
    .await
    .map_err(fail(&ctx))?;
    state
        .service
        .delete(&id, version, plan_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
