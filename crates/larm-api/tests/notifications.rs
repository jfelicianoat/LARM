//! HTTP P113: autorización, contrato estricto, paginación y CAS de lectura.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::{
    auth::{ApiScope, PrincipalKind},
    error::LarmError,
    ports::{MutationContext, NotificationRepository, PrincipalAuthenticator},
    watchers::Notification,
};
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct MemoryNotifications(Mutex<Notification>);

#[async_trait]
impl NotificationRepository for MemoryNotifications {
    async fn notification(&self, id: &str) -> Result<Option<Notification>, LarmError> {
        let row = self.0.lock();
        Ok((row.id == id).then(|| row.clone()))
    }

    async fn page_notifications(
        &self,
        _after: Option<(String, String)>,
        _limit: u32,
    ) -> Result<Vec<Notification>, LarmError> {
        Ok(vec![self.0.lock().clone()])
    }

    async fn set_notification_read(
        &self,
        id: &str,
        if_match: i64,
        read: bool,
        _ctx: &MutationContext,
    ) -> Result<Notification, LarmError> {
        let mut row = self.0.lock();
        if row.id != id {
            return Err(LarmError::new(
                larm_domain::error::ErrorCode::NotFound,
                "No existe",
                "not_found",
            ));
        }
        if row.row_version != if_match {
            return Err(LarmError::new(
                larm_domain::error::ErrorCode::StaleRevision,
                "Cambió",
                "stale_revision",
            ));
        }
        if row.read_at.is_some() != read {
            row.row_version += 1;
            row.read_at = read.then(|| larm_domain::bridge::timestamp(chrono::Utc::now()));
        }
        Ok(row.clone())
    }
}

fn notification() -> Notification {
    Notification {
        id: "0192f000-0000-7000-8000-000000000321".into(),
        created_at: "2026-10-25T10:00:00.000Z".into(),
        updated_at: "2026-10-25T10:00:00.000Z".into(),
        row_version: 1,
        metadata: Default::default(),
        dedupe_key: "disk-transition".into(),
        severity: "warning".into(),
        message_code: "LOW_DISK_SPACE".into(),
        resource_id: None,
        action: None,
        read_at: None,
    }
}

async fn harness() -> (axum::Router, String, String, String) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "notifications-test".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "writer",
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![],
            Some("writer-token-notifications-123456".into()),
            &ctx,
        )
        .await
        .unwrap();
    let (_, reader) = principals
        .enroll(
            PrincipalKind::Operator,
            "reader",
            vec![ApiScope::Read],
            vec![],
            Some("reader-token-notifications-123456".into()),
            &ctx,
        )
        .await
        .unwrap();
    let id = notification().id;
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let router = larm_api::notifications::routes(larm_api::notifications::NotificationsState {
        repository: Arc::new(MemoryNotifications(Mutex::new(notification()))),
        cursors: Arc::new(CursorSigner::default()),
    })
    .layer(axum::middleware::from_fn_with_state(
        auth,
        larm_api::security::guard,
    ));
    (router, writer, reader, id)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&str>,
    if_match: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(version) = if_match {
        request = request.header("if-match", version);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_owned())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn notification_routes_enforce_scope_strict_patch_and_if_match() {
    let (app, writer, reader, id) = harness().await;
    let (status, page) = call(
        &app,
        "GET",
        "/api/v1/notifications?limit=10",
        &reader,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], id);

    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        &reader,
        Some(r#"{"read":true}"#),
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        &writer,
        Some(r#"{"read":true,"dismiss":true}"#),
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, updated) = call(
        &app,
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        &writer,
        Some(r#"{"read":true}"#),
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["row_version"], 2);
    assert!(updated["read_at"].is_string());
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        &writer,
        Some(r#"{"read":false}"#),
        Some("1"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}
