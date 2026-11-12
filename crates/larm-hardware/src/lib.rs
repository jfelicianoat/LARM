//! Profilers de hardware: CPU, memoria, GPU, NPU y fingerprint reproducible.
// `unsafe` prohibido por el workspace; única excepción: `native` (ADR-0010).

pub mod accel;
pub mod cim;
pub mod cpu;
pub mod devices;
pub mod libs;
pub mod memory;
#[cfg(windows)]
pub mod native;
pub mod native_report;
pub mod os;
pub mod process;
pub mod profiler;
pub mod storage;
pub mod tools;

pub use profiler::WindowsProfiler;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-hardware";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-hardware");
    }
}
