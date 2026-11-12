//! Dominio de modelos: bundles, variantes, fuentes federadas y watchers.
#![forbid(unsafe_code)]

pub mod acquisition;
pub mod bundle;
pub mod identity;
pub mod import;
pub mod import_worker;
pub mod lifecycle;
pub mod rebuild;
pub mod registry;
pub mod scan;
pub mod search;
pub mod verification;
pub mod watcher;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-models";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-models");
    }
}
