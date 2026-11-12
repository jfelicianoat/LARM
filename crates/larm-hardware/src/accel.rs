//! GPU, NPU y matriz de APIs de aceleración (P011, spec §22, contrato 04).
//!
//! Función pura: recibe lo observado (informe nativo del proceso hijo,
//! drivers del registro, `vulkaninfo`, `clinfo` y DLL encontradas) y produce
//! los `devices`, las `dependencies` y la matriz para `metadata.accelerators`.
//!
//! - **VERIFIED**: una prueba real funcionó (dispositivo creado o enumerado).
//! - **UNKNOWN**: hay componentes, pero falta la prueba o falló; lleva `reason`.
//! - **ABSENT**: se buscaron los componentes y no están.
//!
//! «Detectado» nunca se presenta como «operativo»: solo las APIs VERIFIED
//! ligadas a un dispositivo entran en su `backends`.

use std::collections::BTreeMap;

use larm_domain::hardware::{Dependency, DeviceInfo, DeviceKind, SYSTEM_RAM_POOL};
use larm_domain::units::DecimalU64;
use serde::Serialize;
use serde_json::{json, Value};

use crate::libs::APIS;
use crate::native_report::{NativeAdapter, NativeReport, ProbeStatus};
use crate::tools::{OpenClDevice, VulkanDevice};

/// Fallo normalizado de una fuente (proceso hijo o herramienta).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: String,
    pub detail: String,
}

/// Resultado de una herramienta de consulta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome<T> {
    /// La herramienta no está instalada.
    NotInstalled,
    Failed(Failure),
    Ran(T),
}

/// Driver de un dispositivo según `Control\Class\<clave>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverInfo {
    pub desc: Option<String>,
    pub version: Option<String>,
    pub provider: Option<String>,
}

/// DLL encontrada, con su versión de fichero y hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFile {
    pub api: &'static str,
    pub path: String,
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub error: Option<String>,
}

pub struct AccelInputs {
    pub native: Result<NativeReport, Failure>,
    /// Por clave de driver en minúsculas (`{guid}\0000`).
    pub drivers: BTreeMap<String, DriverInfo>,
    pub vulkan: ToolOutcome<Vec<VulkanDevice>>,
    pub opencl: ToolOutcome<Vec<OpenClDevice>>,
    pub libraries: Vec<LibraryFile>,
    /// Carpetas recorridas al buscar DLL (para la evidencia de ABSENT).
    pub searched_dirs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AccelStatus {
    Verified,
    Unknown,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Component {
    pub path: String,
    pub version: Option<String>,
    pub sha256: Option<String>,
}

/// Fila de la matriz de capacidades.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accelerator {
    pub api: &'static str,
    pub status: AccelStatus,
    /// `device_id` de los dispositivos en los que está verificada.
    pub devices: Vec<String>,
    pub version: Option<String>,
    pub components: Vec<Component>,
    pub evidence: String,
    pub reason: Option<String>,
}

pub struct AccelOutput {
    pub devices: Vec<DeviceInfo>,
    pub dependencies: Vec<Dependency>,
    pub accelerators: Vec<Accelerator>,
    /// Entradas para `metadata.unknown`.
    pub unknown: Vec<Value>,
}

/// GPU física: uno o varios adaptadores DXGI con la misma identidad PCI
/// (Windows duplica el adaptador para pantallas remotas o indirectas).
struct Gpu {
    device_id: String,
    adapters: Vec<NativeAdapter>,
    info: DeviceInfo,
}

/// APIs que se prueban en el proceso hijo nativo.
const NATIVE_APIS: [&str; 3] = ["directml", "winml", "rocm"];

/// `PCI\VEN_xxxx&DEV_xxxx&SUBSYS_xxxxxxxx&REV_xx`, la identidad estable.
#[must_use]
pub fn pci_id(vendor: u32, device: u32, subsys: u32, revision: u32) -> String {
    format!("PCI\\VEN_{vendor:04X}&DEV_{device:04X}&SUBSYS_{subsys:08X}&REV_{revision:02X}")
}

/// Rasterizador por software de Microsoft (WARP / Basic Render Driver):
/// no es una GPU aunque DXGI no siempre lo marque como software.
fn is_software(a: &NativeAdapter) -> bool {
    a.software || (a.vendor_id == 0x1414 && a.device_id == 0x008c)
}

/// `1002` a partir de `PCI\VEN_1002&…`.
fn vendor_of(hardware_id: &str) -> Option<String> {
    let upper = hardware_id.to_ascii_uppercase();
    let start = upper.find("VEN_")? + 4;
    upper.get(start..start + 4).map(str::to_ascii_lowercase)
}

/// Versión `6.4.50101` de `HIP_VERSION` (mayor·10⁷ + menor·10⁵ + parche).
#[must_use]
pub fn hip_version(raw: &str) -> Option<String> {
    let v: u64 = raw.parse().ok()?;
    Some(format!(
        "{}.{}.{}",
        v / 10_000_000,
        (v / 100_000) % 100,
        v % 100_000
    ))
}

fn bytes(n: u64) -> Option<DecimalU64> {
    DecimalU64::new(n)
}

fn gpus(report: &NativeReport, drivers: &BTreeMap<String, DriverInfo>) -> Vec<Gpu> {
    let mut groups: BTreeMap<String, Vec<NativeAdapter>> = BTreeMap::new();
    let mut order = Vec::new();
    for a in report.adapters.iter().filter(|a| !is_software(a)) {
        let id = pci_id(a.vendor_id, a.device_id, a.subsys_id, a.revision);
        if !groups.contains_key(&id) {
            order.push(id.clone());
        }
        groups.entry(id).or_default().push(a.clone());
    }
    let display: Vec<_> = report.pnp.iter().filter(|p| p.class == "DISPLAY").collect();
    let mut out = Vec::new();
    for id in order {
        let adapters = groups.remove(&id).unwrap_or_default();
        let first = &adapters[0];
        let pnp = display.iter().find(|p| {
            p.instance_id
                .to_ascii_uppercase()
                .starts_with(&format!("{id}\\"))
        });
        let driver = pnp
            .and_then(|p| p.driver_key.as_ref())
            .and_then(|k| drivers.get(&k.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default();
        let mut evidence = vec![format!(
            "dxgi:{}",
            adapters
                .iter()
                .map(|a| a.luid.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )];
        if let Some(p) = pnp {
            evidence.push(format!("pnp:{}", p.instance_id));
            if let Some(k) = &p.driver_key {
                evidence.push(format!("registry:Control\\Class\\{k}"));
            }
        }
        // El LUID de sesión elegido es el del adaptador que usa Windows ML (el
        // de alto rendimiento) si es de esta GPU; si no, el primero enumerado.
        let winml_luid = report.winml.luid.as_ref();
        let luid = adapters
            .iter()
            .map(|a| &a.luid)
            .find(|l| Some(*l) == winml_luid)
            .unwrap_or(&first.luid)
            .clone();
        out.push(Gpu {
            device_id: id.clone(),
            info: DeviceInfo {
                kind: DeviceKind::Gpu,
                vendor: Some(format!("{:04x}", first.vendor_id)),
                device_id: Some(id),
                luid: Some(luid),
                name: Some(first.description.clone()).filter(|n| !n.is_empty()),
                driver_version: driver.version,
                dedicated_memory_bytes: bytes(first.dedicated_video_memory),
                shared_memory_bytes: bytes(first.shared_system_memory),
                unified_pool_id: (first.uma == Some(true)).then(|| SYSTEM_RAM_POOL.to_string()),
                backends: Vec::new(),
                evidence_refs: evidence,
            },
            adapters,
        });
    }
    // Controladoras de pantalla PCI presentes que DXGI no enumera (driver
    // con problema, por ejemplo): detectadas, sin ninguna API operativa.
    for p in display
        .iter()
        .filter(|p| p.instance_id.to_ascii_uppercase().starts_with("PCI\\"))
    {
        let hardware_id = p
            .instance_id
            .rsplit_once('\\')
            .map_or(p.instance_id.as_str(), |(h, _)| h)
            .to_ascii_uppercase();
        if out.iter().any(|g| g.device_id == hardware_id) {
            continue;
        }
        let driver = p
            .driver_key
            .as_ref()
            .and_then(|k| drivers.get(&k.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default();
        let mut evidence = vec![format!("pnp:{}", p.instance_id)];
        if let Some(code) = p.problem {
            evidence.push(format!("pnp:problem={code}"));
        }
        out.push(Gpu {
            device_id: hardware_id.clone(),
            adapters: Vec::new(),
            info: DeviceInfo {
                kind: DeviceKind::Gpu,
                vendor: vendor_of(&hardware_id),
                device_id: Some(hardware_id),
                luid: None,
                name: driver.desc,
                driver_version: driver.version,
                dedicated_memory_bytes: None,
                shared_memory_bytes: None,
                unified_pool_id: None,
                backends: Vec::new(),
                evidence_refs: evidence,
            },
        });
    }
    out
}

fn npus(report: &NativeReport, drivers: &BTreeMap<String, DriverInfo>) -> Vec<DeviceInfo> {
    report
        .pnp
        .iter()
        .filter(|p| p.class == "COMPUTE_ACCELERATOR")
        .map(|p| {
            let hardware_id = p
                .instance_id
                .rsplit_once('\\')
                .map_or(p.instance_id.as_str(), |(h, _)| h)
                .to_ascii_uppercase();
            let driver = p
                .driver_key
                .as_ref()
                .and_then(|k| drivers.get(&k.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default();
            let mut evidence = vec![format!("pnp:{}", p.instance_id)];
            if let Some(k) = &p.driver_key {
                evidence.push(format!("registry:Control\\Class\\{k}"));
            }
            if let Some(code) = p.problem {
                evidence.push(format!("pnp:problem={code}"));
            }
            DeviceInfo {
                kind: DeviceKind::Npu,
                vendor: vendor_of(&hardware_id),
                device_id: Some(hardware_id),
                luid: None,
                name: driver.desc,
                driver_version: driver.version,
                dedicated_memory_bytes: None,
                shared_memory_bytes: None,
                unified_pool_id: None,
                backends: Vec::new(),
                evidence_refs: evidence,
            }
        })
        .collect()
}

fn components(libraries: &[LibraryFile], api: &str) -> Vec<Component> {
    libraries
        .iter()
        .filter(|l| l.api == api)
        .map(|l| Component {
            path: l.path.clone(),
            version: l.version.clone(),
            sha256: l.sha256.clone(),
        })
        .collect()
}

struct Row {
    status: AccelStatus,
    devices: Vec<String>,
    version: Option<String>,
    evidence: String,
    reason: Option<String>,
}

impl Row {
    fn verified(devices: Vec<String>, version: Option<String>, evidence: String) -> Self {
        Self {
            status: AccelStatus::Verified,
            devices,
            version,
            evidence,
            reason: None,
        }
    }
    fn unknown(reason: &str, evidence: String) -> Self {
        Self {
            status: AccelStatus::Unknown,
            devices: vec![],
            version: None,
            evidence,
            reason: Some(reason.into()),
        }
    }
    fn absent(evidence: String) -> Self {
        Self {
            status: AccelStatus::Absent,
            devices: vec![],
            version: None,
            evidence,
            reason: None,
        }
    }
}

fn tool_row<T>(
    outcome: &ToolOutcome<Vec<T>>,
    tool: &str,
    matches: impl Fn(&T) -> Option<String>,
    version: impl Fn(&[&T]) -> Option<String>,
    describe: impl Fn(&T) -> String,
) -> Row {
    match outcome {
        ToolOutcome::NotInstalled => Row::unknown(
            "tool_missing",
            format!("componentes presentes; {tool} no está instalado y no hay otra prueba de dispositivo"),
        ),
        ToolOutcome::Failed(f) => Row::unknown(&f.reason, format!("{tool}: {}", f.detail)),
        ToolOutcome::Ran(found) => {
            let bound: Vec<(&T, String)> = found.iter().filter_map(|d| matches(d).map(|id| (d, id))).collect();
            if bound.is_empty() {
                let seen = found.iter().map(&describe).collect::<Vec<_>>().join(", ");
                Row::unknown(
                    "no_device",
                    format!("{tool} no enumera ninguna GPU de este equipo (enumera: [{seen}])"),
                )
            } else {
                let mut devices: Vec<String> = bound.iter().map(|(_, id)| id.clone()).collect();
                devices.sort();
                devices.dedup();
                let names = bound.iter().map(|(d, _)| describe(d)).collect::<Vec<_>>().join(", ");
                let refs: Vec<&T> = bound.iter().map(|(d, _)| *d).collect();
                Row::verified(devices, version(&refs), format!("{tool} enumera {names}"))
            }
        }
    }
}

/// Monta dispositivos, dependencias y matriz.
#[must_use]
pub fn assemble(inputs: &AccelInputs) -> AccelOutput {
    let mut unknown = Vec::new();
    let (mut gpus, npus) = match &inputs.native {
        Ok(report) => {
            if report.dxgi.status != ProbeStatus::Verified {
                unknown.push(json!({"field": "devices.gpu", "reason": "dxgi_failed", "detail": report.dxgi.detail}));
            }
            if report.pnp_probe.status != ProbeStatus::Verified {
                unknown.push(json!({"field": "devices.npu", "reason": "pnp_failed", "detail": report.pnp_probe.detail}));
            }
            (gpus(report, &inputs.drivers), npus(report, &inputs.drivers))
        }
        Err(f) => {
            unknown.push(json!({"field": "devices", "reason": f.reason, "detail": f.detail}));
            (Vec::new(), Vec::new())
        }
    };

    let gpu_by_ids = |vendor: u32, device: u32| -> Option<String> {
        gpus.iter()
            .find(|g| {
                g.adapters
                    .first()
                    .is_some_and(|a| a.vendor_id == vendor && a.device_id == device)
            })
            .map(|g| g.device_id.clone())
    };
    let mut rows: Vec<(&'static str, Row)> = Vec::new();
    for api in APIS {
        let comps = components(&inputs.libraries, api.api);
        let absent = || {
            let names = api
                .patterns
                .iter()
                .map(|p| match p {
                    crate::libs::Pattern::Exact(n) => (*n).to_string(),
                    crate::libs::Pattern::Prefix(p) => format!("{p}*.dll"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Row::absent(format!(
                "{names} no encontrado en {} carpetas (System32, PATH y SDK)",
                inputs.searched_dirs
            ))
        };
        let row = match api.api {
            // Las APIs con prueba nativa se juzgan por la prueba: la búsqueda
            // de DLL no puede desmentir un dispositivo creado de verdad.
            _ if comps.is_empty() && !NATIVE_APIS.contains(&api.api) => absent(),
            "vulkan" => tool_row(
                &inputs.vulkan,
                "vulkaninfo --summary",
                |d: &VulkanDevice| gpu_by_ids(d.vendor_id, d.device_id),
                |ds| ds.iter().filter_map(|d| d.api_version.clone()).max(),
                |d| d.name.clone(),
            ),
            "opencl" => tool_row(
                &inputs.opencl,
                "clinfo",
                |d: &OpenClDevice| {
                    gpus.iter()
                        .find(|g| {
                            g.adapters.first().is_some_and(|a| Some(a.vendor_id) == d.vendor_id)
                                && g.info.name.as_deref() == Some(d.name.as_str())
                        })
                        .map(|g| g.device_id.clone())
                },
                |ds| ds.iter().filter_map(|d| d.platform_version.clone()).max(),
                |d| d.name.clone(),
            ),
            "directml" | "winml" | "rocm" => match &inputs.native {
                Err(_) if comps.is_empty() => absent(),
                Err(f) => Row::unknown(&f.reason, format!("sondeo nativo: {}", f.detail)),
                Ok(report) => native_row(api.api, report, &gpus, &comps),
            },
            other => Row::unknown(
                "not_verified",
                format!(
                    "componentes presentes ({}); LARM aún no tiene una prueba de dispositivo para {other}",
                    comps.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", ")
                ),
            ),
        };
        rows.push((api.api, row));
    }

    // Sólo las APIs verificadas y ligadas a un dispositivo son sus backends.
    for (api, row) in &rows {
        if row.status != AccelStatus::Verified {
            continue;
        }
        for g in gpus
            .iter_mut()
            .filter(|g| row.devices.contains(&g.device_id))
        {
            g.info.backends.push((*api).to_string());
        }
    }

    let mut devices: Vec<DeviceInfo> = gpus.into_iter().map(|g| g.info).collect();
    devices.extend(npus);
    for d in &mut devices {
        d.backends.sort();
        let id = d.device_id.clone().unwrap_or_default();
        if d.driver_version.is_none() {
            unknown.push(json!({"field": format!("devices[{id}].driver_version"), "reason": "registry_unreadable", "detail": "sin DriverVersion en la clave del driver"}));
        }
        if d.name.is_none() {
            unknown.push(json!({"field": format!("devices[{id}].name"), "reason": "registry_unreadable", "detail": "sin nombre del dispositivo"}));
        }
    }

    let mut dependencies: Vec<Dependency> = inputs
        .libraries
        .iter()
        .map(|l| Dependency {
            name: std::path::Path::new(&l.path).file_name().map_or_else(
                || l.path.clone(),
                |n| n.to_string_lossy().to_ascii_lowercase(),
            ),
            version: l.version.clone(),
            hash: l.sha256.clone(),
        })
        .collect();
    dependencies
        .sort_by(|a, b| (&a.name, &a.version, &a.hash).cmp(&(&b.name, &b.version, &b.hash)));
    dependencies.dedup();
    for l in inputs.libraries.iter().filter(|l| l.error.is_some()) {
        unknown.push(json!({"field": format!("dependencies[{}]", l.path), "reason": "file_unreadable", "detail": l.error}));
    }

    let accelerators = rows
        .into_iter()
        .map(|(api, row)| Accelerator {
            api,
            status: row.status,
            devices: row.devices,
            version: row.version,
            components: components(&inputs.libraries, api),
            evidence: row.evidence,
            reason: row.reason,
        })
        .collect();
    AccelOutput {
        devices,
        dependencies,
        accelerators,
        unknown,
    }
}

fn native_row(api: &str, report: &NativeReport, gpus: &[Gpu], comps: &[Component]) -> Row {
    match api {
        "directml" => {
            let verified: Vec<String> = gpus
                .iter()
                .filter(|g| {
                    g.adapters
                        .iter()
                        .any(|a| a.directml.status == ProbeStatus::Verified)
                })
                .map(|g| g.device_id.clone())
                .collect();
            let all_absent = gpus
                .iter()
                .flat_map(|g| g.adapters.iter())
                .all(|a| a.directml.status == ProbeStatus::Absent);
            if verified.is_empty() && all_absent && comps.is_empty() {
                Row::absent("DirectML.dll no está en System32".into())
            } else if verified.is_empty() {
                let details: Vec<String> = gpus
                    .iter()
                    .flat_map(|g| {
                        g.adapters
                            .iter()
                            .map(|a| format!("{}: {}", a.description, a.directml.detail))
                    })
                    .collect();
                Row::unknown(
                    "probe_failed",
                    format!("ningún IDMLDevice creado: [{}]", details.join("; ")),
                )
            } else {
                let version = comps.iter().find_map(|c| c.version.clone());
                Row::verified(
                    verified,
                    version,
                    "IDMLDevice creado sobre un ID3D12Device de la GPU".into(),
                )
            }
        }
        "winml" => match report.winml.status {
            ProbeStatus::Verified => {
                let device = gpus
                    .iter()
                    .find(|g| {
                        g.adapters
                            .iter()
                            .any(|a| Some(&a.luid) == report.winml.luid.as_ref())
                    })
                    .map(|g| g.device_id.clone());
                match device {
                    Some(id) => Row::verified(
                        vec![id],
                        None,
                        format!(
                            "{} en el adaptador {}",
                            report.winml.detail,
                            report.winml.luid.clone().unwrap_or_default()
                        ),
                    ),
                    None => Row::unknown(
                        "no_device",
                        format!(
                            "{} pero en un adaptador que no es una GPU física",
                            report.winml.detail
                        ),
                    ),
                }
            }
            ProbeStatus::Absent => Row::absent(report.winml.detail.clone()),
            ProbeStatus::Failed => Row::unknown("probe_failed", report.winml.detail.clone()),
        },
        _ => match report.hip.status {
            ProbeStatus::Absent if comps.is_empty() => Row::absent(report.hip.detail.clone()),
            ProbeStatus::Absent => Row::unknown(
                "not_loadable",
                format!(
                    "DLL de HIP fuera de System32 y %HIP_PATH%\\bin: {}",
                    comps
                        .iter()
                        .map(|c| c.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            ProbeStatus::Failed => Row::unknown("probe_failed", report.hip.detail.clone()),
            ProbeStatus::Verified => {
                let amd: Vec<String> = gpus
                    .iter()
                    .filter(|g| g.adapters.first().is_some_and(|a| a.vendor_id == 0x1002))
                    .map(|g| g.device_id.clone())
                    .collect();
                let count = report.hip.devices.unwrap_or(0) as usize;
                let version = report.hip.version.as_deref().and_then(hip_version);
                let library = report.hip.library.clone().unwrap_or_default();
                if count == amd.len() {
                    Row::verified(amd, version, format!("{} con {library}", report.hip.detail))
                } else {
                    let mut row = Row::verified(
                        vec![],
                        version,
                        format!("{} con {library}, pero hay {} GPU AMD: no se puede ligar a una concreta", report.hip.detail, amd.len()),
                    );
                    row.reason = Some("ambiguous_binding".into());
                    row
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::native_report::{ApiProbe, PnpDevice};

    const NATIVE_PCIA: &str = include_str!("../../../tests/fixtures/hardware/native-pcia.json");
    const VULKAN_PCIA: &str =
        include_str!("../../../tests/fixtures/hardware/vulkaninfo-summary-pcia.txt");
    const CLINFO_PCIA: &str = include_str!("../../../tests/fixtures/hardware/clinfo-pcia.txt");
    const GPU_ID: &str = "PCI\\VEN_1002&DEV_1586&SUBSYS_10111F55&REV_C1";
    const NPU_ID: &str = "PCI\\VEN_1022&DEV_17F0&SUBSYS_17F01022&REV_11";

    fn lib(api: &'static str, path: &str) -> LibraryFile {
        LibraryFile {
            api,
            path: path.into(),
            version: Some("1.0.0.0".into()),
            sha256: Some("a".repeat(64)),
            error: None,
        }
    }

    fn pcia() -> AccelInputs {
        let mut drivers = BTreeMap::new();
        drivers.insert(
            "{4d36e968-e325-11ce-bfc1-08002be10318}\\0000".into(),
            DriverInfo {
                desc: Some("AMD Radeon(TM) 8060S Graphics".into()),
                version: Some("32.0.21030.2001".into()),
                provider: Some("Advanced Micro Devices, Inc.".into()),
            },
        );
        drivers.insert(
            "{f01a9d53-3ff6-48d2-9f97-c8a7004be10c}\\0000".into(),
            DriverInfo {
                desc: Some("NPU Compute Accelerator Device".into()),
                version: Some("32.0.20101.3760".into()),
                provider: Some("AMD".into()),
            },
        );
        AccelInputs {
            native: Ok(serde_json::from_str(NATIVE_PCIA).unwrap()),
            drivers,
            vulkan: ToolOutcome::Ran(crate::tools::parse_vulkaninfo(VULKAN_PCIA)),
            opencl: ToolOutcome::Ran(crate::tools::parse_clinfo(CLINFO_PCIA)),
            libraries: vec![
                lib("vulkan", "C:\\Windows\\System32\\vulkan-1.dll"),
                lib("rocm", "C:\\Windows\\System32\\amdhip64_6.dll"),
                lib("directml", "C:\\Windows\\System32\\DirectML.dll"),
                lib("winml", "C:\\Windows\\System32\\winml.dll"),
                lib("opencl", "C:\\Windows\\System32\\OpenCL.dll"),
            ],
            searched_dirs: 40,
        }
    }

    fn row<'a>(out: &'a AccelOutput, api: &str) -> &'a Accelerator {
        out.accelerators.iter().find(|a| a.api == api).unwrap()
    }

    #[test]
    fn pc_ia_real_una_gpu_una_npu_y_cinco_apis_verificadas() {
        let out = assemble(&pcia());
        assert_eq!(
            out.devices.len(),
            2,
            "WARP y el adaptador duplicado por la pantalla remota no cuentan"
        );
        let gpu = &out.devices[0];
        assert_eq!(gpu.device_id.as_deref(), Some(GPU_ID));
        assert_eq!(gpu.vendor.as_deref(), Some("1002"));
        assert_eq!(gpu.driver_version.as_deref(), Some("32.0.21030.2001"));
        assert_eq!(gpu.unified_pool_id.as_deref(), Some(SYSTEM_RAM_POOL));
        assert_eq!(gpu.dedicated_memory_bytes.unwrap().get(), 68_719_476_736);
        assert_eq!(
            gpu.backends,
            ["directml", "opencl", "rocm", "vulkan", "winml"]
        );
        assert!(gpu.evidence_refs[0].starts_with("dxgi:") && gpu.evidence_refs[0].contains(','));
        let npu = &out.devices[1];
        assert_eq!(
            (npu.kind, npu.device_id.as_deref()),
            (DeviceKind::Npu, Some(NPU_ID))
        );
        assert_eq!(npu.driver_version.as_deref(), Some("32.0.20101.3760"));
        assert!(
            npu.backends.is_empty(),
            "sin prueba de VitisAI la NPU no tiene backends"
        );
        assert_eq!(row(&out, "rocm").version.as_deref(), Some("6.4.50101"));
        for api in [
            "cuda", "sycl", "vitisai", "migraphx", "openvino", "qnn", "tensorrt",
        ] {
            assert_eq!(row(&out, api).status, AccelStatus::Absent, "{api}");
        }
        assert_eq!(out.accelerators.len(), 12);
        assert!(out.unknown.is_empty(), "{:?}", out.unknown);
        assert_eq!(out.dependencies.len(), 5);
    }

    #[test]
    fn componente_presente_sin_prueba_es_unknown_nunca_verified() {
        let mut inputs = pcia();
        inputs
            .libraries
            .push(lib("cuda", "C:\\Windows\\System32\\nvcuda.dll"));
        inputs
            .libraries
            .push(lib("openvino", "C:\\ov\\openvino.dll"));
        inputs.vulkan = ToolOutcome::NotInstalled;
        let out = assemble(&inputs);
        for api in ["cuda", "openvino"] {
            let r = row(&out, api);
            assert_eq!(
                (r.status, r.reason.as_deref()),
                (AccelStatus::Unknown, Some("not_verified")),
                "{api}"
            );
            assert!(r.devices.is_empty());
        }
        let v = row(&out, "vulkan");
        assert_eq!(
            (v.status, v.reason.as_deref()),
            (AccelStatus::Unknown, Some("tool_missing"))
        );
        assert!(!out.devices[0].backends.contains(&"vulkan".to_string()));
    }

    #[test]
    fn hijo_nativo_caido_deja_dispositivos_y_apis_nativas_desconocidos() {
        let mut inputs = pcia();
        inputs.native = Err(Failure {
            reason: "native_timeout".into(),
            detail: "sin respuesta en 30000 ms".into(),
        });
        let out = assemble(&inputs);
        assert!(out.devices.is_empty());
        for api in ["directml", "winml", "rocm"] {
            assert_eq!(
                row(&out, api).reason.as_deref(),
                Some("native_timeout"),
                "{api}"
            );
        }
        // Vulkan necesita una GPU conocida para ligarse: sin DXGI no se inventa.
        assert_eq!(row(&out, "vulkan").reason.as_deref(), Some("no_device"));
        assert!(out
            .unknown
            .iter()
            .any(|u| u["field"] == "devices" && u["reason"] == "native_timeout"));
    }

    #[test]
    fn sin_dll_las_apis_son_absent_y_herramientas_rotas_unknown() {
        let mut inputs = pcia();
        inputs.libraries.clear();
        if let Ok(r) = inputs.native.as_mut() {
            r.hip = ApiProbe::new(ProbeStatus::Absent, "amdhip64*.dll no está");
            r.winml = ApiProbe::new(ProbeStatus::Absent, "Windows ML no está registrado");
            for a in &mut r.adapters {
                a.directml = ApiProbe::new(ProbeStatus::Absent, "DirectML.dll no está en System32");
            }
        }
        let out = assemble(&inputs);
        for api in ["vulkan", "rocm", "directml", "winml", "opencl"] {
            assert_eq!(row(&out, api).status, AccelStatus::Absent, "{api}");
        }
        assert!(out.devices[0].backends.is_empty());
        assert!(out.dependencies.is_empty());

        let mut inputs = pcia();
        inputs.opencl = ToolOutcome::Failed(Failure {
            reason: "tool_timeout".into(),
            detail: "clinfo colgado".into(),
        });
        let out = assemble(&inputs);
        assert_eq!(row(&out, "opencl").reason.as_deref(), Some("tool_timeout"));
    }

    #[test]
    fn directml_fallido_e_hip_ambiguo() {
        let mut inputs = pcia();
        if let Ok(r) = inputs.native.as_mut() {
            for a in &mut r.adapters {
                a.directml = ApiProbe::new(ProbeStatus::Failed, "DMLCreateDevice 0x887a0004");
            }
            r.hip.devices = Some(2);
        }
        let out = assemble(&inputs);
        let dml = row(&out, "directml");
        assert_eq!(
            (dml.status, dml.reason.as_deref()),
            (AccelStatus::Unknown, Some("probe_failed"))
        );
        assert!(dml.evidence.contains("0x887a0004"));
        let hip = row(&out, "rocm");
        assert_eq!(
            (hip.status, hip.reason.as_deref()),
            (AccelStatus::Verified, Some("ambiguous_binding"))
        );
        assert!(hip.devices.is_empty());
        assert!(!out.devices[0].backends.contains(&"rocm".to_string()));
    }

    #[test]
    fn controladora_pci_con_problema_sin_dxgi_se_detecta_sin_backends() {
        let mut inputs = pcia();
        if let Ok(r) = inputs.native.as_mut() {
            r.pnp.push(PnpDevice {
                class: "DISPLAY".into(),
                instance_id: "PCI\\VEN_10DE&DEV_2684&SUBSYS_16F110DE&REV_A1\\4&1&0&0008".into(),
                driver_key: None,
                problem: Some(43),
            });
        }
        let out = assemble(&inputs);
        let nvidia = out
            .devices
            .iter()
            .find(|d| d.vendor.as_deref() == Some("10de"))
            .unwrap();
        assert!(nvidia.backends.is_empty() && nvidia.luid.is_none());
        assert!(nvidia.evidence_refs.contains(&"pnp:problem=43".to_string()));
        assert!(out
            .unknown
            .iter()
            .any(|u| u["field"].as_str().unwrap().contains("VEN_10DE")
                && u["reason"] == "registry_unreadable"));
    }

    #[test]
    fn la_prueba_nativa_manda_sobre_la_busqueda_de_dll() {
        // Búsqueda sin resultados (System32 alterado o PATH vacío) pero la
        // prueba nativa crea los dispositivos: VERIFIED, no ABSENT.
        let mut inputs = pcia();
        inputs.libraries.clear();
        let out = assemble(&inputs);
        for api in ["directml", "winml", "rocm"] {
            assert_eq!(row(&out, api).status, AccelStatus::Verified, "{api}");
        }
        for api in ["vulkan", "opencl"] {
            assert_eq!(row(&out, api).status, AccelStatus::Absent, "{api}");
        }
        // Y con el hijo caído y sin DLL, ausente es el único hecho que hay.
        inputs.native = Err(Failure {
            reason: "native_failed".into(),
            detail: "x".into(),
        });
        let out = assemble(&inputs);
        assert_eq!(row(&out, "directml").status, AccelStatus::Absent);
    }

    #[test]
    fn version_hip() {
        assert_eq!(hip_version("60450101").as_deref(), Some("6.4.50101"));
        assert_eq!(hip_version("70000000").as_deref(), Some("7.0.0"));
        assert_eq!(hip_version("x"), None);
    }
}
