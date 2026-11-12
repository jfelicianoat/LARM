//! Sondeo de dispositivos y APIs de aceleración (P011): reúne lo observado y
//! se lo pasa a [`crate::accel::assemble`].
//!
//! Todo lo que carga código del driver o de un fabricante corre en el proceso
//! hijo `larm-daemon probe-accelerators` (ADR-0010) o en herramientas
//! externas (`vulkaninfo`, `clinfo`), siempre con timeout. Aquí solo se leen
//! carpetas, ficheros y el registro.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::accel::{AccelInputs, AccelOutput, DriverInfo, Failure, LibraryFile, ToolOutcome};
use crate::native_report::{NativeReport, NATIVE_REPORT_VERSION};

/// Cómo lanzar el sondeo nativo (el daemon se lanza a sí mismo).
#[derive(Debug, Clone)]
pub struct NativeCommand {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

/// Configuración del sondeo de dispositivos.
#[derive(Debug, Clone)]
pub struct DeviceProbeConfig {
    /// `None`: no hay sondeo nativo disponible (sus datos quedan UNKNOWN).
    pub native: Option<NativeCommand>,
    pub tool_timeout: Duration,
    pub system32: PathBuf,
    pub path_var: Option<std::ffi::OsString>,
}

impl DeviceProbeConfig {
    /// Configuración del equipo actual: `%SystemRoot%\System32` y el `PATH`.
    #[must_use]
    pub fn from_environment(native: Option<NativeCommand>) -> Self {
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        Self {
            native,
            tool_timeout: Duration::from_secs(20),
            system32: root.join("System32"),
            path_var: std::env::var_os("PATH"),
        }
    }
}

/// Ejecuta el hijo nativo y valida su informe.
///
/// # Errors
/// [`Failure`] con `native_unavailable`, `native_timeout`, `native_failed`,
/// `native_unparseable` o `native_version`.
pub async fn run_native(
    command: &NativeCommand,
    files: &[PathBuf],
) -> Result<NativeReport, Failure> {
    let mut args = command.args.clone();
    for f in files {
        args.push("--file".into());
        args.push(f.display().to_string());
    }
    let out = crate::process::run(&command.program, &args, command.timeout)
        .await
        .map_err(|f| Failure {
            reason: format!("native_{}", f.kind),
            detail: f.detail,
        })?;
    let report: NativeReport = serde_json::from_str(out.trim()).map_err(|e| Failure {
        reason: "native_unparseable".into(),
        detail: format!("informe nativo ilegible: {e}"),
    })?;
    if report.report_version != NATIVE_REPORT_VERSION {
        return Err(Failure {
            reason: "native_version".into(),
            detail: format!(
                "informe nativo v{} y este perfilador espera v{NATIVE_REPORT_VERSION}",
                report.report_version
            ),
        });
    }
    Ok(report)
}

/// Primera ruta de `name` en las carpetas dadas.
#[must_use]
pub fn find_tool(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Ejecuta una herramienta de consulta y analiza su salida.
pub async fn run_tool<T>(
    tool: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    parse: fn(&str) -> T,
) -> ToolOutcome<T> {
    let Some(tool) = tool else {
        return ToolOutcome::NotInstalled;
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    match crate::process::run(&tool.display().to_string(), &args, timeout).await {
        Ok(out) => ToolOutcome::Ran(parse(&out)),
        Err(f) => ToolOutcome::Failed(Failure {
            reason: format!("tool_{}", f.kind),
            detail: f.detail,
        }),
    }
}

/// Valores del driver bajo `HKLM\SYSTEM\CurrentControlSet\Control\Class\<clave>`.
#[cfg(windows)]
fn read_driver(key: &str) -> DriverInfo {
    let Ok(k) = windows_registry::LOCAL_MACHINE
        .open(format!(r"SYSTEM\CurrentControlSet\Control\Class\{key}"))
    else {
        return DriverInfo::default();
    };
    let get = |name: &str| {
        k.get_string(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    DriverInfo {
        desc: get("DriverDesc"),
        version: get("DriverVersion"),
        provider: get("ProviderName"),
    }
}

#[cfg(not(windows))]
fn read_driver(_: &str) -> DriverInfo {
    DriverInfo::default()
}

/// Sondea dispositivos y APIs. Nunca falla: lo que no se puede observar
/// queda UNKNOWN con su motivo.
pub async fn probe(config: &DeviceProbeConfig) -> (AccelOutput, BTreeMap<&'static str, String>) {
    let env = |v: &str| std::env::var_os(v).map(PathBuf::from);
    let dirs = crate::libs::search_dirs(&config.system32, config.path_var.as_deref(), &env);
    let scan_dirs = dirs.clone();
    let found = tokio::task::spawn_blocking(move || crate::libs::scan(&scan_dirs))
        .await
        .unwrap_or_default();
    let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();

    let vulkaninfo = find_tool("vulkaninfo.exe", &dirs);
    let clinfo = find_tool("clinfo.exe", &dirs);
    let hash_paths = paths.clone();
    let (native, vulkan, opencl, hashes) = tokio::join!(
        async {
            match &config.native {
                Some(command) => run_native(command, &paths).await,
                None => Err(Failure {
                    reason: "native_unavailable".into(),
                    detail: "este proceso no tiene sondeo nativo configurado".into(),
                }),
            }
        },
        run_tool(
            vulkaninfo.as_deref(),
            &["--summary"],
            config.tool_timeout,
            crate::tools::parse_vulkaninfo
        ),
        run_tool(
            clinfo.as_deref(),
            &[],
            config.tool_timeout,
            crate::tools::parse_clinfo
        ),
        tokio::task::spawn_blocking(move || {
            hash_paths
                .iter()
                .map(|p| crate::libs::sha256_file(p).map_err(|e| e.to_string()))
                .collect::<Vec<_>>()
        }),
    );
    let hashes = hashes.unwrap_or_default();

    let versions: BTreeMap<String, (Option<String>, Option<String>)> = native
        .as_ref()
        .map(|r| {
            r.files
                .iter()
                .map(|f| {
                    (
                        f.path.to_ascii_lowercase(),
                        (f.version.clone(), f.error.clone()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let libraries = found
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let path = f.path.display().to_string();
            // La versión de fichero es opcional (no todas las DLL la tienen);
            // un hash ilegible sí es un dato perdido.
            let (version, _) = versions
                .get(&path.to_ascii_lowercase())
                .cloned()
                .unwrap_or((None, None));
            let (sha256, hash_error) = match hashes.get(i) {
                Some(Ok(h)) => (Some(h.clone()), None),
                Some(Err(e)) => (None, Some(e.clone())),
                None => (None, Some("sin hash".into())),
            };
            LibraryFile {
                api: f.api,
                path,
                version,
                sha256,
                error: hash_error,
            }
        })
        .collect();

    let keys: Vec<String> = native
        .as_ref()
        .map(|r| r.pnp.iter().filter_map(|p| p.driver_key.clone()).collect())
        .unwrap_or_default();
    let drivers = tokio::task::spawn_blocking(move || {
        keys.into_iter()
            .map(|k| (k.to_ascii_lowercase(), read_driver(&k)))
            .collect::<BTreeMap<_, _>>()
    })
    .await
    .unwrap_or_default();

    let mut sources = BTreeMap::new();
    sources.insert(
        "devices",
        config.native.as_ref().map_or_else(
            || "sin sondeo nativo".to_string(),
            |c| format!("{} {}:DXGI+D3D12+cfgmgr32", c.program, c.args.join(" ")),
        ),
    );
    sources.insert(
        "devices.driver_version",
        r"registry:HKLM\SYSTEM\CurrentControlSet\Control\Class".to_string(),
    );
    sources.insert(
        "vulkan",
        vulkaninfo.map_or_else(
            || "vulkaninfo no instalado".into(),
            |p| format!("{} --summary", p.display()),
        ),
    );
    sources.insert(
        "opencl",
        clinfo.map_or_else(|| "clinfo no instalado".into(), |p| p.display().to_string()),
    );

    let output = crate::accel::assemble(&AccelInputs {
        native,
        drivers,
        vulkan,
        opencl,
        libraries,
        searched_dirs: dirs.len(),
    });
    (output, sources)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn powershell(script: &str, timeout_ms: u64) -> NativeCommand {
        NativeCommand {
            program: "powershell".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    #[tokio::test]
    async fn hijo_inexistente_colgado_roto_basura_o_de_otra_version() {
        let missing = NativeCommand {
            program: "larm-no-existe.exe".into(),
            args: vec![],
            timeout: Duration::from_secs(5),
        };
        assert_eq!(
            run_native(&missing, &[]).await.unwrap_err().reason,
            "native_unavailable"
        );
        if cfg!(windows) {
            let cases = [
                (
                    powershell("Start-Sleep -Seconds 60", 1500),
                    "native_timeout",
                ),
                (powershell("exit 3", 10_000), "native_failed"),
                (
                    powershell("Write-Output 'no es json'", 10_000),
                    "native_unparseable",
                ),
                (
                    powershell("Write-Output '{\"report_version\": 99}'", 10_000),
                    "native_unparseable",
                ),
            ];
            for (command, reason) in cases {
                assert_eq!(
                    run_native(&command, &[]).await.unwrap_err().reason,
                    reason,
                    "{:?}",
                    command.args
                );
            }
            // Un informe completo de otra versión se rechaza por versión.
            let mut report: serde_json::Value = serde_json::from_str(include_str!(
                "../../../tests/fixtures/hardware/native-pcia.json"
            ))
            .unwrap();
            report["report_version"] = 2.into();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("r.json");
            std::fs::write(&file, report.to_string()).unwrap();
            let cmd = powershell(&format!("Get-Content -Raw '{}'", file.display()), 10_000);
            assert_eq!(
                run_native(&cmd, &[]).await.unwrap_err().reason,
                "native_version"
            );
        }
    }

    #[tokio::test]
    async fn herramienta_ausente_o_colgada() {
        let none = run_tool(
            None,
            &[],
            Duration::from_secs(1),
            crate::tools::parse_clinfo,
        )
        .await;
        assert_eq!(none, ToolOutcome::NotInstalled);
        if cfg!(windows) {
            let hang = run_tool(
                Some(Path::new("powershell")),
                &["-NoProfile", "-Command", "Start-Sleep -Seconds 60"],
                Duration::from_millis(1500),
                crate::tools::parse_clinfo,
            )
            .await;
            assert!(
                matches!(hang, ToolOutcome::Failed(Failure { ref reason, .. }) if reason == "tool_timeout"),
                "{hang:?}"
            );
        }
    }

    #[test]
    fn busca_herramientas_en_orden() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("clinfo.exe"), b"x").unwrap();
        let dirs = [a.path().to_path_buf(), b.path().to_path_buf()];
        assert_eq!(
            find_tool("clinfo.exe", &dirs),
            Some(b.path().join("clinfo.exe"))
        );
        assert_eq!(find_tool("vulkaninfo.exe", &dirs), None);
    }
}
