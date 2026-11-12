//! API P113 para consultar notificaciones y marcar leído/no leído con CAS.

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::error::ApiError;
use larm_domain::ports::NotificationRepository;
use larm_domain::watchers::Notification;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct NotificationsState {
    pub repository: Arc<dyn NotificationRepository>,
    pub cursors: Arc<CursorSigner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkNotificationRead {
    read: bool,
}

pub fn routes(state: NotificationsState) -> Router {
    Router::new()
        .route("/api/v1/notifications", get(list))
        .route("/api/v1/notifications/{id}", get(read).patch(mark_read))
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |error| Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn version(headers: &HeaderMap, ctx: &AuthContext) -> Result<i64, Failure> {
    i64::try_from(
        crate::http::if_match(headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?,
    )
    .map_err(|_| {
        fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })
}

async fn list(
    State(state): State<NotificationsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    let page = state
        .cursors
        .page(
            "notifications",
            cursor.as_deref(),
            limit,
            |notification: &Notification| {
                (notification.created_at.clone(), notification.id.clone())
            },
            |after, count| state.repository.page_notifications(after, count),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read(
    State(state): State<NotificationsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let notification = state
        .repository
        .notification(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| {
            fail(&ctx)(ApiError::new(
                "NOT_FOUND",
                "Notificación desconocida",
                "not_found",
            ))
        })?;
    Ok(json(StatusCode::OK, &notification))
}

async fn mark_read(
    State(state): State<NotificationsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let if_match = version(&headers, &ctx)?;
    let request: MarkNotificationRead =
        strict(&body, &headers).map_err(|error| error.recorrelate(&ctx.correlation_id))?;
    let notification = state
        .repository
        .set_notification_read(&id, if_match, request.read, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &notification))
}
