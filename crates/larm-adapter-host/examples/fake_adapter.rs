//! Adaptador falso para probar el host (P021). Usa el SDK para la sesión
//! normal y un modo de fallo elegido por argumentos del manifiesto:
//!
//! ```text
//! fake_adapter --manifest <fichero> --mode <modo>
//! ```
//!
//! | Modo | Conducta |
//! |---|---|
//! | `ok` | Normal (`benchmark.run` con `{"ms": n}` espera n ms) |
//! | `silent` | Nunca responde al handshake |
//! | `bad-nonce` | Responde al handshake con otro nonce |
//! | `crash` | Sale con código 3 al recibir `benchmark.run` |
//! | `garbage` | Escribe líneas que no son JSON en stdout al recibir `benchmark.run` |
//! | `stderr-flood` | Escribe 12 MiB en stderr al arrancar y sigue normal |
//! | `deaf` | `benchmark.run` nunca responde e ignora `core.cancel` |
//! | `deaf-tree` | Como `deaf`, pero antes lanza un proceso nieto (`--sleeper`) y escribe su pid en stderr |
//! | `deaf-tree-no-stdio` | Nieto sin stdin/stdout/stderr heredados; el cierre no puede depender de EOF |
//! | `deaf-tree-many` | Cuatro nietos con canales heredados; todos deben terminar |
//! | `no-heartbeat` | Deja de responder a `core.heartbeat` |
//! | `env` | `benchmark.describe` devuelve las variables de entorno que ve |
#![forbid(unsafe_code)]

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::{decode, encode, Frame, Response, RpcError};
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod benchmark_fixture;

struct Fake {
    manifest: AdapterManifest,
    mode: String,
    registry: Option<larm_adapter_sdk::capabilities::CapabilityRegistry>,
}

#[async_trait]
impl AdapterHandler for Fake {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.registry.as_ref()
    }

    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        match (method, self.mode.as_str()) {
            ("benchmark.describe", "env") => {
                let vars: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
                Ok(benchmark_fixture::describe(vars))
            }
            ("benchmark.run", "crash") => std::process::exit(3),
            ("benchmark.run", "tail-after-exit") => {
                let mut command =
                    std::process::Command::new(std::env::current_exe().unwrap_or_default());
                command.arg("--stdio-tail");
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x0800_0000);
                }
                let _child = command.spawn();
                std::process::exit(3);
            }
            ("benchmark.run", "garbage") => {
                let mut out = std::io::stdout();
                for _ in 0..5 {
                    let _ = writeln!(out, "esto no es un frame");
                    let _ = out.flush();
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Ok(json!({"ok": true}))
            }
            ("benchmark.run", "deaf-tree" | "deaf-tree-no-stdio" | "deaf-tree-many") => {
                // Lanza un nieto y se queda sordo: matar al worker debe matar el árbol.
                let count = if self.mode == "deaf-tree-many" { 4 } else { 1 };
                for _ in 0..count {
                    let mut command =
                        std::process::Command::new(std::env::current_exe().unwrap_or_default());
                    command.arg("--sleeper");
                    if self.mode == "deaf-tree-no-stdio" {
                        command
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());
                    }
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        command.creation_flags(0x0800_0000);
                    }
                    let child = command.spawn();
                    if let Ok(c) = child {
                        eprintln!("child_pid={}", c.id());
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                Ok(json!({}))
            }
            ("benchmark.run", "deaf") => {
                // Ignora la cancelación: el host tendrá que matarlo.
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                Ok(json!({}))
            }
            ("benchmark.run", _) => {
                let ms = benchmark_fixture::amount(&params);
                let mut waited = 0;
                while waited < ms {
                    if ctx.is_cancelled() {
                        return Err(RpcError::domain(larm_domain::error::LarmError::new(
                            larm_domain::error::ErrorCode::Conflict,
                            "cancelada",
                            "request_cancelled",
                        )));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    waited += 10;
                }
                Ok(benchmark_fixture::output(&params, json!({"ms": ms})))
            }
            ("benchmark.describe", _) => Ok(benchmark_fixture::describe(vec![])),
            _ => Err(RpcError::new(-32601, "no implementado")),
        }
    }
}

/// Sesión a mano para los modos que rompen el protocolo en el handshake o
/// en el heartbeat.
async fn manual(manifest: AdapterManifest, mode: &str) {
    let mut lines = BufReader::new(tokio::io::stdin()).split(b'\n');
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_segment().await {
        let Ok(Frame::Request(r)) = decode(&line) else {
            continue;
        };
        let reply = match (r.method.as_str(), mode) {
            (_, "silent") => continue,
            ("core.handshake", "bad-nonce") => {
                json!({"protocol_version": "1.0", "adapter_version": manifest.version,
                "methods": manifest.methods, "capabilities": manifest.capabilities, "nonce": "otro"})
            }
            ("core.handshake", _) => {
                json!({"protocol_version": "1.0", "adapter_version": manifest.version,
                "methods": manifest.methods, "capabilities": manifest.capabilities,
                "nonce": r.params.get("nonce").cloned().unwrap_or(Value::Null)})
            }
            ("core.heartbeat", "no-heartbeat") => continue,
            _ => json!({"ok": true}),
        };
        if let Ok(bytes) = encode(&Frame::Response(Response::ok(&r.id, reply))) {
            let _ = stdout.write_all(&bytes).await;
            let _ = stdout.flush().await;
        }
        if matches!(mode, "deaf-input" | "deaf-input-once") && r.method == "core.handshake" {
            if mode == "deaf-input-once" {
                let _ = std::fs::write("deaf-once.marker", b"first handshake completed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            return;
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--guardian-writer") {
        let pid = std::process::id();
        let path = format!("writer-{pid}.bin");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        if let Ok(mut file) = options.open(&path) {
            let _ = std::fs::write(
                format!("writer-{pid}.json"),
                serde_json::json!({"pid":pid,"path":path}).to_string(),
            );
            // Both the file and inherited stdout/stderr remain open.
            loop {
                let _ = file.write_all(b"still writing\n");
                let _ = file.flush();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        return;
    }
    if args.iter().any(|arg| arg == "--guardian-spawner") {
        if let Ok(exe) = std::env::current_exe() {
            let mut command = std::process::Command::new(exe);
            command.arg("--guardian-writer");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let _ = command.spawn();
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
        return;
    }
    if args.iter().any(|a| a == "--stdio-tail") {
        std::thread::sleep(std::time::Duration::from_millis(600));
        eprintln!("owned-stdio-tail-finished");
        return;
    }
    if let Ok(path) = std::env::var("LARM_TEST_PID_FILE") {
        let _ = std::fs::write(path, std::process::id().to_string());
    }
    if args.iter().any(|a| a == "--sleeper") {
        std::thread::sleep(std::time::Duration::from_secs(3600));
        return;
    }
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let manifest = arg("--manifest")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| AdapterManifest::parse(&t).ok());
    let Some(manifest) = manifest else {
        eprintln!("manifiesto ausente o inválido");
        std::process::exit(2);
    };
    let mut mode = arg("--mode").unwrap_or_else(|| "ok".into());
    if mode == "guardian-tree" {
        if let Ok(exe) = std::env::current_exe() {
            for (argument, inherit) in [
                ("--guardian-writer", true),
                ("--guardian-writer", false),
                ("--guardian-spawner", true),
            ] {
                let mut command = std::process::Command::new(&exe);
                command.arg(argument).stdin(std::process::Stdio::null());
                if !inherit {
                    command
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x0800_0000);
                }
                let _ = command.spawn();
            }
        }
    }
    if mode == "deaf-input-once" && std::path::Path::new("deaf-once.marker").exists() {
        mode = "ok".into();
    }
    if mode == "stderr-flood" {
        let chunk = "x".repeat(1023);
        let mut err = std::io::stderr();
        let _ = writeln!(err, "token=hf_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd");
        for _ in 0..(12 * 1024) {
            let _ = writeln!(err, "{chunk}");
        }
    }
    if matches!(
        mode.as_str(),
        "silent" | "bad-nonce" | "no-heartbeat" | "deaf-input" | "deaf-input-once"
    ) {
        manual(manifest, &mode).await;
        return;
    }
    let registry = benchmark_fixture::registry(&manifest.capabilities).unwrap_or_else(|_| {
        eprintln!("contrato de fixture inválido");
        std::process::exit(2)
    });
    let late_reply = mode == "late-extra";
    let handler = Arc::new(Fake {
        manifest,
        mode,
        registry,
    });
    let _ = serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        handler,
    )
    .await;
    if late_reply {
        // Fixture de un reply inesperado después de que la sesión reciba EOF.
        if let Ok(bytes) = encode(&Frame::Response(Response::err(
            None,
            RpcError::new(-32700, "late"),
        ))) {
            let mut stdout = tokio::io::stdout();
            let _ = stdout.write_all(&bytes).await;
            let _ = stdout.flush().await;
        }
    }
}
