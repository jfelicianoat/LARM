//! Servidor MCP de administración por stdio: transporte JSON-RPC 2.0 NDJSON
//! (`initialize`/`tools/list`/`tools/call`), autorización H06 (permiso MCP
//! **y** scope API antes del caso de uso; admin nunca implícito), puerta de
//! confirmación para operaciones destructivas, salidas compactas con job IDs
//! y audit log de tool calls sin secretos. Las herramientas se despachan a
//! los casos de uso Core por la API HTTP del daemon con la credencial del
//! propio principal — nunca acceso directo a base de datos ni filesystem.
use super::permissions::ToolCatalog;
use larm_adapter_sdk::manifest::MethodTable;
use larm_domain::auth::{authorize, ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Descriptor operador-registrado de un principal MCP (forma sellada
/// `McpPrincipal` del schema): el servidor resuelve la credencial presentada
/// por su hash y aplica ambos halves de la autorización localmente; el daemon
/// los repite ante cualquier uso directo de la API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPrincipal {
    pub id: String,
    pub name: String,
    pub token_sha256: String,
    pub api_scopes: Vec<String>,
    pub mcp_scopes: Vec<String>,
}

/// Registro operador-registrado de principales MCP.
#[derive(Debug, Clone, Default)]
pub struct PrincipalRegistry {
    by_token: BTreeMap<String, RegisteredPrincipal>,
}

impl PrincipalRegistry {
    pub fn from_entries(entries: Vec<RegisteredPrincipal>) -> Result<Self, LarmError> {
        let mut by_token = BTreeMap::new();
        for principal in entries {
            if principal.token_sha256.len() != 64
                || !principal
                    .token_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(LarmError::new(
                    ErrorCode::ValidationFailed,
                    "Registro de principal MCP no válido",
                    "mcp_principal_token_hash_invalid",
                ));
            }
            by_token.insert(principal.token_sha256.clone(), principal);
        }
        Ok(Self { by_token })
    }

    #[must_use]
    pub fn resolve(&self, bearer_token: &str) -> Option<&RegisteredPrincipal> {
        let digest = hex::encode(Sha256::digest(bearer_token.as_bytes()));
        self.by_token.get(&digest)
    }
}

/// Despachador hacia los casos de uso Core por la API HTTP del daemon.
pub trait CoreDispatcher: Send + Sync {
    /// Ejecuta la operación con la credencial del principal MCP.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        bearer_token: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(u16, Value), LarmError>;
}

/// Configuración operador-registrada del servidor.
#[derive(Debug, Clone)]
pub struct McpAdminConfig {
    pub base_url: String,
    pub audit_log_path: PathBuf,
    pub registry: PrincipalRegistry,
}

/// Servidor MCP de administración. El bucle stdio vive en [`serve_until_eof`];
/// [`handle_request`] es puro respecto al transporte y es lo que se prueba.
pub struct McpAdminServer {
    pub catalog: ToolCatalog,
    pub config: McpAdminConfig,
    pub dispatcher: Arc<dyn CoreDispatcher>,
    session_token: Option<String>,
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

impl McpAdminServer {
    pub fn new(
        config: McpAdminConfig,
        dispatcher: Arc<dyn CoreDispatcher>,
    ) -> Result<Self, LarmError> {
        Ok(Self {
            catalog: ToolCatalog::shared()?,
            config,
            dispatcher,
            session_token: None,
        })
    }

    /// Procesa un mensaje JSON-RPC y devuelve la respuesta (`None` para
    /// notificaciones sin id). `[&mut self]` porque initialize fija sesión.
    pub fn handle_request(&mut self, request: &Value) -> Option<Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => Some(self.initialize(id, request.get("params"))),
            "notifications/initialized" => None,
            "tools/list" => Some(self.tools_list(id)),
            "tools/call" => Some(self.tools_call(id, request.get("params"))),
            other => Some(rpc_error(
                id,
                -32601,
                &format!("método MCP no soportado: {other}"),
            )),
        }
    }

    fn initialize(&mut self, id: Value, params: Option<&Value>) -> Value {
        let token = params
            .and_then(|params| params.get("larm_token"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if self.config.registry.resolve(&token).is_none() {
            return rpc_error(id, -32001, "credencial MCP desconocida para larm-mcp-admin");
        }
        self.session_token = Some(token);
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "larm-mcp-admin", "version": "1.0.0"},
                "capabilities": {"tools": {}}
            }
        })
    }

    fn tools_list(&self, id: Value) -> Value {
        let mut table = MethodTable::contract();
        let _ = &mut table;
        let tools: Vec<Value> = self
            .catalog
            .tools()
            .iter()
            .map(|spec| {
                json!({
                    "name": spec.tool,
                    "description": format!(
                        "{} {} (permiso MCP: {}{})",
                        spec.method, spec.path, spec.permission.as_str(),
                        if spec.destructive { "; requiere confirmación" } else { "" }
                    ),
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "arguments": {"type": "object"},
                            "confirm": {"type": "boolean"}
                        },
                        "additionalProperties": false
                    }
                })
            })
            .collect();
        json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}})
    }

    fn tools_call(&mut self, id: Value, params: Option<&Value>) -> Value {
        let Some(params) = params else {
            return rpc_error(id, -32602, "tools/call exige name y arguments");
        };
        let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(bearer) = self.session_token.clone() else {
            return rpc_error(id, -32002, "sesión no inicializada con credencial MCP");
        };
        let Some(registered) = self.config.registry.resolve(&bearer) else {
            return rpc_error(id, -32001, "credencial MCP desconocida");
        };
        let spec = match self.catalog.find(tool_name) {
            Ok(spec) => spec,
            Err(error) => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"content": [{"type": "text", "text": format!("isError: {}", error.details.reason)}], "isError": true}
                })
            }
        };
        let spec = spec.clone();
        let outcome = self.authorize_and_dispatch(registered, &spec, &arguments);
        let text = match &outcome {
            Ok(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".into()),
            Err(error) => format!("isError: {}", error.details.reason),
        };
        self.audit(tool_name, &registered.id, &outcome);
        let structured = outcome.ok();
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": text}],
                "isError": structured.is_none(),
                "structuredContent": structured.unwrap_or(Value::Null),
            }
        })
    }

    /// Autorización H06 (permiso MCP **y** scope API, FORBIDDEN antes del
    /// caso de uso; admin nunca añade scopes) + puerta de confirmación +
    /// despacho con la credencial del principal.
    fn authorize_and_dispatch(
        &self,
        registered: &RegisteredPrincipal,
        spec: &super::permissions::ToolSpec,
        arguments: &Value,
    ) -> Result<Value, LarmError> {
        // Half 1: permiso MCP (admin actúa como comodín de permisos MCP).
        let mcp_ok = registered.mcp_scopes.iter().any(|scope| {
            scope == spec.permission.as_str() || scope == McpPermission::Admin.as_str()
        });
        if !mcp_ok {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El principal MCP no tiene el permiso de esta herramienta",
                "mcp_permission_missing",
            ));
        }
        // Half 2: scope API — con el principal sellado del dominio y la
        // operación contractual exacta (admin nunca añade scopes API).
        let mut api_scopes = std::collections::BTreeSet::new();
        for scope in &registered.api_scopes {
            api_scopes.insert(ApiScope::parse(scope).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "Registro de principal MCP no válido",
                    "mcp_principal_api_scope_invalid",
                )
            })?);
        }
        let mut mcp_scopes = std::collections::BTreeSet::new();
        for scope in &registered.mcp_scopes {
            mcp_scopes.insert(McpPermission::parse(scope).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "Registro de principal MCP no válido",
                    "mcp_principal_mcp_scope_invalid",
                )
            })?);
        }
        let principal = Principal::new(
            registered.id.clone(),
            registered.name.clone(),
            PrincipalKind::Mcp,
            api_scopes,
            mcp_scopes,
        )?;
        let operation = self.catalog.operation(spec)?;
        authorize(&principal, operation)?;
        // Puerta de confirmación: las destructivas no se ejecutan sin
        // aprobación explícita del operador en los argumentos.
        if spec.destructive && arguments.get("confirm") != Some(&json!(true)) {
            return Ok(json!({
                "status": "confirmation_required",
                "tool": spec.tool,
                "detail": "operación destructiva: repita la llamada con confirm=true tras revisarla",
            }));
        }
        // Despacho al caso de uso Core con la credencial del principal.
        let body = arguments.get("arguments").filter(|value| value.is_object());
        let idempotency = arguments.get("idempotency_key").and_then(Value::as_str);
        let (status, payload) = self.dispatcher.call(
            &spec.method,
            &spec.path,
            body,
            &self.session_token.clone().unwrap_or_default(),
            idempotency,
        )?;
        if !(200..300).contains(&status) {
            return Err(LarmError::new(
                ErrorCode::UpstreamUnavailable,
                "El caso de uso Core rechazó la operación",
                "core_use_case_rejected",
            )
            .with_resource(&spec.api_operation_id));
        }
        Ok(compact_output(&payload))
    }

    fn audit(&self, tool: &str, principal_id: &str, outcome: &Result<Value, LarmError>) {
        let entry = json!({
            "at": larm_domain::bridge::timestamp(chrono::Utc::now()),
            "tool": tool,
            "principal_id": principal_id,
            "outcome": match outcome {
                Ok(_) => "ok",
                Err(error) => error.details.reason.as_str(),
            },
            // Sin secretos ni argumentos: sólo el resultado compacto cuando
            // es ok (job ids) y el motivo estable cuando falla.
            "result": outcome.as_ref().ok().map(|value| value.to_string()).unwrap_or_default(),
        });
        use std::io::Write;
        if let Some(parent) = self.config.audit_log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.audit_log_path)
        {
            let _ = writeln!(log, "{entry}");
        }
    }

    /// Bucle stdio NDJSON hasta EOF. Cada línea es un mensaje JSON-RPC.
    ///
    /// # Errors
    /// [`LarmError`] si una línea no cabe en el límite o falla la IO.
    pub fn serve_until_eof(
        &mut self,
        input: &mut dyn std::io::BufRead,
        output: &mut dyn std::io::Write,
    ) -> Result<(), LarmError> {
        loop {
            let mut line = String::new();
            let read = input.read_line(&mut line).map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "Fallo leyendo el transporte MCP",
                    "mcp_stdio_read_failed",
                )
                .with_technical(error.to_string())
            })?;
            if read == 0 {
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let request: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => {
                    let _ = writeln!(
                        output,
                        "{}",
                        rpc_error(Value::Null, -32700, "JSON inválido")
                    );
                    continue;
                }
            };
            if let Some(response) = self.handle_request(&request) {
                let _ = writeln!(output, "{response}");
            }
        }
    }
}

/// Salida compacta: job IDs para operaciones largas, estado para receipts.
fn compact_output(payload: &Value) -> Value {
    if let Some(job_id) = payload.get("job_id").and_then(Value::as_str) {
        let state = payload.get("state").cloned().unwrap_or(Value::Null);
        return json!({"job_id": job_id, "state": state, "status_url": payload.get("status_url")});
    }
    if let Some(plan_id) = payload.get("plan_id").and_then(Value::as_str) {
        return json!({
            "plan_id": plan_id,
            "plan_hash": payload.get("plan_hash"),
            "hint": "confirme el plan con la herramienta correspondiente",
        });
    }
    payload.clone()
}
