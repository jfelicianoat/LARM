//! Dominio puro de LARM: entidades, estados e invariantes (contrato 01).
#![forbid(unsafe_code)]

pub mod acquisition;
pub mod adapter_packages;
pub mod adapter_probes;
pub mod adapter_registry;
pub mod adapter_updates;
pub mod adapters;
pub mod approval;
pub mod artifacts;
pub mod auth;
pub mod benchmark_comparison;
pub mod bridge;
pub mod canonical;
pub mod compatibility;
pub mod conformance;
pub mod conformance_profiles;
pub mod correlation;
pub mod credential;
pub mod custom_rules;
pub mod download;
pub mod download_admission;
pub mod error;
pub mod estimate;
pub mod evaluation;
pub mod guard;
pub mod hardware;
pub mod instances;
pub mod launch;
pub mod launch_profile;
pub mod locations;
pub mod model;
pub mod model_import;
pub mod model_verification;
pub mod ports;
pub mod protocol;
pub mod redact;
pub mod runtime;
pub mod runtime_execution;
pub mod runtime_install;
pub mod runtime_ports;
pub mod runtime_profiles;
pub mod search;
pub mod source_accounts;
pub mod source_credentials;
pub mod source_probes;
pub mod sources;
pub mod storage;
pub mod transformation_execution;
pub mod transformation_output;
pub mod transformation_publication;
pub mod transformations;
pub mod units;
pub mod watchers;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-domain";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-domain");
    }
}
