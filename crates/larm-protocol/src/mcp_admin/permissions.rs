//! Catálogo de herramientas MCP desde `docs/contracts/mcp-permissions.json`.
//!
//! El contrato sella la regla de autorización: `MCP_PERMISSION_AND_API_SCOPE`
//! — el principal necesita el permiso MCP de la herramienta **y** el scope de
//! la operación API mapeada; `admin` es un comodín explícito de permisos MCP
//! que nunca añade scopes API. Cada herramienta mapea a una operación API
//! existente del catálogo (`api_operation_id`), nunca a acceso directo a
//! base de datos o filesystem.
use larm_adapter_sdk::manifest::MethodTable;
use larm_domain::auth::{Catalog, McpPermission, Operation};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::Value;

/// mcp-permissions.json del contrato, embebido para no depender de disco.
pub const PERMISSIONS_JSON: &str = include_str!("../../../../docs/contracts/mcp-permissions.json");

/// Herramienta MCP mapeada a su operación API contractual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub tool: String,
    pub permission: McpPermission,
    pub api_operation_id: String,
    pub api_scope: String,
    pub method: String,
    pub path: String,
    /// Operaciones que mutan o interrumpen trabajo: exigen confirmación
    /// explícita del operador antes de ejecutarse (`confirmation_required`).
    pub destructive: bool,
}

/// Herramientas que mutan estado o interrumpen trabajo en curso.
const DESTRUCTIVE_TOOLS: [&str; 4] = [
    "larm.move_model",
    "larm.delete_model",
    "larm.update_runtime",
    "larm.stop_model",
];

fn parse_permission(value: &Value) -> Option<McpPermission> {
    McpPermission::parse(value.as_str()?)
}

/// Catálogo inmutable de herramientas MCP con su tabla de métodos validada.
pub struct ToolCatalog {
    tools: Vec<ToolSpec>,
    catalog: &'static Catalog,
}

impl ToolCatalog {
    /// Carga y valida el contrato embebido: 12 herramientas, permisos del
    /// enumerado sellado y operaciones existentes en el catálogo de API.
    ///
    /// # Errors
    /// [`LarmError`] si el documento no corresponde al contrato sellado.
    pub fn shared() -> Result<Self, LarmError> {
        let document: Value = serde_json::from_str(PERMISSIONS_JSON)
            .map_err(|_| invalid("mcp_permissions_invalid"))?;
        let entries = document
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("mcp_permissions_invalid"))?;
        let catalog = Catalog::contract();
        let mut tools: Vec<ToolSpec> = Vec::new();
        for entry in entries {
            let tool = entry
                .get("tool")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?
                .to_string();
            let permission = entry
                .get("permission")
                .and_then(parse_permission)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?;
            let method = entry
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?
                .to_string();
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?
                .to_string();
            let api_operation_id = entry
                .get("api_operation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?
                .to_string();
            let api_scope = entry
                .get("api_scope")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mcp_permissions_invalid"))?
                .to_string();
            // El mapeo debe apuntar a una operación publicada del contrato.
            catalog
                .find(&method, &path)
                .ok_or_else(|| invalid("mcp_permissions_operation_missing"))?;
            if DESTRUCTIVE_TOOLS.contains(&tool.as_str()) {
                tools.push(ToolSpec {
                    tool,
                    permission,
                    api_operation_id,
                    api_scope,
                    method,
                    path,
                    destructive: true,
                });
            } else {
                tools.push(ToolSpec {
                    tool,
                    permission,
                    api_operation_id,
                    api_scope,
                    method,
                    path,
                    destructive: false,
                });
            }
        }
        if tools.len() != 12 {
            return Err(invalid("mcp_permissions_incomplete"));
        }
        Ok(Self { tools, catalog })
    }

    #[must_use]
    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    pub fn find(&self, tool: &str) -> Result<&ToolSpec, LarmError> {
        self.tools
            .iter()
            .find(|spec| spec.tool == tool)
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::UnsupportedCapability,
                    "Herramienta MCP no anunciada",
                    "mcp_tool_not_announced",
                )
            })
    }

    /// Operación API mapeada por la herramienta (método/ruta → contrato).
    pub fn operation(&self, spec: &ToolSpec) -> Result<&Operation, LarmError> {
        self.catalog
            .find(&spec.method, &spec.path)
            .ok_or_else(|| invalid("mcp_permissions_operation_missing"))
    }

    /// Tabla de métodos contractual (para validar manifiestos de adaptador).
    pub fn method_table() -> MethodTable {
        MethodTable::contract()
    }
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "El contrato de permisos MCP no corresponde al sellado",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn catalogo_con_las_doce_herramientas_y_operaciones_publicadas() {
        let catalog = ToolCatalog::shared().unwrap();
        assert_eq!(catalog.tools().len(), 12);
        for spec in catalog.tools() {
            catalog.operation(spec).unwrap();
        }
        let names: Vec<&str> = catalog.tools().iter().map(|s| s.tool.as_str()).collect();
        assert!(names.contains(&"larm.search_models"));
        assert!(names.contains(&"larm.delete_model"));
        assert!(names.contains(&"larm.get_job"));
    }

    #[test]
    fn destructive_son_move_delete_update_stop() {
        let catalog = ToolCatalog::shared().unwrap();
        for name in DESTRUCTIVE_TOOLS {
            let spec = catalog.find(name).unwrap();
            assert!(spec.destructive, "{name}");
        }
        let read = catalog.find("larm.get_hardware").unwrap();
        assert!(!read.destructive);
    }

    #[test]
    fn permisos_separados_start_stop_install_update() {
        let catalog = ToolCatalog::shared().unwrap();
        assert_eq!(
            catalog.find("larm.start_model").unwrap().permission,
            McpPermission::Start
        );
        assert_eq!(
            catalog.find("larm.stop_model").unwrap().permission,
            McpPermission::Stop
        );
        assert_eq!(
            catalog.find("larm.install_runtime").unwrap().permission,
            McpPermission::Install
        );
        assert_eq!(
            catalog.find("larm.update_runtime").unwrap().permission,
            McpPermission::Update
        );
        assert_eq!(
            catalog.find("larm.move_model").unwrap().permission,
            McpPermission::Move
        );
        assert_eq!(
            catalog.find("larm.delete_model").unwrap().permission,
            McpPermission::Delete
        );
    }
}
