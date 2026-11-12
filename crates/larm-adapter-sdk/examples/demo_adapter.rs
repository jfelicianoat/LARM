//! Adaptador de ejemplo de la familia `benchmark` (P020) sobre el SDK: sirve
//! para probar el protocolo v1 con un proceso real.
//!
//! ```text
//! demo_adapter --manifest <fichero> [--protocol-version 1.0]
//! ```
//!
//! - `benchmark.describe`: describe una suite fija.
//! - `benchmark.run`: DTO Benchmark; suite_id `fixture.transport:N`; emite
//!   `core.progress` en cada iteración y atiende `core.cancel`.
//! - No cualifica inferencia: salida BLOCKED_REAL con datos de transporte.
//!
//! `--protocol-version` permite anunciar otra versión en el handshake para
//! probar el rechazo del host.
#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::RpcError;
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod benchmark_fixture;

struct Demo {
    manifest: AdapterManifest,
    protocol: String,
    registry: Option<larm_adapter_sdk::capabilities::CapabilityRegistry>,
}

#[async_trait]
impl AdapterHandler for Demo {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn protocol_version(&self) -> &str {
        &self.protocol
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.registry.as_ref()
    }

    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        match method {
            "benchmark.describe" => Ok(benchmark_fixture::describe(vec![])),
            "benchmark.run" => {
                let iterations = benchmark_fixture::amount(&params);
                let delay = 20;
                for i in 0..iterations {
                    if ctx.is_cancelled() {
                        return Err(RpcError::domain(
                            LarmError::new(
                                ErrorCode::Conflict,
                                "Ejecución cancelada",
                                "request_cancelled",
                            )
                            .retryable(true),
                        ));
                    }
                    ctx.progress("RUNNING", Some(i), Some(iterations), Some("iteration"));
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Ok(benchmark_fixture::output(
                    &params,
                    json!({"iterations": iterations}),
                ))
            }
            "benchmark.validate" => Ok(
                json!({"valid":false,"errors":["transport_fixture_not_an_inference_qualification"]}),
            ),
            _ => Err(RpcError::new(-32601, "no implementado")),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let Some(path) = arg("--manifest") else {
        eprintln!("uso: demo_adapter --manifest <fichero> [--protocol-version X]");
        std::process::exit(2);
    };
    let manifest = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| AdapterManifest::parse(&t).map_err(|e| e.details.reason))
    {
        Ok(m) => m,
        Err(e) => {
            // Los logs van a stderr: stdout es solo del protocolo.
            eprintln!("manifiesto inválido: {e}");
            std::process::exit(2);
        }
    };
    let registry = benchmark_fixture::registry(&manifest.capabilities).unwrap_or_else(|_| {
        eprintln!("contrato de fixture inválido");
        std::process::exit(2)
    });
    let handler = Arc::new(Demo {
        registry,
        manifest,
        protocol: arg("--protocol-version").unwrap_or_else(|| "1.0".into()),
    });
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    if let Err(e) = serve(stdin, tokio::io::stdout(), handler).await {
        eprintln!("fin de la sesión: {e}");
        std::process::exit(1);
    }
}
