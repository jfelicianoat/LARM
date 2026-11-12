//! Consultas CIM/WMI con timeout. Sólo se usan para datos que Windows no
//! expone por una API segura disponible (la RAM instalada en módulos). Un
//! WMI colgado o roto deja el dato desconocido; nunca bloquea el sondeo.

use std::time::Duration;

use async_trait::async_trait;

/// Motivo normalizado de un fallo CIM (va a `metadata.unknown[].reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CimFailure {
    /// `wmi_unavailable`, `wmi_timeout`, `wmi_failed` o `wmi_unparseable`.
    pub reason: &'static str,
    pub detail: String,
}

impl CimFailure {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// Ejecuta un script CIM que imprime un único valor.
#[async_trait]
pub trait CimQuery: Send + Sync {
    async fn scalar(&self, script: &str) -> Result<String, CimFailure>;
    /// Identificador de la fuente para la evidencia.
    fn source(&self) -> String;
}

/// `Get-CimInstance` mediante Windows PowerShell, con timeout y muerte del
/// proceso si vence.
#[derive(Debug, Clone)]
pub struct PowerShellCim {
    pub program: String,
    pub timeout: Duration,
}

impl Default for PowerShellCim {
    fn default() -> Self {
        Self {
            program: "powershell".into(),
            timeout: Duration::from_secs(10),
        }
    }
}

#[async_trait]
impl CimQuery for PowerShellCim {
    async fn scalar(&self, script: &str) -> Result<String, CimFailure> {
        let args = ["-NoProfile", "-NonInteractive", "-Command", script].map(String::from);
        crate::process::run(&self.program, &args, self.timeout)
            .await
            .map(|out| out.trim().to_string())
            .map_err(|f| match f.kind {
                "unavailable" => CimFailure::new("wmi_unavailable", f.detail),
                "timeout" => CimFailure::new("wmi_timeout", f.detail),
                _ => CimFailure::new("wmi_failed", f.detail),
            })
    }

    fn source(&self) -> String {
        format!("{}:Get-CimInstance", self.program)
    }
}

/// Script que suma `Win32_PhysicalMemory.Capacity` (bytes instalados).
pub const INSTALLED_RAM_SCRIPT: &str =
    "(Get-CimInstance -ClassName Win32_PhysicalMemory -ErrorAction Stop | Measure-Object -Property Capacity -Sum).Sum";

/// Interpreta la salida de [`INSTALLED_RAM_SCRIPT`]. Cero o vacío no es una
/// cantidad de RAM: es un WMI que no informa módulos.
///
/// # Errors
/// `wmi_unparseable` si no es un entero positivo.
pub fn parse_installed_ram(text: &str) -> Result<u64, CimFailure> {
    let value = text.trim();
    match value.parse::<u64>() {
        Ok(0) => Err(CimFailure::new(
            "wmi_unparseable",
            "Win32_PhysicalMemory no informa módulos",
        )),
        Ok(n) => Ok(n),
        Err(_) => Err(CimFailure::new(
            "wmi_unparseable",
            format!("salida no numérica: {value:?}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn interpreta_la_suma_de_capacidades() {
        assert_eq!(
            parse_installed_ram("137438953472\r\n").unwrap(),
            137_438_953_472
        );
        for bad in ["", "0", "-1", "abc", "1.5E+11", "99999999999999999999"] {
            assert_eq!(
                parse_installed_ram(bad).unwrap_err().reason,
                "wmi_unparseable",
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn programa_inexistente_es_wmi_unavailable() {
        let cim = PowerShellCim {
            program: "larm-no-existe-powershell.exe".into(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(cim.scalar("1").await.unwrap_err().reason, "wmi_unavailable");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn consulta_colgada_vence_y_no_bloquea() {
        let cim = PowerShellCim {
            program: "powershell".into(),
            timeout: Duration::from_millis(1500),
        };
        let started = std::time::Instant::now();
        let err = cim.scalar("Start-Sleep -Seconds 60; 1").await.unwrap_err();
        assert_eq!(err.reason, "wmi_timeout");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn clase_inexistente_es_wmi_failed() {
        let cim = PowerShellCim::default();
        let err = cim
            .scalar("Get-CimInstance -ClassName Win32_LarmNoExiste -ErrorAction Stop")
            .await
            .unwrap_err();
        assert_eq!(err.reason, "wmi_failed");
    }
}
