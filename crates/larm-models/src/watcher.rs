//! Planificación y detección incremental de cambios en fuentes (P050).

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use larm_adapter_sdk::source::{ModelInspection, SyncRequest, SyncResult};
use larm_domain::{
    bridge::{timestamp, ApiError},
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    sources::{SourceOperationalStatus, SourceRateLimit},
    watchers::{SourceWatch, SourceWatchSyncFailure, SourceWatchSyncSuccess, WatchDetection},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;

const MAX_BACKOFF_SECONDS: u64 = 86_400;
const DEFAULT_JITTER_MAX_SECONDS: u64 = 300;

#[async_trait]
pub trait SourceWatchStore: Send + Sync {
    async fn due(&self, now: &str, limit: u32) -> Result<Vec<SourceWatch>, ApiError>;
    async fn recover_interrupted(
        &self,
        recovered_at: &str,
        ctx: &MutationContext,
    ) -> Result<u64, ApiError>;
    async fn started(
        &self,
        watch: &SourceWatch,
        started_at: &str,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn succeeded(
        &self,
        watch: &SourceWatch,
        success: &SourceWatchSyncSuccess,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn failed(
        &self,
        watch: &SourceWatch,
        failure: &SourceWatchSyncFailure,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
}

#[async_trait]
pub trait SourceSyncExecutor: Send + Sync {
    async fn sync(&self, request: SyncRequest) -> Result<SyncResult, LarmError>;
}

#[async_trait]
pub trait WatchCompatibility: Send + Sync {
    /// `None` conserva compatibilidad desconocida y nunca dispara la alerta
    /// específica de modelos compatibles.
    async fn compatible(
        &self,
        watch: &SourceWatch,
        model: &ModelInspection,
    ) -> Result<Option<bool>, LarmError>;
}

#[derive(Default)]
pub struct UnknownWatchCompatibility;

#[async_trait]
impl WatchCompatibility for UnknownWatchCompatibility {
    async fn compatible(
        &self,
        _watch: &SourceWatch,
        _model: &ModelInspection,
    ) -> Result<Option<bool>, LarmError> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchRunSummary {
    pub due: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub stale: usize,
}

pub struct SourceWatchWorker {
    store: Arc<dyn SourceWatchStore>,
    source: Arc<dyn SourceSyncExecutor>,
    compatibility: Arc<dyn WatchCompatibility>,
    jitter_max_seconds: u64,
    max_backoff_seconds: u64,
}

impl SourceWatchWorker {
    #[must_use]
    pub fn new(
        store: Arc<dyn SourceWatchStore>,
        source: Arc<dyn SourceSyncExecutor>,
        compatibility: Arc<dyn WatchCompatibility>,
    ) -> Self {
        Self {
            store,
            source,
            compatibility,
            jitter_max_seconds: DEFAULT_JITTER_MAX_SECONDS,
            max_backoff_seconds: MAX_BACKOFF_SECONDS,
        }
    }

    #[must_use]
    pub fn with_schedule_limits(
        mut self,
        jitter_max_seconds: u64,
        max_backoff_seconds: u64,
    ) -> Self {
        self.jitter_max_seconds = jitter_max_seconds.min(DEFAULT_JITTER_MAX_SECONDS);
        self.max_backoff_seconds = max_backoff_seconds.clamp(60, MAX_BACKOFF_SECONDS);
        self
    }

    pub async fn run_due_once(
        &self,
        now: DateTime<Utc>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<WatchRunSummary, ApiError> {
        let now_text = timestamp(now);
        let watches = self.store.due(&now_text, limit).await?;
        let mut summary = WatchRunSummary {
            due: watches.len(),
            ..WatchRunSummary::default()
        };
        for watch in watches {
            match self.sync_watch(&watch, now, ctx).await {
                Ok(()) => summary.succeeded += 1,
                Err(WatchRunError::Stale) => summary.stale += 1,
                Err(WatchRunError::PersistedFailure) => summary.failed += 1,
                Err(WatchRunError::Store(error)) => return Err(error),
            }
        }
        Ok(summary)
    }

    pub async fn recover_interrupted(
        &self,
        recovered_at: &str,
        ctx: &MutationContext,
    ) -> Result<u64, ApiError> {
        self.store.recover_interrupted(recovered_at, ctx).await
    }

    async fn sync_watch(
        &self,
        watch: &SourceWatch,
        now: DateTime<Utc>,
        ctx: &MutationContext,
    ) -> Result<(), WatchRunError> {
        let started_at = timestamp(Utc::now());
        match self.store.started(watch, &started_at, ctx).await {
            Ok(()) => {}
            Err(error) if error.code == "STALE_REVISION" => return Err(WatchRunError::Stale),
            Err(error) => return Err(WatchRunError::Store(error)),
        }
        let request = SyncRequest {
            source_id: watch.source_id.clone(),
            cursor: watch.cursor.clone(),
            etag: watch.etag.clone(),
            query: watch.query.clone(),
        };
        match self.source.sync(request).await {
            Ok(result) => {
                let rate_limit_observation = result.rate_limit.as_ref().and_then(Option::as_ref);
                if let Some(rate_limit) = rate_limit_observation {
                    if let Err(error) = rate_limit.validate() {
                        return self
                            .persist_failure(
                                watch,
                                now,
                                ctx,
                                &error,
                                SourceOperationalStatus::Error,
                                None,
                            )
                            .await;
                    }
                }
                let rate_limit = rate_limit_observation.map(source_rate_limit);
                let detections = if result.changed {
                    match self.detections(watch, &result).await {
                        Ok(detections) => detections,
                        Err(error) => {
                            return self
                                .persist_failure(
                                    watch,
                                    now,
                                    ctx,
                                    &error,
                                    SourceOperationalStatus::Healthy,
                                    rate_limit.clone(),
                                )
                                .await;
                        }
                    }
                } else {
                    Vec::new()
                };
                let observed = timestamp(Utc::now());
                let next = add_seconds(now, success_delay(watch, self.jitter_max_seconds));
                let success = SourceWatchSyncSuccess {
                    cursor: result.cursor.clone(),
                    etag: result.etag.clone(),
                    observed_at: observed,
                    next_run_at: next,
                    detections,
                    rate_limit,
                };
                match self.store.succeeded(watch, &success, ctx).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.code == "STALE_REVISION" => Err(WatchRunError::Stale),
                    Err(error) => Err(WatchRunError::Store(error)),
                }
            }
            Err(error) => {
                let status = failure_status(&error);
                let rate_limit = larm_adapter_sdk::source::RateLimit::from_error(&error)
                    .as_ref()
                    .map(source_rate_limit);
                self.persist_failure(watch, now, ctx, &error, status, rate_limit)
                    .await
            }
        }
    }

    async fn persist_failure(
        &self,
        watch: &SourceWatch,
        now: DateTime<Utc>,
        ctx: &MutationContext,
        error: &LarmError,
        status: SourceOperationalStatus,
        rate_limit: Option<SourceRateLimit>,
    ) -> Result<(), WatchRunError> {
        let failures = failure_count(watch).saturating_add(1);
        let delay = failure_delay(
            watch.interval_seconds,
            failures,
            self.max_backoff_seconds,
            self.jitter_max_seconds,
            &watch.id,
        );
        let commit = SourceWatchSyncFailure {
            reason: stable_error_reason(error),
            retry_at: add_seconds(now, delay),
            observed_at: timestamp(Utc::now()),
            operational_status: status,
            rate_limit,
        };
        match self.store.failed(watch, &commit, ctx).await {
            Ok(()) => Err(WatchRunError::PersistedFailure),
            Err(store_error) if store_error.code == "STALE_REVISION" => Err(WatchRunError::Stale),
            Err(store_error) => Err(WatchRunError::Store(store_error)),
        }
    }

    async fn detections(
        &self,
        watch: &SourceWatch,
        result: &SyncResult,
    ) -> Result<Vec<WatchDetection>, LarmError> {
        let requires_compatibility = watch
            .query
            .get("compatible")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || watch
                .query
                .get("compatibility")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("COMPATIBLE"));
        let mut detections = Vec::new();
        for model in &result.models {
            let Some(upstream_identity) = model
                .upstream_identity
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !matches_watch_query(&watch.query, model) {
                continue;
            }
            let compatible = self.compatibility.compatible(watch, model).await?;
            let compatibility_filter = watch
                .query
                .get("compatibility")
                .and_then(Value::as_str)
                .unwrap_or(if requires_compatibility {
                    "COMPATIBLE"
                } else {
                    "ALL"
                });
            match compatibility_filter {
                "ALL" => {}
                "COMPATIBLE" => match compatible {
                    Some(true) => {}
                    Some(false) => continue,
                    None => {
                        return Err(LarmError::new(
                            ErrorCode::UnsupportedCapability,
                            "La evaluación de compatibilidad de esta vigilancia no está disponible",
                            "watch_compatibility_unknown",
                        ));
                    }
                },
                "INCOMPATIBLE" if compatible != Some(false) => continue,
                "UNKNOWN" if compatible.is_some() => continue,
                "INCOMPATIBLE" | "UNKNOWN" => {}
                _ => continue,
            }
            let revision_fingerprint = larm_domain::canonical::sha256_hex(
                &serde_json::to_value(model).map_err(|_| invalid("model_fingerprint"))?,
            )
            .map_err(|_| invalid("model_fingerprint"))?;
            detections.push(WatchDetection {
                upstream_identity: upstream_identity.to_owned(),
                revision_fingerprint,
                compatible: compatible == Some(true),
            });
        }
        Ok(detections)
    }
}

fn matches_watch_query(query: &serde_json::Map<String, Value>, model: &ModelInspection) -> bool {
    if let Some(needle) = query
        .get("q")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let needle = needle.to_lowercase();
        let searchable = [
            model.upstream_identity.as_deref().unwrap_or_default(),
            model.architecture.as_deref().unwrap_or_default(),
            model.format.as_deref().unwrap_or_default(),
            model.quantization.as_deref().unwrap_or_default(),
            model.license.as_deref().unwrap_or_default(),
        ]
        .into_iter()
        .chain(model.capabilities.iter().map(String::as_str))
        .chain(
            model
                .artifacts
                .iter()
                .map(|artifact| artifact.relative_path.as_str()),
        );
        if !searchable
            .into_iter()
            .any(|value| value.to_lowercase().contains(&needle))
        {
            return false;
        }
    }
    if let Some(required) = query.get("capabilities").and_then(Value::as_array) {
        if !required
            .iter()
            .filter_map(Value::as_str)
            .all(|capability| model.capabilities.iter().any(|actual| actual == capability))
        {
            return false;
        }
    }
    let Some(range) = query.get("size_bytes").and_then(Value::as_object) else {
        return true;
    };
    let minimum = range
        .get("minimum")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok());
    let maximum = range
        .get("maximum")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok());
    if minimum.is_none() && maximum.is_none() {
        return true;
    }
    let Some(size) = exact_model_size(model) else {
        return false;
    };
    minimum.is_none_or(|limit| size >= limit) && maximum.is_none_or(|limit| size <= limit)
}

fn exact_model_size(model: &ModelInspection) -> Option<u64> {
    if model.artifacts.is_empty() {
        return None;
    }
    model.artifacts.iter().try_fold(0_u64, |total, artifact| {
        let size = artifact
            .size_bytes
            .map(larm_domain::units::DecimalU64::get)
            .or_else(|| {
                artifact.size_estimate.as_ref().and_then(|estimate| {
                    (estimate.confidence == larm_domain::model::ArtifactSizeConfidence::Exact)
                        .then(|| estimate.bytes.map(larm_domain::units::DecimalU64::get))
                        .flatten()
                })
            })?;
        total.checked_add(size)
    })
}

enum WatchRunError {
    Stale,
    PersistedFailure,
    Store(ApiError),
}

fn source_rate_limit(value: &larm_adapter_sdk::source::RateLimit) -> SourceRateLimit {
    SourceRateLimit {
        limit: value.limit,
        remaining: value.remaining,
        reset_at: value.reset_at.clone(),
        retry_after_seconds: value.retry_after_seconds,
    }
}

fn failure_status(error: &LarmError) -> SourceOperationalStatus {
    if matches!(error.code, ErrorCode::AuthRequired | ErrorCode::Forbidden) {
        SourceOperationalStatus::AuthRequired
    } else if error.details.reason == "source_rate_limited"
        || larm_adapter_sdk::source::RateLimit::from_error(error).is_some()
    {
        SourceOperationalStatus::RateLimited
    } else {
        SourceOperationalStatus::Error
    }
}

fn failure_count(watch: &SourceWatch) -> u32 {
    watch
        .metadata
        .get("watcher")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("failure_count"))
        .and_then(Value::as_u64)
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or_default()
}

fn success_delay(watch: &SourceWatch, jitter_max: u64) -> u64 {
    watch
        .interval_seconds
        .saturating_add(jitter(&watch.id, jitter_max))
}

fn failure_delay(
    interval_seconds: u64,
    failures: u32,
    max_backoff: u64,
    jitter_max: u64,
    watch_id: &str,
) -> u64 {
    let multiplier = 1_u64.checked_shl(failures.min(63)).unwrap_or(u64::MAX);
    interval_seconds
        .saturating_mul(multiplier)
        .min(max_backoff)
        .saturating_add(jitter(watch_id, jitter_max))
        .min(max_backoff)
}

fn jitter(watch_id: &str, max_seconds: u64) -> u64 {
    if max_seconds == 0 {
        return 0;
    }
    let digest = Sha256::digest(watch_id.as_bytes());
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    u64::from(value) % (max_seconds + 1)
}

fn add_seconds(now: DateTime<Utc>, seconds: u64) -> String {
    let seconds = i64::try_from(seconds.min(i64::MAX as u64)).unwrap_or(i64::MAX);
    timestamp(now + ChronoDuration::seconds(seconds))
}

fn stable_error_reason(error: &LarmError) -> String {
    let reason = error.details.reason.as_str();
    if !reason.is_empty()
        && reason.len() <= 96
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        reason.to_owned()
    } else {
        "source_sync_failed".into()
    }
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "No se pudo calcular una observación estable de la fuente",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    type SuccessRecord = (Option<String>, Option<String>, String);

    #[derive(Default)]
    struct MemoryStore {
        watches: Mutex<Vec<SourceWatch>>,
        success: Mutex<Vec<SuccessRecord>>,
        failures: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl SourceWatchStore for MemoryStore {
        async fn due(&self, _now: &str, _limit: u32) -> Result<Vec<SourceWatch>, ApiError> {
            Ok(self.watches.lock().unwrap().clone())
        }

        async fn recover_interrupted(
            &self,
            _recovered_at: &str,
            _ctx: &MutationContext,
        ) -> Result<u64, ApiError> {
            Ok(0)
        }

        async fn started(
            &self,
            _watch: &SourceWatch,
            _started_at: &str,
            _ctx: &MutationContext,
        ) -> Result<(), ApiError> {
            Ok(())
        }

        async fn succeeded(
            &self,
            _watch: &SourceWatch,
            success: &SourceWatchSyncSuccess,
            _ctx: &MutationContext,
        ) -> Result<(), ApiError> {
            self.success.lock().unwrap().push((
                success.cursor.clone(),
                success.etag.clone(),
                success.next_run_at.clone(),
            ));
            Ok(())
        }

        async fn failed(
            &self,
            _watch: &SourceWatch,
            failure: &SourceWatchSyncFailure,
            _ctx: &MutationContext,
        ) -> Result<(), ApiError> {
            self.failures
                .lock()
                .unwrap()
                .push((failure.reason.clone(), failure.retry_at.clone()));
            Ok(())
        }
    }

    struct MemorySource {
        request: Mutex<Option<SyncRequest>>,
        result: Result<SyncResult, LarmError>,
    }

    #[async_trait]
    impl SourceSyncExecutor for MemorySource {
        async fn sync(&self, request: SyncRequest) -> Result<SyncResult, LarmError> {
            *self.request.lock().unwrap() = Some(request);
            self.result.clone()
        }
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "watcher-test".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000399".into(),
        }
    }

    fn sync_result(changed: bool) -> SyncResult {
        serde_json::from_value(json!({
            "cursor":"cursor-next",
            "etag":"etag-next",
            "changed":changed,
            "models":[]
        }))
        .expect("sync result fixture conforms")
    }

    fn watch() -> SourceWatch {
        serde_json::from_value(json!({
            "id":"0192f000-0000-7000-8000-000000000301",
            "created_at":"2026-10-25T00:00:00.000Z",
            "updated_at":"2026-10-25T00:00:00.000Z",
            "row_version":1,
            "metadata":{},
            "source_id":"0192f000-0000-7000-8000-000000000302",
            "query":{},
            "cursor":"cursor-1",
            "etag":"etag-1",
            "interval_seconds":21600,
            "policy":{},
            "next_run_at":"2026-10-25T06:00:00.000Z",
            "enabled":true
        }))
        .expect("source watch fixture conforms")
    }

    #[test]
    fn query_filters_capabilities_and_only_exact_size_values() {
        let model: ModelInspection = serde_json::from_value(json!({
            "upstream_identity":"fixture/model",
            "architecture":null,"format":null,"quantization":null,
            "capabilities":["chat","vision"],"limits":{},
            "artifacts":[{"relative_path":"weights.bin","size_bytes":"10","size_estimate":null,"sha256":null,"required":true,"role":"WEIGHTS"}],
            "license":null,"evidence_refs":[]
        })).expect("inspection fixture conforms");
        let matching =
            json!({"capabilities":["chat"],"size_bytes":{"minimum":"10","maximum":"10"}});
        assert!(matches_watch_query(matching.as_object().unwrap(), &model));
        let too_small = json!({"size_bytes":{"minimum":"11","maximum":null}});
        assert!(!matches_watch_query(too_small.as_object().unwrap(), &model));
        let missing_capability = json!({"capabilities":["audio"]});
        assert!(!matches_watch_query(
            missing_capability.as_object().unwrap(),
            &model
        ));
        let unknown_size: ModelInspection = serde_json::from_value(json!({
            "upstream_identity":"fixture/unknown-size",
            "architecture":null,"format":null,"quantization":null,
            "capabilities":["chat"],"limits":{},
            "artifacts":[{"relative_path":"weights.bin","size_bytes":null,"size_estimate":null,"sha256":null,"required":true,"role":"WEIGHTS"}],
            "license":null,"evidence_refs":[]
        })).expect("unknown size remains a valid inspection");
        assert!(!matches_watch_query(
            matching.as_object().unwrap(),
            &unknown_size
        ));
    }

    #[test]
    fn jitter_is_stable_and_within_contract_bound() {
        let id = watch().id;
        let first = jitter(&id, 300);
        assert_eq!(first, jitter(&id, 300));
        assert!(first <= 300);
        assert_eq!(jitter(&id, 0), 0);
    }

    #[test]
    fn retry_backoff_grows_and_caps_at_one_day_including_jitter() {
        let first = failure_delay(21_600, 1, MAX_BACKOFF_SECONDS, 300, "watch-a");
        let second = failure_delay(21_600, 2, MAX_BACKOFF_SECONDS, 300, "watch-a");
        let capped = failure_delay(21_600, 8, MAX_BACKOFF_SECONDS, 300, "watch-a");
        assert!((43_200..=43_500).contains(&first));
        assert_eq!(second, MAX_BACKOFF_SECONDS);
        assert_eq!(capped, MAX_BACKOFF_SECONDS);
    }

    #[test]
    fn errors_persist_only_stable_reason_codes() {
        let safe = LarmError::new(
            ErrorCode::ResourceUnavailable,
            "source unavailable",
            "timeout",
        );
        let unsafe_error = LarmError::new(
            ErrorCode::ResourceUnavailable,
            "source unavailable",
            "request failed at C:\\Users\\alice\\secret",
        );
        assert_eq!(stable_error_reason(&safe), "timeout");
        assert_eq!(stable_error_reason(&unsafe_error), "source_sync_failed");
    }

    #[test]
    fn operational_status_distinguishes_auth_rate_limit_and_other_errors() {
        let required = LarmError::new(ErrorCode::AuthRequired, "auth", "auth_required");
        let forbidden = LarmError::new(ErrorCode::Forbidden, "forbidden", "upstream_forbidden");
        let rate = larm_adapter_sdk::source::RateLimit {
            limit: Some(larm_domain::units::DecimalU64::new(120).unwrap()),
            remaining: Some(larm_domain::units::DecimalU64::new(0).unwrap()),
            reset_at: Some("2026-10-25T12:05:00.000Z".into()),
            retry_after_seconds: Some(larm_domain::units::DecimalU64::new(60).unwrap()),
        };
        let limited = rate.to_error().unwrap();
        let unavailable = LarmError::new(
            ErrorCode::ResourceUnavailable,
            "upstream unavailable",
            "upstream_timeout",
        );
        assert_eq!(
            failure_status(&required),
            SourceOperationalStatus::AuthRequired
        );
        assert_eq!(
            failure_status(&forbidden),
            SourceOperationalStatus::AuthRequired
        );
        assert_eq!(
            failure_status(&limited),
            SourceOperationalStatus::RateLimited
        );
        assert_eq!(failure_status(&unavailable), SourceOperationalStatus::Error);
        assert_eq!(
            larm_adapter_sdk::source::RateLimit::from_error(&limited)
                .as_ref()
                .map(source_rate_limit),
            Some(SourceRateLimit {
                limit: rate.limit,
                remaining: rate.remaining,
                reset_at: rate.reset_at,
                retry_after_seconds: rate.retry_after_seconds,
            })
        );
    }

    #[tokio::test]
    async fn incremental_sync_reuses_persisted_cursor_and_etag() {
        let watch = watch();
        let store = Arc::new(MemoryStore::default());
        store.watches.lock().unwrap().push(watch.clone());
        let source = Arc::new(MemorySource {
            request: Mutex::new(None),
            result: Ok(sync_result(false)),
        });
        let worker = SourceWatchWorker::new(
            store.clone(),
            source.clone(),
            Arc::new(UnknownWatchCompatibility),
        );
        let summary = worker
            .run_due_once(
                DateTime::parse_from_rfc3339("2026-10-25T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                10,
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(summary.succeeded, 1);
        let request = source.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.cursor.as_deref(), Some("cursor-1"));
        assert_eq!(request.etag.as_deref(), Some("etag-1"));
        assert_eq!(request.query, watch.query);
        let saved = store.success.lock().unwrap();
        assert_eq!(saved[0].0.as_deref(), Some("cursor-next"));
        assert_eq!(saved[0].1.as_deref(), Some("etag-next"));
    }

    #[tokio::test]
    async fn source_failure_is_persisted_with_capped_retry_schedule() {
        let mut watch = watch();
        watch.metadata.insert(
            "watcher".into(),
            json!({"failure_count":1,"last_error":"previous_timeout"}),
        );
        let store = Arc::new(MemoryStore::default());
        store.watches.lock().unwrap().push(watch);
        let source = Arc::new(MemorySource {
            request: Mutex::new(None),
            result: Err(LarmError::new(
                ErrorCode::UpstreamUnavailable,
                "upstream unavailable",
                "upstream_timeout",
            )),
        });
        let worker =
            SourceWatchWorker::new(store.clone(), source, Arc::new(UnknownWatchCompatibility));
        let now = DateTime::parse_from_rfc3339("2026-10-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let summary = worker.run_due_once(now, 10, &context()).await.unwrap();
        assert_eq!(summary.failed, 1);
        let failures = store.failures.lock().unwrap();
        assert_eq!(failures[0].0, "upstream_timeout");
        assert_eq!(failures[0].1, timestamp(now + ChronoDuration::hours(24)));
    }

    #[tokio::test]
    async fn compatible_watch_does_not_advance_cursor_when_compatibility_is_unknown() {
        let mut watch = watch();
        watch.query.insert("compatible".into(), Value::Bool(true));
        let store = Arc::new(MemoryStore::default());
        store.watches.lock().unwrap().push(watch);
        let source = Arc::new(MemorySource {
            request: Mutex::new(None),
            result: Ok(serde_json::from_value(json!({
                "cursor":"cursor-next",
                "etag":"etag-next",
                "changed":true,
                "models":[{
                    "upstream_identity":"fixture/model",
                    "architecture":null,
                    "format":null,
                    "quantization":null,
                    "capabilities":[],
                    "limits":{},
                    "artifacts":[],
                    "license":null,
                    "evidence_refs":[]
                }]
            }))
            .expect("sync result fixture conforms")),
        });
        let worker =
            SourceWatchWorker::new(store.clone(), source, Arc::new(UnknownWatchCompatibility));
        let summary = worker
            .run_due_once(
                DateTime::parse_from_rfc3339("2026-10-25T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                10,
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(summary.failed, 1);
        assert!(store.success.lock().unwrap().is_empty());
        assert_eq!(
            store.failures.lock().unwrap()[0].0,
            "watch_compatibility_unknown"
        );
    }
}
