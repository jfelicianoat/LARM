//! Informe del sondeo nativo (`larm-daemon probe-accelerators`, ADR-0010).
//! Es lo único que cruza la frontera del proceso hijo: el perfilador lo
//! valida (schema cerrado y versión) antes de usar un solo dato.

use serde::{Deserialize, Serialize};

/// Versión del formato; un hijo de otra versión se rechaza.
pub const NATIVE_REPORT_VERSION: u32 = 1;

/// Resultado de probar una API: `Verified` sólo si se creó un dispositivo o
/// se obtuvo un recuento real; `Absent` si el componente no existe; `Failed`
/// si existe pero la prueba falló (con el error normalizado en `detail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProbeStatus {
    Verified,
    Absent,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiProbe {
    pub status: ProbeStatus,
    pub detail: String,
    /// Versión que informa la propia API (no la del fichero).
    pub version: Option<String>,
    /// Dispositivos que la API enumera, si aplica.
    pub devices: Option<u32>,
    /// LUID del adaptador al que quedó ligada la prueba, si aplica.
    pub luid: Option<String>,
    /// DLL cargada para la prueba.
    pub library: Option<String>,
}

impl ApiProbe {
    #[must_use]
    pub fn new(status: ProbeStatus, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            version: None,
            devices: None,
            luid: None,
            library: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAdapter {
    /// `HHHHHHHH-LLLLLLLL` (parte alta y baja en hexadecimal). Sólo vale
    /// dentro de esta sesión de Windows.
    pub luid: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub subsys_id: u32,
    pub revision: u32,
    pub description: String,
    pub dedicated_video_memory: u64,
    pub dedicated_system_memory: u64,
    pub shared_system_memory: u64,
    pub software: bool,
    /// Creación de un `ID3D12Device` en este adaptador.
    pub d3d12: ApiProbe,
    /// `D3D12_FEATURE_DATA_ARCHITECTURE.UMA` si hubo dispositivo D3D12.
    pub uma: Option<bool>,
    /// Creación de un `IDMLDevice` sobre ese dispositivo D3D12.
    pub directml: ApiProbe,
}

/// Dispositivo PnP presente de las clases Display o ComputeAccelerator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PnpDevice {
    /// `DISPLAY` o `COMPUTE_ACCELERATOR`.
    pub class: String,
    /// Identificador de instancia (`PCI\VEN_…&DEV_…&SUBSYS_…&REV_…\…`).
    pub instance_id: String,
    /// Clave del driver bajo `Control\Class` (`{guid}\0000`).
    pub driver_key: Option<String>,
    /// Código de problema de Configuration Manager; `None` si funciona.
    pub problem: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersion {
    pub path: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReport {
    pub report_version: u32,
    /// `Failed` si DXGI no pudo enumerar (entonces `adapters` va vacío).
    pub dxgi: ApiProbe,
    pub adapters: Vec<NativeAdapter>,
    pub hip: ApiProbe,
    pub winml: ApiProbe,
    /// `Failed` si Configuration Manager no pudo enumerar.
    pub pnp_probe: ApiProbe,
    pub pnp: Vec<PnpDevice>,
    pub files: Vec<FileVersion>,
}

/// Formato `HHHHHHHH-LLLLLLLL` de un LUID.
#[must_use]
pub fn luid_text(high: i32, low: u32) -> String {
    format!("{:08x}-{low:08x}", high.cast_unsigned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn luid_estable_y_campos_extra_rechazados() {
        assert_eq!(luid_text(0, 0xc6f0), "00000000-0000c6f0");
        assert_eq!(luid_text(-1, 1), "ffffffff-00000001");
        let report = NativeReport {
            report_version: NATIVE_REPORT_VERSION,
            dxgi: ApiProbe::new(ProbeStatus::Verified, "ok"),
            adapters: vec![],
            hip: ApiProbe::new(ProbeStatus::Absent, "sin DLL"),
            winml: ApiProbe::new(ProbeStatus::Failed, "0x80004005"),
            pnp_probe: ApiProbe::new(ProbeStatus::Verified, "0 dispositivos"),
            pnp: vec![],
            files: vec![],
        };
        let mut value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["hip"]["status"], "ABSENT");
        assert_eq!(
            serde_json::from_value::<NativeReport>(value.clone()).unwrap(),
            report
        );
        value["extra"] = 1.into();
        assert!(serde_json::from_value::<NativeReport>(value).is_err());
    }
}

/// Versión del informe de almacenamiento (`larm-daemon probe-storage`).
pub const STORAGE_REPORT_VERSION: u32 = 1;

/// Volumen local enumerado con `FindFirstVolumeW`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeVolume {
    /// Nombre de volumen (`\\?\Volume{GUID}\`).
    pub volume_path: String,
    pub mount_points: Vec<String>,
    pub label: Option<String>,
    pub filesystem: Option<String>,
    pub read_only: bool,
    /// `GetDriveTypeW`.
    pub drive_type: u32,
    pub capacity_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// `STORAGE_BUS_TYPE` del dispositivo, si respondió la consulta.
    pub bus_type: Option<u32>,
    pub seek_penalty: Option<bool>,
    pub removable_media: Option<bool>,
    pub product: Option<String>,
    /// Errores no fatales de este volumen (`GetVolumeInformationW: …`).
    pub errors: Vec<String>,
}

/// Ruta de red: unidades mapeadas y raíces UNC pedidas por el perfilador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeShare {
    /// `\\servidor\recurso[\…]`.
    pub unc_path: String,
    /// Letra mapeada (`Z:`), si viene de una unidad de red.
    pub drive_letter: Option<String>,
    pub available: bool,
    pub filesystem: Option<String>,
    pub read_only: bool,
    pub capacity_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageReport {
    pub report_version: u32,
    /// `Failed` si `FindFirstVolumeW` no pudo enumerar.
    pub volumes_probe: ApiProbe,
    pub volumes: Vec<NativeVolume>,
    pub shares: Vec<NativeShare>,
}
