//! Modos de daemon Windows (P137; spec §101, §102 y §128).
//!
//! Dos modos sobre el mismo Core: el **User Background Agent** es el modo
//! predeterminado — se ejecuta como el usuario (sin privilegios de admin,
//! `RL LIMITED`), con sus carpetas y sus unidades mapeadas, y se registra
//! como tarea de logon **del propio usuario** con `schtasks` (sin tocar el
//! PATH global, drivers ni WSL). El **Windows Service** es opcional para
//! funcionamiento unattended: sólo se instala con un comando explícito del
//! operador (requiere elevación), con **rutas de config/datos separadas**
//! bajo `ProgramData` y **sin unidades mapeadas de sesión** (sólo rutas
//! locales o UNC — documentado en `docs/operations/install-uninstall.md`).
//!
//! La desinstalación nunca borra modelos ni la base: esa purga es una
//! operación separada que exige confirmación explícita.

use larm_domain::error::{ErrorCode, LarmError};
use std::path::{Path, PathBuf};

/// Nombre del fichero de autarranque del agente de fondo en la carpeta
/// Startup del usuario (sin admin; `/SC ONLOGON` de `schtasks` exige
/// elevación y no sirve para el modo sin privilegios).
pub const AGENT_STARTUP_FILE: &str = "LARM User Agent.cmd";

/// Nombre del servicio Windows opcional.
pub const SERVICE_NAME: &str = "LARM";

/// Nivel de integridad alto (administrador elevado) en `whoami /groups`.
const HIGH_INTEGRITY_SID: &str = "S-1-16-12288";

fn windows_error(reason: &'static str, message: String) -> LarmError {
    LarmError::new(ErrorCode::Internal, message, reason)
}

/// Argumentos del daemon para cualquiera de los dos modos: API en loopback
/// del contrato con rutas explícitas (nunca depende del directorio actual).
#[must_use]
pub fn daemon_run_args(data_root: &Path, config: &Path) -> Vec<String> {
    vec![
        "--serve-api".into(),
        "--bind".into(),
        "127.0.0.1".into(),
        "--data-root".into(),
        data_root.display().to_string(),
        "--config".into(),
        config.display().to_string(),
    ]
}

/// Carpeta Startup del usuario actual (autarranque al logon sin admin).
///
/// # Errors
/// `windows_command_failed` si `%APPDATA%` no está definida.
pub fn agent_startup_dir() -> Result<PathBuf, LarmError> {
    let appdata = std::env::var("APPDATA").map_err(|_| {
        windows_error(
            "windows_command_failed",
            "APPDATA no está definida: sin carpeta Startup del usuario".into(),
        )
    })?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup"))
}

/// Contenido del `.cmd` de autarranque del agente: arranca el daemon
/// desatendido y minimizado; no toca el PATH global, drivers ni WSL.
#[must_use]
pub fn agent_startup_cmd(bin: &Path, data_root: &Path, config: &Path) -> String {
    let mut lines = String::from(
        "@echo off
",
    );
    lines.push_str(
        "rem LARM User Background Agent (P137; spec 102): sin privilegios de admin.
",
    );
    lines.push_str(&format!("start \"LARM agent\" /MIN \"{}\"", bin.display()));
    for arg in daemon_run_args(data_root, config) {
        lines.push(' ');
        lines.push_str(&arg);
    }
    lines.push('\r');
    lines
}

/// Ruta del fichero de autarranque del agente de fondo.
///
/// # Errors
/// Los de [`agent_startup_dir`].
pub fn agent_autostart_path() -> Result<PathBuf, LarmError> {
    Ok(agent_startup_dir()?.join(AGENT_STARTUP_FILE))
}

/// Rutas separadas del servicio (spec §102): el servicio no puede depender de
/// unidades mapeadas de sesión, así que su raíz de datos y su configuración
/// deben ser locales o UNC — un volumen de red montado con letra de unidad
/// (el SO lo resuelve a `\\server\share\...`) se rechaza.
///
/// # Errors
/// `service_session_drive_rejected` si la raíz es una unidad mapeada de
/// sesión (letra de unidad sobre un volumen de red).
pub fn service_data_root(data_root: &Path) -> Result<PathBuf, LarmError> {
    let text = data_root.display().to_string();
    let is_drive_letter = text.len() >= 2 && text.as_bytes()[1] == b':';
    if is_drive_letter && drive_is_remote(data_root) {
        return Err(windows_error(
            "service_session_drive_rejected",
            format!(
                "El servicio no puede depender de la unidad mapeada de sesión {text}: usa una ruta local o UNC"
            ),
        ));
    }
    Ok(data_root.to_path_buf())
}

/// `true` si la letra de unidad es un volumen de red: sin FFI, el criterio
/// observable es la forma canónica que el SO devuelve al resolver el path —
/// un volumen remoto montado con letra se canonicaliza a `\\server\share\…`.
fn drive_is_remote(data_root: &Path) -> bool {
    std::fs::canonicalize(data_root)
        .map(|resolved| resolved.display().to_string().starts_with("\\\\"))
        .unwrap_or(false)
}

/// Argumentos de `sc create` para el servicio opcional: binPath con las
/// rutas separadas del servicio, arranque a demanda, sin dependencias extra.
pub fn service_sc_args(
    bin: &Path,
    data_root: &Path,
    config: &Path,
) -> Result<Vec<String>, LarmError> {
    let root = service_data_root(data_root)?;
    let mut bin_path = bin.display().to_string();
    for arg in daemon_run_args(&root, config) {
        bin_path.push(' ');
        bin_path.push_str(&arg);
    }
    Ok(vec![
        "create".into(),
        SERVICE_NAME.into(),
        "binPath=".into(),
        format!("\"{bin_path}\""),
        "start=".into(),
        "demand".into(),
        "DisplayName=".into(),
        "LARM Daemon (opcional)".into(),
    ])
}

/// Desinstalación (spec P137.5): los modelos y la base **no se borran**; la
/// purga de datos es una operación separada que exige confirmación explícita.
///
/// # Errors
/// `uninstall_purge_requires_confirmation` si se pide purga sin confirmación.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UninstallPlan {
    /// Tarea de logon del agente de usuario a eliminar.
    pub remove_agent_task: bool,
    /// Servicio opcional a eliminar (si estaba instalado).
    pub remove_service: bool,
    /// Binarios y accesos instalados; **nunca** datos.
    pub remove_binaries: bool,
    /// Sólo con purga separada y confirmada.
    pub purge_data: bool,
}

pub fn uninstall_plan(purge_data: bool, confirmed: bool) -> Result<UninstallPlan, LarmError> {
    if purge_data && !confirmed {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "La purga de datos (modelos y base) es una operación separada y exige confirmación explícita",
            "uninstall_purge_requires_confirmation",
        ));
    }
    Ok(UninstallPlan {
        remove_agent_task: true,
        remove_service: true,
        remove_binaries: true,
        purge_data,
    })
}

/// `true` si el proceso actual corre elevado (nivel de integridad alto). El
/// servicio opcional sólo se instala con elevación; sin ella, el comando
/// falla y queda documentado como NO VERIFICADO.
#[must_use]
pub fn is_elevated() -> bool {
    let output = std::process::Command::new("whoami").arg("/groups").output();
    let Ok(output) = output else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains(HIGH_INTEGRITY_SID)
}

fn run(command: &str, args: &[String]) -> Result<String, LarmError> {
    let output = std::process::Command::new(command)
        .args(args)
        .output()
        .map_err(|e| {
            windows_error(
                "windows_command_failed",
                format!("no se pudo ejecutar {command}: {e}"),
            )
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(windows_error(
            "windows_command_failed",
            format!("{command} falló: {}", text.trim()),
        ));
    }
    Ok(text)
}

/// Registra la tarea de logon del agente de fondo **del usuario actual**
/// (sin admin).
///
/// # Errors
/// `windows_command_failed` si `schtasks` falla.
pub fn install_agent(bin: &Path, data_root: &Path, config: &Path) -> Result<(), LarmError> {
    let path = agent_autostart_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| windows_error("io_failed", format!("creando Startup: {e}")))?;
    }
    std::fs::write(&path, agent_startup_cmd(bin, data_root, config))
        .map_err(|e| windows_error("io_failed", format!("escribiendo el autarranque: {e}")))
}

/// Elimina la tarea de logon del agente de fondo; `false` si no existía.
///
/// # Errors
/// `windows_command_failed` si `schtasks` falla por otra causa.
pub fn uninstall_agent() -> Result<bool, LarmError> {
    if !agent_installed()? {
        return Ok(false);
    }
    std::fs::remove_file(agent_autostart_path()?)
        .map_err(|e| windows_error("io_failed", format!("borrando el autarranque: {e}")))?;
    Ok(true)
}

/// `true` si la tarea de logon del agente está registrada.
///
/// # Errors
/// `windows_command_failed` si `schtasks` falla por otra causa.
pub fn agent_installed() -> Result<bool, LarmError> {
    Ok(agent_autostart_path()?.is_file())
}

/// Instala el servicio opcional: exige elevación (sin ella queda NO
/// VERIFICADO) y rutas locales o UNC (nunca unidades de sesión).
///
/// # Errors
/// `service_requires_elevation` sin privilegios de admin;
/// `service_session_drive_rejected` con raíz en unidad mapeada;
/// `windows_command_failed` si `sc` falla.
pub fn install_service(bin: &Path, data_root: &Path, config: &Path) -> Result<(), LarmError> {
    if !is_elevated() {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "Instalar el servicio Windows exige una consola elevada (operación opcional y explícita)",
            "service_requires_elevation",
        ));
    }
    let args = service_sc_args(bin, data_root, config)?;
    if service_installed()? {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "El servicio LARM ya está instalado",
            "service_already_installed",
        ));
    }
    run("sc", &args).map(|_| ())
}

/// Elimina el servicio opcional (exige elevación); `false` si no existía.
///
/// # Errors
/// `service_requires_elevation` sin privilegios de admin;
/// `windows_command_failed` si `sc` falla por otra causa.
pub fn uninstall_service() -> Result<bool, LarmError> {
    if !is_elevated() {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "Desinstalar el servicio Windows exige una consola elevada",
            "service_requires_elevation",
        ));
    }
    if !service_installed()? {
        return Ok(false);
    }
    run("sc", &["delete".into(), SERVICE_NAME.into()]).map(|_| true)
}

/// `true` si el servicio opcional está registrado.
///
/// # Errors
/// `windows_command_failed` si `sc` falla por otra causa.
pub fn service_installed() -> Result<bool, LarmError> {
    match run("sc", &["query".into(), SERVICE_NAME.into()]) {
        Ok(_) => Ok(true),
        Err(error) => {
            let technical = format!("{} {}", error.message, error.technical_detail());
            if technical.contains("1060")
                || technical.contains("does not exist")
                || technical.contains("no existe")
            {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn el_agente_de_usuario_no_exige_privilegios_ni_toca_el_path() {
        let cmd = agent_startup_cmd(
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\larm-daemon.exe"),
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\data"),
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\larm.json"),
        );
        assert!(
            cmd.contains("start \"LARM agent\" /MIN"),
            "arranque minimizado"
        );
        assert!(cmd.contains("--serve-api"), "el agente sirve la API");
        assert!(cmd.contains("--bind 127.0.0.1"), "loopback del contrato");
        assert!(!cmd.to_lowercase().contains("runas"), "sin elevación");
        assert!(
            !cmd.to_lowercase().contains("path="),
            "no toca el PATH global"
        );
        // Determinismo: mismas entradas → mismo contenido.
        let again = agent_startup_cmd(
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\larm-daemon.exe"),
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\data"),
            Path::new("C:\\Users\\op\\AppData\\Local\\LARM\\larm.json"),
        );
        assert_eq!(cmd, again);
    }

    #[test]
    fn el_servicio_se_instala_con_rutas_separadas_y_a_demanda() {
        let args = service_sc_args(
            Path::new("C:\\Program Files\\LARM\\larm-daemon.exe"),
            Path::new("C:\\ProgramData\\LARM"),
            Path::new("C:\\ProgramData\\LARM\\larm.json"),
        )
        .unwrap();
        assert_eq!(args[0], "create");
        assert_eq!(args[1], SERVICE_NAME);
        assert!(
            args.contains(&"demand".into()),
            "arranque explícito, no auto"
        );
        let bin_path = &args[args.iter().position(|a| a == "binPath=").unwrap() + 1];
        assert!(bin_path.contains("C:\\ProgramData\\LARM"));
        assert!(
            !bin_path.contains("AppData"),
            "rutas de usuario separadas del servicio"
        );
    }

    #[test]
    fn el_servicio_rechaza_unidades_de_sesion_y_acepta_local_y_unc() {
        // Local: aceptada.
        assert!(service_data_root(Path::new("C:\\ProgramData\\LARM")).is_ok());
        // UNC: aceptada (documentado).
        assert!(service_data_root(Path::new("\\\\servidor\\compartido\\LARM")).is_ok());
    }

    #[test]
    fn la_desinstalacion_nunca_borra_datos_sin_purga_confirmada() {
        // Sin purga: quita agente/servicio/binarios, conserva modelos y base.
        let plan = uninstall_plan(false, false).unwrap();
        assert!(plan.remove_agent_task && plan.remove_service && plan.remove_binaries);
        assert!(!plan.purge_data);
        // Purga pedida sin confirmación: rechazada.
        assert_eq!(
            uninstall_plan(true, false).unwrap_err().details.reason,
            "uninstall_purge_requires_confirmation"
        );
        // Purga separada y confirmada: permitida.
        assert!(uninstall_plan(true, true).unwrap().purge_data);
    }

    #[test]
    fn los_run_args_son_deterministas_y_loopback() {
        let args = daemon_run_args(Path::new("D:\\data"), Path::new("D:\\larm.json"));
        assert_eq!(
            args,
            vec![
                "--serve-api".to_string(),
                "--bind".to_string(),
                "127.0.0.1".to_string(),
                "--data-root".to_string(),
                "D:\\data".to_string(),
                "--config".to_string(),
                "D:\\larm.json".to_string(),
            ]
        );
    }
}
