//! P104: servidor MCP de administración con scopes separados (H06), puerta de
//! confirmación para destructivas, salidas compactas con job IDs y audit log
//! sin secretos. Ver `permissions.rs` (catálogo sellado) y `server.rs`
//! (transporte stdio JSON-RPC + autorización + despacho a casos de uso Core).

pub mod permissions;
pub mod server;
#[cfg(test)]
mod tests;

pub use permissions::{ToolCatalog, ToolSpec};
pub use server::{
    CoreDispatcher, McpAdminConfig, McpAdminServer, PrincipalRegistry, RegisteredPrincipal,
};
