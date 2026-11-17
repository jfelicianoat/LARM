//! Aplicación HTTP (P005) con Tower: health/readiness, envelope de error,
//! timeout, límite de cuerpo, CORS, SSE y gateway fuera del guard.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use futures::StreamExt;
use larm_api::app::{
    app, AppConfig, AppState, EventBus, EventEnvelope, EventReplayBatch, EventReplayStore,
    Readiness,
};
use larm_core::search::{ModelSearchService, SearchResult};
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::{ApiError, Page};
use larm_domain::ports::MutationContext;
use larm_domain::search::Search;
use serde_json::Value;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

/// 0 = lista, 1 = degradada, 2 = no comprobable.
struct Probe(AtomicU8);

#[async_trait]
impl Readiness for Probe {
    async fn ready(&self) -> Result<bool, ApiError> {
        match self.0.load(Ordering::SeqCst) {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(ApiError::new(
                "UPSTREAM_UNAVAILABLE",
                "Base de datos no disponible",
                "database_unreachable",
            )
            .retryable(true)),
        }
    }
}

struct H {
    app: Router,
    reader: String,
    probe: Arc<Probe>,
    bus: EventBus,
    search_calls: Arc<AtomicUsize>,
}

struct EmptySearch(Arc<AtomicUsize>);

#[async_trait]
impl ModelSearchService for EmptySearch {
    async fn search(&self, request: &Search) -> Result<SearchResult, ApiError> {
        request.validate()?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(SearchResult {
            page: Page {
                items: Vec::new(),
                next_cursor: None,
                snapshot_at: "2026-10-22T10:00:00.000Z".into(),
            },
            warnings: Vec::new(),
        })
    }
}

async fn harness(origins: Vec<String>) -> H {
    harness_with_replay(origins, None).await
}

async fn harness_with_replay(origins: Vec<String>, replay: Option<Arc<dyn EventReplayStore>>) -> H {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "t".into(),
        correlation_id: "c".into(),
    };
    let (_, reader) = principals
        .enroll(
            PrincipalKind::Operator,
            "lector",
            vec![ApiScope::Read],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let probe = Arc::new(Probe(AtomicU8::new(0)));
    let bus = replay.map_or_else(EventBus::default, |store| {
        EventBus::default().with_replay_store(store)
    });
    let state = AppState {
        auth: principals,
        auth_limiter: Arc::new(larm_api::auth::AuthRateLimiter::new()),
        readiness: probe.clone(),
        bus: bus.clone(),
        config: AppConfig {
            request_body_bytes: 1024,
            request_timeout: Duration::from_millis(300),
            allowed_origins: origins,
            heartbeat: Duration::from_secs(15),
        },
    };
    // Rutas administrativas de prueba: una lenta (para el timeout) y las dos
    // rutas reales de búsqueda.
    let search_calls = Arc::new(AtomicUsize::new(0));
    let admin = Router::new()
        .route(
            "/api/v1/hardware",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                "tarde"
            }),
        )
        .merge(larm_api::search::routes(larm_api::search::SearchState {
            service: Arc::new(EmptySearch(search_calls.clone())),
            idempotency: Arc::new(MemoryIdempotency::default()),
        }));
    let gateway = Router::new().route(
        "/api/v1/gateway/{lease}/v1/{*path}",
        post(|| async { (StatusCode::UNAUTHORIZED, "gateway propio") }),
    );
    H {
        app: app(state, admin, gateway),
        reader,
        probe,
        bus,
        search_calls,
    }
}

struct PausedReplayStore {
    batch: EventReplayBatch,
    started: Notify,
    release: Notify,
}

#[async_trait]
impl EventReplayStore for PausedReplayStore {
    async fn replay_after(&self, _after: Option<i64>) -> Result<EventReplayBatch, ApiError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(self.batch.clone())
    }
}

struct ErrorReplayStore(ApiError);

#[async_trait]
impl EventReplayStore for ErrorReplayStore {
    async fn replay_after(&self, _after: Option<i64>) -> Result<EventReplayBatch, ApiError> {
        Err(self.0.clone())
    }
}

fn fixture_event(event_id: i64) -> EventEnvelope {
    EventEnvelope {
        schema_version: 1,
        event_id: event_id.to_string(),
        event_type: "fixture.changed".into(),
        occurred_at: "2026-10-22T00:00:00.000Z".into(),
        correlation_id: "0192aaaa-0000-7000-8000-000000000009".into(),
        actor_id: "test".into(),
        resource_type: "fixture".into(),
        resource_id: "fixture-1".into(),
        resource_version: event_id,
        data: serde_json::json!({"id": event_id}),
    }
}

async fn call(h: &H, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let res = h.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get_req(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn live_publico_y_ready_con_scope_read() {
    let h = harness(vec![]).await;
    let (s, _, v) = call(&h, get_req("/api/v1/health/live", None)).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            serde_json::json!({"status": "LIVE", "schema_version": "1"})
        )
    );
    let (s, _, v) = call(&h, get_req("/api/v1/health/ready", None)).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AUTH_REQUIRED"))
    );
    let (s, _, v) = call(&h, get_req("/api/v1/health/ready", Some(&h.reader))).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("READY")));
    h.probe.0.store(1, Ordering::SeqCst);
    let (_, _, v) = call(&h, get_req("/api/v1/health/ready", Some(&h.reader))).await;
    assert_eq!(v["status"], "DEGRADED");
    h.probe.0.store(2, Ordering::SeqCst);
    let (s, _, v) = call(&h, get_req("/api/v1/health/ready", Some(&h.reader))).await;
    assert_eq!(
        (s, v["code"].as_str(), v["retryable"].as_bool()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("UPSTREAM_UNAVAILABLE"),
            Some(true)
        )
    );
}

#[tokio::test]
async fn envelope_en_rutas_desconocidas_no_implementadas_y_rechazos_del_framework() {
    let h = harness(vec![]).await;
    let envelope_keys = ["code", "message", "retryable", "correlation_id", "details"];
    // fuera del catálogo: el guard
    let (s, headers, v) = call(&h, get_req("/api/v1/no-existe", Some(&h.reader))).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::NOT_FOUND, Some("operation_unknown"))
    );
    assert!(envelope_keys.iter().all(|k| v.get(k).is_some()));
    assert_eq!(
        headers["x-correlation-id"].to_str().unwrap(),
        v["correlation_id"].as_str().unwrap()
    );
    // en el catálogo pero sin implementar en esta versión
    let (s, _, v) = call(&h, get_req("/api/v1/runtimes", Some(&h.reader))).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::NOT_FOUND, Some("operation_not_implemented"))
    );
    // cuerpo por encima del límite
    let big = Request::post("/api/v1/models/search")
        .header("authorization", format!("Bearer {}", h.reader))
        .body(Body::from(vec![b'x'; 4096]))
        .unwrap();
    let (s, _, v) = call(&h, big).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("body_too_large"))
    );
}

#[tokio::test]
async fn search_respeta_scope_schema_e_idempotencia() {
    let h = harness(vec![]).await;
    let body = serde_json::json!({
        "q":"", "capabilities":[], "source_ids":[], "compatibility":"ALL",
        "architectures":[], "formats":[], "quantizations":[], "licenses":[],
        "runtime_ids":[], "parameter_count":{"minimum":null,"maximum":null},
        "size_bytes":{"minimum":null,"maximum":null},
        "trust_score":{"minimum":null,"maximum":null}, "certification_statuses":[],
        "only_executable":false,"only_recommended":false,"show_incompatible":true,
        "cursor":null,"limit":50
    });
    let request = || {
        Request::post("/api/v1/models/search")
            .header("authorization", format!("Bearer {}", h.reader))
            .header("content-type", "application/json")
            .header("idempotency-key", "search-read-1")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (status, _, result) = call(&h, request()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(result["items"], serde_json::json!([]));
    assert_eq!(result["snapshot_at"], "2026-10-22T10:00:00.000Z");
    let (status, _, replay) = call(&h, request()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay, result);
    assert_eq!(h.search_calls.load(Ordering::SeqCst), 1);

    let mut altered = body;
    altered["only_recommended"] = true.into();
    let (status, _, conflict) = call(
        &h,
        Request::post("/api/v1/models/search")
            .header("authorization", format!("Bearer {}", h.reader))
            .header("content-type", "application/json")
            .header("idempotency-key", "search-read-1")
            .body(Body::from(altered.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["details"]["reason"], "idempotency_body_mismatch");

    let (status, _, denied) = call(
        &h,
        Request::post("/api/v1/sources/search")
            .header("authorization", format!("Bearer {}", h.reader))
            .header("content-type", "application/json")
            .header("idempotency-key", "source-search-read-1")
            .body(Body::from(serde_json::json!({}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(denied["details"]["reason"], "scope_missing");
}

#[tokio::test]
async fn metodo_incorrecto_es_405_con_allow_y_correlacion() {
    let h = harness(vec![]).await;
    let correlation = uuid::Uuid::now_v7().to_string();
    let req = Request::post("/api/v1/health/live")
        .header("x-correlation-id", &correlation)
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = call(&h, req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "GET, HEAD");
    assert_eq!(headers["x-correlation-id"], correlation);
    assert_eq!(body["correlation_id"], correlation);
    assert_eq!(body["details"]["reason"], "method_not_allowed");
    // HEAD conserva la autenticación de la operación GET.
    let (status, _, _) = call(
        &h,
        Request::head("/api/v1/health/ready")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &h,
        Request::head("/api/v1/health/live")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn timeout_devuelve_504_con_envelope() {
    let h = harness(vec![]).await;
    let started = std::time::Instant::now();
    let (s, _, v) = call(&h, get_req("/api/v1/hardware", Some(&h.reader))).await;
    assert_eq!(
        (s, v["code"].as_str(), v["retryable"].as_bool()),
        (StatusCode::GATEWAY_TIMEOUT, Some("TIMEOUT"), Some(true))
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cortó en el timeout, no esperó al handler"
    );
}

#[tokio::test]
async fn cors_desactivado_por_defecto_y_exacto_si_se_configura() {
    let h = harness(vec![]).await;
    let req = Request::get("/api/v1/health/live")
        .header("origin", "http://evil.example")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = call(&h, req).await;
    assert!(headers.get("access-control-allow-origin").is_none());
    let pre = Request::options("/api/v1/health/live")
        .header("origin", "http://evil.example")
        .body(Body::empty())
        .unwrap();
    let (s, headers, _) = call(&h, pre).await;
    assert_ne!(s, StatusCode::NO_CONTENT);
    assert!(headers.get("access-control-allow-origin").is_none());

    let h = harness(vec!["http://127.0.0.1:1420".into()]).await;
    let pre = Request::options("/api/v1/health/ready")
        .header("origin", "http://127.0.0.1:1420")
        .body(Body::empty())
        .unwrap();
    let (s, headers, _) = call(&h, pre).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        headers["access-control-allow-origin"],
        "http://127.0.0.1:1420"
    );
    let other = Request::get("/api/v1/health/live")
        .header("origin", "http://127.0.0.1:9999")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = call(&h, other).await;
    assert!(
        headers.get("access-control-allow-origin").is_none(),
        "solo coincidencia exacta, nunca *"
    );
}

#[tokio::test]
async fn gateway_fuera_del_guard() {
    let h = harness(vec![]).await;
    let req =
        Request::post("/api/v1/gateway/0192aaaa-0000-7000-8000-000000000001/v1/chat/completions")
            .body(Body::empty())
            .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(
        &body[..],
        b"gateway propio",
        "lo responde el gateway, no el guard"
    );
}

#[tokio::test]
async fn sse_conecta_entrega_eventos_y_libera_al_cerrar() {
    let h = harness(vec![]).await;
    let (s, _, _) = call(&h, get_req("/api/v1/events", None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let res = h
        .app
        .clone()
        .oneshot(get_req("/api/v1/events", Some(&h.reader)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let mut body = res.into_body().into_data_stream();
    let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
    assert!(first.contains(": conectado"), "{first}");
    assert_eq!(h.bus.subscribers(), 1);
    h.bus.publish(EventEnvelope {
        schema_version: 1,
        event_id: "42".into(),
        event_type: "credential.created".into(),
        occurred_at: "2026-10-11T00:00:00.000Z".into(),
        correlation_id: "0192aaaa-0000-7000-8000-000000000009".into(),
        actor_id: "a".into(),
        resource_type: "credential_reference".into(),
        resource_id: "0192aaaa-0000-7000-8000-000000000001".into(),
        resource_version: 1,
        data: serde_json::json!({}),
    });
    let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(
        text.contains("id: 42")
            && text.contains("event: credential.created")
            && text.contains("\"schema_version\":1"),
        "{text}"
    );
    drop(body);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        h.bus.subscribers(),
        0,
        "cerrar la conexión libera la suscripción"
    );
}

#[tokio::test]
async fn sse_reanuda_por_last_event_id_y_filtra_la_carrera_con_el_bus() {
    let store = Arc::new(PausedReplayStore {
        batch: EventReplayBatch {
            events: vec![fixture_event(42)],
            through_event_id: 42,
        },
        started: Notify::new(),
        release: Notify::new(),
    });
    let h = harness_with_replay(vec![], Some(store.clone())).await;
    let app = h.app.clone();
    let reader = h.reader.clone();
    let req = Request::get("/api/v1/events")
        .header("authorization", format!("Bearer {reader}"))
        .header("last-event-id", "40")
        .body(Body::empty())
        .unwrap();
    let response_task = tokio::spawn(async move { app.oneshot(req).await.unwrap() });

    store.started.notified().await;
    assert!(h.bus.publish(fixture_event(42)));
    assert!(h.bus.publish(fixture_event(43)));
    store.release.notify_one();

    let response = response_task.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let _hello = body.next().await.unwrap().unwrap();
    let replay = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let replay = String::from_utf8(replay.to_vec()).unwrap();
    assert!(replay.contains("id: 42"), "{replay}");
    let live = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let live = String::from_utf8(live.to_vec()).unwrap();
    assert!(live.contains("id: 43"), "{live}");
    assert!(
        !live.contains("id: 42"),
        "no repite el evento del replay: {live}"
    );
}

#[tokio::test]
async fn sse_cursor_expirado_devuelve_410_antes_de_abrir_stream() {
    let replay = Arc::new(ErrorReplayStore(ApiError::new(
        "EVENT_CURSOR_EXPIRED",
        "El cursor de eventos ya no está disponible; resincroniza el estado antes de reconectar",
        "event_cursor_expired",
    )));
    let h = harness_with_replay(vec![], Some(replay)).await;
    let req = Request::get("/api/v1/events")
        .header("authorization", format!("Bearer {}", h.reader))
        .header("last-event-id", "10")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = call(&h, req).await;
    assert_eq!(status, StatusCode::GONE);
    assert_ne!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(body["code"], "EVENT_CURSOR_EXPIRED");
    assert!(!body.to_string().contains("snapshot_url"));
}

#[tokio::test]
async fn sse_rechaza_cursor_no_decimal_y_cursor_futuro() {
    let replay = Arc::new(ErrorReplayStore(ApiError::new(
        "VALIDATION_FAILED",
        "El cursor de eventos está por delante del servidor",
        "event_cursor_future",
    )));
    let h = harness_with_replay(vec![], Some(replay)).await;
    let invalid = Request::get("/api/v1/events")
        .header("authorization", format!("Bearer {}", h.reader))
        .header("last-event-id", "01")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&h, invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["details"]["reason"], "event_cursor_invalid");

    let future = Request::get("/api/v1/events")
        .header("authorization", format!("Bearer {}", h.reader))
        .header("last-event-id", "999")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = call(&h, future).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["details"]["reason"], "event_cursor_future");
}

#[tokio::test]
async fn sse_cola_desbordada_envia_resync_y_cierra() {
    let h = harness(vec![]).await;
    let res = h
        .app
        .clone()
        .oneshot(get_req("/api/v1/events", Some(&h.reader)))
        .await
        .unwrap();
    let mut body = res.into_body().into_data_stream();
    let _hello = body.next().await;
    for i in 0..(larm_api::app::SUBSCRIBER_QUEUE + 10) {
        h.bus.publish(EventEnvelope {
            schema_version: 1,
            event_id: i.to_string(),
            event_type: "download.progress".into(),
            occurred_at: "2026-10-11T00:00:00.000Z".into(),
            correlation_id: "c".into(),
            actor_id: "a".into(),
            resource_type: "download_job".into(),
            resource_id: "r".into(),
            resource_version: 1,
            data: serde_json::json!({}),
        });
    }
    let mut saw_resync = false;
    while let Ok(Some(chunk)) = tokio::time::timeout(Duration::from_secs(2), body.next()).await {
        if String::from_utf8_lossy(&chunk.unwrap()).contains("event: resync_required") {
            saw_resync = true;
        }
    }
    assert!(
        saw_resync,
        "el suscriptor lento recibe resync_required y el stream termina"
    );
}

#[tokio::test]
async fn cerrar_el_bus_termina_los_streams_sse() {
    let h = harness(vec![]).await;
    let res = h
        .app
        .clone()
        .oneshot(get_req("/api/v1/events", Some(&h.reader)))
        .await
        .unwrap();
    let mut body = res.into_body().into_data_stream();
    let _hello = body.next().await;
    h.bus.close();
    let end = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = body.next().await {
            chunk.unwrap();
        }
    })
    .await;
    assert!(
        end.is_ok(),
        "el stream termina al cerrar el bus (apagado ordenado)"
    );
}
