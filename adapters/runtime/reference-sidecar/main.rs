//! Proceso standalone para Protocol Sidecar; configuración JSON desde stdin.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use larm_protocol::sidecar::{RunningSidecar, SidecarConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_CONFIG_BYTES: u64 = 1 << 20;

#[tokio::main]
async fn main() -> ExitCode {
    let mut input = Vec::new();
    if tokio::io::stdin()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut input)
        .await
        .is_err()
        || input.len() as u64 > MAX_CONFIG_BYTES
    {
        eprintln!("larm-protocol-sidecar: startup input rejected");
        return ExitCode::from(2);
    }
    let config: SidecarConfig = match serde_json::from_slice(&input) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("larm-protocol-sidecar: startup input rejected");
            return ExitCode::from(2);
        }
    };
    input.fill(0);
    let sidecar = match RunningSidecar::start(config).await {
        Ok(sidecar) => sidecar,
        Err(error) => {
            eprintln!(
                "larm-protocol-sidecar: startup failed ({})",
                error.details.reason
            );
            return ExitCode::from(2);
        }
    };
    let ready = serde_json::json!({
        "status": "ready",
        "endpoint": sidecar.endpoint(),
        "profile_hash": sidecar.profile_hash(),
        "protocol": "CUSTOM_REST"
    });
    let mut output = match serde_json::to_vec(&ready) {
        Ok(output) => output,
        Err(_) => return ExitCode::from(2),
    };
    output.push(b'\n');
    if tokio::io::stdout().write_all(&output).await.is_err()
        || tokio::io::stdout().flush().await.is_err()
    {
        return ExitCode::from(2);
    }
    output.fill(0);
    if tokio::signal::ctrl_c().await.is_err() {
        return ExitCode::from(1);
    }
    sidecar.shutdown().await;
    ExitCode::SUCCESS
}
