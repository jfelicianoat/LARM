use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub fn daemon() -> Command {
    Command::new(env!("CARGO_BIN_EXE_larm-daemon"))
}

pub struct RunningDaemon {
    pub(super) child: Child,
    log_path: PathBuf,
}

impl RunningDaemon {
    pub fn start(data_root: &Path, config: &Path, port: u16, log_path: PathBuf) -> Self {
        Self::start_filtered(data_root, config, port, log_path, "error", false)
    }

    /// Phase telemetry for owned native performance/cancellation cases. The
    /// CLI accepts named levels; select numeric source phases when analyzing.
    #[allow(dead_code)] // shared helper is also compiled by other test targets
    pub fn start_measured(data_root: &Path, config: &Path, port: u16, log_path: PathBuf) -> Self {
        Self::start_filtered(data_root, config, port, log_path, "debug", true)
    }

    #[allow(dead_code)] // shared helper is compiled by independent test targets
    pub fn start_desktop_fixture(
        data_root: &Path,
        config: &Path,
        port: u16,
        log_path: PathBuf,
    ) -> Self {
        Self::start_filtered(data_root, config, port, log_path, "info", true)
    }

    fn start_filtered(
        data_root: &Path,
        config: &Path,
        port: u16,
        log_path: PathBuf,
        filter: &str,
        console: bool,
    ) -> Self {
        let log = std::fs::File::create(&log_path).expect("crear log del daemon de prueba");
        let mut command = daemon();
        let larm_environment: Vec<String> = std::env::vars()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("LARM_"))
            .collect();
        for name in larm_environment {
            command.env_remove(name);
        }
        command
            .args(["--serve-api", "--bind", "127.0.0.1", "--log-level", filter])
            .arg("--port")
            .arg(port.to_string())
            .arg("--data-root")
            .arg(data_root)
            .arg("--config")
            .arg(config)
            .stdout(if console {
                Stdio::from(log.try_clone().expect("clonar log nativo"))
            } else {
                Stdio::null()
            })
            .stderr(Stdio::from(log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().expect("arrancar proceso real del daemon");
        Self { child, log_path }
    }

    pub fn wait_live(&mut self, port: u16, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if health_live(port) {
                return Ok(());
            }
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("no se pudo observar el daemon: {error}"))?
            {
                return Err(format!(
                    "el daemon terminó antes de servir health/live ({status}); log={}",
                    self.log_text()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "el daemon no respondió a health/live en {timeout:?}; log={}",
            self.log_text()
        ))
    }

    pub fn stop(&mut self) {
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_else(|error| error.to_string())
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn health_live(port: u16) -> bool {
    let address = format!("127.0.0.1:{port}");
    let Ok(mut stream) = TcpStream::connect_timeout(
        &address.parse().expect("dirección de loopback válida"),
        Duration::from_millis(250),
    ) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .is_err()
    {
        return false;
    }
    if stream
        .write_all(
            b"GET /api/v1/health/live HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return false;
    }
    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return false;
    }
    let response = String::from_utf8_lossy(&response);
    response.starts_with("HTTP/1.1 200")
        && response.contains("\"status\":\"LIVE\"")
        && response.contains("\"schema_version\":\"1\"")
}
