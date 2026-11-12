//! Procesos externos del sondeo (CIM, `vulkaninfo`, `clinfo`, el hijo
//! nativo): siempre con timeout y muerte del proceso si vence, sin consola
//! interactiva y con el error normalizado.

use std::time::Duration;

/// Fallo normalizado de un proceso del sondeo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFailure {
    /// `unavailable` (no se pudo lanzar), `timeout`, `failed` (exit ≠ 0).
    pub kind: &'static str,
    pub detail: String,
}

/// Ejecuta `program args` y devuelve su stdout (UTF-8 con pérdidas).
///
/// # Errors
/// [`ProcessFailure`] si no arranca, vence el timeout o sale con error.
pub async fn run(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, ProcessFailure> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ProcessFailure {
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .trim()
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(ProcessFailure {
            kind: "failed",
            detail: format!("{program}: {}: {tail}", output.status),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
