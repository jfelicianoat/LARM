//! Sidecar HTTP local para runtimes CLI, aislado del daemon.
//!
//! El primer perfil implementa un REST propio y estrecho: texto sin streaming,
//! herramientas ni media. No anuncia compatibilidad OpenAI. La versión del
//! perfil debe cualificarse por runtime antes de uso de producto.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State as AxumState};
use axum::http::header::{HeaderValue, AUTHORIZATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use larm_adapter_sdk::contracts::Contracts;
use larm_domain::canonical::sha256_hex;
use larm_domain::correlation::{CorrelationId, CORRELATION_HEADER};
use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MAX_REQUEST_BYTES: usize = 1 << 20;
const MAX_ARGS_BYTES: usize = 64 << 10;
const MAX_STDERR_BYTES: usize = 256 << 10;
const SUPPORTED_CAPABILITY: &str = "text.generate";

/// Startup configuration read from a private parent-to-child pipe by the binary.
/// The authentication token is deliberately omitted from `Debug` and from logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarConfig {
    pub model_id: String,
    pub runtime_instance_id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub auth_token: String,
    pub fingerprint: Value,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_concurrent_requests: usize,
}

impl SidecarConfig {
    /// Creates a profile with a fresh per-instance endpoint token and conservative limits.
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        runtime_instance_id: impl Into<String>,
        executable: PathBuf,
        arguments: Vec<String>,
        fingerprint: Value,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            runtime_instance_id: runtime_instance_id.into(),
            executable,
            arguments,
            environment: BTreeMap::new(),
            auth_token: generate_ephemeral_token(),
            fingerprint,
            timeout_ms: 120_000,
            max_output_bytes: 1 << 20,
            max_concurrent_requests: 1,
        }
    }
}

/// Generates 244 bits of unpredictable per-instance bearer material.
#[must_use]
pub fn generate_ephemeral_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[derive(Clone)]
struct CliProfile {
    model_id: String,
    runtime_instance_id: String,
    executable: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    fingerprint: Value,
    timeout: Duration,
    max_output_bytes: usize,
    profile_hash: String,
}

struct Metrics {
    requests: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    timed_out: AtomicU64,
}

impl Metrics {
    fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            succeeded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
        }
    }
}

struct State {
    profile: CliProfile,
    token_hash: [u8; 32],
    pending: StdMutex<BTreeMap<String, Arc<Cancellation>>>,
    slots: Arc<Semaphore>,
    metrics: Metrics,
}

#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl Cancellation {
    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct PendingGuard {
    state: Arc<State>,
    request_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.remove(&self.request_id);
        }
    }
}

/// Aggregate metrics scoped to one sidecar process/runtime instance.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SidecarMetrics {
    pub requests_total: u64,
    pub succeeded_total: u64,
    pub failed_total: u64,
    pub cancelled_total: u64,
    pub timed_out_total: u64,
    pub active_requests: usize,
}

/// A running sidecar exposes only its loopback URL and profile hash.
pub struct RunningSidecar {
    endpoint: String,
    profile_hash: String,
    state: Arc<State>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl RunningSidecar {
    /// Starts an authenticated HTTP listener bound to IPv4 loopback and an ephemeral port.
    pub async fn start(mut config: SidecarConfig) -> Result<Self, LarmError> {
        let executable = validate_config(&config)?;
        let mut token_hash = [0_u8; 32];
        token_hash.copy_from_slice(&Sha256::digest(config.auth_token.as_bytes()));
        config.auth_token.clear();

        let fingerprint_hash = config
            .fingerprint
            .get("hash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let environment_names: Vec<_> = config.environment.keys().cloned().collect();
        let profile_hash = sha256_hex(&json!({
            "protocol": "CUSTOM_REST",
            "model_id": config.model_id,
            "runtime_instance_id": config.runtime_instance_id,
            "executable_sha256": hash_file(&executable)?,
            "arguments": config.arguments,
            "environment_names": environment_names,
            "timeout_ms": config.timeout_ms,
            "max_output_bytes": config.max_output_bytes,
            "max_concurrent_requests": config.max_concurrent_requests,
            "fingerprint_hash": fingerprint_hash,
        }))
        .map_err(|_| internal_error("sidecar_profile_hash_failed"))?;

        let state = Arc::new(State {
            profile: CliProfile {
                model_id: config.model_id,
                runtime_instance_id: config.runtime_instance_id,
                executable,
                arguments: config.arguments,
                environment: config.environment,
                fingerprint: config.fingerprint,
                timeout: Duration::from_millis(config.timeout_ms),
                max_output_bytes: config.max_output_bytes,
                profile_hash: profile_hash.clone(),
            },
            token_hash,
            pending: StdMutex::new(BTreeMap::new()),
            slots: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            metrics: Metrics::new(),
        });

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .map_err(|_| {
                LarmError::new(
                    ErrorCode::UpstreamUnavailable,
                    "No se pudo iniciar el sidecar",
                    "sidecar_bind_failed",
                )
            })?;
        let address = listener
            .local_addr()
            .map_err(|_| internal_error("sidecar_address_failed"))?;
        let router = Router::new()
            .route("/v1/describe", get(describe))
            .route("/v1/metrics", get(metrics))
            .route("/v1/invoke", post(invoke))
            .route("/v1/cancel", post(cancel))
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let serve = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
            if serve.is_err() {
                tracing::error!(target: "larm_protocol::sidecar", event = "server_failed");
            }
        });

        Ok(Self {
            endpoint: format!("http://{address}"),
            profile_hash,
            state,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[must_use]
    pub fn profile_hash(&self) -> &str {
        &self.profile_hash
    }

    #[must_use]
    pub fn metrics_snapshot(&self) -> SidecarMetrics {
        SidecarMetrics {
            requests_total: self.state.metrics.requests.load(Ordering::Relaxed),
            succeeded_total: self.state.metrics.succeeded.load(Ordering::Relaxed),
            failed_total: self.state.metrics.failed.load(Ordering::Relaxed),
            cancelled_total: self.state.metrics.cancelled.load(Ordering::Relaxed),
            timed_out_total: self.state.metrics.timed_out.load(Ordering::Relaxed),
            active_requests: self
                .state
                .pending
                .lock()
                .map(|pending| pending.len())
                .unwrap_or_default(),
        }
    }

    /// Cancels active requests and gracefully stops the HTTP listener.
    pub async fn shutdown(mut self) {
        if let Ok(pending) = self.state.pending.lock() {
            for cancellation in pending.values() {
                cancellation.cancel();
            }
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for RunningSidecar {
    fn drop(&mut self) {
        if let Ok(pending) = self.state.pending.lock() {
            for cancellation in pending.values() {
                cancellation.cancel();
            }
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn validate_config(config: &SidecarConfig) -> Result<PathBuf, LarmError> {
    if config.model_id.is_empty()
        || config.model_id.len() > 256
        || config.model_id.chars().any(char::is_control)
    {
        return Err(validation_error("sidecar_model_id_invalid"));
    }
    CorrelationId::parse(&config.runtime_instance_id)
        .map_err(|_| validation_error("sidecar_instance_id_invalid"))?;
    if config.auth_token.len() < 32
        || config.auth_token.len() > 256
        || !config.auth_token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(validation_error("sidecar_auth_token_invalid"));
    }
    if !(1..=600_000).contains(&config.timeout_ms)
        || !(1..=16 << 20).contains(&config.max_output_bytes)
        || !(1..=64).contains(&config.max_concurrent_requests)
    {
        return Err(validation_error("sidecar_limits_invalid"));
    }
    if config.arguments.iter().any(|arg| arg.contains('\0'))
        || config.arguments.iter().map(String::len).sum::<usize>() > MAX_ARGS_BYTES
    {
        return Err(validation_error("sidecar_arguments_invalid"));
    }
    let mut environment_names = BTreeMap::new();
    for (name, value) in &config.environment {
        if !valid_environment_name(name)
            || value.contains('\0')
            || environment_names
                .insert(name.to_ascii_uppercase(), ())
                .is_some()
            || matches!(
                name.to_ascii_uppercase().as_str(),
                "LARM_CORRELATION_ID"
                    | "LARM_REQUEST_ID"
                    | "LARM_RUNTIME_INSTANCE_ID"
                    | "LARM_MODEL_ID"
            )
        {
            return Err(validation_error("sidecar_environment_invalid"));
        }
    }
    let executable = config
        .executable
        .canonicalize()
        .map_err(|_| validation_error("sidecar_executable_invalid"))?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err(validation_error("sidecar_executable_invalid"));
    }
    #[cfg(windows)]
    if !executable
        .extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("exe"))
    {
        return Err(validation_error("sidecar_executable_invalid"));
    }
    validate_fingerprint(&config.fingerprint)?;
    let description = protocol_description("0".repeat(64));
    Contracts::shared()
        .and_then(|contracts| contracts.response("protocol.describe", &description))
        .map_err(|_| validation_error("sidecar_description_invalid"))?;
    Ok(executable)
}

fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn validate_fingerprint(value: &Value) -> Result<(), LarmError> {
    let Some(root) = value.as_object() else {
        return Err(validation_error("sidecar_fingerprint_invalid"));
    };
    if root.len() != 2
        || root
            .get("hash")
            .and_then(Value::as_str)
            .is_none_or(|hash| !is_hash(hash))
        || !root.get("components").is_some_and(|components| {
            components.as_object().is_some_and(|values| {
                !values.is_empty()
                    && values.iter().all(|(name, component)| {
                        !name.is_empty()
                            && component.as_object().is_some_and(|fields| {
                                fields.len() == 3
                                    && fields.get("value").is_some_and(|value| {
                                        value.is_null()
                                            || value.as_str().is_some_and(|value| !value.is_empty())
                                    })
                                    && fields.get("status").and_then(Value::as_str).is_some_and(
                                        |status| {
                                            matches!(
                                                status,
                                                "VERIFIED" | "DECLARED" | "UNAVAILABLE"
                                            )
                                        },
                                    )
                                    && fields
                                        .get("evidence_refs")
                                        .and_then(Value::as_array)
                                        .is_some_and(|refs| refs.iter().all(Value::is_string))
                            })
                    })
            })
        })
    {
        return Err(validation_error("sidecar_fingerprint_invalid"));
    }
    Ok(())
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn protocol_description(profile_hash: String) -> Value {
    json!({
        "protocol": "CUSTOM_REST",
        "profile_hash": profile_hash,
        "capabilities": [SUPPORTED_CAPABILITY],
        "streaming": false,
        "tools": false,
        "request_schema_id": "larm.schema.json#/$defs/CapabilityRequest",
        "response_schema_id": "larm.schema.json#/$defs/CapabilityResponse"
    })
}

fn parse_correlation(headers: &HeaderMap) -> Result<CorrelationId, LarmError> {
    match headers.get(CORRELATION_HEADER) {
        Some(value) => {
            let value = value
                .to_str()
                .map_err(|_| validation_error("correlation_id_invalid"))?;
            CorrelationId::parse(value).map_err(|_| validation_error("correlation_id_invalid"))
        }
        None => Ok(CorrelationId::new()),
    }
}

fn authenticated(headers: &HeaderMap, state: &State) -> bool {
    let Some(header) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    constant_time_eq(&candidate, &state.token_hash)
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn json_response(status: StatusCode, value: Value, correlation: CorrelationId) -> Response {
    let mut response = (status, Json(value)).into_response();
    if let Ok(value) = HeaderValue::from_str(&correlation.to_string()) {
        response.headers_mut().insert(CORRELATION_HEADER, value);
    }
    response
}

fn no_content(correlation: CorrelationId) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(&correlation.to_string()) {
        response.headers_mut().insert(CORRELATION_HEADER, value);
    }
    response
}

fn api_error(mut error: LarmError, correlation: CorrelationId) -> Response {
    error.correlation_id = correlation.to_string();
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let value = serde_json::to_value(error).unwrap_or_else(|_| json!({"code":"INTERNAL"}));
    json_response(status, value, correlation)
}

fn validation_error(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "La solicitud del sidecar no cumple el contrato",
        reason,
    )
}

fn unsupported_error(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::UnsupportedCapability,
        "Esta instancia no admite la función solicitada",
        reason,
    )
}

fn internal_error(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Internal,
        "No se pudo completar la operación del sidecar",
        reason,
    )
}

fn verify_request(method: &str, value: &Value) -> Result<(), LarmError> {
    Contracts::shared()?.request(method, value)
}

async fn describe(AxumState(state): AxumState<Arc<State>>, headers: HeaderMap) -> Response {
    let correlation = match parse_correlation(&headers) {
        Ok(id) => id,
        Err(error) => return api_error(error, CorrelationId::new()),
    };
    if !authenticated(&headers, &state) {
        return api_error(
            LarmError::new(
                ErrorCode::AuthRequired,
                "Autenticación requerida",
                "sidecar_auth_required",
            ),
            correlation,
        );
    }
    let description = protocol_description(state.profile.profile_hash.clone());
    if let Err(error) = Contracts::shared()
        .and_then(|contracts| contracts.response("protocol.describe", &description))
    {
        return api_error(error, correlation);
    }
    json_response(StatusCode::OK, description, correlation)
}

async fn metrics(AxumState(state): AxumState<Arc<State>>, headers: HeaderMap) -> Response {
    let correlation = match parse_correlation(&headers) {
        Ok(id) => id,
        Err(error) => return api_error(error, CorrelationId::new()),
    };
    if !authenticated(&headers, &state) {
        return api_error(
            LarmError::new(
                ErrorCode::AuthRequired,
                "Autenticación requerida",
                "sidecar_auth_required",
            ),
            correlation,
        );
    }
    let snapshot = SidecarMetrics {
        requests_total: state.metrics.requests.load(Ordering::Relaxed),
        succeeded_total: state.metrics.succeeded.load(Ordering::Relaxed),
        failed_total: state.metrics.failed.load(Ordering::Relaxed),
        cancelled_total: state.metrics.cancelled.load(Ordering::Relaxed),
        timed_out_total: state.metrics.timed_out.load(Ordering::Relaxed),
        active_requests: state
            .pending
            .lock()
            .map(|pending| pending.len())
            .unwrap_or_default(),
    };
    let value = serde_json::to_value(snapshot).unwrap_or_else(|_| json!({}));
    json_response(StatusCode::OK, value, correlation)
}

async fn invoke(
    AxumState(state): AxumState<Arc<State>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let correlation = match parse_correlation(&headers) {
        Ok(id) => id,
        Err(error) => return api_error(error, CorrelationId::new()),
    };
    if !authenticated(&headers, &state) {
        return api_error(
            LarmError::new(
                ErrorCode::AuthRequired,
                "Autenticación requerida",
                "sidecar_auth_required",
            ),
            correlation,
        );
    }
    if body.len() > MAX_REQUEST_BYTES {
        return api_error(validation_error("sidecar_request_too_large"), correlation);
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                validation_error("sidecar_request_invalid_json"),
                correlation,
            )
        }
    };
    if let Err(error) = verify_request("protocol.invoke", &value) {
        return api_error(error, correlation);
    }
    let request: CapabilityRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(_) => return api_error(validation_error("sidecar_request_invalid"), correlation),
    };
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);
    if request.capability != SUPPORTED_CAPABILITY {
        state.metrics.failed.fetch_add(1, Ordering::Relaxed);
        return api_error(
            unsupported_error("sidecar_capability_not_supported"),
            correlation,
        );
    }
    if request.stream || request.generation.is_some() {
        state.metrics.failed.fetch_add(1, Ordering::Relaxed);
        return api_error(
            unsupported_error("sidecar_stream_or_generation_not_supported"),
            correlation,
        );
    }
    let prompt = match role_tagged_prompt(&request.input) {
        Ok(prompt) => prompt,
        Err(error) => {
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            return api_error(error, correlation);
        }
    };
    let permit = match state.slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            return api_error(
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "La instancia está ocupada",
                    "sidecar_capacity_reached",
                )
                .retryable(true),
                correlation,
            );
        }
    };
    let cancellation = Arc::new(Cancellation::default());
    {
        let Ok(mut pending) = state.pending.lock() else {
            return api_error(internal_error("sidecar_pending_lock_failed"), correlation);
        };
        if pending.contains_key(&request.request_id) {
            return api_error(
                LarmError::new(
                    ErrorCode::Conflict,
                    "La solicitud ya está activa",
                    "sidecar_request_id_conflict",
                ),
                correlation,
            );
        }
        pending.insert(request.request_id.clone(), cancellation.clone());
    }
    let _pending_guard = PendingGuard {
        state: state.clone(),
        request_id: request.request_id.clone(),
    };
    let started = Instant::now();
    let task = tokio::spawn(run_cli(
        state.profile.clone(),
        request.request_id.clone(),
        correlation,
        prompt,
    ));
    let mut child_task = AbortOnDrop::new(task);
    let timeout = tokio::time::sleep(state.profile.timeout);
    tokio::pin!(timeout);
    let result = tokio::select! {
        result = &mut child_task.task => {
            child_task.disarm();
            match result {
                Ok(result) => result,
                Err(_) => Err(LarmError::new(ErrorCode::UpstreamUnavailable, "El proceso del runtime terminó inesperadamente", "sidecar_worker_join_failed")),
            }
        }
        _ = cancellation.cancelled() => {
            child_task.abort_and_wait().await;
            state.metrics.cancelled.fetch_add(1, Ordering::Relaxed);
            drop(permit);
            tracing::info!(target: "larm_protocol::sidecar", correlation_id = %correlation, request_id = %request.request_id, runtime_instance_id = %state.profile.runtime_instance_id, event = "cancelled");
            return no_content(correlation);
        }
        _ = &mut timeout => {
            child_task.abort_and_wait().await;
            state.metrics.timed_out.fetch_add(1, Ordering::Relaxed);
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(target: "larm_protocol::sidecar", correlation_id = %correlation, request_id = %request.request_id, runtime_instance_id = %state.profile.runtime_instance_id, event = "timeout");
            drop(permit);
            return api_error(LarmError::new(ErrorCode::Timeout, "La inferencia superó el límite de tiempo", "sidecar_invocation_timeout").retryable(true), correlation);
        }
    };
    drop(permit);
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    match result {
        Ok(output_text) => {
            let response = json!({
                "request_id": request.request_id,
                "capability": request.capability,
                "output": {"text": output_text, "finish_reason": "stop", "tool_calls": []},
                "fingerprint": state.profile.fingerprint,
                "usage": {"tokens_input": null, "tokens_output": null, "latency_ms": elapsed_ms}
            });
            match Contracts::shared()
                .and_then(|contracts| contracts.response("protocol.invoke", &response))
            {
                Ok(()) => {
                    state.metrics.succeeded.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(target: "larm_protocol::sidecar", correlation_id = %correlation, request_id = %request.request_id, runtime_instance_id = %state.profile.runtime_instance_id, elapsed_ms, event = "completed");
                    json_response(StatusCode::OK, response, correlation)
                }
                Err(error) => {
                    state.metrics.failed.fetch_add(1, Ordering::Relaxed);
                    api_error(error, correlation)
                }
            }
        }
        Err(error) => {
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(target: "larm_protocol::sidecar", correlation_id = %correlation, request_id = %request.request_id, runtime_instance_id = %state.profile.runtime_instance_id, event = "upstream_error", reason = %error.details.reason);
            api_error(error, correlation)
        }
    }
}

async fn cancel(
    AxumState(state): AxumState<Arc<State>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let correlation = match parse_correlation(&headers) {
        Ok(id) => id,
        Err(error) => return api_error(error, CorrelationId::new()),
    };
    if !authenticated(&headers, &state) {
        return api_error(
            LarmError::new(
                ErrorCode::AuthRequired,
                "Autenticación requerida",
                "sidecar_auth_required",
            ),
            correlation,
        );
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return api_error(validation_error("sidecar_cancel_invalid_json"), correlation),
    };
    if let Err(error) = verify_request("protocol.cancel", &value) {
        return api_error(error, correlation);
    }
    let request: CancelRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(_) => return api_error(validation_error("sidecar_cancel_invalid"), correlation),
    };
    let cancellation = state
        .pending
        .lock()
        .ok()
        .and_then(|pending| pending.get(&request.request_id).cloned());
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
        let value = json!({"cancelled":true});
        if let Err(error) =
            Contracts::shared().and_then(|contracts| contracts.response("protocol.cancel", &value))
        {
            return api_error(error, correlation);
        }
        return json_response(StatusCode::OK, value, correlation);
    }
    let value = json!({"cancelled":false});
    if let Err(error) =
        Contracts::shared().and_then(|contracts| contracts.response("protocol.cancel", &value))
    {
        return api_error(error, correlation);
    }
    json_response(StatusCode::OK, value, correlation)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityRequest {
    #[serde(rename = "lease_id")]
    _lease_id: String,
    request_id: String,
    capability: String,
    input: Value,
    generation: Option<Value>,
    stream: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRequest {
    request_id: String,
}

fn role_tagged_prompt(input: &Value) -> Result<String, LarmError> {
    let Some(input) = input.as_object() else {
        return Err(validation_error("sidecar_text_input_invalid"));
    };
    if input.len() != 2 {
        return Err(validation_error("sidecar_text_input_invalid"));
    }
    let Some(messages) = input.get("messages").and_then(Value::as_array) else {
        return Err(validation_error("sidecar_text_messages_invalid"));
    };
    let Some(tools) = input.get("tools").and_then(Value::as_array) else {
        return Err(validation_error("sidecar_text_tools_invalid"));
    };
    if messages.is_empty() {
        return Err(validation_error("sidecar_text_messages_empty"));
    }
    if !tools.is_empty() {
        return Err(unsupported_error("sidecar_tools_not_supported"));
    }
    let mut prompt = String::new();
    for message in messages {
        let Some(message) = message.as_object() else {
            return Err(validation_error("sidecar_text_message_invalid"));
        };
        if message.len() != 4
            || !message.contains_key("role")
            || !message.contains_key("text")
            || !message.contains_key("media")
            || !message.contains_key("tool_call_id")
        {
            return Err(validation_error("sidecar_text_message_invalid"));
        }
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = message
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| validation_error("sidecar_text_message_invalid"))?;
        if !matches!(role, "system" | "user" | "assistant" | "tool") {
            return Err(validation_error("sidecar_text_role_invalid"));
        }
        if message
            .get("media")
            .and_then(Value::as_array)
            .is_none_or(|media| !media.is_empty())
        {
            return Err(unsupported_error("sidecar_media_not_supported"));
        }
        if role == "tool" || !message.get("tool_call_id").is_some_and(Value::is_null) {
            return Err(unsupported_error("sidecar_tool_messages_not_supported"));
        }
        prompt.push('[');
        prompt.push_str(role);
        prompt.push_str("]\n");
        prompt.push_str(text);
        prompt.push('\n');
    }
    if prompt.len() > MAX_REQUEST_BYTES {
        return Err(validation_error("sidecar_prompt_too_large"));
    }
    Ok(prompt)
}

struct AbortOnDrop<T> {
    task: JoinHandle<T>,
    armed: bool,
}

impl<T> AbortOnDrop<T> {
    fn new(task: JoinHandle<T>) -> Self {
        Self { task, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn abort_and_wait(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
        self.armed = false;
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if self.armed {
            self.task.abort();
        }
    }
}

async fn run_cli(
    profile: CliProfile,
    request_id: String,
    correlation: CorrelationId,
    prompt: String,
) -> Result<String, LarmError> {
    let mut command = Command::new(&profile.executable);
    command
        .args(&profile.arguments)
        .env_clear()
        .envs(&profile.environment)
        .env("LARM_REQUEST_ID", &request_id)
        .env("LARM_CORRELATION_ID", correlation.to_string())
        .env("LARM_RUNTIME_INSTANCE_ID", &profile.runtime_instance_id)
        .env("LARM_MODEL_ID", &profile.model_id)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let mut child = command.spawn().map_err(|_| {
        LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "No se pudo iniciar el runtime",
            "sidecar_process_start_failed",
        )
        .retryable(true)
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| internal_error("sidecar_stdin_unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| internal_error("sidecar_stdout_unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| internal_error("sidecar_stderr_unavailable"))?;
    let stdout_limit = profile.max_output_bytes;
    let stdout_task = tokio::spawn(read_pipe(stdout, stdout_limit, true));
    let stderr_task = tokio::spawn(read_pipe(stderr, MAX_STDERR_BYTES, false));
    if stdin.write_all(prompt.as_bytes()).await.is_err() || stdin.shutdown().await.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        return Err(LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "El runtime no pudo recibir la solicitud",
            "sidecar_stdin_write_failed",
        ));
    }
    drop(stdin);
    let status = child.wait().await.map_err(|_| {
        LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "El proceso del runtime falló",
            "sidecar_process_wait_failed",
        )
    })?;
    let (stdout, stdout_oversized, _) = stdout_task
        .await
        .map_err(|_| internal_error("sidecar_stdout_join_failed"))?;
    let (_, _, _) = stderr_task
        .await
        .map_err(|_| internal_error("sidecar_stderr_join_failed"))?;
    if stdout_oversized {
        return Err(LarmError::new(
            ErrorCode::ProtocolError,
            "La salida del runtime superó el límite permitido",
            "sidecar_output_too_large",
        ));
    }
    if !status.success() {
        return Err(LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "El runtime terminó con un error",
            "sidecar_process_failed",
        )
        .retryable(true));
    }
    String::from_utf8(stdout).map_err(|_| {
        LarmError::new(
            ErrorCode::ProtocolError,
            "El runtime devolvió texto inválido",
            "sidecar_output_not_utf8",
        )
    })
}

async fn read_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    keep: bool,
) -> (Vec<u8>, bool, usize) {
    let mut collected = Vec::with_capacity(limit.min(8192));
    let mut exceeded = false;
    let mut total = 0_usize;
    let mut buffer = [0_u8; 8192];
    while let Ok(read) = reader.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        if keep {
            let remaining = limit.saturating_sub(collected.len());
            let take = remaining.min(read);
            collected.extend_from_slice(&buffer[..take]);
            exceeded |= take != read;
        } else {
            exceeded |= total > limit;
        }
    }
    (collected, exceeded, total)
}

fn hash_file(path: &Path) -> Result<String, LarmError> {
    let mut file =
        File::open(path).map_err(|_| validation_error("sidecar_executable_unreadable"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| validation_error("sidecar_executable_unreadable"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formato_aceptado_conserva_roles_y_texto() {
        let value = json!({
            "messages": [
                {"role":"system","text":"rules","media":[],"tool_call_id":null},
                {"role":"user","text":"hello","media":[],"tool_call_id":null}
            ],
            "tools": []
        });
        assert_eq!(
            role_tagged_prompt(&value).unwrap_or_default(),
            "[system]\nrules\n[user]\nhello\n"
        );
    }

    #[test]
    fn rechaza_tools_media_mensajes_tool_y_campos_ajenos() {
        let base = json!({"messages":[{"role":"user","text":"hi","media":[],"tool_call_id":null}],"tools":[]});
        let mut tools = base.clone();
        tools["tools"] = json!([{"name":"x","description":"x","input_schema":{}}]);
        assert_eq!(
            role_tagged_prompt(&tools).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedCapability)
        );
        let mut media = base.clone();
        media["messages"][0]["media"] = json!([{}]);
        assert_eq!(
            role_tagged_prompt(&media).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedCapability)
        );
        let mut tool_message = base.clone();
        tool_message["messages"][0]["role"] = json!("tool");
        assert_eq!(
            role_tagged_prompt(&tool_message).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedCapability)
        );
        let mut unknown = base;
        unknown["unlisted"] = json!(true);
        assert_eq!(
            role_tagged_prompt(&unknown).err().map(|e| e.details.reason),
            Some("sidecar_text_input_invalid".into())
        );
    }

    #[test]
    fn tokens_endpoint_separados_y_igualdad_constante() {
        let first = generate_ephemeral_token();
        let second = generate_ephemeral_token();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
        let digest: [u8; 32] = Sha256::digest(first.as_bytes()).into();
        assert!(constant_time_eq(&digest, &digest));
        let other: [u8; 32] = Sha256::digest(second.as_bytes()).into();
        assert!(!constant_time_eq(&digest, &other));
    }
}
