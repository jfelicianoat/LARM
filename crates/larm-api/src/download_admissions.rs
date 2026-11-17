//! P119: API de la política opt-in de descargas automáticas y su historial de
//! admisiones. La política es un documento singleton versionado (PUT con
//! If-Match, GET read); las admisiones se listan paginadas y la revocación es
//! inmediata (POST revoke sobre una admisión ADMITTED). El acceso a datos pasa
//! por el trait [`AdmissionStore`] — el daemon compone el repositorio SQL —
//! para mantener larm-api sin dependencia de SQLite.
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::download_admission::DownloadAdmissionPolicy;
use larm_domain::error::LarmError;
use larm_domain::ports::MutationContext;
use serde_json::{json, Value};

use crate::http::{json as http_json, strict, Answer, Failure};
use crate::security::AuthContext;

/// Contrato del store de política/admisiones que el daemon compone (SQL real).
/// La API sólo ve este trait: sin acceso directo a SQLite desde larm-api.
#[async_trait::async_trait]
pub trait AdmissionStore: Send + Sync {
    /// Devuelve `None` si la política nunca fue registrada.
    async fn load(&self) -> Result<Option<(DownloadAdmissionPolicy, i64, String)>, LarmError>;
    /// Guarda la política y devuelve `(row_version, updated_at)`.
    async fn save(
        &self,
        policy: &DownloadAdmissionPolicy,
        ctx: &MutationContext,
    ) -> Result<(i64, String), LarmError>;
    /// Historial paginado como documentos `download_admission`.
    async fn list(&self, limit: u32) -> Result<Vec<Value>, LarmError>;
    /// Revocación inmediata de una admisión ADMITTED.
    async fn revoke(&self, id: &str, ctx: &MutationContext) -> Result<Value, LarmError>;
}

#[derive(Clone)]
pub struct AdmissionState {
    pub store: Arc<dyn AdmissionStore>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: AdmissionState) -> Router {
    Router::new()
        .route(
            "/api/v1/download-admission-policy",
            get(get_policy).put(put_policy),
        )
        .route("/api/v1/download-admissions", get(list_admissions))
        .route(
            "/api/v1/download-admissions/{id}/revoke",
            post(revoke_admission),
        )
        .with_state(state)
}

fn policy_body(policy: &DownloadAdmissionPolicy, row_version: i64, updated_at: &str) -> Value {
    let mut value = serde_json::to_value(policy).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("row_version".into(), json!(row_version));
        object.insert("updated_at".into(), json!(updated_at));
    }
    value
}

fn map_failure(ctx: &AuthContext, error: LarmError) -> Failure {
    Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn stale_error() -> LarmError {
    LarmError::new(
        larm_domain::error::ErrorCode::Conflict,
        "La política cambió desde la lectura",
        "admission_policy_stale",
    )
    .with_resource("download-admission-policy")
}

async fn get_policy(
    State(state): State<AdmissionState>,
    Extension(ctx): Extension<AuthContext>,
) -> Answer {
    let stored = match state.store.load().await {
        Ok(stored) => stored,
        Err(error) => return Err(map_failure(&ctx, error)),
    };
    match stored {
        Some((policy, row_version, updated_at)) => Ok(http_json(
            StatusCode::OK,
            &policy_body(&policy, row_version, &updated_at),
        )),
        None => {
            // Sin política registrada: documento opt-in apagado por defecto.
            Ok(http_json(
                StatusCode::OK,
                &policy_body(
                    &DownloadAdmissionPolicy {
                        enabled: false,
                        source_allowlist: vec![],
                        capability_allowlist: vec![],
                        license_allowlist: vec![],
                        min_trust: "CANDIDATE".into(),
                        daily_quota_bytes: None,
                        storage_location_id: None,
                    },
                    0,
                    &larm_domain::bridge::timestamp(chrono::Utc::now()),
                ),
            ))
        }
    }
}

async fn put_policy(
    State(state): State<AdmissionState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let policy: DownloadAdmissionPolicy =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let expected = crate::http::if_match(&headers).ok();
    let existing = match state.store.load().await {
        Ok(existing) => existing,
        Err(error) => return Err(map_failure(&ctx, error)),
    };
    match (&existing, expected) {
        (Some((_, stored_version, _)), Some(version)) if *stored_version != version as i64 => {
            return Err(map_failure(&ctx, stale_error()));
        }
        (None, Some(_)) => return Err(map_failure(&ctx, stale_error())),
        _ => {}
    }
    let (row_version, updated_at) = match state.store.save(&policy, &ctx.mutation()).await {
        Ok(saved) => saved,
        Err(error) => return Err(map_failure(&ctx, error)),
    };
    Ok(http_json(
        StatusCode::OK,
        &policy_body(&policy, row_version, &updated_at),
    ))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionQuery {
    limit: Option<u32>,
}

async fn list_admissions(
    State(state): State<AdmissionState>,
    Extension(ctx): Extension<AuthContext>,
    Query(query): Query<AdmissionQuery>,
) -> Answer {
    let limit = query.limit.unwrap_or(100).min(200);
    let admissions = match state.store.list(limit).await {
        Ok(admissions) => admissions,
        Err(error) => return Err(map_failure(&ctx, error)),
    };
    Ok(http_json(
        StatusCode::OK,
        &serde_json::json!({
            "items": admissions,
            "next_cursor": null,
            "snapshot_at": larm_domain::bridge::timestamp(chrono::Utc::now()),
        }),
    ))
}

async fn revoke_admission(
    State(state): State<AdmissionState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let _: Value = strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let admission = match state.store.revoke(&id, &ctx.mutation()).await {
        Ok(admission) => admission,
        Err(error) => return Err(map_failure(&ctx, error)),
    };
    Ok(http_json(StatusCode::OK, &admission))
}
