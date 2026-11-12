//! Adaptadores declarativos (P023): `POST /adapters/declarative`
//! (`adapters:write`, `Idempotency-Key`) → 201 con `adapter_installation`.
//!
//! El documento lo valida y registra `larm-core`; aquí solo HTTP. La lista,
//! el detalle, la exportación/importación y el sondeo son de P024/P026/P028.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::adapters::DeclarativeRegistry;
use larm_domain::adapters::AdapterFilter;
use larm_domain::bridge::ApiError;
use larm_domain::ports::AdapterCatalogRepository;
use larm_domain::ports::IdempotencyStore;
use serde_json::Value;

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct AdaptersState {
    pub registry: Arc<DeclarativeRegistry>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

#[derive(Clone)]
pub struct AdapterCatalogState {
    pub repo: Arc<dyn AdapterCatalogRepository>,
    pub cursors: Arc<larm_core::security::CursorSigner>,
}

#[derive(Clone)]
pub struct AdapterExportState {
    pub exporter: Arc<dyn larm_domain::ports::AdapterPackageExporter>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

#[derive(Clone)]
pub struct AdapterRegistryState {
    pub manager: Arc<dyn larm_domain::adapter_registry::AdapterRegistryManager>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn registry_routes(state: AdapterRegistryState) -> Router {
    Router::new()
        .route("/api/v1/adapters/registry", get(registry_status))
        .route("/api/v1/adapters/registry/sync", post(registry_sync))
        .with_state(state)
}

async fn registry_status(
    State(state): State<AdapterRegistryState>,
    Extension(ctx): Extension<AuthContext>,
) -> Answer {
    let status = state
        .manager
        .status()
        .await
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    Ok(json(StatusCode::OK, &status))
}

async fn registry_sync(
    State(state): State<AdapterRegistryState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let _: larm_domain::adapter_registry::RegistrySyncRequest =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let manager = state.manager.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let receipt = manager.sync(&mutation).await?;
            Ok((
                StatusCode::OK.as_u16(),
                serde_json::to_value(receipt).unwrap_or_default(),
            ))
        },
    )
    .await
}

pub fn export_routes(state: AdapterExportState) -> Router {
    Router::new()
        .route("/api/v1/adapters/export", post(export))
        .with_state(state)
}

pub fn import_routes(service: Arc<dyn larm_domain::ports::AdapterImporter>) -> Router {
    Router::new()
        .route("/api/v1/adapters/import", post(import))
        .with_state(service)
}

async fn import(
    State(service): State<Arc<dyn larm_domain::ports::AdapterImporter>>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    use sha2::{Digest, Sha256};
    let fail = |e: ApiError| Failure::from(e).recorrelate(&ctx.correlation_id);
    let request: larm_domain::adapter_updates::AdapterImport =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail)?;
    let key =
        crate::http::idempotency_key(&headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let receipt = service
        .request_import(
            &request,
            &hex::encode(Sha256::digest(&body)),
            key,
            &ctx.mutation(),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

async fn export(
    State(state): State<AdapterExportState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: larm_domain::adapter_packages::AdapterExport =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    request
        .validate()
        .map_err(|e| Failure::from(e).recorrelate(&ctx.correlation_id))?;
    let exporter = state.exporter.clone();
    let mutation = ctx.mutation();
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let receipt = exporter.export_package(&request, &mutation).await?;
            Ok((201, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}

pub fn update_routes(service: Arc<dyn larm_domain::ports::AdapterUpdater>) -> Router {
    Router::new()
        .route("/api/v1/adapters/{id}/update", post(update))
        .with_state(service)
}

pub fn probe_routes(service: Arc<dyn larm_domain::ports::AdapterProber>) -> Router {
    Router::new()
        .route("/api/v1/adapters/{id}/probe", post(probe))
        .with_state(service)
}
async fn probe(
    State(service): State<Arc<dyn larm_domain::ports::AdapterProber>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    use sha2::{Digest, Sha256};
    let fail = |e: ApiError| Failure::from(e).recorrelate(&ctx.correlation_id);
    crate::http::uuid_param(&id, &headers)?;
    let request: larm_domain::hardware::Probe =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    larm_domain::adapter_probes::validate_request(&id, &request).map_err(fail)?;
    let key =
        crate::http::idempotency_key(&headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let receipt = service
        .request_probe(
            &id,
            &request,
            &hex::encode(Sha256::digest(&body)),
            key,
            &ctx.mutation(),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

async fn update(
    State(service): State<Arc<dyn larm_domain::ports::AdapterUpdater>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    use sha2::{Digest, Sha256};
    crate::http::uuid_param(&id, &headers)?;
    let fail = |e: ApiError| Failure::from(e).recorrelate(&ctx.correlation_id);
    let request: larm_domain::adapter_updates::AdapterImport =
        strict(&body, &headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail)?;
    let key =
        crate::http::idempotency_key(&headers).map_err(|e| e.recorrelate(&ctx.correlation_id))?;
    let receipt = service
        .request_update(
            &id,
            &request,
            &hex::encode(Sha256::digest(&body)),
            key,
            &ctx.mutation(),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

pub fn catalog_routes(state: AdapterCatalogState) -> Router {
    Router::new()
        .route("/api/v1/adapters", get(list))
        .route("/api/v1/adapters/{id}", get(detail))
        .with_state(state)
}

async fn list(
    State(state): State<AdapterCatalogState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let fail = |e: ApiError| Failure::from(e).recorrelate(&ctx.correlation_id);
    let (cursor, limit) = crate::http::list_params(&params, &["q", "state"]).map_err(fail)?;
    let filter = AdapterFilter {
        q: params.get("q").cloned(),
        state: params
            .get("state")
            .map(|value| {
                serde_json::from_value(Value::String(value.clone())).map_err(|_| {
                    ApiError::new(
                        "VALIDATION_FAILED",
                        "Estado de adaptador desconocido",
                        "adapter_state_invalid",
                    )
                })
            })
            .transpose()
            .map_err(fail)?,
    };
    let digest = larm_domain::canonical::sha256_hex(
        &serde_json::json!({"list":"adapters", "q":filter.q,"state":filter.state}),
    )
    .map_err(|_| {
        fail(ApiError::new(
            "VALIDATION_FAILED",
            "Filtros no válidos",
            "adapter_filter_invalid",
        ))
    })?;
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |a: &larm_domain::adapters::AdapterInstallation| (a.created_at.clone(), a.id.clone()),
            |after, limit| state.repo.page(after, limit, &filter),
        )
        .await
        .map_err(fail)?;
    Ok(json(StatusCode::OK, &page))
}

async fn detail(
    State(state): State<AdapterCatalogState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let fail = |e: ApiError| Failure::from(e).recorrelate(&ctx.correlation_id);
    let installation = state.repo.get(&id).await.map_err(fail)?.ok_or_else(|| {
        fail(
            ApiError::new(
                "NOT_FOUND",
                "Instalación de adaptador desconocida",
                "not_found",
            )
            .with_resource(id),
        )
    })?;
    Ok(json(StatusCode::OK, &installation))
}

pub fn routes(state: AdaptersState) -> Router {
    Router::new()
        .route("/api/v1/adapters/declarative", post(create))
        .with_state(state)
}

async fn create(
    State(state): State<AdaptersState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let document: Value =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let (registry, mutation) = (state.registry.clone(), ctx.mutation());
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let created = registry.register(&document, &mutation).await?;
            Ok((201, serde_json::to_value(created).unwrap_or_default()))
        },
    )
    .await
}
