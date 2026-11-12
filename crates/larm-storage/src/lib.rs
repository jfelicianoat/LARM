//! Almacenamiento: volúmenes, ubicaciones, reservas, store content-addressed.
#![forbid(unsafe_code)]

pub mod analyzer;
pub mod artifacts;
pub mod cas;
pub mod cleanup;
pub mod estimate;
pub mod import;
pub mod locations;
pub mod move_job;
pub mod rebuild;
pub mod reconcile;
pub mod runtime_install;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-storage";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-storage");
    }
}
