//! Protocolo con AI Broker: LocalRuntimeResolver, sidecars y MCP.
#![forbid(unsafe_code)]

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-protocol";

pub mod mcp_admin;
pub mod sidecar;

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-protocol");
    }
}
