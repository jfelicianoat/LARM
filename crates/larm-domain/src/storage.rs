//! Identidad de volumen y datos de almacenamiento (P012; spec §22, §28 y §115;
//! contrato 02).
//!
//! - `volume_guid`: GUID de volumen de Windows en minúsculas y sin llaves
//!   (`fb4f4fbc-…`), la convención fijada en P000.
//! - `volume_key`, la unidad de reservas: el `volume_guid` para un volumen
//!   local; `\\servidor\recurso` en minúsculas para UNC. La letra de unidad es
//!   solo presentación.
//! - Sin identidad verificable no hay `volume_key`: el volumen queda UNKNOWN
//!   y no se inventa un GUID.

use serde::{Deserialize, Serialize};

use crate::units::DecimalU64;

/// Clase de rendimiento (spec §115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PerformanceClass {
    Nvme,
    Ssd,
    Hdd,
    Network,
    Removable,
    Unknown,
}

/// Tipo de unidad según `GetDriveTypeW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DriveKind {
    Fixed,
    Removable,
    Remote,
    Cdrom,
    Ramdisk,
    Unknown,
}

/// Bus del dispositivo de almacenamiento (`STORAGE_BUS_TYPE`), sólo los que
/// deciden la clase; el resto se conserva con su número.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BusKind {
    Nvme,
    Sata,
    Sas,
    Scsi,
    Ata,
    Usb,
    Sd,
    Mmc,
    Virtual,
    FileBackedVirtual,
    Raid,
    Spaces,
    Other,
}

impl BusKind {
    /// Desde el valor numérico de `STORAGE_BUS_TYPE` (winioctl.h).
    #[must_use]
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0x11 => Self::Nvme,
            0x0B => Self::Sata,
            0x0A => Self::Sas,
            0x01 => Self::Scsi,
            0x03 => Self::Ata,
            0x07 => Self::Usb,
            0x0C => Self::Sd,
            0x0D => Self::Mmc,
            0x0E => Self::Virtual,
            0x0F => Self::FileBackedVirtual,
            0x08 => Self::Raid,
            0x10 => Self::Spaces,
            _ => Self::Other,
        }
    }
}

/// Clasificación con evidencia real (P012 paso 3). Sin evidencia suficiente,
/// UNKNOWN: nunca se deduce de la letra, el nombre o el tamaño.
///
/// - red → NETWORK; extraíble o bus USB/SD/MMC → REMOVABLE;
/// - bus NVMe → NVME;
/// - si no, la penalización de búsqueda decide: sin ella SSD, con ella HDD;
/// - bus virtual (VHD/VHDX, Storage Spaces) sin penalización conocida → UNKNOWN.
#[must_use]
pub fn classify(
    drive: DriveKind,
    bus: Option<BusKind>,
    seek_penalty: Option<bool>,
) -> PerformanceClass {
    match (drive, bus, seek_penalty) {
        (DriveKind::Remote, _, _) => PerformanceClass::Network,
        (DriveKind::Removable, _, _) | (_, Some(BusKind::Usb | BusKind::Sd | BusKind::Mmc), _) => {
            PerformanceClass::Removable
        }
        (DriveKind::Fixed, Some(BusKind::Nvme), _) => PerformanceClass::Nvme,
        (DriveKind::Fixed, Some(_), Some(false)) => PerformanceClass::Ssd,
        (DriveKind::Fixed, Some(_), Some(true)) => PerformanceClass::Hdd,
        _ => PerformanceClass::Unknown,
    }
}

/// GUID de `\\?\Volume{GUID}\` en minúsculas y sin llaves.
#[must_use]
pub fn volume_guid_from_path(path: &str) -> Option<String> {
    let inner = path
        .strip_prefix(r"\\?\Volume{")
        .and_then(|r| r.strip_suffix(r"}\").or_else(|| r.strip_suffix('}')))?;
    let ok = inner.len() == 36
        && inner.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    ok.then(|| inner.to_ascii_lowercase())
}

/// Raíz UNC `\\servidor\recurso` de una ruta de red.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncRoot {
    pub server: String,
    pub share: String,
    /// Resto de la ruta bajo el recurso, con `\` y sin `\` inicial.
    pub rest: String,
}

impl UncRoot {
    /// `\\servidor\recurso` tal como se escribió.
    #[must_use]
    pub fn root(&self) -> String {
        format!(r"\\{}\{}", self.server, self.share)
    }

    /// `volume_key` UNC: servidor y recurso en minúsculas (casefold).
    #[must_use]
    pub fn volume_key(&self) -> String {
        self.root().to_lowercase()
    }
}

/// Analiza `\\servidor\recurso[\…]` o `\\?\UNC\servidor\recurso[\…]`, con
/// `/` o `\`. Rechaza rutas de dispositivo (`\\.\`, `\\?\C:`), recursos
/// administrativos ocultos no se tratan aparte, y componentes `.`/`..` o vacíos
/// en servidor/recurso.
///
/// # Errors
/// Motivo legible.
pub fn parse_unc(path: &str) -> Result<UncRoot, String> {
    let normalized = path.replace('/', "\\");
    let body = if let Some(rest) = normalized.strip_prefix(r"\\?\UNC\") {
        rest.to_string()
    } else if normalized.starts_with(r"\\?\") || normalized.starts_with(r"\\.\") {
        return Err(format!("{path} es una ruta de dispositivo, no UNC"));
    } else if let Some(rest) = normalized.strip_prefix(r"\\") {
        rest.to_string()
    } else {
        return Err(format!("{path} no empieza por \\\\"));
    };
    let mut parts = body.splitn(3, '\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default().trim_end_matches('\\');
    for (what, value) in [("servidor", server), ("recurso", share)] {
        if value.is_empty() || value == "." || value == ".." {
            return Err(format!("{path}: {what} vacío o inválido"));
        }
        if value
            .chars()
            .any(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        {
            return Err(format!("{path}: {what} con caracteres no válidos"));
        }
    }
    if rest.split('\\').any(|c| c == "..") {
        return Err(format!("{path}: '..' no se admite"));
    }
    Ok(UncRoot {
        server: server.to_string(),
        share: share.to_string(),
        rest: rest.to_string(),
    })
}

/// Identidad de un volumen (spec §28).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeIdentity {
    /// `None` si la identidad no se pudo verificar.
    pub volume_key: Option<String>,
    pub volume_guid: Option<String>,
    pub unc_path: Option<String>,
    /// Rutas de montaje (`C:\`, `D:\Datos\Montaje\`), ordenadas.
    pub mount_points: Vec<String>,
    /// Primera letra, solo para presentación (`C:`).
    pub drive_letter: Option<String>,
}

/// Datos de un volumen observado (Storage Device Info).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageVolume {
    pub identity: VolumeIdentity,
    pub label: Option<String>,
    pub filesystem: Option<String>,
    pub drive_kind: DriveKind,
    pub bus: Option<BusKind>,
    pub seek_penalty: Option<bool>,
    pub performance_class: PerformanceClass,
    pub capacity_bytes: Option<DecimalU64>,
    pub free_bytes: Option<DecimalU64>,
    pub read_only: bool,
    /// Volumen accesible en el momento del sondeo.
    pub online: bool,
    pub evidence_refs: Vec<String>,
}

/// Resultado de un sondeo de almacenamiento.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageObservation {
    pub volumes: Vec<StorageVolume>,
    /// Lo que no se pudo observar: `{field, reason, detail}`.
    pub unknown: Vec<serde_json::Value>,
    pub observed_at: String,
}

impl StorageObservation {
    /// Volumen con esa `volume_key` (comparación exacta: las claves ya van normalizadas).
    #[must_use]
    pub fn by_key(&self, key: &str) -> Option<&StorageVolume> {
        self.volumes
            .iter()
            .find(|v| v.identity.volume_key.as_deref() == Some(key))
    }
}

/// Estado de una `storage_location` según lo observado (P012/P014). La letra
/// no interviene: solo la `volume_key`.
///
/// - volumen ausente o caído → OFFLINE (los registros se conservan);
/// - volumen de solo lectura → READ_ONLY;
/// - volumen presente pero sin la carpeta raíz → MISSING;
/// - capacidad o espacio desconocidos → UNKNOWN (no se inventa nivel);
/// - nivel CRITICAL de H09 → LOW_SPACE (pausa nuevas asignaciones);
/// - si no, ONLINE.
#[must_use]
pub fn location_state(
    volume: Option<&StorageVolume>,
    root_present: Option<bool>,
    thresholds: Option<crate::locations::DiskThresholds>,
) -> crate::locations::LocationState {
    use crate::locations::{disk_level, DiskLevel, LocationState};
    let Some(v) = volume.filter(|v| v.online) else {
        return LocationState::Offline;
    };
    if v.read_only {
        return LocationState::ReadOnly;
    }
    if root_present == Some(false) {
        return LocationState::Missing;
    }
    let (Some(capacity), Some(free)) = (v.capacity_bytes, v.free_bytes) else {
        return LocationState::Unknown;
    };
    match thresholds.and_then(|t| disk_level(free.get(), capacity.get(), t)) {
        Some(DiskLevel::Critical) => LocationState::LowSpace,
        _ => LocationState::Online,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn volume(online: bool, read_only: bool, sized: bool) -> StorageVolume {
        StorageVolume {
            identity: VolumeIdentity {
                volume_key: Some("k".into()),
                volume_guid: Some("k".into()),
                unc_path: None,
                mount_points: vec![],
                drive_letter: None,
            },
            label: None,
            filesystem: Some("NTFS".into()),
            drive_kind: DriveKind::Fixed,
            bus: Some(BusKind::Nvme),
            seek_penalty: Some(false),
            performance_class: PerformanceClass::Nvme,
            capacity_bytes: sized.then(|| DecimalU64::new(10).unwrap()),
            free_bytes: sized.then(|| DecimalU64::new(5).unwrap()),
            read_only,
            online,
            evidence_refs: vec![],
        }
    }

    #[test]
    fn estado_observado() {
        use crate::locations::{DiskThresholds, LocationState as S};
        let pct = Some(DiskThresholds::Percent {
            warning_bp: 1500,
            critical_bp: 500,
        });
        assert_eq!(location_state(None, None, pct), S::Offline);
        assert_eq!(
            location_state(Some(&volume(false, false, true)), Some(true), pct),
            S::Offline
        );
        assert_eq!(
            location_state(Some(&volume(true, true, true)), Some(true), pct),
            S::ReadOnly
        );
        assert_eq!(
            location_state(Some(&volume(true, false, true)), Some(false), pct),
            S::Missing
        );
        assert_eq!(
            location_state(Some(&volume(true, false, false)), Some(true), pct),
            S::Unknown
        );
        // 5 libres de 10 = 50 %: ONLINE; con 0 libres (0 %) es CRITICAL.
        assert_eq!(
            location_state(Some(&volume(true, false, true)), None, pct),
            S::Online
        );
        let mut full = volume(true, false, true);
        full.free_bytes = DecimalU64::new(0);
        assert_eq!(location_state(Some(&full), Some(true), pct), S::LowSpace);
        assert_eq!(
            location_state(Some(&full), Some(true), None),
            S::Online,
            "sin umbrales no se inventa nivel"
        );
        let obs = StorageObservation {
            volumes: vec![volume(true, false, true)],
            unknown: vec![],
            observed_at: String::new(),
        };
        assert!(obs.by_key("k").is_some() && obs.by_key("K").is_none());
    }

    #[test]
    fn clasifica_solo_con_evidencia() {
        use BusKind::*;
        use DriveKind::*;
        assert_eq!(classify(Fixed, Some(Nvme), None), PerformanceClass::Nvme);
        assert_eq!(
            classify(Fixed, Some(Sata), Some(false)),
            PerformanceClass::Ssd
        );
        assert_eq!(
            classify(Fixed, Some(Sata), Some(true)),
            PerformanceClass::Hdd
        );
        assert_eq!(classify(Fixed, Some(Sata), None), PerformanceClass::Unknown);
        assert_eq!(
            classify(Fixed, None, Some(false)),
            PerformanceClass::Unknown
        );
        assert_eq!(
            classify(Fixed, Some(Usb), Some(false)),
            PerformanceClass::Removable
        );
        assert_eq!(classify(Removable, None, None), PerformanceClass::Removable);
        assert_eq!(classify(Remote, None, None), PerformanceClass::Network);
        assert_eq!(classify(Cdrom, None, None), PerformanceClass::Unknown);
        assert_eq!(
            classify(Fixed, Some(FileBackedVirtual), None),
            PerformanceClass::Unknown
        );
        assert_eq!(BusKind::from_raw(0x11), Nvme);
        assert_eq!(BusKind::from_raw(0x99), Other);
    }

    #[test]
    fn guid_de_volumen() {
        assert_eq!(
            volume_guid_from_path(r"\\?\Volume{FB4F4FBC-5441-4427-A73A-0E1EA94AD8D3}\").as_deref(),
            Some("fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3")
        );
        assert!(
            volume_guid_from_path(r"\\?\Volume{fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3}").is_some()
        );
        for bad in [
            r"C:\",
            r"\\?\Volume{xyz}\",
            r"\\?\Volume{fb4f4fbc54414427a73a0e1ea94ad8d3}\",
            "",
        ] {
            assert!(volume_guid_from_path(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn rutas_unc() {
        let u = parse_unc(r"\\NAS01\Modelos\llm\qwen").unwrap();
        assert_eq!(
            (u.server.as_str(), u.share.as_str(), u.rest.as_str()),
            ("NAS01", "Modelos", r"llm\qwen")
        );
        assert_eq!(u.volume_key(), r"\\nas01\modelos");
        assert_eq!(
            parse_unc(r"\\?\UNC\nas01\modelos\x").unwrap().volume_key(),
            r"\\nas01\modelos"
        );
        assert_eq!(parse_unc("//nas01/modelos/").unwrap().rest, "");
        assert_eq!(
            parse_unc(r"\\localhost\Temp").unwrap().root(),
            r"\\localhost\Temp"
        );
        for bad in [
            r"C:\Modelos",
            r"\\.\PhysicalDrive0",
            r"\\?\C:\x",
            r"\\servidor",
            r"\\servidor\",
            r"\\\recurso",
            r"\\..\recurso",
            r"\\servidor\re:curso",
            r"\\servidor\recurso\..\otro",
        ] {
            assert!(parse_unc(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn serializa_con_nombres_del_contrato() {
        let v = serde_json::to_value(PerformanceClass::Nvme).unwrap();
        assert_eq!(v, "NVME");
        assert_eq!(serde_json::to_value(DriveKind::Remote).unwrap(), "REMOTE");
        assert_eq!(
            serde_json::to_value(BusKind::FileBackedVirtual).unwrap(),
            "FILE_BACKED_VIRTUAL"
        );
    }
}
