//! P081: abstracción de hosts de ejecución (spec §5.1). Todo el módulo es
//! puro y read-only respecto al host: mapeo explícito de rutas
//! Windows↔WSL/contenedor, política de puertos sólo loopback y validación de
//! planes que rechaza cualquier comando que mute el entorno virtualizado del
//! operador (instalar distros, habilitar features de Windows o alterar el
//! estado global del motor de contenedores sin autorización explícita).

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::runtime::{ExecutionHostKind, InstallationStrategy};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Solicitud de host de ejecución no válida",
        reason,
    )
}

fn denied(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Forbidden,
        "El plan de ejecución muta el entorno del host",
        reason,
    )
}

/// Segmento de ruta prohibido: subida de directorio o separador mezclado.
fn has_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| segment == "..")
}

/// Mapeo explícito de rutas para una distro WSL concreta. El lado Windows usa
/// el prefijo `\\wsl.localhost\<distro>\` (y acepta el legado `\\wsl$\` al
/// leer); el lado WSL es la ruta absoluta POSIX del mismo fichero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslPathMapper {
    distro: String,
}

impl WslPathMapper {
    /// El nombre de distro es el observado por `wsl --list --verbose`: no
    /// vacío, sin separadores ni caracteres de control.
    pub fn new(distro: &str) -> Result<Self, LarmError> {
        let distro = distro.trim();
        if distro.is_empty()
            || distro.len() > 64
            || distro
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            || distro.starts_with('.')
        {
            return Err(invalid("execution_host_distro_invalid"));
        }
        Ok(Self {
            distro: distro.to_string(),
        })
    }

    #[must_use]
    pub fn distro(&self) -> &str {
        &self.distro
    }

    /// `\\wsl.localhost\<distro>\home\u\x` → `/home/u/x` (acepta `\\wsl$`).
    pub fn windows_to_wsl(&self, windows: &str) -> Result<String, LarmError> {
        let normalized = windows.replace('/', "\\");
        for prefix in [
            format!("\\\\wsl.localhost\\{}", self.distro),
            format!("\\\\wsl$\\{}", self.distro),
        ] {
            if let Some(rest) = normalized
                .to_ascii_lowercase()
                .strip_prefix(&prefix.to_ascii_lowercase())
            {
                let rest = rest.replace('\\', "/");
                if rest.is_empty() {
                    return Err(invalid("execution_host_path_root"));
                }
                if !rest.starts_with('/') || has_traversal(&rest) {
                    return Err(invalid("execution_host_path_invalid"));
                }
                return Ok(rest);
            }
        }
        Err(invalid("execution_host_path_outside_distro"))
    }

    /// `/home/u/x` → `\\wsl.localhost\<distro>\home\u\x`.
    pub fn wsl_to_windows(&self, wsl: &str) -> Result<String, LarmError> {
        let wsl = wsl.trim();
        if !wsl.starts_with('/') || has_traversal(wsl) || wsl.contains('\\') {
            return Err(invalid("execution_host_path_invalid"));
        }
        Ok(format!(
            "\\\\wsl.localhost\\{}{}",
            self.distro,
            wsl.replace('/', "\\")
        ))
    }
}

/// Montaje explícito de un directorio del host Windows en un contenedor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPathMapping {
    pub host: String,
    pub container: String,
}

impl ContainerPathMapping {
    /// Ambas rutas absolutas, sin subida de directorio y sin `\0`.
    pub fn new(host: &str, container: &str) -> Result<Self, LarmError> {
        for path in [host, container] {
            if path.contains('\0') || has_traversal(path) {
                return Err(invalid("execution_host_path_invalid"));
            }
        }
        let host_ok = host.len() >= 2
            && host.as_bytes()[1] == b':'
            && host.as_bytes()[0].is_ascii_alphabetic();
        if !host_ok || !container.starts_with('/') {
            return Err(invalid("execution_host_path_invalid"));
        }
        Ok(Self {
            host: host.to_string(),
            container: container.to_string(),
        })
    }
}

/// Exposición de un puerto del huésped en el host Windows. La política de
/// seguridad sólo permite binds en loopback: ningún servicio de runtime se
/// publica en la LAN sin una decisión explícita fuera de esta abstracción.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub guest_port: u16,
    pub loopback_only: bool,
}

impl PortMapping {
    pub fn new(host_port: u16, guest_port: u16, loopback_only: bool) -> Result<Self, LarmError> {
        if !loopback_only {
            return Err(denied("execution_host_port_must_bind_loopback"));
        }
        if host_port == 0 || guest_port == 0 {
            return Err(invalid("execution_host_port_ephemeral"));
        }
        Ok(Self {
            host_port,
            guest_port,
            loopback_only: true,
        })
    }
}

/// Los verbos de `wsl.exe` que cambian el estado global del subsistema o
/// instalan distros: el sondeo y los planes de ejecución nunca los emiten.
const WSL_MUTATING_VERBS: [&str; 7] = [
    "--install",
    "--uninstall",
    "--update",
    "--set-version",
    "--set-default",
    "--set-default-version",
    "--manage",
];

/// Los verbos de `docker` que alteran el estado del motor o ejecutan con
/// privilegios/red del host.
const DOCKER_MUTATING_VERBS: [&str; 7] =
    ["pull", "push", "build", "save", "load", "import", "commit"];

/// Los programas que habilitan features de Windows o cambian el sistema.
const WINDOWS_MUTATING_PROGRAMS: [&str; 3] = ["dism.exe", "wusa.exe", "netsh.exe"];

fn program_name(program: &str) -> String {
    program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase()
}

/// Valida un comando que el host de ejecución podría emitir. La detección
/// read-only (estado/lista/versión/info) pasa; cualquier mutación del
/// subsistema WSL, del motor de contenedores o del sistema Windows se
/// rechaza con `execution_host_plan_mutates_host`.
pub fn validate_execution_command(program: &str, args: &[&str]) -> Result<(), LarmError> {
    let name = program_name(program);
    if WINDOWS_MUTATING_PROGRAMS.contains(&name.as_str()) {
        return Err(denied("execution_host_plan_mutates_host"));
    }
    if name == "powershell.exe" || name == "pwsh.exe" {
        let lower = args
            .iter()
            .map(|a| a.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if lower.iter().any(|a| {
            a.contains("enable-windowsoptionalfeature")
                || a.contains("add-windowscapability")
                || a.contains("wsl --install")
        }) {
            return Err(denied("execution_host_plan_mutates_host"));
        }
        return Ok(());
    }
    if name == "wsl.exe" {
        if args
            .iter()
            .any(|a| WSL_MUTATING_VERBS.contains(&a.to_ascii_lowercase().as_str()))
        {
            return Err(denied("execution_host_plan_mutates_host"));
        }
        return Ok(());
    }
    if name == "docker.exe" || name == "docker" {
        let mut scan = args.iter().map(|a| a.to_ascii_lowercase());
        while let Some(arg) = scan.next() {
            if DOCKER_MUTATING_VERBS.contains(&arg.as_str()) {
                return Err(denied("execution_host_plan_mutates_host"));
            }
            if arg == "run" {
                let rest: Vec<String> = scan.by_ref().collect();
                if rest.iter().any(|a| {
                    a == "--privileged"
                        || a.starts_with("--network=host")
                        || a == "--network" && rest.iter().any(|n| n == "host")
                }) {
                    return Err(denied("execution_host_plan_mutates_host"));
                }
                break;
            }
        }
        return Ok(());
    }
    Ok(())
}

/// Comprueba que la combinación instalación↔host de ejecución es coherente:
/// la estrategia WSL exige un host WSL2 y la CONTAINER un host CONTAINER;
/// las estrategias nativas no pueden colgar de un host virtualizado.
pub fn validate_installation_host(
    strategy: InstallationStrategy,
    host: Option<ExecutionHostKind>,
) -> Result<(), LarmError> {
    match (strategy, host) {
        (_, None)
            if matches!(
                strategy,
                InstallationStrategy::Wsl | InstallationStrategy::Container
            ) =>
        {
            Err(invalid("execution_host_reference_missing"))
        }
        (_, Some(kind)) => {
            if kind.allows_strategy(strategy) {
                Ok(())
            } else {
                Err(invalid("execution_host_strategy_mismatch"))
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // los test escriben expectativas literales
mod tests {
    use super::*;

    #[test]
    fn mapeo_wsl_ida_y_vuelta() {
        let mapper = WslPathMapper::new("Ubuntu").unwrap();
        let wsl = mapper
            .windows_to_wsl(r"\\wsl.localhost\Ubuntu\home\op\modelo.gguf")
            .unwrap();
        assert_eq!(wsl, "/home/op/modelo.gguf");
        let back = mapper.wsl_to_windows(&wsl).unwrap();
        assert_eq!(back, r"\\wsl.localhost\Ubuntu\home\op\modelo.gguf");
        // Prefijo legado \\wsl$ y mayúsculas del distro.
        let legacy = mapper
            .windows_to_wsl(r"\\WSL$\UBUNTU\home\op\modelo.gguf")
            .unwrap();
        assert_eq!(legacy, "/home/op/modelo.gguf");
    }

    #[test]
    fn mapeo_wsl_rechaza_fueras_del_distro_y_traversal() {
        let mapper = WslPathMapper::new("Ubuntu").unwrap();
        assert!(mapper.windows_to_wsl(r"C:\home\op\x").is_err());
        assert!(mapper
            .windows_to_wsl(r"\\wsl.localhost\Debian\home\x")
            .is_err());
        assert!(mapper.windows_to_wsl(r"\\wsl.localhost\Ubuntu").is_err());
        assert!(mapper
            .windows_to_wsl(r"\\wsl.localhost\Ubuntu\..\host\etc")
            .is_err());
        assert!(mapper.wsl_to_windows("/etc/../secret").is_err());
        assert!(mapper.wsl_to_windows("home/rel").is_err());
        assert!(WslPathMapper::new("../Debian").is_err());
        assert!(WslPathMapper::new("Deb/ian").is_err());
        assert!(WslPathMapper::new("").is_err());
    }

    #[test]
    fn mapeo_contenedor_valida_absolutas() {
        let mapping = ContainerPathMapping::new(r"C:\modelos\gguf", "/models/gguf").unwrap();
        assert_eq!(mapping.host, r"C:\modelos\gguf");
        assert_eq!(mapping.container, "/models/gguf");
        assert!(ContainerPathMapping::new("rel/ativo", "/models").is_err());
        assert!(ContainerPathMapping::new(r"C:\x", "../etc").is_err());
        assert!(ContainerPathMapping::new(r"C:\x\..\etc", "/models").is_err());
    }

    #[test]
    fn politica_de_puertos_solo_loopback() {
        let mapping = PortMapping::new(8080, 8080, true).unwrap();
        assert!(mapping.loopback_only);
        assert!(PortMapping::new(8080, 8080, false)
            .unwrap_err()
            .details
            .reason
            .contains("loopback"));
        assert!(PortMapping::new(0, 8080, true).is_err());
        assert!(PortMapping::new(8080, 0, true).is_err());
    }

    #[test]
    fn comandos_de_sondeo_pasan_y_mutaciones_se_rechazan() {
        // Lecturas del sondeo: permitidas.
        validate_execution_command("wsl.exe", &["--status"]).unwrap();
        validate_execution_command("wsl.exe", &["--list", "--verbose"]).unwrap();
        validate_execution_command("docker.exe", &["--version"]).unwrap();
        validate_execution_command("docker.exe", &["info", "--format", "{{.ServerVersion}}"])
            .unwrap();
        validate_execution_command("docker.exe", &["run", "--rm", "imagen"]).unwrap();
        // Mutaciones del subsistema/motor/sistema: rechazadas.
        for (program, args) in [
            ("wsl.exe", vec!["--install", "-d", "Ubuntu"]),
            ("wsl.exe", vec!["--set-default", "Ubuntu"]),
            ("wsl.exe", vec!["--update"]),
            ("dism.exe", vec!["/Online", "/Enable-Feature", "/FeatureName:VirtualMachinePlatform"]),
            ("wusa.exe", vec!["/quiet"]),
            ("powershell.exe", vec!["-Command", "Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Windows-Subsystem-Linux"]),
            ("docker.exe", vec!["pull", "ubuntu:24.04"]),
            ("docker.exe", vec!["run", "--privileged", "imagen"]),
            ("docker.exe", vec!["run", "--network", "host", "imagen"]),
            ("docker.exe", vec!["commit", "contenedor", "imagen"]),
        ] {
            let error = validate_execution_command(program, &args).unwrap_err();
            assert_eq!(error.details.reason, "execution_host_plan_mutates_host", "{program} {args:?}");
        }
    }

    #[test]
    fn combinaciones_instalacion_host() {
        // WSL/CONTAINER exigen host y del tipo correcto.
        assert!(validate_installation_host(InstallationStrategy::Wsl, None).is_err());
        assert!(validate_installation_host(InstallationStrategy::Container, None).is_err());
        assert!(validate_installation_host(
            InstallationStrategy::Wsl,
            Some(ExecutionHostKind::Wsl2)
        )
        .is_ok());
        assert!(validate_installation_host(
            InstallationStrategy::Wsl,
            Some(ExecutionHostKind::Container)
        )
        .is_err());
        assert!(validate_installation_host(
            InstallationStrategy::Container,
            Some(ExecutionHostKind::Container)
        )
        .is_ok());
        // Estrategias nativas: sin host o con host nativo/externo.
        assert!(validate_installation_host(InstallationStrategy::PrebuiltBinary, None).is_ok());
        assert!(validate_installation_host(
            InstallationStrategy::PrebuiltBinary,
            Some(ExecutionHostKind::WindowsNative)
        )
        .is_ok());
        assert!(validate_installation_host(
            InstallationStrategy::ExternalAdoption,
            Some(ExecutionHostKind::ExternalLocal)
        )
        .is_ok());
        assert!(validate_installation_host(
            InstallationStrategy::PrebuiltBinary,
            Some(ExecutionHostKind::Wsl2)
        )
        .is_err());
        // Un runtime Linux bajo WSL jamás se anuncia como nativo.
        assert!(!ExecutionHostKind::WindowsNative.allows_strategy(InstallationStrategy::Wsl));
        assert!(!ExecutionHostKind::WindowsNative.allows_strategy(InstallationStrategy::Container));
        assert!(ExecutionHostKind::Wsl2.allows_strategy(InstallationStrategy::Wsl));
        assert!(ExecutionHostKind::Container.allows_strategy(InstallationStrategy::Container));
    }
}
