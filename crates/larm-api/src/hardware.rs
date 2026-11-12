//! Perfil de hardware en la API (P010/P011), detrás del guard:
//!
//! - `GET /api/v1/hardware` (scope `read`): perfiles persistidos, del más
//!   antiguo al más reciente, con la paginación del contrato 07.
//! - `POST /api/v1/hardware/refresh` (scope `admin`, `Idempotency-Key`):
//!   encola un nuevo sondeo y responde 202 con un `JobReceipt`. Si ya hay uno
//!   en curso devuelve ese job. El sondeo solo lee: no instala drivers.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::hardware::HardwareRefresh;
use larm_core::security::CursorSigner;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::hardware::{HardwareProfile, Probe};
use larm_domain::ports::{HardwareRepository, IdempotencyStore};

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct HardwareState {
    pub repo: Arc<dyn HardwareRepository>,
    pub cursors: Arc<CursorSigner>,
    pub refresh: Arc<HardwareRefresh>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: HardwareState) -> Router {
    Router::new()
        .route("/api/v1/hardware", get(list_hardware))
        .route("/api/v1/hardware/refresh", post(refresh_hardware))
        .with_state(state)
}

async fn list_hardware(
    State(state): State<HardwareState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let fail = |e| Failure::from(e).recorrelate(&ctx.correlation_id);
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail)?;
    let page = state
        .cursors
        .page(
            "hardware:all",
            cursor.as_deref(),
            limit,
            |p: &HardwareProfile| (p.created_at.clone(), p.id.clone()),
            |after, n| state.repo.list(after, n),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::OK, &page))
}

fn invalid(reason: &str, message: &str) -> ApiError {
    ApiError::new("VALIDATION_FAILED", message, reason)
}

async fn refresh_hardware(
    State(state): State<HardwareState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let fail = |e| Failure::from(e).recorrelate(&ctx.correlation_id);
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        fail(
            invalid("schema_violation", "El cuerpo no es JSON").with_original(
                "larm-api",
                "SERDE",
                &e.to_string(),
            ),
        )
    })?;
    let probe = Probe::from_json(&value).map_err(|e| {
        fail(
            invalid("schema_violation", "El cuerpo no cumple el schema Probe")
                .with_original("larm-api", "SERDE", &e),
        )
    })?;
    if probe.profile_id.is_some() {
        return Err(fail(invalid(
            "profile_id_not_applicable",
            "El refresco de hardware no usa profile_id; envía null",
        )));
    }
    if let Some(id) = &probe.resource_id {
        if !is_uuid_v7(id) {
            return Err(fail(invalid(
                "resource_id_invalid",
                "resource_id no es UUIDv7",
            )));
        }
        if state.repo.get(id).await.map_err(fail)?.is_none() {
            return Err(fail(
                ApiError::new("NOT_FOUND", "Perfil de hardware desconocido", "not_found")
                    .with_resource(id.clone()),
            ));
        }
    }
    let refresh = state.refresh.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let (receipt, task) = refresh
                .start(&mutation, probe.resource_id.as_deref())
                .await?;
            if let Some(task) = task {
                tokio::spawn(task);
            }
            Ok((202, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}
