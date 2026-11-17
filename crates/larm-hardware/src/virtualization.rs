//! P081: sondeo read-only de hosts de ejecución virtualizados (WSL2 y motor de
//! contenedores). Sólo observa comandos `--status`/`--list`/`--version`/`info`
//! declarados por cada herramienta; nunca instala distros, habilita features de
//! Windows ni cambia la configuración global de Docker. Cada subproceso corre
//! con timeout y muerte al vencer, entorno mínimo sin credenciales heredadas,
//! sin consola y con salida acotada. La ausencia de una herramienta es una
//! observación válida (`available=false`), no un error del sondeo.

use crate::process::ProcessFailure;
use std::time::Duration;

/// Tamaño máximo decodificado por comando: la salida de `wsl`/`docker` es
/// decenas de bytes; un exceso indica un entorno anómalo y se corta.
const OUTPUT_CAP_BYTES: usize = 1 << 20;

/// Distrito WSL observado por `wsl --list --verbose`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WslDistro {
    pub name: String,
    pub wsl_version: u8,
    pub default: bool,
}

/// Observación WSL completa: la ausencia del binario es un resultado válido.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct WslObservation {
    /// `wsl.exe` existe y `--status` terminó con éxito.
    pub available: bool,
    pub default_distro: Option<String>,
    pub default_wsl_version: Option<u8>,
    pub distros: Vec<WslDistro>,
    /// Fallos parciales observados (p. ej. `--list` tras un `--status` OK).
    pub errors: Vec<String>,
}

/// Observación del motor de contenedores: cliente presente no implica daemon
/// vivo; `daemon_running=None` cuando no pudo observarse.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ContainerObservation {
    pub client_available: bool,
    pub client_version: Option<String>,
    pub build: Option<String>,
    pub daemon_running: Option<bool>,
    pub daemon_version: Option<String>,
    pub errors: Vec<String>,
}

/// Junta el conjunto de observaciones del host.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct VirtualizationObservation {
    pub wsl: WslObservation,
    pub container: ContainerObservation,
}

/// Entorno mínimo del sondeo: sin credenciales heredadas.
fn probe_env() -> Vec<(String, String)> {
    [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok().map(|v| (name.to_string(), v)))
    .collect()
}

/// Ejecuta un comando del sondeo con entorno mínimo y decodificación de la
/// salida (wsl.exe puede emitir UTF-16LE según el tipo de tubería).
async fn probe_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, ProcessFailure> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in probe_env() {
        cmd.env(name, value);
    }
    let child = cmd.spawn().map_err(|e| ProcessFailure {
        kind: "unavailable",
        detail: format!("{program}: {e}"),
    })?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ProcessFailure {
            kind: "timeout",
            detail: format!("{program}: sin respuesta en {} ms", timeout.as_millis()),
        })?
        .map_err(|e| ProcessFailure {
            kind: "failed",
            detail: format!("{program}: {e}"),
        })?;
    if !output.status.success() {
        let stderr = decode_output(&output.stderr);
        let tail: String = stderr.trim().chars().rev().take(300).collect();
        return Err(ProcessFailure {
            kind: "failed",
            detail: format!("{program}: {}: {tail}", output.status),
        });
    }
    let mut bytes = output.stdout;
    bytes.truncate(OUTPUT_CAP_BYTES);
    Ok(decode_output(&bytes))
}

/// Decodifica la salida de un comando del sondeo: BOM UTF-16LE, heurística de
/// UTF-16LE (pares con byte alto nulo) y UTF-8 con pérdidas como último caso.
#[must_use]
pub fn decode_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        return String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        );
    }
    let odd_zero = bytes.len() > 4
        && bytes.iter().skip(1).step_by(2).all(|b| *b == 0)
        && bytes.iter().step_by(2).any(|b| *b != 0);
    if odd_zero {
        return String::from_utf16_lossy(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        );
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// `wsl --status` con salida en el locale del host: líneas `clave: valor`.
/// Tolerante al idioma: la versión por defecto es un valor entero 1|2 y el
/// nombre por defecto, el primer valor no entero y no vacío.
pub fn parse_wsl_status(text: &str) -> (Option<String>, Option<u8>) {
    let mut default_distro = None;
    let mut default_version = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if let Ok(number) = value.parse::<u8>() {
            if matches!(number, 1 | 2) && default_version.is_none() {
                default_version = Some(number);
            }
        } else if default_distro.is_none()
            && key.trim().chars().all(|c| c.is_alphabetic() || c == ' ')
        {
            default_distro = Some(value.to_string());
        }
    }
    (default_distro, default_version)
}

/// `wsl --list --verbose`: columnas NAME/STATE/VERSION con el asterisco del
/// distrito por defecto; se ignora la cabecera y las líneas no conformes.
pub fn parse_wsl_list(text: &str) -> Vec<WslDistro> {
    let mut distros = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (default, rest) = if let Some(rest) = line.strip_prefix('*') {
            (true, rest.trim())
        } else {
            (false, line)
        };
        let mut tokens = rest.split_whitespace();
        let Some(name) = tokens.next() else {
            continue;
        };
        let Some(state) = tokens.next() else {
            continue;
        };
        let Some(version) = tokens.next().and_then(|v| v.parse::<u8>().ok()) else {
            continue;
        };
        // La cabecera NAME/STATE/VERSION no tiene versión numérica; un estado
        // reconocible descarta filas ambiguas sin bloquear distros válidos.
        if state.eq_ignore_ascii_case("name") {
            continue;
        }
        distros.push(WslDistro {
            name: name.to_string(),
            wsl_version: version,
            default,
        });
    }
    distros
}

/// `docker --version`: `Docker version X.Y.Z, build abcdef0`.
#[must_use]
pub fn parse_docker_version(text: &str) -> Option<(String, Option<String>)> {
    let text = text.trim();
    let rest = text.strip_prefix("Docker version")?;
    let rest = rest.trim();
    let mut parts = rest.split(", ");
    let version = parts.next()?.trim().to_string();
    if version.is_empty() || !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let build = parts
        .next()
        .and_then(|piece| piece.trim().strip_prefix("build "))
        .map(|build| build.trim().to_string())
        .filter(|build| !build.is_empty());
    Some((version, build))
}

/// Sondeo WSL read-only: `--status` determina la presencia; `--list --verbose`
/// aporta los distros. La ausencia del binario deja `available=false`.
pub async fn probe_wsl(program: &str, timeout: Duration) -> WslObservation {
    let mut observation = WslObservation::default();
    match probe_command(program, &["--status"], timeout).await {
        Ok(text) => {
            observation.available = true;
            let (distro, version) = parse_wsl_status(&text);
            observation.default_distro = distro;
            observation.default_wsl_version = version;
        }
        Err(failure) if failure.kind == "unavailable" => return observation,
        Err(failure) => observation
            .errors
            .push(format!("--status: {}", failure.detail)),
    }
    match probe_command(program, &["--list", "--verbose"], timeout).await {
        Ok(text) => {
            observation.available = true;
            observation.distros = parse_wsl_list(&text);
        }
        Err(failure) => observation
            .errors
            .push(format!("--list: {}", failure.detail)),
    }
    observation
}

/// Sondeo del motor de contenedores read-only: `--version` (cliente) y
/// `info --format` (daemon). El fallo del daemon deja `daemon_running=false`
/// sólo si el cliente está presente; el cliente ausente es observación vacía.
pub async fn probe_container(program: &str, timeout: Duration) -> ContainerObservation {
    let mut observation = ContainerObservation::default();
    let version = match probe_command(program, &["--version"], timeout).await {
        Ok(text) => {
            let parsed = parse_docker_version(&text);
            observation.client_available = parsed.is_some();
            match parsed {
                Some((version, build)) => {
                    observation.client_version = Some(version);
                    observation.build = build;
                    Some(text)
                }
                None => {
                    observation
                        .errors
                        .push("--version: salida no reconocida".to_string());
                    None
                }
            }
        }
        Err(failure) if failure.kind == "unavailable" => return observation,
        Err(failure) => {
            observation
                .errors
                .push(format!("--version: {}", failure.detail));
            None
        }
    };
    if version.is_some() {
        match probe_command(
            program,
            &["info", "--format", "{{.ServerVersion}}"],
            timeout,
        )
        .await
        {
            Ok(text) => {
                let version = text.trim().to_string();
                if version.is_empty() {
                    observation.daemon_running = Some(false);
                } else {
                    observation.daemon_running = Some(true);
                    observation.daemon_version = Some(version);
                }
            }
            Err(failure) => {
                observation.daemon_running = Some(false);
                observation.errors.push(format!("info: {}", failure.detail));
            }
        }
    }
    observation
}

/// Sondeo completo del host: WSL y contenedores en secuencia read-only.
pub async fn probe(
    wsl_program: &str,
    container_program: &str,
    timeout: Duration,
) -> VirtualizationObservation {
    VirtualizationObservation {
        wsl: probe_wsl(wsl_program, timeout).await,
        container: probe_container(container_program, timeout).await,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // los fixtures de test son literales fijos
mod tests {
    use super::*;

    /// Captura real de este equipo (2026-11-14, locale español).
    const WSL_STATUS_ES: &str =
        "Distribución predeterminada: Ubuntu\r\nVersión predeterminada: 2\r\n";
    const WSL_LIST_ES: &str =
        "  NAME              STATE           VERSION\r\n* Ubuntu            Stopped         2\r\n  docker-desktop    Stopped         2\r\n";
    const DOCKER_VERSION: &str = "Docker version 29.7.2, build a7dcaa6\n";

    #[test]
    fn decodifica_utf8_y_utf16le() {
        assert_eq!(decode_output(WSL_STATUS_ES.as_bytes()), WSL_STATUS_ES);
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in WSL_STATUS_ES.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_output(&utf16), WSL_STATUS_ES);
        // Heurística sin BOM: UTF-16LE con byte alto nulo en cada par.
        let bare: Vec<u8> = WSL_STATUS_ES
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(decode_output(&bare), WSL_STATUS_ES);
        assert_eq!(decode_output(b""), "");
    }

    #[test]
    fn parsea_wsl_status_en_espanol_y_ingles() {
        let (distro, version) = parse_wsl_status(WSL_STATUS_ES);
        assert_eq!(distro.as_deref(), Some("Ubuntu"));
        assert_eq!(version, Some(2));
        let en = "Default Distribution: Debian\r\nDefault Version: 1\r\n";
        assert_eq!(parse_wsl_status(en), (Some("Debian".into()), Some(1)));
        assert_eq!(parse_wsl_status(""), (None, None));
    }

    #[test]
    fn parsea_wsl_list_con_distrito_por_defecto() {
        let distros = parse_wsl_list(WSL_LIST_ES);
        assert_eq!(
            distros,
            vec![
                WslDistro {
                    name: "Ubuntu".into(),
                    wsl_version: 2,
                    default: true
                },
                WslDistro {
                    name: "docker-desktop".into(),
                    wsl_version: 2,
                    default: false
                },
            ]
        );
        assert!(parse_wsl_list("").is_empty());
        assert!(parse_wsl_list("  NAME              STATE           VERSION\r\n").is_empty());
    }

    #[test]
    fn parsea_docker_version() {
        let (version, build) = parse_docker_version(DOCKER_VERSION).expect("versión reconocible");
        assert_eq!(version, "29.7.2");
        assert_eq!(build.as_deref(), Some("a7dcaa6"));
        assert_eq!(parse_docker_version("command not found"), None);
        assert_eq!(parse_docker_version("Docker version"), None);
    }

    #[tokio::test]
    async fn ausencia_de_binarios_es_observacion_valida() {
        let wsl = probe_wsl("larm-inexistente-wsl.exe", Duration::from_secs(5)).await;
        assert!(!wsl.available);
        assert!(wsl.distros.is_empty());
        let container =
            probe_container("larm-inexistente-docker.exe", Duration::from_secs(5)).await;
        assert!(!container.client_available);
        assert_eq!(container.daemon_running, None);
    }
}
