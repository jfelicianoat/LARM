//! HTTP P050: permisos, DTO estricto, paginación, replay idempotente y CAS.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::{
    auth::{ApiScope, PrincipalKind},
    error::{ErrorCode, LarmError},
    ports::{MutationContext, PrincipalAuthenticator, SourceWatchRepository},
    watchers::{SourceWatch, SourceWatchCreate, SourceWatchSyncFailure, SourceWatchSyncSuccess},
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

type IdempotencyKey = (String, String, String);
type IdempotencyValue = (String, SourceWatch);
type IdempotencyRows = BTreeMap<IdempotencyKey, IdempotencyValue>;

#[derive(Default)]
struct MemoryWatches {
    rows: Mutex<BTreeMap<String, SourceWatch>>,
    idempotency: Mutex<IdempotencyRows>,
}

#[async_trait]
impl SourceWatchRepository for MemoryWatches {
    async fn source_watch(&self, id: &str) -> Result<Option<SourceWatch>, LarmError> {
        Ok(self.rows.lock().get(id).cloned())
    }

    async fn page_source_watches(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<SourceWatch>, LarmError> {
        let mut rows: Vec<_> = self.rows.lock().values().cloned().collect();
        rows.sort_by(|left, right| {
            (&left.created_at, &left.id).cmp(&(&right.created_at, &right.id))
        });
        if let Some(after) = after {
            rows.retain(|row| (&row.created_at, &row.id) > (&after.0, &after.1));
        }
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn create_source_watch(
        &self,
        request: &SourceWatchCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        let idempotency_key = (
            ctx.actor_id.clone(),
            "P050_post__source_watches".into(),
            key.into(),
        );
        if let Some((previous_hash, previous)) =
            self.idempotency.lock().get(&idempotency_key).cloned()
        {
            if previous_hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Clave ya usada",
                    "idempotency_body_mismatch",
                ));
            }
            return Ok(previous);
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let id = uuid::Uuid::now_v7().to_string();
        let watch = SourceWatch {
            id: id.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            row_version: 1,
            metadata: Default::default(),
            source_id: request.source_id.clone(),
            query: request.query.clone(),
            cursor: None,
            etag: None,
            interval_seconds: request.interval_seconds,
            policy: request.policy.clone(),
            next_run_at: request.enabled.then_some(now),
            enabled: request.enabled,
        };
        self.rows.lock().insert(id, watch.clone());
        self.idempotency
            .lock()
            .insert(idempotency_key, (body_hash.into(), watch.clone()));
        Ok(watch)
    }

    async fn update_source_watch(
        &self,
        id: &str,
        if_match: i64,
        request: &SourceWatchCreate,
        _ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        let mut rows = self.rows.lock();
        let watch = rows
            .get_mut(id)
            .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "No existe", "not_found"))?;
        if watch.row_version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "Cambió",
                "stale_revision",
            ));
        }
        watch.row_version += 1;
        watch.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        watch.source_id = request.source_id.clone();
        watch.query = request.query.clone();
        watch.interval_seconds = request.interval_seconds;
        watch.policy = request.policy.clone();
        watch.enabled = request.enabled;
        watch.next_run_at = request.enabled.then_some(watch.updated_at.clone());
        Ok(watch.clone())
    }

    async fn delete_source_watch(
        &self,
        id: &str,
        if_match: i64,
        _ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut rows = self.rows.lock();
        let watch = rows
            .get(id)
            .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "No existe", "not_found"))?;
        if watch.row_version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "Cambió",
                "stale_revision",
            ));
        }
        rows.remove(id);
        Ok(())
    }

    async fn due_source_watches(
        &self,
        _now: &str,
        limit: u32,
    ) -> Result<Vec<SourceWatch>, LarmError> {
        Ok(self
            .rows
            .lock()
            .values()
            .filter(|watch| watch.enabled)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn recover_interrupted_source_syncs(
        &self,
        _recovered_at: &str,
        _ctx: &MutationContext,
    ) -> Result<u64, LarmError> {
        Ok(0)
    }

    async fn start_source_watch_sync(
        &self,
        _id: &str,
        _if_match: i64,
        _started_at: &str,
        _ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        Ok(())
    }

    async fn record_source_watch_success(
        &self,
        _id: &str,
        _if_match: i64,
        _commit: &SourceWatchSyncSuccess,
        _ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "No implementado",
            "test_only",
        ))
    }

    async fn record_source_watch_failure(
        &self,
        _id: &str,
        _if_match: i64,
        _commit: &SourceWatchSyncFailure,
        _ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "No implementado",
            "test_only",
        ))
    }
}

async fn harness() -> (axum::Router, String, String) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "api-test".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "writer",
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![],
            Some("writer-token-source-watches-123456".into()),
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
            Some("reader-token-source-watches-123456".into()),
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let router = larm_api::source_watches::routes(larm_api::source_watches::SourceWatchesState {
        repository: Arc::new(MemoryWatches::default()),
        cursors: Arc::new(CursorSigner::default()),
    })
    .layer(axum::middleware::from_fn_with_state(
        auth,
        larm_api::security::guard,
    ));
    (router, writer, reader)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&str>,
    key: Option<&str>,
    if_match: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(version) = if_match {
        request = request.header("if-match", version);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_owned())))
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
async fn source_watch_routes_enforce_contract_scope_dto_replay_and_if_match() {
    let (app, writer, reader) = harness().await;
    let body = json!({
        "source_id":"0192f000-0000-7000-8000-000000000301",
        "query":{"q":"fixture"}, "interval_seconds":21600, "policy":{}, "enabled":true
    })
    .to_string();
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/source-watches",
        &reader,
        Some(&body),
        Some("reader-key"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, watch) = call(
        &app,
        "POST",
        "/api/v1/source-watches",
        &writer,
        Some(&body),
        Some("watch-create-once"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{watch}");
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/source-watches",
        &writer,
        Some(&body),
        Some("watch-create-once"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay["id"], watch["id"]);
    let (status, page) = call(
        &app,
        "GET",
        "/api/v1/source-watches?limit=1",
        &reader,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], watch["id"]);
    let changed = json!({
        "source_id":watch["source_id"], "query":{"q":"updated"}, "interval_seconds":21600, "policy":{}, "enabled":false
    }).to_string();
    let uri = format!("/api/v1/source-watches/{}", watch["id"].as_str().unwrap());
    let (status, updated) = call(
        &app,
        "PATCH",
        &uri,
        &writer,
        Some(&changed),
        None,
        Some("\"1\""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["row_version"], 2);
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        &writer,
        Some(&changed),
        None,
        Some("\"1\""),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "DELETE", &uri, &writer, None, None, Some("\"2\"")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let invalid = json!({
        "source_id":"0192f000-0000-7000-8000-000000000301", "query":{}, "interval_seconds":21600, "policy":{}, "enabled":true, "cursor":"forged"
    }).to_string();
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/source-watches",
        &writer,
        Some(&invalid),
        Some("invalid-watch"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
