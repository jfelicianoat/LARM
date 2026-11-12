//! Ubicaciones de almacenamiento (P014), detrás del guard:
//!
//! - `GET /storage-locations`, `/storage` y `/storage/locations` (`read`):
//!   lista con filtros `q` y `state` y cursor firmado ligado a los filtros;
//! - `POST /storage-locations` (`storage:write`, `Idempotency-Key`): 201;
//! - `GET /storage-locations/{id}` (`read`);
//! - `PATCH /storage-locations/{id}` (`storage:write`, `If-Match`): DTO completo;
//! - `DELETE /storage-locations/{id}` (`storage:write`, `If-Match`): 204;
//! - `POST /storage-locations/{id}/probe` (`storage:write`, `Idempotency-Key`):
//!   202 con `JobReceipt`.
//!
//! La validación de rutas y el disco son de `larm-storage`; aquí solo HTTP.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::locations::LocationProbe;
use larm_core::security::CursorSigner;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::hardware::Probe;
use larm_domain::locations::{LocationState, StorageLocation, StorageLocationCreate};
use larm_domain::ports::{IdempotencyStore, LocationFilter, LocationService};

use crate::http::{if_match, json, strict, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct StorageState {
    pub locations: Arc<dyn LocationService>,
    pub probe: Arc<LocationProbe>,
    pub cursors: Arc<CursorSigner>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: StorageState) -> Router {
    Router::new()
        .route("/api/v1/storage-locations", get(list).post(create))
        .route(
            "/api/v1/storage-locations/{id}",
            get(read).patch(update).delete(remove),
        )
        .route("/api/v1/storage-locations/{id}/probe", post(probe))
        .route("/api/v1/storage", get(list))
        .route("/api/v1/storage/locations", get(list))
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

fn version(headers: &HeaderMap, ctx: &AuthContext) -> Result<i64, Failure> {
    let v = if_match(headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    i64::try_from(v).map_err(|_| {
        fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })
}

async fn list(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &["q", "state"]).map_err(fail(&ctx))?;
    let filter_state = params
        .get("state")
        .map(|s| {
            serde_json::from_value::<LocationState>(serde_json::Value::String(s.clone())).map_err(
                |_| {
                    ApiError::new(
                        "VALIDATION_FAILED",
                        format!("Estado desconocido: {s}"),
                        "filter_invalid",
                    )
                },
            )
        })
        .transpose()
        .map_err(fail(&ctx))?;
    let filter = LocationFilter {
        q: params.get("q").cloned(),
        state: filter_state,
    };
    // El cursor queda ligado a los filtros: con otros filtros es inválido.
    let digest = format!(
        "storage-locations:q={}:state={}",
        filter.q.clone().unwrap_or_default(),
        filter.state.map(LocationState::as_str).unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |l: &StorageLocation| (l.created_at.clone(), l.id.clone()),
            |after, n| state.locations.list(after, n, &filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn create(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: StorageLocationCreate =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let (locations, mutation) = (state.locations.clone(), ctx.mutation());
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let created = locations.create(&request, &mutation).await?;
            Ok((201, serde_json::to_value(created).unwrap_or_default()))
        },
    )
    .await
}

async fn read(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let location = state.locations.get(&id).await.map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &location))
}

async fn update(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let v = version(&headers, &ctx)?;
    let request: StorageLocationCreate =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let updated = state
        .locations
        .update(&id, v, &request, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &updated))
}

async fn remove(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    id_param(&id, &ctx)?;
    let v = version(&headers, &ctx)?;
    state
        .locations
        .delete(&id, v, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn probe(
    State(state): State<StorageState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        fail(&ctx)(
            ApiError::new(
                "VALIDATION_FAILED",
                "El cuerpo no es JSON",
                "schema_violation",
            )
            .with_original("larm-api", "SERDE", &e.to_string()),
        )
    })?;
    let request = Probe::from_json(&value).map_err(|e| {
        fail(&ctx)(
            ApiError::new(
                "VALIDATION_FAILED",
                "El cuerpo no cumple el schema Probe",
                "schema_violation",
            )
            .with_original("larm-api", "SERDE", &e),
        )
    })?;
    if request.profile_id.is_some() {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "El sondeo de una ubicación no usa profile_id; envía null",
            "profile_id_not_applicable",
        )));
    }
    if request.resource_id.as_deref().is_some_and(|r| r != id) {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "resource_id debe ser null o el id de la ruta",
            "resource_id_mismatch",
        )));
    }
    let (probe, mutation) = (state.probe.clone(), ctx.mutation());
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let (receipt, task) = probe.start(&mutation, &id).await?;
            if let Some(task) = task {
                tokio::spawn(task);
            }
            Ok((202, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}
