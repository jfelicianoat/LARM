//! Facade pública del dominio de transformación (P093).
#![forbid(unsafe_code)]

pub use larm_domain::transformations::*;

pub mod environment;
pub mod input;
pub mod output;
pub mod output_validation;
pub mod publication;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-transform";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-transform");
    }
}
