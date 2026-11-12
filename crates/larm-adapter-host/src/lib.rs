//! Host de adaptadores: workers, timeouts, crash y ciclo de vida.
#![forbid(unsafe_code)]

pub mod conformance;
pub mod daemon_owner;
mod framing;
#[cfg(windows)]
pub mod guardian;
pub mod import_service;
pub mod imports;
pub mod input_files;
pub mod package;
mod package_paths;
pub mod policy;
pub mod registry;
pub mod runtime;
pub mod source;
pub mod source_probe_service;
pub mod transformation;
pub mod update_service;
pub mod updates;
pub mod worker;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-adapter-host";

/// Época del último arranque del sistema (segundos). Cambia en cada reinicio
/// del sistema y lo prueba a nivel de sistema operativo: ningún proceso ni
/// handle de una sesión anterior sobrevive. Cero si el sistema no lo informa.
#[must_use]
pub fn system_boot_session() -> u64 {
    sysinfo::System::boot_time()
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-adapter-host");
    }
}
