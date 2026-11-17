//! Dominio larm-db: pool con pragmas del contrato, migraciones idempotentes,
//! backup/restore verificado y helper transaccional (P004).
#![forbid(unsafe_code)]

pub mod acquisition;
pub mod adapter_imports;
pub mod adapter_probes;
pub mod adapter_registry;
pub mod adapter_selection;
pub mod adapter_updates;
pub mod adapters;
pub mod artifacts;
pub mod backup;
pub mod catalog_acquisition;
pub mod compatibility;
pub mod conformance;
pub mod conformance_profiles;
pub mod custom_rules;
pub mod download_admissions;
mod download_bundle;
pub mod download_jobs;
pub mod download_plans;
pub mod download_progress;
pub mod dto;
pub mod events;
pub mod execution_hosts;
pub mod guard;
pub mod hardware;
pub mod inference_endpoints;
pub mod instances;
pub mod jobs;
pub mod launch_profiles;
pub mod locations;
pub mod migrate;
pub mod model_import;
pub mod model_verification;
pub mod models;
pub mod notifications;
pub mod pool;
pub mod reservations;
pub mod runtime_instance_stops;
pub mod runtime_leases;
pub mod runtime_profiles;
pub mod runtimes;
pub mod security;
pub mod settings;
pub mod source_accounts;
pub mod source_probes;
pub mod source_watches;
pub mod sources;
pub mod storage;
pub mod testing;
pub mod transformation_inputs;
pub mod transformation_plans;
pub mod transformations;
pub mod tx;
pub mod variants;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-db";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-db");
    }
}
