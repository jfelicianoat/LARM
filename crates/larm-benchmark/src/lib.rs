//! Benchmarks y evaluaciones: suites, fingerprints y comparaciones.
#![forbid(unsafe_code)]

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-benchmark";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-benchmark");
    }
}
