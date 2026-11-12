//! SDK para las cinco familias de plugins (runtime/source/protocol/transformation/benchmark).
#![forbid(unsafe_code)]

pub mod capabilities;
pub mod contracts;
pub mod declarative;
pub mod generic;
pub mod manifest;
pub mod permissions;
pub mod protocol;
pub mod server;
pub mod source;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-adapter-sdk";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-adapter-sdk");
    }
}
