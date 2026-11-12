//! Sistema operativo desde el registro (`HKLM`, sólo lectura).
//!
//! `EditionID` es la edición canónica (`Professional`, `Core`…); `ProductName`
//! no sirve porque en Windows 11 sigue diciendo «Windows 10». La build incluye
//! la revisión (`UBR`) porque una actualización acumulativa cambia drivers y
//! componentes del sistema.

use larm_domain::hardware::OsInfo;

/// Clave del registro con la edición y la build.
pub const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
/// Clave con `PROCESSOR_ARCHITECTURE` del sistema (no la del proceso).
pub const ENVIRONMENT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// Valores crudos del registro; `None` si el valor no existe o no se pudo leer.
#[derive(Debug, Clone, Default)]
pub struct RawOs {
    pub edition_id: Option<String>,
    pub current_build: Option<String>,
    pub ubr: Option<u32>,
    pub architecture: Option<String>,
}

fn text(value: Option<&String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Convierte los valores crudos. Sin `CurrentBuild` la build es desconocida
/// aunque exista `UBR`.
#[must_use]
pub fn map(raw: &RawOs) -> OsInfo {
    let build = text(raw.current_build.as_ref())
        .filter(|b| b.bytes().all(|c| c.is_ascii_digit()))
        .map(|b| match raw.ubr {
            Some(ubr) => format!("{b}.{ubr}"),
            None => b,
        });
    OsInfo {
        edition: text(raw.edition_id.as_ref()),
        build,
        architecture: text(raw.architecture.as_ref()),
    }
}

/// Lee el registro del equipo.
#[cfg(windows)]
#[must_use]
pub fn read() -> RawOs {
    use windows_registry::LOCAL_MACHINE;
    let version = LOCAL_MACHINE.open(CURRENT_VERSION_KEY).ok();
    let string = |name: &str| version.as_ref().and_then(|k| k.get_string(name).ok());
    RawOs {
        edition_id: string("EditionID"),
        current_build: string("CurrentBuild"),
        ubr: version.as_ref().and_then(|k| k.get_u32("UBR").ok()),
        architecture: LOCAL_MACHINE
            .open(ENVIRONMENT_KEY)
            .ok()
            .and_then(|k| k.get_string("PROCESSOR_ARCHITECTURE").ok()),
    }
}

#[cfg(not(windows))]
#[must_use]
pub fn read() -> RawOs {
    RawOs::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(build: Option<&str>, ubr: Option<u32>) -> RawOs {
        RawOs {
            edition_id: Some("Professional".into()),
            current_build: build.map(Into::into),
            ubr,
            architecture: Some("AMD64".into()),
        }
    }

    #[test]
    fn build_con_y_sin_revision() {
        assert_eq!(
            map(&raw(Some("26200"), Some(6899))).build.as_deref(),
            Some("26200.6899")
        );
        assert_eq!(
            map(&raw(Some("26200"), None)).build.as_deref(),
            Some("26200")
        );
        assert_eq!(map(&raw(None, Some(6899))).build, None);
        assert_eq!(map(&raw(Some(" "), Some(1))).build, None);
        assert_eq!(map(&raw(Some("26x00"), Some(1))).build, None);
    }

    #[test]
    fn valores_ausentes_son_desconocidos() {
        let os = map(&RawOs::default());
        assert_eq!((os.edition, os.build, os.architecture), (None, None, None));
    }

    #[cfg(windows)]
    #[test]
    fn el_registro_real_tiene_edicion_build_y_arquitectura() {
        let os = map(&read());
        assert!(
            os.edition.is_some() && os.build.is_some() && os.architecture.is_some(),
            "{os:?}"
        );
    }
}
