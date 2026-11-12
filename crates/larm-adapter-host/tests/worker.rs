//! Host de adaptadores (P021) con procesos reales: el adaptador falso
//! (`examples/fake_adapter.rs`, que `cargo test` compila) en un paquete
//! temporal. Handshake, timeout, crash, stdout corrupto, stderr enorme,
//! cancelación, heartbeat, entorno mínimo, cola y parada.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use larm_adapter_host::worker::{HostConfig, TreeKill, Worker, WorkerSpec, WorkerState};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::Request;
use larm_domain::error::ErrorCode;
use serde_json::json;
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod benchmark_fixture;

fn fake_exe() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/examples");
    let exe = root.join(if cfg!(windows) {
        "fake_adapter.exe"
    } else {
        "fake_adapter"
    });
    assert!(
        exe.exists(),
        "falta {} (lo compila cargo test)",
        exe.display()
    );
    exe
}

struct Package {
    _dir: tempfile::TempDir,
    spec: WorkerSpec,
}

fn package(mode: &str) -> Package {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::copy(fake_exe(), pkg.join("bin").join("fake_adapter.exe")).unwrap();
    let mut m: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/fake_adapter/manifest.json"
    ))
    .unwrap();
    let manifest_path = pkg.join("manifest.json");
    m["executable"]["args"] = json!([
        "--manifest",
        manifest_path.display().to_string(),
        "--mode",
        mode
    ]);
    std::fs::write(&manifest_path, m.to_string()).unwrap();
    let manifest = AdapterManifest::parse(&m.to_string()).unwrap();
    Package {
        spec: WorkerSpec {
            package_dir: pkg,
            manifest,
            environment: vec![("LARM_ADAPTER_PROFILE".into(), "pruebas".into())],
            work_dir: dir.path().join("work"),
        },
        _dir: dir,
    }
}

fn fast() -> HostConfig {
    HostConfig {
        handshake_timeout: Duration::from_secs(2),
        rpc_timeout: Duration::from_secs(5),
        heartbeat: Duration::from_millis(300),
        cancel_grace: Duration::from_millis(500),
        ..HostConfig::default()
    }
}

async fn start(
    mode: &str,
    config: HostConfig,
) -> (Package, Result<Arc<Worker>, larm_domain::error::LarmError>) {
    let p = package(mode);
    let w = Worker::start(p.spec.clone(), config, 1, Arc::new(TreeKill)).await;
    (p, w)
}

async fn wait_state(w: &Worker, want: WorkerState) {
    for _ in 0..200 {
        if w.state() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "estado {:?}, se esperaba {want:?} (motivo {:?})",
        w.state(),
        w.reason()
    );
}

#[tokio::test]
async fn handshake_correcto_y_llamadas() {
    let (_p, w) = start("ok", fast()).await;
    let w = w.unwrap();
    assert_eq!(w.state(), WorkerState::Ready);
    assert_eq!(
        w.handshake().capabilities,
        vec!["benchmark.fake".to_string()]
    );
    assert_eq!(
        w.call("benchmark.run", benchmark_fixture::request(20), None, None)
            .await
            .unwrap()["metrics"]["ms"],
        20
    );
    // Método no anunciado: no llega al proceso.
    let e = w
        .call("runtime.health", json!({}), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.details.reason.as_str()),
        (ErrorCode::UnsupportedCapability, "method_not_announced")
    );
    w.stop().await;
    wait_state(&w, WorkerState::Stopped).await;
    assert!(w.exited());
    assert!(w.io_drained());
    let e = w
        .call("benchmark.run", benchmark_fixture::request(10), None, None)
        .await
        .unwrap_err();
    assert_eq!(e.details.reason, "worker_not_ready");
}

fn owned_pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let ids = [Pid::from_u32(pid)];
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing(),
        );
        system.process(ids[0]).is_some()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

#[cfg(windows)]
struct ParentOnlyKill;

#[cfg(windows)]
impl larm_adapter_host::worker::Containment for ParentOnlyKill {
    fn kill_tree(&self, pid: u32) {
        let system = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let _ = std::process::Command::new(system.join("System32").join("taskkill.exe"))
            .args(["/PID", &pid.to_string(), "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(windows)]
#[tokio::test]
async fn job_object_al_cerrar_worker_termina_descendiente_aunque_muera_el_padre() {
    close_tree("deaf-tree").await;
}

#[cfg(windows)]
#[tokio::test]
async fn cierre_espera_descendiente_sin_canales_heredados() {
    close_tree("deaf-tree-no-stdio").await;
}

#[cfg(windows)]
#[tokio::test]
async fn cierre_espera_todos_los_descendientes_con_canales_heredados() {
    close_tree("deaf-tree-many").await;
}

#[cfg(windows)]
async fn close_tree(mode: &str) {
    let p = package(mode);
    let w = Worker::start(
        p.spec.clone(),
        HostConfig {
            heartbeat: Duration::from_secs(60),
            ..fast()
        },
        1,
        Arc::new(ParentOnlyKill),
    )
    .await
    .unwrap();
    let caller = w.clone();
    let call = tokio::spawn(async move {
        caller
            .call(
                "benchmark.run",
                benchmark_fixture::request(10_000),
                Some(Duration::from_secs(30)),
                None,
            )
            .await
    });

    let expected_children = if mode == "deaf-tree-many" { 4 } else { 1 };
    let child_pids = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let children: std::collections::BTreeSet<u32> = w
                .recent_logs()
                .iter()
                .filter_map(|line| {
                    line.line
                        .strip_prefix("child_pid=")
                        .and_then(|value| value.parse::<u32>().ok())
                })
                .collect();
            if children.len() == expected_children {
                break children;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !child_pids.iter().all(|pid| owned_pid_alive(*pid)) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("el nieto debe estar vivo antes del cierre");

    w.stop_and_wait().await.unwrap();
    assert!(w.io_drained());
    assert!(
        child_pids.iter().all(|pid| !owned_pid_alive(*pid)),
        "el cierre confirma también la salida del nieto antes de regresar"
    );
    let _ = tokio::time::timeout(Duration::from_secs(2), call).await;
}

#[tokio::test]
async fn cierre_espera_canales_heredados_despues_de_terminar_el_proceso_raiz() {
    let (_p, w) = start("tail-after-exit", fast()).await;
    let w = w.unwrap();
    let _ = w
        .call("benchmark.run", benchmark_fixture::request(10), None, None)
        .await;
    for _ in 0..200 {
        if w.exited() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(w.exited());
    w.stop_and_wait().await.unwrap();
    assert!(w.io_drained());
    assert!(
        w.recent_logs()
            .iter()
            .any(|line| line.line == "owned-stdio-tail-finished"),
        "el cierre espera también stderr del hijo que heredó el canal"
    );
    w.stop_and_wait().await.unwrap();
}

#[tokio::test]
async fn cancelar_arranque_silencioso_termina_el_proceso_lanzado() {
    let mut p = package("silent");
    let pid_file = p._dir.path().join("started.pid");
    p.spec.environment.push((
        "LARM_TEST_PID_FILE".into(),
        pid_file.to_string_lossy().into_owned(),
    ));
    let spec = p.spec.clone();
    let start = tokio::spawn(async move {
        Worker::start(
            spec,
            HostConfig {
                handshake_timeout: Duration::from_secs(30),
                ..fast()
            },
            1,
            Arc::new(TreeKill),
        )
        .await
    });
    // En Windows, varios workers se lanzan en paralelo en esta suite; deja
    // margen para carga de procesos antes de declarar que no hubo arranque.
    tokio::time::timeout(Duration::from_secs(15), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    assert!(owned_pid_alive(pid));
    start.abort();
    assert!(start.await.err().unwrap().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while owned_pid_alive(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelar el futuro de start no abandona el worker");
}

#[tokio::test]
async fn soltar_ultimo_propietario_termina_el_worker_sin_pedir_stop() {
    let (_p, w) = start("ok", fast()).await;
    let w = w.unwrap();
    let pid = w.pid;
    drop(w);
    tokio::time::timeout(Duration::from_secs(5), async {
        while owned_pid_alive(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn parada_no_espera_indefinidamente_un_escritor_bloqueado_en_stdin() {
    use larm_adapter_host::worker::Containment;
    let (_p, w) = start(
        "deaf-input",
        HostConfig {
            heartbeat: Duration::from_secs(30),
            cancel_grace: Duration::from_millis(100),
            ..fast()
        },
    )
    .await;
    let w = w.unwrap();
    let caller = w.clone();
    let call = tokio::spawn(async move {
        caller
            .call(
                "benchmark.run",
                json!({"padding":"x".repeat(512 << 10)}),
                Some(Duration::from_secs(30)),
                None,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stopped = tokio::time::timeout(Duration::from_secs(5), w.stop_and_wait()).await;
    if stopped.is_err() && !w.exited() {
        TreeKill.kill_tree(w.pid);
    }
    assert!(call.await.unwrap().is_err());
    assert!(
        matches!(stopped, Ok(Ok(()))),
        "no debe esperar al timeout RPC de 30 s para poder matar"
    );
    assert!(w.io_drained());
}

#[tokio::test]
async fn handshake_fallido() {
    let (_p, w) = start("silent", fast()).await;
    let e = w.err().unwrap();
    assert_eq!(
        (e.code, e.details.reason.as_str()),
        (ErrorCode::Timeout, "handshake_timeout")
    );
    let (_p, w) = start("bad-nonce", fast()).await;
    assert_eq!(w.err().unwrap().details.reason, "nonce_mismatch");
    // Ejecutable ausente: nunca se lanza nada.
    let mut p = package("ok");
    std::fs::remove_file(p.spec.package_dir.join("bin").join("fake_adapter.exe")).unwrap();
    p.spec.work_dir = p.spec.package_dir.join("w");
    let e = Worker::start(p.spec.clone(), fast(), 1, Arc::new(TreeKill))
        .await
        .err()
        .unwrap();
    assert_eq!(e.details.reason, "executable_path_invalid");
    // Variable con un nombre que permitiría inyectar: rechazada.
    let mut p = package("ok");
    p.spec.environment = vec![("A=B C".into(), "x".into())];
    let e = Worker::start(p.spec.clone(), fast(), 1, Arc::new(TreeKill))
        .await
        .err()
        .unwrap();
    assert_eq!(e.details.reason, "environment_invalid");
}

#[tokio::test]
async fn timeout_con_cancelacion_atendida_y_sin_atender() {
    // El adaptador atiende la cancelación: TIMEOUT y el worker sigue listo.
    let (_p, w) = start("ok", fast()).await;
    let w = w.unwrap();
    let e = w
        .call(
            "benchmark.run",
            benchmark_fixture::request(10_000),
            Some(Duration::from_millis(200)),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.details.reason.as_str()),
        (ErrorCode::Timeout, "rpc_timeout")
    );
    assert_eq!(w.state(), WorkerState::Ready);
    assert!(w
        .call("benchmark.run", benchmark_fixture::request(10), None, None)
        .await
        .is_ok());
    // No la atiende: tras la gracia se mata el proceso (último recurso).
    let (_p, w) = start("deaf", fast()).await;
    let w = w.unwrap();
    let e = w
        .call(
            "benchmark.run",
            benchmark_fixture::request(10),
            Some(Duration::from_millis(200)),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.details.reason, "rpc_timeout");
    wait_state(&w, WorkerState::Failed).await;
    assert_eq!(w.reason().as_deref(), Some("cancel_grace_expired"));
    for _ in 0..100 {
        if w.exited() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(w.exited(), "proceso terminado");
}

#[tokio::test]
async fn cancelacion_pedida_por_el_host() {
    let (_p, w) = start("ok", fast()).await;
    let w = w.unwrap();
    let req = Request::new("benchmark.run", benchmark_fixture::request(10_000));
    let id = req.id.clone();
    let worker = w.clone();
    let call = tokio::spawn(async move { worker.call_request(req, None, None).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(w.cancel(&id).await.unwrap());
    let e = call.await.unwrap().unwrap_err();
    assert_eq!(e.details.reason, "request_cancelled");
    let mut still_running = true;
    for _ in 0..100 {
        still_running = w.cancel(&id).await.unwrap();
        if !still_running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !still_running,
        "el worker libera el ID después de vaciar el reply"
    );
    assert_eq!(w.state(), WorkerState::Ready);
}

#[tokio::test]
async fn crash_del_proceso() {
    let (_p, w) = start("crash", fast()).await;
    let w = w.unwrap();
    let e = w
        .call("benchmark.run", benchmark_fixture::request(10), None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::AdapterCrashed);
    wait_state(&w, WorkerState::Failed).await;
    assert_eq!(w.reason().as_deref(), Some("process_exited"));
}

#[tokio::test]
async fn stdout_corrupto() {
    let (_p, w) = start("garbage", fast()).await;
    let w = w.unwrap();
    let r = w
        .call("benchmark.run", benchmark_fixture::request(10), None, None)
        .await;
    // Tres líneas que no son frames: FAILED y el proceso muere.
    wait_state(&w, WorkerState::Failed).await;
    assert_eq!(w.reason().as_deref(), Some("stdout_corrupt"));
    assert!(r.is_err(), "la llamada no se da por buena");
}

#[tokio::test]
async fn stderr_enorme_no_bloquea_y_se_limita() {
    // El fixture escribe 12 MiB antes del handshake; bajo la suite completa
    // puede necesitar más margen de planificación que los tests ligeros.
    let mut config = fast();
    config.handshake_timeout = Duration::from_secs(10);
    let (_p, w) = start("stderr-flood", config).await;
    let w = w.unwrap();
    let mut logs = w.subscribe_logs();
    assert!(
        w.call("benchmark.run", benchmark_fixture::request(10), None, None)
            .await
            .is_ok(),
        "stderr lleno no bloquea"
    );
    for _ in 0..100 {
        if w.stderr_dropped() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(w.stderr_dropped() > 0, "lo que pasa de 10 MiB se descarta");
    let recent = w.recent_logs();
    assert!(
        recent.iter().any(|l| l.line.contains("token=")),
        "la línea del token llega, redactada"
    );
    for l in &recent {
        assert!(
            !l.line.contains("hf_ABCDEFGH"),
            "nada de secretos en los logs"
        );
        assert_eq!(l.adapter_id, "larm.fake-adapter");
    }
    let seen = recent.len();
    let _ = logs.try_recv();
    assert!(seen > 0, "stderr llega como logs etiquetados");
}

#[tokio::test]
async fn heartbeat_ausente() {
    let (_p, w) = start("no-heartbeat", fast()).await;
    let w = w.unwrap();
    wait_state(&w, WorkerState::Failed).await;
    assert_eq!(w.reason().as_deref(), Some("heartbeat_missed"));
}

#[tokio::test]
async fn entorno_minimo_explicito() {
    let (_p, w) = start("env", fast()).await;
    let w = w.unwrap();
    let env = w
        .call("benchmark.describe", json!({}), None, None)
        .await
        .unwrap();
    let names: Vec<String> = serde_json::from_value(env["settings_schema"]["env"].clone()).unwrap();
    let upper: Vec<String> = names.iter().map(|n| n.to_uppercase()).collect();
    assert!(
        upper.contains(&"LARM_ADAPTER_PROFILE".to_string()),
        "lo del perfil sí"
    );
    assert!(upper.contains(&"TEMP".to_string()));
    for forbidden in ["USERPROFILE", "APPDATA", "USERNAME", "PATH"] {
        assert!(
            !upper.contains(&forbidden.to_string()),
            "{forbidden} no se hereda: {names:?}"
        );
    }
}

#[tokio::test]
async fn cola_acotada() {
    let config = HostConfig {
        queue: 2,
        max_concurrent: 1,
        ..fast()
    };
    let (_p, w) = start("ok", config).await;
    let w = w.unwrap();
    let a = {
        let w = w.clone();
        tokio::spawn(async move {
            w.call("benchmark.run", benchmark_fixture::request(600), None, None)
                .await
        })
    };
    let b = {
        let w = w.clone();
        tokio::spawn(async move {
            w.call("benchmark.run", benchmark_fixture::request(600), None, None)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let e = w
        .call("benchmark.run", benchmark_fixture::request(1), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        (e.details.reason.as_str(), e.retryable),
        ("queue_full", true)
    );
    assert!(a.await.unwrap().is_ok() && b.await.unwrap().is_ok());
    // Mutaciones sobre el mismo recurso: en serie aunque haya plazas.
    let config = HostConfig {
        max_concurrent: 4,
        ..fast()
    };
    let (_p, w) = start("ok", config).await;
    let w = w.unwrap();
    let t0 = std::time::Instant::now();
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let w = w.clone();
            tokio::spawn(async move {
                w.call(
                    "benchmark.run",
                    benchmark_fixture::request(200),
                    None,
                    Some("variant:x"),
                )
                .await
            })
        })
        .collect();
    for c in calls {
        c.await.unwrap().unwrap();
    }
    assert!(
        t0.elapsed() >= Duration::from_millis(550),
        "serializadas: {:?}",
        t0.elapsed()
    );
}

#[tokio::test]
async fn matar_es_el_ultimo_recurso_y_mata_el_arbol() {
    let (_p, w) = start("deaf-tree", fast()).await;
    let w = w.unwrap();
    let mut logs = w.subscribe_logs();
    let e = w
        .call(
            "benchmark.run",
            benchmark_fixture::request(10),
            Some(Duration::from_millis(300)),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.details.reason, "rpc_timeout");
    wait_state(&w, WorkerState::Failed).await;
    let mut child = None;
    while let Ok(l) = logs.try_recv() {
        if let Some(pid) = l.line.strip_prefix("child_pid=") {
            child = pid.trim().parse::<u32>().ok();
        }
    }
    let child = child.expect("el nieto escribió su pid");
    let alive = |pid: u32| {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    };
    for _ in 0..100 {
        if !alive(child) && !alive(w.pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!alive(w.pid), "worker muerto");
    assert!(!alive(child), "el nieto también: se mata el árbol");
}
