//! P104: pruebas del servidor MCP de administración — matriz H06 (start/stop,
//! install/update, admin nunca implícito), scope API ausente niega, puerta de
//! confirmación para destructivas, bucle stdio con JSON inválido y audit sin
//! secretos. El dispatcher falso observa que una herramienta denegada nunca
//! llega al caso de uso Core.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use larm_domain::error::LarmError;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct FakeDispatcher {
    calls: Mutex<Vec<(String, String)>>,
    reply: Value,
    status: u16,
}
impl CoreDispatcher for FakeDispatcher {
    fn call(
        &self,
        method: &str,
        path: &str,
        _body: Option<&Value>,
        _bearer_token: &str,
        _idempotency_key: Option<&str>,
    ) -> Result<(u16, Value), LarmError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), path.to_string()));
        Ok((self.status, self.reply.clone()))
    }
}

const TOKEN: &str = "larm-mcp-fixture-token-0123456789abcdef";

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn registry() -> PrincipalRegistry {
    PrincipalRegistry::from_entries(vec![RegisteredPrincipal {
        id: "0192aaaa-0000-7000-8000-000000000001".into(),
        name: "mcp-starter".into(),
        token_sha256: digest(TOKEN),
        // start con el scope API que su operación exige (leases:manage) SIN
        // stop: el half MCP separa arrancar de detener con el mismo scope.
        api_scopes: vec!["leases:manage".into(), "read".into()],
        mcp_scopes: vec!["start".into(), "read".into()],
    }])
    .unwrap()
}

fn initialized_server(dispatcher: Arc<dyn CoreDispatcher>) -> McpAdminServer {
    let mut server = McpAdminServer::new(
        McpAdminConfig {
            base_url: "http://127.0.0.1:1".into(),
            audit_log_path: PathBuf::from("target/p104-audit-test.ndjson"),
            registry: registry(),
        },
        dispatcher,
    )
    .unwrap();
    let reply = server.handle_request(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"larm_token": TOKEN}
    }));
    assert!(reply.is_some());
    server
}

fn call(server: &mut McpAdminServer, id: i64, name: &str, arguments: Value) -> Value {
    server
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .unwrap()
}

#[test]
fn initialize_rechaza_credencial_desconocida_y_valida_anuncia_doce() {
    let mut server = McpAdminServer::new(
        McpAdminConfig {
            base_url: "http://127.0.0.1:1".into(),
            audit_log_path: PathBuf::from("target/p104-audit-test.ndjson"),
            registry: registry(),
        },
        Arc::new(FakeDispatcher {
            calls: Mutex::new(vec![]),
            reply: json!({}),
            status: 200,
        }),
    )
    .unwrap();
    let reply = server
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"larm_token": "token-desconocido"}
        }))
        .unwrap();
    assert_eq!(reply["error"]["code"], -32001);
    let reply = server
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"larm_token": TOKEN}
        }))
        .unwrap();
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        json!("larm-mcp-admin")
    );
    let listing = server
        .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .unwrap();
    assert_eq!(listing["result"]["tools"].as_array().unwrap().len(), 12);
    let unknown = server
        .handle_request(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "larm.inexistente", "arguments": {}}
        }))
        .unwrap();
    assert!(unknown["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_tool_not_announced"));
}

#[test]
fn h06_start_permitido_y_stop_denegado_para_el_mismo_principal() {
    let dispatcher = Arc::new(FakeDispatcher {
        calls: Mutex::new(vec![]),
        reply: json!({"job_id": "job-1", "state": "QUEUED", "status_url": "/api/v1/jobs/job-1"}),
        status: 202,
    });
    let mut server = initialized_server(dispatcher.clone());
    // start_model permitido: permiso start + leases:manage.
    let reply = call(&mut server, 2, "larm.start_model", json!({"arguments": {}}));
    assert_eq!(reply["result"]["isError"], json!(false));
    // Compacto con job ID para operación larga.
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("job-1"), "{text}");
    // stop_model con leases:manage API pero SIN permiso MCP stop → FORBIDDEN.
    let reply = call(&mut server, 3, "larm.stop_model", json!({"arguments": {}}));
    assert_eq!(reply["result"]["isError"], json!(true));
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_permission_missing"));
    assert_eq!(
        dispatcher.calls.lock().unwrap().len(),
        1,
        "stop no llegó al caso de uso"
    );
}

#[test]
fn h06_install_y_update_denegados_para_principal_de_start() {
    let dispatcher = Arc::new(FakeDispatcher {
        calls: Mutex::new(vec![]),
        reply: json!({"job_id": "job-2", "state": "QUEUED"}),
        status: 202,
    });
    let mut server = initialized_server(dispatcher.clone());
    // install_model exige permiso download + jobs:write: el principal de start
    // no tiene ninguno → FORBIDDEN por el half MCP.
    let reply = call(
        &mut server,
        1,
        "larm.install_model",
        json!({"arguments": {}}),
    );
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_permission_missing"));
    // update_runtime exige permiso update: FORBIDDEN igualmente.
    let reply = call(
        &mut server,
        2,
        "larm.update_runtime",
        json!({"arguments": {}}),
    );
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_permission_missing"));
    assert!(dispatcher.calls.lock().unwrap().is_empty());
}

#[test]
fn scope_api_ausente_niega_incluso_con_permiso_mcp() {
    // Un principal con permiso MCP admin pero SIN scopes API: admin permite
    // la herramienta MCP, pero sin el scope API el half API niega (FORBIDDEN)
    // — admin nunca añade scopes API.
    let registry = PrincipalRegistry::from_entries(vec![RegisteredPrincipal {
        id: "0192aaaa-0000-7000-8000-000000000003".into(),
        name: "mcp-admin-only".into(),
        token_sha256: digest("larm-mcp-admin-only-token-9911223344"),
        api_scopes: vec![],
        mcp_scopes: vec!["admin".into()],
    }])
    .unwrap();
    let mut server = McpAdminServer::new(
        McpAdminConfig {
            base_url: "http://127.0.0.1:1".into(),
            audit_log_path: PathBuf::from("target/p104-audit-test.ndjson"),
            registry,
        },
        Arc::new(FakeDispatcher {
            calls: Mutex::new(vec![]),
            reply: json!({}),
            status: 200,
        }),
    )
    .unwrap();
    server.handle_request(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"larm_token": "larm-mcp-admin-only-token-9911223344"}
    }));
    let reply = call(&mut server, 2, "larm.get_hardware", json!({}));
    assert_eq!(reply["result"]["isError"], json!(true));
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    // admin añadió el permiso MCP pero sin el scope API el half API niega:
    // el motivo es scope_missing (FORBIDDEN antes del caso de uso).
    assert!(text.contains("scope_missing"), "texto: {text}");
}

#[test]
fn puerta_de_confirmacion_bloquea_destructiva_sin_ejecutar() {
    struct Recorder;
    impl CoreDispatcher for Recorder {
        fn call(
            &self,
            _method: &str,
            _path: &str,
            _body: Option<&Value>,
            _bearer_token: &str,
            _idempotency_key: Option<&str>,
        ) -> Result<(u16, Value), LarmError> {
            Ok((200, json!({"ok": true})))
        }
    }
    // Principal con delete: la destructiva sin confirm=true devuelve
    // confirmation_required SIN llamar al dispatcher; con confirm=true ejecuta.
    let registry = PrincipalRegistry::from_entries(vec![RegisteredPrincipal {
        id: "0192aaaa-0000-7000-8000-000000000004".into(),
        name: "mcp-deleter".into(),
        token_sha256: digest("larm-mcp-deleter-token-5566778899aabbcc"),
        api_scopes: vec!["models:write".into(), "read".into()],
        mcp_scopes: vec!["delete".into(), "read".into()],
    }])
    .unwrap();
    let mut server = McpAdminServer::new(
        McpAdminConfig {
            base_url: "http://127.0.0.1:1".into(),
            audit_log_path: PathBuf::from("target/p104-audit-test.ndjson"),
            registry,
        },
        Arc::new(Recorder),
    )
    .unwrap();
    server.handle_request(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"larm_token": "larm-mcp-deleter-token-5566778899aabbcc"}
    }));
    let reply = call(
        &mut server,
        2,
        "larm.delete_model",
        json!({"arguments": {"id": "x"}}),
    );
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirmation_required"), "{text}");
    let reply = call(
        &mut server,
        3,
        "larm.delete_model",
        json!({"arguments": {"id": "x"}, "confirm": true}),
    );
    assert_eq!(reply["result"]["isError"], json!(false));
}

#[test]
fn bucle_stdio_respuesta_por_linea_y_json_invalido_protocol_error() {
    let mut server = initialized_server(Arc::new(FakeDispatcher {
        calls: Mutex::new(vec![]),
        reply: json!({}),
        status: 200,
    }));
    let mut input = "{bad json\n".to_string();
    input.push_str(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}).to_string());
    input.push('\n');
    let mut input = input.as_bytes();
    let mut output: Vec<u8> = Vec::new();
    server.serve_until_eof(&mut input, &mut output).unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
    assert_eq!(lines.len(), 2, "protocol error + tools/list");
    assert!(lines[0].contains("-32700"));
    assert!(lines[1].contains("\"tools\""));
}

#[test]
fn audit_log_registra_llamadas_sin_secretos() {
    let dir = tempfile::tempdir().unwrap();
    let audit = dir.path().join("audit.ndjson");
    let mut server = McpAdminServer::new(
        McpAdminConfig {
            base_url: "http://127.0.0.1:1".into(),
            audit_log_path: audit.clone(),
            registry: registry(),
        },
        Arc::new(FakeDispatcher {
            calls: Mutex::new(vec![]),
            reply: json!({"job_id": "job-9", "state": "QUEUED"}),
            status: 202,
        }),
    )
    .unwrap();
    server.handle_request(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"larm_token": TOKEN}
    }));
    call(
        &mut server,
        2,
        "larm.start_model",
        json!({"arguments": {"secret_value": "abc"}}),
    );
    let content = std::fs::read_to_string(&audit).unwrap();
    assert!(content.contains("\"tool\":\"larm.start_model\""));
    assert!(
        !content.contains("secret_value"),
        "el audit no guarda valores de argumentos"
    );
}

#[test]
fn herramienta_desconocida_y_argumentos_malformados_fallan() {
    let mut server = initialized_server(Arc::new(FakeDispatcher {
        calls: Mutex::new(vec![]),
        reply: json!({}),
        status: 200,
    }));
    let reply = call(&mut server, 1, "larm.inexistente", json!({}));
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp_tool_not_announced"));
    let reply = server
        .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call"}))
        .unwrap();
    assert_eq!(reply["error"]["code"], -32602);
}
