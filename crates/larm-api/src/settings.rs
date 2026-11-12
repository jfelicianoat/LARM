//! P003 Settings API: revision CAS, strict validation and live H08 updates.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use larm_core::config::{LowDiskThresholds, Settings};
use larm_domain::bridge::ApiError;
use larm_domain::locations::DiskThresholds;
use larm_domain::ports::{SettingsRepository, SettingsSnapshot};
use larm_domain::units::DecimalU64;
use serde_json::Value;

use crate::{
    http::{Answer, Failure},
    security::AuthContext,
};

pub type BandwidthLimitApplier = Arc<dyn Fn(Option<DecimalU64>) + Send + Sync>;
pub type LowDiskThresholdApplier = Arc<dyn Fn(DiskThresholds, i64) + Send + Sync>;

#[derive(Clone)]
pub struct SettingsState {
    pub repository: Arc<dyn SettingsRepository>,
    /// Initial persistent base (defaults plus config file) used only on first start.
    pub initial: Value,
    pub file_overrides: Value,
    pub environment_overrides: Value,
    pub cli_overrides: Value,
    pub apply_bandwidth_limit: BandwidthLimitApplier,
    /// Actualiza el sondeo H09 tras guardar una revisión nueva de umbrales.
    pub apply_low_disk_thresholds: LowDiskThresholdApplier,
}

pub fn routes(state: SettingsState) -> Router {
    Router::new()
        .route("/api/v1/settings", get(read).patch(update))
        .with_state(state)
}

async fn snapshot(state: &SettingsState) -> Result<SettingsSnapshot, ApiError> {
    state
        .repository
        .get_or_initialize(state.initial.clone())
        .await
}

fn effective(state: &SettingsState, payload: &Value) -> Result<Settings, ApiError> {
    Settings::compose_with_base(
        Some(payload),
        Some(&state.file_overrides),
        Some(&state.environment_overrides),
        Some(&state.cli_overrides),
    )
}

fn thresholds(settings: &Settings) -> DiskThresholds {
    match &settings.storage.low_disk_thresholds {
        LowDiskThresholds::Percent {
            warning_basis_points,
            critical_basis_points,
        } => DiskThresholds::Percent {
            warning_bp: u64::from(*warning_basis_points),
            critical_bp: u64::from(*critical_basis_points),
        },
        LowDiskThresholds::Bytes {
            warning_bytes,
            critical_bytes,
        } => DiskThresholds::Bytes {
            warning: warning_bytes.get(),
            critical: critical_bytes.get(),
        },
    }
}

fn response(status: StatusCode, revision: i64, settings: &Settings) -> Response {
    let mut response = crate::http::json(status, settings);
    if let Ok(value) = HeaderValue::from_str(&format!("\"{revision}\"")) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

async fn read(
    State(state): State<SettingsState>,
    Extension(ctx): Extension<AuthContext>,
) -> Answer {
    let fail = |error: ApiError| Failure::from(error).recorrelate(&ctx.correlation_id);
    let current = snapshot(&state).await.map_err(fail)?;
    let settings = effective(&state, &current.payload).map_err(fail)?;
    Ok(response(StatusCode::OK, current.revision, &settings))
}

async fn update(
    State(state): State<SettingsState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let fail = |error: ApiError| Failure::from(error).recorrelate(&ctx.correlation_id);
    let expected = crate::http::if_match(&headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected = i64::try_from(expected).map_err(|_| {
        crate::http::Failure::from(ApiError::new(
            "VALIDATION_FAILED",
            "If-Match debe ser la revisión de Settings",
            "if_match_invalid",
        ))
        .recorrelate(&ctx.correlation_id)
    })?;
    let request: Settings = crate::http::strict(&body, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail)?;
    let before = snapshot(&state).await.map_err(fail)?;
    let before = effective(&state, &before.payload).map_err(fail)?;
    let payload = serde_json::to_value(&request).map_err(|error| {
        fail(
            ApiError::new(
                "INTERNAL",
                "No se pudo preparar Settings",
                "settings_encode_failed",
            )
            .with_technical(error.to_string()),
        )
    })?;
    let saved = state
        .repository
        .update(expected, payload, &ctx.mutation())
        .await
        .map_err(fail)?;
    let settings = effective(&state, &saved.payload).map_err(fail)?;
    let thresholds_changed =
        before.storage.low_disk_thresholds != settings.storage.low_disk_thresholds;
    (state.apply_bandwidth_limit)(settings.storage.download_bandwidth_limit_bytes_per_second);
    if thresholds_changed {
        (state.apply_low_disk_thresholds)(thresholds(&settings), saved.revision);
    }
    Ok(response(StatusCode::OK, saved.revision, &settings))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;
    use axum::{body::Body, http::Request};
    use larm_domain::auth::{Principal, PrincipalKind};
    use std::{collections::BTreeSet, sync::Mutex};
    use tower::ServiceExt;

    #[derive(Default)]
    struct MemorySettings(Mutex<Option<SettingsSnapshot>>);

    #[async_trait::async_trait]
    impl SettingsRepository for MemorySettings {
        async fn get_or_initialize(&self, initial: Value) -> Result<SettingsSnapshot, ApiError> {
            let mut current = self
                .0
                .lock()
                .map_err(|_| ApiError::new("INTERNAL", "test mutex", "test_mutex"))?;
            Ok(current
                .get_or_insert(SettingsSnapshot {
                    payload: initial,
                    revision: 1,
                })
                .clone())
        }

        async fn update(
            &self,
            expected_revision: i64,
            payload: Value,
            _ctx: &larm_domain::ports::MutationContext,
        ) -> Result<SettingsSnapshot, ApiError> {
            let mut current = self
                .0
                .lock()
                .map_err(|_| ApiError::new("INTERNAL", "test mutex", "test_mutex"))?;
            let saved = current
                .as_mut()
                .ok_or_else(|| ApiError::new("INTERNAL", "missing", "settings_missing"))?;
            if saved.revision != expected_revision {
                return Err(ApiError::new("STALE_REVISION", "stale", "stale_revision"));
            }
            saved.revision += 1;
            saved.payload = payload;
            Ok(saved.clone())
        }
    }

    fn app(applied: Arc<std::sync::atomic::AtomicU64>) -> Router {
        let initial = serde_json::to_value(Settings::defaults()).unwrap();
        routes(SettingsState {
            repository: Arc::new(MemorySettings::default()),
            initial,
            file_overrides: serde_json::json!({}),
            environment_overrides: serde_json::json!({}),
            cli_overrides: serde_json::json!({}),
            apply_bandwidth_limit: Arc::new(move |limit| {
                applied.store(
                    limit.map_or(0, DecimalU64::get),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }),
            apply_low_disk_thresholds: Arc::new(|_, _| {}),
        })
        .layer(Extension(crate::security::AuthContext {
            principal: Principal {
                id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103".into(),
                name: "test".into(),
                kind: PrincipalKind::Operator,
                api_scopes: BTreeSet::new(),
                mcp_scopes: BTreeSet::new(),
            },
            operation_id: "P003_patch__settings".into(),
            correlation_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a104".into(),
        }))
    }

    fn app_with_threshold_hook(hook: Arc<Mutex<Vec<(DiskThresholds, i64)>>>) -> Router {
        let initial = serde_json::to_value(Settings::defaults()).unwrap();
        routes(SettingsState {
            repository: Arc::new(MemorySettings::default()),
            initial,
            file_overrides: serde_json::json!({}),
            environment_overrides: serde_json::json!({}),
            cli_overrides: serde_json::json!({}),
            apply_bandwidth_limit: Arc::new(|_| {}),
            apply_low_disk_thresholds: Arc::new(move |thresholds, revision| {
                hook.lock().unwrap().push((thresholds, revision));
            }),
        })
        .layer(Extension(crate::security::AuthContext {
            principal: Principal {
                id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103".into(),
                name: "test".into(),
                kind: PrincipalKind::Operator,
                api_scopes: BTreeSet::new(),
                mcp_scopes: BTreeSet::new(),
            },
            operation_id: "P003_patch__settings".into(),
            correlation_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a104".into(),
        }))
    }

    async fn send(
        app: Router,
        method: &str,
        revision: Option<&str>,
        body: Option<String>,
    ) -> Response {
        let mut builder = Request::builder().method(method).uri("/api/v1/settings");
        if let Some(revision) = revision {
            builder = builder.header(header::IF_MATCH, revision);
        }
        app.oneshot(
            builder
                .body(body.map_or_else(Body::empty, Body::from))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn etag_cas_and_bandwidth_update_apply_live() {
        let applied = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let app = app(applied.clone());
        let read = send(app.clone(), "GET", None, None).await;
        assert_eq!(read.status(), StatusCode::OK);
        assert_eq!(read.headers()[header::ETAG], "\"1\"");

        let mut settings = Settings::defaults();
        settings.storage.download_bandwidth_limit_bytes_per_second = DecimalU64::new(4096);
        let saved = send(
            app.clone(),
            "PATCH",
            Some("1"),
            Some(serde_json::to_string(&settings).unwrap()),
        )
        .await;
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(saved.headers()[header::ETAG], "\"2\"");
        assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 4096);

        let stale = send(
            app,
            "PATCH",
            Some("1"),
            Some(serde_json::to_string(&settings).unwrap()),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn patch_requires_if_match_and_rejects_zero_bandwidth() {
        let app = app(Arc::new(std::sync::atomic::AtomicU64::new(0)));
        let settings = Settings::defaults();
        let missing = send(
            app.clone(),
            "PATCH",
            None,
            Some(serde_json::to_string(&settings).unwrap()),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::PRECONDITION_REQUIRED);

        let mut invalid_settings = settings;
        invalid_settings
            .storage
            .download_bandwidth_limit_bytes_per_second = DecimalU64::new(0);
        let invalid = send(
            app,
            "PATCH",
            Some("1"),
            Some(serde_json::to_string(&invalid_settings).unwrap()),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn changing_h09_thresholds_applies_the_saved_revision() {
        let hook = Arc::new(Mutex::new(Vec::new()));
        let app = app_with_threshold_hook(hook.clone());
        let mut settings = Settings::defaults();
        settings.storage.low_disk_thresholds = LowDiskThresholds::Bytes {
            warning_bytes: DecimalU64::new(100).unwrap(),
            critical_bytes: DecimalU64::new(10).unwrap(),
        };
        let response = send(
            app,
            "PATCH",
            Some("1"),
            Some(serde_json::to_string(&settings).unwrap()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *hook.lock().unwrap(),
            vec![(
                DiskThresholds::Bytes {
                    warning: 100,
                    critical: 10
                },
                2
            )]
        );
    }

    #[tokio::test]
    async fn effective_settings_matches_startup_with_file_and_higher_overrides() {
        let mut persisted = Settings::defaults();
        persisted.storage.download_bandwidth_limit_bytes_per_second = DecimalU64::new(65536);
        let payload = serde_json::to_value(&persisted).unwrap();
        let cases = [
            (
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":null}}),
                serde_json::json!({}),
                serde_json::json!({}),
            ),
            (
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"8192"}}),
                serde_json::json!({}),
                serde_json::json!({}),
            ),
            (
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":null}}),
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"16384"}}),
                serde_json::json!({}),
            ),
            (
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"8192"}}),
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"16384"}}),
                serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"32768"}}),
            ),
            (
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({}),
            ),
        ];
        for (file, env, cli) in cases {
            let expected =
                Settings::compose_with_base(Some(&payload), Some(&file), Some(&env), Some(&cli))
                    .unwrap();
            let state = SettingsState {
                repository: Arc::new(MemorySettings::default()),
                initial: payload.clone(),
                file_overrides: file,
                environment_overrides: env,
                cli_overrides: cli,
                apply_bandwidth_limit: Arc::new(|_| {}),
                apply_low_disk_thresholds: Arc::new(|_, _| {}),
            };
            assert_eq!(
                serde_json::to_value(effective(&state, &payload).unwrap()).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn patch_applies_effective_file_limit_and_keeps_persisted_intent() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let repository = Arc::new(MemorySettings::default());
        let applied = Arc::new(AtomicU64::new(u64::MAX));
        let captured = applied.clone();
        let threshold_updates = Arc::new(AtomicU64::new(0));
        let captured_thresholds = threshold_updates.clone();
        let mut state = SettingsState {
            repository: repository.clone(),
            initial: serde_json::to_value(Settings::defaults()).unwrap(),
            file_overrides: serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":null,"low_disk_thresholds":{"mode":"BYTES","warning_bytes":"2","critical_bytes":"1"}}}),
            environment_overrides: serde_json::json!({}),
            cli_overrides: serde_json::json!({}),
            apply_bandwidth_limit: Arc::new(move |limit| {
                captured.store(limit.map_or(0, DecimalU64::get), Ordering::SeqCst)
            }),
            apply_low_disk_thresholds: Arc::new(move |_, _| {
                captured_thresholds.fetch_add(1, Ordering::SeqCst);
            }),
        };
        let mut request = Settings::defaults();
        request.storage.download_bandwidth_limit_bytes_per_second = DecimalU64::new(65536);
        let ctx = AuthContext {
            principal: Principal {
                id: uuid::Uuid::now_v7().to_string(),
                name: "file precedence test".into(),
                kind: PrincipalKind::Operator,
                api_scopes: BTreeSet::new(),
                mcp_scopes: BTreeSet::new(),
            },
            operation_id: "P003_patch__settings".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let response = send(
            routes(state.clone()).layer(Extension(ctx)),
            "PATCH",
            Some("1"),
            Some(serde_json::to_string(&request).unwrap()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let response_body: Value = serde_json::from_slice(&response_body).unwrap();
        assert!(response_body["storage"]["download_bandwidth_limit_bytes_per_second"].is_null());
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        assert_eq!(threshold_updates.load(Ordering::SeqCst), 0);
        assert_eq!(
            response_body["storage"]["low_disk_thresholds"]["mode"],
            "BYTES"
        );
        let saved = snapshot(&state).await.unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(
            saved.payload["storage"]["download_bandwidth_limit_bytes_per_second"],
            "65536"
        );
        state.file_overrides = serde_json::json!({});
        assert_eq!(
            effective(&state, &saved.payload)
                .unwrap()
                .storage
                .download_bandwidth_limit_bytes_per_second,
            DecimalU64::new(65536)
        );
    }
}
