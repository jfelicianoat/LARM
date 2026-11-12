//! Gestor de descargas: cola, reanudación, checksums y admisión autorizada.
#![forbid(unsafe_code)]

pub mod activation_service;
mod catalog_plan;
pub mod consumer;
pub mod planner;
pub mod policy;
pub mod preview_service;
pub mod transfer;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-download";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-download");
    }
}
