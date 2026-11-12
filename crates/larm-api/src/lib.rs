//! API administrativa Axum v1: OpenAPI, auth, SSE y operación de endpoints.
#![forbid(unsafe_code)]

pub mod adapters;
pub mod app;
pub mod artifacts;
pub mod broker;
pub mod compatibility;
pub mod custom_rules;
pub mod download_plans;
pub mod downloads;
pub mod hardware;
mod http;
pub mod import_preview;
pub mod instances;
pub mod jobs;
pub mod launch_profiles;
pub mod maintenance;
pub mod model_import;
pub mod models;
pub mod notifications;
pub mod openapi;
pub mod runtime_profiles;
pub mod runtimes;
pub mod search;
pub mod security;
pub mod settings;
pub mod source_accounts;
pub mod source_credentials;
pub mod source_probes;
pub mod source_watches;
pub mod sources;
pub mod storage;
pub mod transformations;

/// Identificador estable del crate para inventario y trazabilidad.
pub const CRATE_NAME: &str = "larm-api";

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_reports_name() {
        assert_eq!(crate::CRATE_NAME, "larm-api");
    }
}
