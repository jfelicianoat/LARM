//! Casos de uso de aplicación; puertos en larm-domain, composición en el daemon.
#![forbid(unsafe_code)]

pub mod adapters;
pub mod bridge;
pub mod compatibility;
pub mod config;
pub mod conformance;
pub mod hardware;
pub mod locations;
pub mod logging;
pub mod recommendation;
pub mod redact;
pub mod search;
pub mod security;
pub mod source_accounts;
pub mod source_credentials;
pub mod sources;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-core";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-core");
    }
}
