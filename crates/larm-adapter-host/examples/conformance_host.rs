//! Observador del host real para paquetes propios mutados por wire_proxy.
//! No concede confianza ni modifica instalaciones/selecciones de Core.
#![forbid(unsafe_code)]
use larm_adapter_host::worker::{HostConfig, TreeKill, Worker, WorkerSpec, WorkerState};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::Request;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    package: PathBuf,
    work: PathBuf,
    method: String,
    params: Value,
    mode: String,
}

async fn run() -> Result<Value, Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("input")?;
    let bytes = std::fs::read(path)?;
    if bytes.len() > 1 << 20 {
        return Err("input too large".into());
    }
    let input: Input = serde_json::from_slice(&bytes)?;
    let manifest = AdapterManifest::parse(&std::fs::read_to_string(
        input.package.join("manifest.json"),
    )?)?;
    let spec = WorkerSpec {
        package_dir: input.package,
        manifest,
        environment: vec![("LARM_CONFORMANCE_FAULT".into(), input.mode.clone())],
        work_dir: input.work,
    };
    let started = Instant::now();
    let config = HostConfig {
        handshake_timeout: Duration::from_secs(5),
        heartbeat: Duration::from_secs(1),
        cancel_grace: Duration::from_millis(500),
        ..HostConfig::default()
    };
    let worker = match Worker::start(spec, config, 1, Arc::new(TreeKill)).await {
        Ok(worker) => worker,
        Err(error) => return Ok(json!({"start_error":error})),
    };
    let mut progress = worker.subscribe_progress();
    let timeout = if input.mode == "stall" {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(5)
    };
    let request = Request::new(&input.method, input.params);
    let request_id = request.id.clone();
    let call = worker.call_request(request, Some(timeout), None).await;
    if !["normal", "stderr-long", "malformed-once"].contains(&input.mode.as_str()) {
        for _ in 0..100 {
            if worker.state() == WorkerState::Failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    if input.mode == "stderr-long" {
        for _ in 0..100 {
            if worker.stderr_dropped() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    let state = worker.state();
    let reason = worker.reason();
    let mut progress_count = 0;
    let mut foreign_progress_count = 0;
    while let Ok(progress) = progress.try_recv() {
        progress_count += 1;
        if progress.request_id != request_id {
            foreign_progress_count += 1;
        }
    }
    let dropped = worker.stderr_dropped();
    let recent_bytes: usize = worker
        .recent_logs()
        .iter()
        .map(|entry| entry.line.len())
        .sum();
    worker.stop().await;
    for _ in 0..100 {
        if worker.exited() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (result, error) = match call {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error)),
    };
    Ok(
        json!({"result":result,"error":error,"state":state,"reason":reason,"worker_pid":worker.pid,"exited":worker.exited(),"request_id":request_id,"progress_count":progress_count,"foreign_progress_count":foreign_progress_count,"stderr_dropped":dropped,"retained_log_bytes":recent_bytes,"elapsed_ms":started.elapsed().as_millis().to_string()}),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    match run().await {
        Ok(value) => println!("{value}"),
        Err(_) => {
            eprintln!("conformance-host: fixture inválida");
            std::process::exit(2);
        }
    }
}
