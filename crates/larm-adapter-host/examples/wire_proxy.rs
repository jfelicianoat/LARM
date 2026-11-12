//! Mutante de transporte exclusivo de tests P026. Ejecuta el worker real y
//! altera su canal; nunca se distribuye ni cualifica como adaptador de producto.
#![forbid(unsafe_code)]
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    real_executable: String,
    trigger_method: String,
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let package = executable
        .parent()
        .and_then(|p| p.parent())
        .ok_or("package")?;
    let config: Config = serde_json::from_slice(&std::fs::read(package.join("wire-proxy.json"))?)?;
    let real = package.join(&config.real_executable).canonicalize()?;
    if !real.starts_with(package.canonicalize()?) || real == executable {
        return Err("real executable".into());
    }
    let mode = std::env::var("LARM_CONFORMANCE_FAULT")?;
    if ![
        "normal",
        "unknown-id",
        "duplicate",
        "unknown-progress",
        "reverse-request",
        "malformed",
        "malformed-once",
        "oversized-no-newline",
        "invalid-result",
        "stall",
        "crash",
        "stderr-long",
    ]
    .contains(&mode.as_str())
    {
        return Err("mode".into());
    }
    let mut command = Command::new(real);
    command
        .args(std::env::args().skip(1))
        .env_remove("LARM_CONFORMANCE_FAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn()?;
    // Instrumentación de la fixture propia, fuera del paquete inmutable.
    std::fs::write(
        PathBuf::from("wire-processes.json"),
        serde_json::to_vec(&json!({"proxy":std::process::id(),"worker":child.id()}))?,
    )?;
    let input = child.stdin.take().ok_or("stdin")?;
    let output = child.stdout.take().ok_or("stdout")?;
    let error = child.stderr.take().ok_or("stderr")?;
    let ids: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let input_ids = ids.clone();
    let input_mode = mode.clone();
    let mut forward_input = tokio::spawn(async move {
        let mut input = input;
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            let frame: Value = serde_json::from_str(&line)?;
            if frame["method"] == config.trigger_method {
                if let Some(id) = frame["id"].as_str() {
                    if let Ok(mut pending) = input_ids.lock() {
                        pending.insert(id.into());
                    }
                }
            }
            if input_mode == "stall" && frame["method"] == "core.cancel" {
                continue;
            }
            input.write_all(line.as_bytes()).await?;
            input.write_all(b"\n").await?;
            input.flush().await?;
        }
        input.shutdown().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let stderr = tokio::spawn(async move {
        let mut error = error;
        tokio::io::copy(&mut error, &mut tokio::io::stderr()).await
    });
    let (crash, mut crashed) = tokio::sync::mpsc::channel::<()>(1);
    let mut forward_output = tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        let mut stdout = tokio::io::stdout();
        while let Some(line) = lines.next_line().await? {
            let frame: Value = serde_json::from_str(&line)?;
            let target = frame["id"]
                .as_str()
                .is_some_and(|id| ids.lock().is_ok_and(|pending| pending.contains(id)));
            if !target {
                stdout.write_all(line.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
            if mode == "crash" {
                let _ = crash.send(()).await;
                std::future::pending::<()>().await;
            }
            if mode == "stall" {
                continue;
            }
            if mode == "malformed-once" {
                stdout.write_all(b"{\n").await?;
                stdout.write_all(line.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
            if mode == "oversized-no-newline" {
                stdout.write_all(&vec![b'x'; (1 << 20) + 1]).await?;
                stdout.flush().await?;
                // No LF ni EOF: debe intervenir el límite durante la lectura.
                continue;
            }
            if mode == "stderr-long" {
                let mut stderr = tokio::io::stderr();
                stderr.write_all(&vec![b'x'; 12 << 20]).await?;
                stderr.write_all(b"\nwire-complete\n").await?;
                stderr.flush().await?;
            }
            let repetitions = if mode == "duplicate" {
                4
            } else if mode == "normal" || mode == "stderr-long" {
                1
            } else {
                3
            };
            for _ in 0..repetitions {
                let mut altered = frame.clone();
                match mode.as_str() {
                    "unknown-id" => altered["id"] = json!(uuid::Uuid::now_v7().to_string()),
                    "unknown-progress" => {
                        altered = json!({"jsonrpc":"2.0","method":"core.progress","params":{"request_id":uuid::Uuid::now_v7().to_string(),"phase":"RUNNING","completed_bytes":null,"total_bytes":null,"message_code":null}})
                    }
                    "reverse-request" => {
                        altered = json!({"jsonrpc":"2.0","id":uuid::Uuid::now_v7().to_string(),"method":"core.heartbeat","params":{}})
                    }
                    "invalid-result" => {
                        altered.as_object_mut().map(|o| o.remove("error"));
                        altered["result"] = json!({"private_fault_marker":"must-not-escape"});
                    }
                    _ => {}
                }
                let bytes = if mode == "malformed" {
                    b"{".to_vec()
                } else {
                    serde_json::to_vec(&altered)?
                };
                stdout.write_all(&bytes).await?;
                stdout.write_all(b"\n").await?;
            }
            stdout.flush().await?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let crash_requested = tokio::select! {
        value = crashed.recv() => value.is_some(),
        _ = child.wait() => false,
        _ = &mut forward_input => { let _ = child.wait().await; false },
        _ = &mut forward_output => { let _ = child.start_kill(); let _ = child.wait().await; false },
    };
    if crash_requested {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    forward_input.abort();
    forward_output.abort();
    stderr.abort();
    if crash_requested {
        return Err("injected crash".into());
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if run().await.is_err() {
        eprintln!("wire-proxy: ejecución de fixture terminada");
        std::process::exit(3);
    }
}
