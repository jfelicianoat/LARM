//! P137: User Background Agent y Windows Service opcional REAL sobre el PC
//! IA — el daemon real arranca como agente de fondo del usuario (sin
//! privilegios) y sirve la API; la tarea de logon se registra y elimina con
//! `schtasks` reales; el servicio opcional exige elevación (sin ella queda
//! NO VERIFICADO); la desinstalación nunca borra datos y la purga es
//! separada y confirmada.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/daemon.rs"]
mod daemon_process;

use daemon_process::RunningDaemon;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn daemon_command(data_root: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_larm-daemon"));
    command
        .arg("--data-root")
        .arg(data_root)
        .arg("--config")
        .arg(config);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn write_config(path: &Path) {
    std::fs::write(
        path,
        serde_json::json!({"paths":{"database_relative_path":"larm.sqlite"}}).to_string(),
    )
    .unwrap();
}

fn run_cli(data_root: &Path, config: &Path, args: &[&str]) -> (bool, String) {
    let output = daemon_command(data_root, config)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn install_status(data_root: &Path, config: &Path) -> Value {
    let (ok, text) = run_cli(data_root, config, &["install-status"]);
    assert!(ok, "install-status: {text}");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agente_de_fondo_y_modo_servicio_reales_en_windows() {
    let dir = tempfile::TempDir::new().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let cfg = dir.path().join("larm.json");
    write_config(&cfg);

    // 0) Mejor esfuerzo: limpiar cualquier tarea de una corrida anterior.
    let _ = run_cli(&data, &cfg, &["uninstall-agent"]);

    // 1) User agent startup test (oráculo): el daemon REAL del usuario,
    //    sin privilegios de administrador, sirve health/live en loopback con
    //    los argumentos del agente de fondo.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut agent = RunningDaemon::start(&data, &cfg, port, data.join("p137-agent.log"));
    agent.wait_live(port, Duration::from_secs(45)).unwrap();
    let status = install_status(&data, &cfg);
    let elevated = status["elevated"].as_bool().unwrap();
    println!(
        "REAL P137: user background agent arranca y sirve la API (elevated={elevated}, mode_default={})",
        status["mode_default"]
    );
    agent.stop();

    // 2) Registro REAL de la tarea de logon del agente (sin admin) y su
    //    eliminación; el estado pasa por `schtasks` reales del PC IA.
    let (ok, text) = run_cli(&data, &cfg, &["install-agent"]);
    assert!(ok, "install-agent: {text}");
    let receipt: Value = serde_json::from_str(text.trim_start()).unwrap_or(Value::Null);
    assert_eq!(receipt["file"], "LARM User Agent.cmd");
    assert_eq!(receipt["mode"], "user-background-agent");
    let status = install_status(&data, &cfg);
    assert_eq!(status["agent_task_registered"], true);
    let startup = std::env::var("APPDATA").unwrap()
        + std::path::MAIN_SEPARATOR_STR
        + "Microsoft\\Windows\\Start Menu\\Programs\\Startup";
    let autostart = std::path::PathBuf::from(&startup).join("LARM User Agent.cmd");
    let content = std::fs::read_to_string(&autostart).expect("autarranque en Startup");
    assert!(content.contains("--serve-api"), "el .cmd arranca la API");
    println!("REAL P137: autarranque del agente en la carpeta Startup del usuario (sin admin)");
    let (ok, text) = run_cli(&data, &cfg, &["uninstall-agent"]);
    assert!(ok, "uninstall-agent: {text}");
    let receipt: Value = serde_json::from_str(text.trim_start()).unwrap_or(Value::Null);
    assert_eq!(receipt["removed"], true);
    assert_eq!(receipt["plan"]["purge_data"], false, "sin purga de datos");
    assert_eq!(install_status(&data, &cfg)["agent_task_registered"], false);
    println!("REAL P137: tarea eliminada; la desinstalación conserva modelos y base");

    // 3) Servicio opcional: sin elevación queda NO VERIFICADO (conducta
    //    esperada); la instalación explícita exige consola elevada.
    if !elevated {
        let (ok, text) = run_cli(&data, &cfg, &["install-service"]);
        assert!(!ok, "sin elevación no se instala el servicio");
        assert!(text.contains("service_requires_elevation"), "{text}");
        println!("REAL P137: servicio opcional sin elevación → NO VERIFICADO (service_requires_elevation)");
    } else {
        println!("REAL P137: consola elevada detectada; la instalación del servicio se deja para el operador (NO VERIFICADO aquí)");
    }

    // 4) Purga de datos: operación SEPARADA y confirmada; sin confirm no
    //    borra nada, con --confirm borra la raíz de datos (modelos y base).
    std::fs::write(data.join("larm.sqlite"), b"sqlite").unwrap_or(());
    let (ok, text) = run_cli(&data, &cfg, &["purge-data"]);
    assert!(!ok, "sin confirmación la purga se rechaza");
    assert!(
        text.contains("uninstall_purge_requires_confirmation"),
        "{text}"
    );
    assert!(data.exists(), "sin confirmación no se borra nada");
    println!("REAL P137: purge sin confirmación rechazada (uninstall_purge_requires_confirmation)");
    let (ok, text) = run_cli(&data, &cfg, &["purge-data", "--confirm"]);
    assert!(ok, "purge-data --confirm: {text}");
    assert!(!data.exists(), "la purga confirmada borra la raíz de datos");
    println!("REAL P137: purge confirmada borra modelos y base (operación separada)");
}
