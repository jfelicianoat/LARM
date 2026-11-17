//! P131: matriz H06 completa REAL sobre el PC IA — start/stop, install/update,
//! read/search y move/delete como concesiones distintas, `admin` nunca
//! implícito (ni comodín MCP que añada scopes API), contra el servidor MCP de
//! administración con principales reales enrolados y despacho observado con
//! un spy hacia el Core real en loopback (hardware + jobs con guard real).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_core::hardware::memory::{MemoryJournalJobs, MemoryProfiles, ScriptedProber};
use larm_core::hardware::HardwareRefresh;
use larm_core::security::memory::MemoryIdempotency;
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, McpPermission, PrincipalKind};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::{
    CpuInfo, HardwareProbe, HardwareProfile, HardwareSnapshot, MemoryInfo, ObservedMetrics, OsInfo,
    SYSTEM_RAM_POOL,
};
use larm_domain::ports::{JobSource, MutationContext, PrincipalAuthenticator};
use larm_protocol::mcp_admin::{
    CoreDispatcher, McpAdminConfig, McpAdminServer, PrincipalRegistry, RegisteredPrincipal,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

struct DaemonDispatcher {
    base_url: String,
    client: reqwest::Client,
}
impl CoreDispatcher for DaemonDispatcher {
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        bearer_token: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(u16, Value), LarmError> {
        let url = format!("{}{}", self.base_url, path);
        let method_string = method.to_string();
        let bearer = bearer_token.to_string();
        let idempotency = idempotency_key.map(str::to_string);
        let body = body.cloned();
        let client = self.client.clone();
        let (status, body) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut request = match method_string.as_str() {
                    "GET" => client.get(&url),
                    "POST" => client.post(&url),
                    "DELETE" => client.delete(&url),
                    "PATCH" => client.patch(&url),
                    other => {
                        return Err(LarmError::new(
                            ErrorCode::ValidationFailed,
                            "Método HTTP no soportado por el dispatcher",
                            "mcp_dispatcher_method_invalid",
                        )
                        .with_technical(other))
                    }
                };
                request = request.bearer_auth(&bearer);
                if let Some(key) = idempotency {
                    request = request.header("Idempotency-Key", key);
                }
                if let Some(body) = body {
                    request = request.json(&body);
                }
                let response = request.send().await.map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "El Core no respondió",
                        "mcp_dispatcher_http_failed",
                    )
                    .with_technical(error.to_string())
                })?;
                let status = response.status().as_u16();
                let body: Value = response.json().await.unwrap_or_else(|_| json!({}));
                Ok((status, body))
            })
        })?;
        Ok((status, body))
    }
}

/// Spy que registra cada despacho antes de delegarlo al Core real: la matriz
/// observa CONDUCTA (qué llamadas llegan al caso de uso), no constantes.
struct SpyDispatcher {
    inner: DaemonDispatcher,
    calls: Mutex<Vec<String>>,
}
impl SpyDispatcher {
    fn dispatched(&self, prefix: &str) -> bool {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.starts_with(prefix))
    }
    fn len(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
impl CoreDispatcher for SpyDispatcher {
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        bearer_token: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(u16, Value), LarmError> {
        self.calls.lock().unwrap().push(format!("{method} {path}"));
        self.inner
            .call(method, path, body, bearer_token, idempotency_key)
    }
}

fn snapshot(build: &str) -> HardwareSnapshot {
    HardwareSnapshot {
        os: OsInfo {
            edition: Some("Professional".into()),
            build: Some(build.into()),
            architecture: Some("AMD64".into()),
        },
        cpu: CpuInfo {
            vendor: None,
            model: None,
            physical_cores: None,
            logical_cores: Some(32),
            instruction_sets: vec!["avx2".into()],
        },
        ram: MemoryInfo {
            physical_bytes: None,
            usable_bytes: larm_domain::units::DecimalU64::new(64 << 30),
            pool_id: SYSTEM_RAM_POOL.into(),
        },
        devices: vec![],
        dependencies: vec![],
        observed_metrics: ObservedMetrics {
            available_ram_bytes: None,
            cpu_usage_percent: Some(1.5),
        },
    }
}

fn profile(n: u32) -> HardwareProfile {
    let snapshot = snapshot(&format!("26200.{n}"));
    HardwareProfile {
        id: format!("0192aaaa-0000-7000-8000-{n:012}"),
        created_at: format!("2026-11-16T10:00:0{n}.000Z"),
        updated_at: format!("2026-11-16T10:00:0{n}.000Z"),
        row_version: 1,
        metadata: serde_json::Map::new(),
        hardware_fingerprint: snapshot.fingerprint().unwrap(),
        snapshot,
        evidence_refs: vec!["P131-REAL".into()],
        probed_at: format!("2026-11-16T10:00:0{n}.000Z"),
    }
}

const ID: &str = "0192aaaa-0000-7000-8000-0000000000ff";

fn initialize(mcp: &mut McpAdminServer, id: i32, token: &str) {
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {"larm_token": token}
        }))
        .unwrap();
    assert!(reply.get("result").is_some(), "initialize: {reply}");
}

fn tool_call(mcp: &mut McpAdminServer, id: i32, tool: &str, arguments: Value) -> Value {
    mcp.handle_request(&json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    }))
    .unwrap()["result"]
        .clone()
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matriz_h06_completa_con_concesiones_distintas_y_sin_admin_implicito() {
    // 1) Core real del PC IA: hardware + jobs con repos en memoria y guard
    //    real en loopback.
    let dir = tempfile::tempdir().unwrap();
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p131-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let scenarios: [(&str, Vec<ApiScope>, Vec<McpPermission>); 7] = [
        (
            "h06-start",
            vec![ApiScope::LeasesManage],
            vec![McpPermission::Start],
        ),
        (
            "h06-installer",
            vec![ApiScope::RuntimesWrite],
            vec![McpPermission::Install],
        ),
        (
            "h06-reader",
            vec![ApiScope::Read],
            vec![McpPermission::Read],
        ),
        (
            "h06-searcher",
            vec![ApiScope::Read],
            vec![McpPermission::Search],
        ),
        (
            "h06-mover",
            vec![ApiScope::StorageWrite],
            vec![McpPermission::Move],
        ),
        (
            "h06-deleter",
            vec![ApiScope::ModelsWrite],
            vec![McpPermission::Delete],
        ),
        ("h06-admin-mcp", vec![], vec![McpPermission::Admin]),
    ];
    let mut entries = Vec::new();
    let mut tokens = Vec::new();
    for (name, api, mcp) in scenarios {
        let (_, token) = principals
            .enroll(
                PrincipalKind::Mcp,
                name,
                api.clone(),
                mcp.clone(),
                None,
                &ctx,
            )
            .await
            .unwrap();
        entries.push(RegisteredPrincipal {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.to_string(),
            token_sha256: hex::encode(Sha256::digest(token.as_bytes())),
            api_scopes: api.iter().map(|scope| scope.as_str().to_string()).collect(),
            mcp_scopes: mcp.iter().map(|scope| scope.as_str().to_string()).collect(),
        });
        tokens.push(token);
    }
    let registry = PrincipalRegistry::from_entries(entries).unwrap();

    let memory = Arc::new(MemoryProfiles {
        rows: parking_lot::Mutex::new(vec![profile(1)]),
    });
    let refresh = Arc::new(HardwareRefresh {
        prober: Arc::new(ScriptedProber {
            probe: Some(HardwareProbe {
                snapshot: snapshot("26200.9457"),
                metadata: serde_json::Map::new(),
                evidence_refs: vec!["P131-REAL".into()],
                probed_at: "2026-11-16T10:00:00.000Z".into(),
            }),
            delay: Duration::from_millis(0),
        }),
        profiles: memory.clone(),
        jobs: Arc::new(MemoryJournalJobs::default()),
        storage: None,
    });
    let sources: Vec<Arc<dyn JobSource>> = vec![refresh.clone()];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let auth: Arc<dyn PrincipalAuthenticator> = principals.clone();
    let app = larm_api::hardware::routes(larm_api::hardware::HardwareState {
        repo: memory.clone(),
        cursors: Arc::new(CursorSigner::default()),
        refresh: refresh.clone(),
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .merge(larm_api::jobs::routes(Arc::new(sources)))
    .layer(axum::middleware::from_fn_with_state(
        auth,
        larm_api::security::guard,
    ));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    let base_url = format!("http://{address}");

    // 2) Servidor MCP con despacho observado (spy) al Core real.
    let audit = dir.path().join("audit.ndjson");
    let spy = Arc::new(SpyDispatcher {
        inner: DaemonDispatcher {
            base_url: base_url.clone(),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        },
        calls: Mutex::new(Vec::new()),
    });
    let mut mcp = McpAdminServer::new(
        McpAdminConfig {
            base_url: base_url.clone(),
            audit_log_path: audit.clone(),
            registry,
        },
        spy.clone(),
    )
    .unwrap();

    // 3) Sesiones y llamadas: cada principal hace initialize con su
    //    credencial real y las llamadas observan el despacho del spy.
    let mut id = 0i32;
    let begin = |mcp: &mut McpAdminServer, token: &str, id: &mut i32| {
        *id += 1;
        initialize(mcp, *id, token);
    };

    // 4) start/stop: con start el arranque SE DESPACHA al Core; detener sin
    //    la concesión stop es FORBIDDEN sin despachar.
    begin(&mut mcp, &tokens[0], &mut id);
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.start_model",
        json!({"arguments": {"id": ID}}),
    );
    assert!(spy.len() > before, "start autorizado llega al caso de uso");
    assert!(
        spy.dispatched("POST /api/v1/leases"),
        "start despacha POST /api/v1/leases"
    );
    // El Core real responde (ruta de leases cualificadas no montada aquí):
    // el rechazo del caso de uso es la conducta real, no un mock.
    println!("REAL H06 start: {}", text_of(&result));
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.stop_model",
        json!({"arguments": {"id": ID}}),
    );
    assert_eq!(spy.len(), before, "stop sin concesión NO despacha");
    assert!(text_of(&result).contains("mcp_permission_missing"));
    println!("REAL H06: start despacha y stop sin concesion es FORBIDDEN");

    // 5) install/update: instalar SE DESPACHA; actualizar sin la concesión
    //    update es FORBIDDEN sin despachar.
    begin(&mut mcp, &tokens[1], &mut id);
    let before = spy.len();
    id += 1;
    tool_call(
        &mut mcp,
        id,
        "larm.install_runtime",
        json!({"arguments": {}}),
    );
    assert!(spy.len() > before && spy.dispatched("POST /api/v1/runtime-installations"));
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.update_runtime",
        json!({"arguments": {}}),
    );
    assert_eq!(spy.len(), before);
    assert!(text_of(&result).contains("mcp_permission_missing"));
    println!("REAL H06: install despacha y update sin concesion es FORBIDDEN");

    // 6) read/search: el lector ejecuta el caso de uso REAL de hardware y no
    //    puede buscar; el buscador despacha la búsqueda y no puede leer.
    begin(&mut mcp, &tokens[2], &mut id);
    id += 1;
    let result = tool_call(&mut mcp, id, "larm.get_hardware", json!({"arguments": {}}));
    assert_eq!(result["isError"], json!(false), "hardware real: {result}");
    assert!(spy.dispatched("GET /api/v1/hardware"));
    let before = spy.len();
    id += 1;
    let result = tool_call(&mut mcp, id, "larm.search_models", json!({"arguments": {}}));
    assert_eq!(spy.len(), before);
    assert!(text_of(&result).contains("mcp_permission_missing"));
    begin(&mut mcp, &tokens[3], &mut id);
    id += 1;
    tool_call(&mut mcp, id, "larm.search_models", json!({"arguments": {}}));
    assert!(spy.dispatched("POST /api/v1/models/search"));
    let before = spy.len();
    id += 1;
    let result = tool_call(&mut mcp, id, "larm.get_hardware", json!({"arguments": {}}));
    assert_eq!(spy.len(), before);
    assert!(text_of(&result).contains("mcp_permission_missing"));
    println!("REAL H06: read y search son concesiones distintas en ambas direcciones");

    // 7) move/delete: destructivas con confirmación; mover sin la concesión
    //    delete y borrar sin la concesión move es FORBIDDEN sin despachar.
    begin(&mut mcp, &tokens[4], &mut id);
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.move_model",
        json!({"arguments": {"id": ID}}),
    );
    assert_eq!(spy.len(), before, "destructiva sin confirm no despacha");
    assert!(text_of(&result).contains("confirmation_required"));
    let before = spy.len();
    id += 1;
    tool_call(
        &mut mcp,
        id,
        "larm.move_model",
        json!({"arguments": {"id": ID}, "confirm": true}),
    );
    assert!(spy.len() > before && spy.dispatched("POST /api/v1/models/"));
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.delete_model",
        json!({"arguments": {"id": ID}}),
    );
    assert_eq!(spy.len(), before);
    assert!(text_of(&result).contains("mcp_permission_missing"));
    begin(&mut mcp, &tokens[5], &mut id);
    id += 1;
    tool_call(
        &mut mcp,
        id,
        "larm.delete_model",
        json!({"arguments": {"id": ID}, "confirm": true}),
    );
    assert!(spy.dispatched("DELETE /api/v1/models/"));
    let before = spy.len();
    id += 1;
    let result = tool_call(
        &mut mcp,
        id,
        "larm.move_model",
        json!({"arguments": {"id": ID}}),
    );
    assert_eq!(spy.len(), before);
    assert!(text_of(&result).contains("mcp_permission_missing"));
    println!("REAL H06: move y delete son concesiones distintas y destructivas con confirmacion");

    // 8) admin nunca implícito: el comodín MCP admin no añade scopes API —
    //    cualquier herramienta falla el half API con scope_missing y no
    //    despacha.
    begin(&mut mcp, &tokens[6], &mut id);
    for (tool, arguments) in [
        ("larm.get_hardware", json!({"arguments": {}})),
        ("larm.start_model", json!({"arguments": {"id": ID}})),
        (
            "larm.delete_model",
            json!({"arguments": {"id": ID}, "confirm": true}),
        ),
    ] {
        let before = spy.len();
        id += 1;
        let result = tool_call(&mut mcp, id, tool, arguments);
        assert_eq!(spy.len(), before, "{tool} con admin MCP no despacha");
        assert!(
            text_of(&result).contains("scope_missing"),
            "{tool}: {result}"
        );
    }
    println!(
        "REAL H06: admin comodin MCP nunca anade scopes API (scope_missing en 3 herramientas)"
    );

    // 9) Audit: registra cada herramienta con su resultado y sin secretos.
    let audit_content = std::fs::read_to_string(&audit).unwrap();
    for tool in [
        "larm.start_model",
        "larm.stop_model",
        "larm.install_runtime",
        "larm.update_runtime",
        "larm.get_hardware",
        "larm.search_models",
        "larm.move_model",
        "larm.delete_model",
    ] {
        assert!(
            audit_content.contains(&format!("\"tool\":\"{tool}\"")),
            "audit: {tool}"
        );
    }
    for token in &tokens {
        assert!(
            !audit_content.contains(token.as_str()),
            "el audit no guarda tokens"
        );
    }

    let _ = shutdown_tx.send(());
    server.await.unwrap();
}
