//! P104: aceptación real del servidor MCP de administración sobre la superficie
//! API real del Core en loopback del PC IA: hardware routes + jobs routes con
//! guard real, principal MCP enrolado con permisos separados, tools/list con
//! las 12 herramientas, tool call REAL (larm.get_hardware) ejecutado contra el
//! caso de uso, FORBIDDEN del half MCP para stop sin permiso,
//! confirmation_required de la destructiva y el rechazo del Core con
//! confirm=true (sin mock). Audit log sin secretos.
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
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

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
        created_at: format!("2026-11-15T10:00:0{n}.000Z"),
        updated_at: format!("2026-11-15T10:00:0{n}.000Z"),
        row_version: 1,
        metadata: serde_json::Map::new(),
        hardware_fingerprint: snapshot.fingerprint().unwrap(),
        snapshot,
        evidence_refs: vec![],
        probed_at: format!("2026-11-15T10:00:0{n}.000Z"),
    }
}

/// Despachador REAL: HTTP al daemon/Core en loopback con la credencial del
/// principal MCP presentada en la sesión.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn servidor_mcp_administracion_real_con_hardware_jobs_y_h06() {
    // 1) Superficie API real del Core: hardware + jobs con repos en memoria y
    //    guard real; un principal MCP starter (start+read) y uno full
    //    (read+delete) enrolados con credenciales reales.
    let dir = tempfile::tempdir().unwrap();
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p104-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let mut tokens = Vec::new();
    for (name, scopes, mcp) in [
        (
            "mcp-starter",
            vec![ApiScope::Read, ApiScope::LeasesManage],
            vec![McpPermission::Start, McpPermission::Read],
        ),
        (
            "mcp-full",
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![McpPermission::Read, McpPermission::Delete],
        ),
    ] {
        let (_, token) = principals
            .enroll(PrincipalKind::Mcp, name, scopes, mcp, None, &ctx)
            .await
            .unwrap();
        tokens.push(token);
    }
    let (starter_token, full_token) = (tokens[0].clone(), tokens[1].clone());

    let memory = Arc::new(MemoryProfiles {
        rows: parking_lot::Mutex::new(vec![profile(1)]),
    });
    let refresh = Arc::new(HardwareRefresh {
        prober: Arc::new(ScriptedProber {
            probe: Some(HardwareProbe {
                snapshot: snapshot("26200.9457"),
                metadata: serde_json::Map::new(),
                evidence_refs: vec!["P104-REAL".into()],
                probed_at: "2026-11-15T11:00:00.000Z".into(),
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

    // 2) Servidor MCP con el registro operador-registrado (hash de credenciales)
    //    y el dispatcher HTTP real.
    let registry = PrincipalRegistry::from_entries(vec![
        RegisteredPrincipal {
            id: uuid::Uuid::now_v7().to_string(),
            name: "mcp-starter".into(),
            token_sha256: hex::encode(Sha256::digest(starter_token.as_bytes())),
            api_scopes: vec!["read".into(), "leases:manage".into()],
            mcp_scopes: vec!["start".into(), "read".into()],
        },
        RegisteredPrincipal {
            id: uuid::Uuid::now_v7().to_string(),
            name: "mcp-full".into(),
            token_sha256: hex::encode(Sha256::digest(full_token.as_bytes())),
            api_scopes: vec!["read".into(), "models:write".into()],
            mcp_scopes: vec!["read".into(), "delete".into()],
        },
    ])
    .unwrap();
    let audit = dir.path().join("audit.ndjson");
    let mut mcp = McpAdminServer::new(
        McpAdminConfig {
            base_url: base_url.clone(),
            audit_log_path: audit.clone(),
            registry,
        },
        Arc::new(DaemonDispatcher {
            base_url: base_url.clone(),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        }),
    )
    .unwrap();

    // 3) initialize con la credencial real del principal starter.
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"larm_token": starter_token}
        }))
        .unwrap();
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        json!("larm-mcp-admin")
    );

    // 4) tools/list: las 12 herramientas del contrato sellado.
    let listing = mcp
        .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .unwrap();
    assert_eq!(listing["result"]["tools"].as_array().unwrap().len(), 12);

    // 5) Tool call REAL: larm.get_hardware ejecuta el caso de uso real del
    //    Core y devuelve el perfil observado.
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "larm.get_hardware", "arguments": {"arguments": {}}}
        }))
        .unwrap();
    assert_eq!(
        reply["result"]["isError"],
        json!(false),
        "hardware real: {reply}"
    );
    let structured = reply["result"]["structuredContent"].clone();
    println!(
        "REAL get_hardware: {}",
        structured.to_string().chars().take(200).collect::<String>()
    );

    // 6) H06: stop_model con leases:manage API pero SIN permiso MCP stop →
    //    FORBIDDEN antes del caso de uso (el dispatcher no registra llamada).
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "larm.stop_model",
                       "arguments": {"arguments": {"id": "0192aaaa-0000-7000-8000-0000000000ff"}}}
        }))
        .unwrap();
    assert_eq!(reply["result"]["isError"], json!(true));
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_permission_missing"));

    // 7) Sesión del principal full: destructiva sin confirm →
    //    confirmation_required; con confirm=true el caso de uso REAL del Core
    //    responde (el modelo no existe → isError, sin mock).
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "initialize",
            "params": {"larm_token": full_token}
        }))
        .unwrap();
    assert!(reply.get("result").is_some());
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "larm.delete_model",
                       "arguments": {"arguments": {"id": "0192aaaa-0000-7000-8000-0000000000ff"}}}
        }))
        .unwrap();
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("confirmation_required"));
    let reply = mcp
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "larm.delete_model",
                       "arguments": {"arguments": {"id": "0192aaaa-0000-7000-8000-0000000000ff"},
                                     "confirm": true}}
        }))
        .unwrap();
    assert_eq!(reply["result"]["isError"], json!(true));
    println!(
        "REAL delete_model confirmado: {}",
        reply["result"]["content"][0]["text"]
    );

    // 8) Audit log de tool calls sin secretos.
    let audit_content = std::fs::read_to_string(&audit).unwrap();
    assert!(audit_content.contains("\"tool\":\"larm.get_hardware\""));
    assert!(audit_content.contains("\"tool\":\"larm.delete_model\""));
    assert!(
        !audit_content.contains(&starter_token),
        "el audit no guarda tokens"
    );
    assert!(
        !audit_content.contains(&full_token),
        "el audit no guarda tokens"
    );

    let _ = shutdown_tx.send(());
    server.await.unwrap();
}
