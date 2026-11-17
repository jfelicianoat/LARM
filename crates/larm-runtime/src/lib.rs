//! Núcleo de runtimes: versiones, instalaciones, instancias, scheduler y leases.
#![forbid(unsafe_code)]

pub mod catalog;
pub mod credentials_pipe;
pub mod discovery_service;
pub mod endpoints;
pub mod execution_host;
pub mod external_adoption;
pub mod install;
pub mod install_service;
pub mod instances;
pub mod lease_recovery;
pub mod memory;
pub mod nuget_lock;
pub mod ports;
pub mod profiles;
pub mod python_env;
pub mod scheduler;
pub mod stop_service;
pub mod supervisor;
pub mod version_discovery;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-runtime";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-runtime");
    }
}
