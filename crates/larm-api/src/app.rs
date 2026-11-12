//! Aplicación HTTP de la API administrativa (P005).
//!
//! Capas, de fuera a dentro: CORS (desactivado salvo orígenes exactos
//! configurados), correlación + log de petición sin cuerpos ni cabeceras,
//! normalización de errores del framework al envelope del contrato 07,
//! límite de cuerpo, timeout y el guard de P009 delante de todas las rutas
//! administrativas. El gateway de inferencia de las leases (P107) va fuera
//! del guard: lo autoriza el token de endpoint de cada lease.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::stream::{self, StreamExt};
use larm_domain::bridge::{ApiError, Health, HealthStatus};
use larm_domain::correlation::{CorrelationId, CORRELATION_HEADER};
use larm_domain::ports::PrincipalAuthenticator;
use tokio::sync::broadcast;

use crate::http::Failure;

/// Capacidad de la cola de cada suscriptor SSE (contrato 07).
pub const SUBSCRIBER_QUEUE: usize = 1024;

/// Evento tal como viaja por SSE (`payload` del contrato 07).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub correlation_id: String,
    pub actor_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resource_version: i64,
    pub data: serde_json::Value,
}

/// Lote persistido para abrir/reanudar un stream sin perder eventos que se
/// publicaron mientras se leía el historial.
#[derive(Debug, Clone, PartialEq)]
pub struct EventReplayBatch {
    pub events: Vec<EventEnvelope>,
    pub through_event_id: i64,
}

/// Contrato de lectura del historial; la implementación SQLite pertenece al
/// daemon, mientras la API conserva la política HTTP y la forma SSE.
#[async_trait]
pub trait EventReplayStore: Send + Sync {
    async fn replay_after(&self, after: Option<i64>) -> Result<EventReplayBatch, ApiError>;
}

/// Bus de eventos en proceso; lo alimenta el outbox persistido (daemon).
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<EventEnvelope>>,
    replay_store: Option<Arc<dyn EventReplayStore>>,
    /// `true` al apagar: los streams SSE terminan y el apagado ordenado no
    /// espera indefinidamente a conexiones de larga duración.
    closed: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self {
            sender: broadcast::channel(SUBSCRIBER_QUEUE).0,
            replay_store: None,
            closed: Arc::new(tokio::sync::watch::channel(false).0),
        }
    }
}

impl EventBus {
    /// Conecta el historial durable que usa SSE para reanudarse.
    #[must_use]
    pub fn with_replay_store(mut self, store: Arc<dyn EventReplayStore>) -> Self {
        self.replay_store = Some(store);
        self
    }

    async fn replay_after(&self, after: Option<i64>) -> Result<EventReplayBatch, ApiError> {
        match &self.replay_store {
            Some(store) => store.replay_after(after).await,
            None if after.is_none() => Ok(EventReplayBatch {
                events: Vec::new(),
                through_event_id: 0,
            }),
            None => Err(ApiError::new(
                "UPSTREAM_UNAVAILABLE",
                "El historial de eventos no está disponible",
                "event_replay_unavailable",
            )
            .retryable(true)),
        }
    }

    /// Publica a los suscriptores conectados e informa si al menos uno recibió
    /// la notificación. El dispatcher conserva el outbox cuando aún no hay SSE.
    pub fn publish(&self, event: EventEnvelope) -> bool {
        self.sender.send(Arc::new(event)).is_ok()
    }

    /// Se suscribe a los eventos publicados por el daemon.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<EventEnvelope>> {
        self.sender.subscribe()
    }

    #[must_use]
    pub fn subscribers(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Cierra todos los streams SSE (apagado del daemon).
    pub fn close(&self) {
        self.closed.send_replace(true);
    }
}

/// Comprobación de disponibilidad para `health/ready`.
#[async_trait]
pub trait Readiness: Send + Sync {
    /// `Ok(true)` lista; `Ok(false)` degradada; `Err` si no se puede comprobar.
    async fn ready(&self) -> Result<bool, ApiError>;
}

/// Configuración de la aplicación (de `Settings`).
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub request_body_bytes: usize,
    pub request_timeout: Duration,
    pub allowed_origins: Vec<String>,
    pub heartbeat: Duration,
}

/// Dependencias de la aplicación.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<dyn PrincipalAuthenticator>,
    pub readiness: Arc<dyn Readiness>,
    pub bus: EventBus,
    pub config: AppConfig,
}

/// Construye la aplicación: `admin` son rutas administrativas (detrás del
/// guard) y `gateway` las del gateway de inferencia (fuera del guard).
pub fn app(state: AppState, admin: Router, gateway: Router) -> Router {
    let config = state.config.clone();
    let core = Router::new()
        .route("/api/v1/health/live", get(live))
        .route("/api/v1/health/ready", get(ready))
        .route("/api/v1/events", get(events))
        .with_state(state.clone());
    let timeout = config.request_timeout;
    let origins = Arc::new(config.allowed_origins.clone());
    // Parte administrativa: guard, fallback, límite de cuerpo, timeout y
    // envelope del contrato para los rechazos del framework.
    let administrative = core
        .merge(admin)
        .layer(from_fn_with_state(
            state.auth.clone(),
            crate::security::guard,
        ))
        .fallback(not_found)
        .layer(axum::extract::DefaultBodyLimit::max(
            config.request_body_bytes,
        ))
        .layer(from_fn(move |req, next| with_timeout(timeout, req, next)))
        .layer(from_fn(normalize));
    // El gateway proxea respuestas del modelo tal cual: sin normalizar ni
    // timeout administrativo (la inferencia puede durar más).
    administrative
        .merge(gateway)
        .layer(from_fn(log_request))
        .layer(from_fn(move |req, next| cors(origins.clone(), req, next)))
}

fn error_response(error: ApiError, correlation_id: &str) -> Response {
    Failure::from(error)
        .recorrelate(correlation_id)
        .into_response()
}

fn request_correlation(req: &Request) -> String {
    correlation_from_headers(req.headers())
}

fn correlation_from_headers(headers: &HeaderMap) -> String {
    headers
        .get(CORRELATION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| CorrelationId::parse(v).ok())
        .unwrap_or_default()
        .to_string()
}

/// Ruta sin handler: fuera del catálogo (`operation_unknown`) o del catálogo
/// pero no implementada aún en esta versión (`operation_not_implemented`).
async fn not_found(req: Request) -> Response {
    let correlation = request_correlation(&req);
    let reason = if larm_domain::auth::Catalog::contract()
        .find(req.method().as_str(), req.uri().path())
        .is_some()
    {
        "operation_not_implemented"
    } else {
        "operation_unknown"
    };
    error_response(
        ApiError::new("NOT_FOUND", "Operación no disponible", reason),
        &correlation,
    )
}

/// Correlación estable para toda la petición y log sin cuerpos ni cabeceras.
async fn log_request(mut req: Request, next: Next) -> Response {
    let correlation = request_correlation(&req);
    if let Ok(v) = HeaderValue::from_str(&correlation) {
        req.headers_mut().insert(CORRELATION_HEADER, v);
    }
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let started = Instant::now();
    let mut response = next.run(req).await;
    if let Ok(v) = HeaderValue::from_str(&correlation) {
        response.headers_mut().insert(CORRELATION_HEADER, v);
    }
    tracing::info!(
        correlation_id = %correlation,
        http.method = %method,
        http.path = %path,
        http.status = response.status().as_u16(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "petición"
    );
    response
}

/// Envelope del contrato 07 para los rechazos del propio framework (413, 405,
/// 415...), que Axum devuelve en texto plano.
async fn normalize(req: Request, next: Next) -> Response {
    let correlation = request_correlation(&req);
    let response = next.run(req).await;
    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json") || v.starts_with("text/event-stream"));
    if !status.is_client_error() || is_json {
        return response;
    }
    let (code, reason) = match status {
        StatusCode::PAYLOAD_TOO_LARGE => ("VALIDATION_FAILED", "body_too_large"),
        StatusCode::METHOD_NOT_ALLOWED => ("VALIDATION_FAILED", "method_not_allowed"),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ("VALIDATION_FAILED", "content_type_invalid"),
        _ => ("VALIDATION_FAILED", "request_rejected"),
    };
    let mut normalized = error_response(
        ApiError::new(code, "La petición no se puede procesar", reason),
        &correlation,
    );
    if status == StatusCode::METHOD_NOT_ALLOWED {
        *normalized.status_mut() = status;
    }
    if let Some(allow) = response.headers().get(header::ALLOW) {
        normalized
            .headers_mut()
            .insert(header::ALLOW, allow.clone());
    }
    normalized
}

/// `api.request_timeout_seconds` hasta tener cabeceras de respuesta (el
/// cuerpo de un SSE sigue abierto después).
async fn with_timeout(timeout: Duration, req: Request, next: Next) -> Response {
    let correlation = request_correlation(&req);
    match tokio::time::timeout(timeout, next.run(req)).await {
        Ok(response) => response,
        Err(_) => error_response(
            ApiError::new(
                "TIMEOUT",
                "La operación superó el tiempo máximo",
                "request_timeout",
            )
            .retryable(true),
            &correlation,
        ),
    }
}

/// CORS desactivado por defecto: sin orígenes configurados no se añade
/// ninguna cabecera y el navegador bloquea. Con orígenes, solo coincidencia
/// exacta (nunca `*`), y la preflight de un origen permitido se responde aquí.
async fn cors(origins: Arc<Vec<String>>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let allowed = origin
        .as_deref()
        .filter(|o| !origins.is_empty() && origins.iter().any(|a| a == o))
        .map(str::to_string);
    if req.method() == Method::OPTIONS {
        if let Some(origin) = &allowed {
            let mut res = StatusCode::NO_CONTENT.into_response();
            let h = res.headers_mut();
            if let Ok(v) = HeaderValue::from_str(origin) {
                h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            }
            h.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, PATCH, DELETE"),
            );
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type, idempotency-key, if-match, x-correlation-id, last-event-id"),
            );
            h.insert(header::VARY, HeaderValue::from_static("Origin"));
            return res;
        }
    }
    let mut res = next.run(req).await;
    if let Some(origin) = allowed {
        if let Ok(v) = HeaderValue::from_str(&origin) {
            res.headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            res.headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Origin"));
        }
    }
    res
}

async fn live() -> Response {
    (
        StatusCode::OK,
        axum::Json(Health {
            status: HealthStatus::Live,
            schema_version: "1".into(),
        }),
    )
        .into_response()
}

async fn ready(State(state): State<AppState>, req: Request<Body>) -> Response {
    let correlation = request_correlation(&req);
    match state.readiness.ready().await {
        Ok(ok) => (
            StatusCode::OK,
            axum::Json(Health {
                status: if ok {
                    HealthStatus::Ready
                } else {
                    HealthStatus::Degraded
                },
                schema_version: "1".into(),
            }),
        )
            .into_response(),
        Err(error) => error_response(error, &correlation),
    }
}

/// SSE `/api/v1/events`: cursor durable, replay antes del stream vivo y
/// heartbeat; la suscripción se crea antes de leer el historial para cerrar
/// la carrera entre el snapshot y nuevas publicaciones.
async fn events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let receiver = state.bus.subscribe();
    let closed = state.bus.closed.subscribe();
    let correlation = correlation_from_headers(&headers);
    let cursor = match headers.get("last-event-id") {
        None => None,
        Some(raw) => {
            let Ok(raw) = raw.to_str() else {
                return error_response(
                    ApiError::new(
                        "VALIDATION_FAILED",
                        "Last-Event-ID debe ser un entero decimal",
                        "event_cursor_invalid",
                    ),
                    &correlation,
                );
            };
            match larm_domain::bridge::parse_byte_count(raw)
                .and_then(|value| i64::try_from(value).ok())
            {
                Some(value) => Some(value),
                None => {
                    return error_response(
                        ApiError::new(
                            "VALIDATION_FAILED",
                            "Last-Event-ID debe ser un entero decimal no negativo",
                            "event_cursor_invalid",
                        ),
                        &correlation,
                    );
                }
            }
        }
    };
    let replay = match state.bus.replay_after(cursor).await {
        Ok(batch) => batch,
        Err(error) => return error_response(error, &correlation),
    };
    let hello =
        stream::once(async { Ok::<Event, Infallible>(Event::default().comment("conectado")) });
    let replay_stream = stream::iter(replay.events.into_iter().map(|event| {
        let data = serde_json::to_string(&event).unwrap_or_default();
        Ok::<Event, Infallible>(
            Event::default()
                .id(event.event_id)
                .event(event.event_type)
                .data(data),
        )
    }));
    let watermark = replay.through_event_id;
    let live = stream::unfold(Some((receiver, closed)), move |slot| async move {
        let (mut receiver, mut closed) = slot?;
        loop {
            if *closed.borrow() {
                return None;
            }
            tokio::select! {
                // Apagado: el stream termina y el apagado ordenado no espera al cliente.
                _ = closed.changed() => return None,
                received = receiver.recv() => match received {
                    Ok(event) => {
                        // El dispatcher puede publicar al bus durante el replay. El evento ya
                        // aparece en el lote persistido; no se envía dos veces al reconectar.
                        if event.event_id.parse::<i64>().is_ok_and(|id| id <= watermark) {
                            continue;
                        }
                        let data = serde_json::to_string(event.as_ref()).unwrap_or_default();
                        let sse = Event::default()
                            .id(event.event_id.clone())
                            .event(event.event_type.clone())
                            .data(data);
                        return Some((Ok(sse), Some((receiver, closed))));
                    }
                    // Cola llena: el cliente debe resincronizar; se cierra sin bloquear productores.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return Some((Ok(Event::default().event("resync_required").data("{}")), None));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    });
    Sse::new(hello.chain(replay_stream).chain(live))
        .keep_alive(
            KeepAlive::new()
                .interval(state.config.heartbeat)
                .text("heartbeat"),
        )
        .into_response()
}
