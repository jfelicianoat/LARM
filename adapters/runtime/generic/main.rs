//! `larm-generic-adapter`: worker de los adaptadores declarativos (P023).
//!
//! Vive en `<paquete>/bin/` y lee `declarative.json` y `manifest.json` de la
//! carpeta del paquete (la padre de `bin`). No recibe argumentos: el host lo
//! lanza con el manifiesto y habla con él por stdio (protocolo v1).
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use larm_adapter_sdk::generic::GenericRuntime;
use larm_adapter_sdk::server::serve;
use tokio::io::BufReader;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    let package = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(std::path::Path::to_path_buf));
    let Some(package) = package else {
        eprintln!("larm-generic-adapter: no se pudo localizar el paquete");
        return ExitCode::from(2);
    };
    let handler = match GenericRuntime::load(&package) {
        Ok(h) => Arc::new(h),
        Err(e) => {
            eprintln!(
                "larm-generic-adapter: paquete inválido: {} ({})",
                e.details.reason,
                e.technical_detail()
            );
            return ExitCode::from(2);
        }
    };
    let served = serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        handler.clone(),
    )
    .await;
    // Fin de sesión: ningún runtime sobrevive a su worker.
    handler.shutdown().await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("larm-generic-adapter: E/S: {e}");
            ExitCode::from(1)
        }
    }
}
