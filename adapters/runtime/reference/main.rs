//! Composición de la referencia runtime: launcher real + contrato echo explícito.
#![forbid(unsafe_code)]
use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::sync::Arc;

use larm_adapter_sdk::capabilities::{CapabilityRegistry, Estimator};
use larm_adapter_sdk::generic::GenericRuntime;
use larm_adapter_sdk::server::serve;
use serde_json::{json, Value};
use tokio::io::BufReader;

fn registry(package: &std::path::Path) -> Result<CapabilityRegistry, Box<dyn std::error::Error>> {
    let value: Value =
        serde_json::from_slice(&std::fs::read(package.join("reference-capabilities.json"))?)?;
    let estimator: Estimator = Arc::new(|input| {
        let size = larm_domain::canonical::canonicalize(input)
            .map_err(|_| {
                larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::ValidationFailed,
                    "Input de referencia inválido",
                    "reference_echo_input_invalid",
                )
            })?
            .len();
        Ok(
            json!({"weights_bytes":null,"kv_bytes":null,"activation_bytes":null,"workspace_bytes":(size+32768).to_string(),"overhead_bytes":null,"total_bytes":null,"multimodal_bytes":null,"temporary_bytes":null,"safety_margin_bytes":null,"per_pool":[],"confidence":"UNKNOWN","evidence_refs":["reference:echo-buffers-process-memory-unknown"]}),
        )
    });
    Ok(CapabilityRegistry::new(
        value,
        BTreeMap::from([("reference.echo.buffers.v1".into(), estimator)]),
        &BTreeSet::from(["reference.http.echo.v1".into()]),
    )?)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let package = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent()?.parent().map(std::path::Path::to_path_buf));
    let Some(package) = package else {
        return ExitCode::from(2);
    };
    let result = GenericRuntime::load(&package)
        .map_err(|e| e.to_string())
        .and_then(|handler| {
            registry(&package)
                .map(|registry| handler.with_capability_registry(registry))
                .map_err(|_| "contrato de referencia inválido".into())
        });
    let handler = match result {
        Ok(handler) => Arc::new(handler),
        Err(_) => {
            eprintln!("reference-runtime: paquete o capacidades inválidos");
            return ExitCode::from(2);
        }
    };
    let served = serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        handler.clone(),
    )
    .await;
    handler.shutdown().await;
    if served.is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("reference-runtime: sesión no completada");
        ExitCode::from(1)
    }
}
