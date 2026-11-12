//! Sondeo completo del equipo: OS, CPU y memoria (P010); GPU, NPU, drivers,
//! APIs de aceleración y dependencias de runtime (P011, [`crate::devices`]).

use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::{HardwareProbe, HardwareSnapshot, ObservedMetrics};
use larm_domain::ports::HardwareProber;
use serde_json::{json, Map, Value};

use crate::cim::{parse_installed_ram, CimQuery, PowerShellCim, INSTALLED_RAM_SCRIPT};
use crate::cpu::RawCpu;
use crate::memory::RawMemory;

/// Nombre y versión del profiler que se guardan con cada snapshot.
pub const PROFILER_NAME: &str = "larm-hardware";
pub const PROFILER_VERSION: &str = env!("CARGO_PKG_VERSION");
const SYSINFO: &str = "sysinfo@0.39.6";

/// Lo que se lee de sysinfo en un hilo bloqueante.
struct SystemSample {
    cpu: RawCpu,
    total_phys: u64,
    avail_phys: u64,
    pagefile_total: u64,
    cpu_usage: Option<f64>,
}

fn sample_system() -> SystemSample {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(CpuRefreshKind::everything()),
    );
    // El uso de CPU necesita dos lecturas separadas por el intervalo mínimo.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    let cpus = sys.cpus();
    let usage = f64::from(sys.global_cpu_usage());
    SystemSample {
        cpu: RawCpu {
            vendor: cpus
                .first()
                .map(|c| c.vendor_id().to_string())
                .unwrap_or_default(),
            brand: cpus
                .first()
                .map(|c| c.brand().to_string())
                .unwrap_or_default(),
            physical_cores: System::physical_core_count(),
            logical_cores: cpus.len(),
        },
        total_phys: sys.total_memory(),
        avail_phys: sys.available_memory(),
        pagefile_total: sys.total_swap(),
        cpu_usage: (usage.is_finite() && !cpus.is_empty()).then(|| usage.clamp(0.0, 100.0)),
    }
}

/// Profiler de Windows: registro, sysinfo, CPUID, CIM con timeout y el
/// sondeo de dispositivos.
pub struct WindowsProfiler {
    cim: Arc<dyn CimQuery>,
    devices: crate::devices::DeviceProbeConfig,
}

impl Default for WindowsProfiler {
    fn default() -> Self {
        Self::new(Arc::new(PowerShellCim::default()))
    }
}

impl WindowsProfiler {
    /// Sin sondeo nativo: GPU/NPU y las APIs que dependen de él quedan UNKNOWN.
    #[must_use]
    pub fn new(cim: Arc<dyn CimQuery>) -> Self {
        Self {
            cim,
            devices: crate::devices::DeviceProbeConfig::from_environment(None),
        }
    }

    /// Con el sondeo nativo en un proceso hijo (ADR-0010).
    #[must_use]
    pub fn with_native(native: crate::devices::NativeCommand) -> Self {
        Self {
            cim: Arc::new(PowerShellCim::default()),
            devices: crate::devices::DeviceProbeConfig::from_environment(Some(native)),
        }
    }

    #[must_use]
    pub fn with_devices(mut self, devices: crate::devices::DeviceProbeConfig) -> Self {
        self.devices = devices;
        self
    }
}

fn unknown(field: &str, reason: &str, detail: &str) -> Value {
    json!({"field": field, "reason": reason, "detail": detail})
}

/// Una entrada `unknown` por cada componente estable que quedó `null`.
fn unknowns(snapshot: &HardwareSnapshot, physical: Option<&crate::cim::CimFailure>) -> Vec<Value> {
    let mut out = Vec::new();
    let os = &snapshot.os;
    for (field, missing) in [
        ("os.edition", os.edition.is_none()),
        ("os.build", os.build.is_none()),
        ("os.architecture", os.architecture.is_none()),
    ] {
        if missing {
            out.push(unknown(
                field,
                "registry_unreadable",
                "valor ausente o ilegible en HKLM",
            ));
        }
    }
    let cpu = &snapshot.cpu;
    for (field, missing) in [
        ("cpu.vendor", cpu.vendor.is_none()),
        ("cpu.model", cpu.model.is_none()),
        ("cpu.physical_cores", cpu.physical_cores.is_none()),
        ("cpu.logical_cores", cpu.logical_cores.is_none()),
    ] {
        if missing {
            out.push(unknown(
                field,
                "os_api_unavailable",
                "el sistema no lo informó",
            ));
        }
    }
    if let Some(failure) = physical {
        out.push(unknown(
            "ram.physical_bytes",
            failure.reason,
            &failure.detail,
        ));
    }
    if snapshot.ram.usable_bytes.is_none() {
        out.push(unknown(
            "ram.usable_bytes",
            "os_api_unavailable",
            "GlobalMemoryStatusEx sin dato",
        ));
    }
    out
}

#[async_trait]
impl HardwareProber for WindowsProfiler {
    async fn probe(&self) -> Result<HardwareProbe, LarmError> {
        let probed_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let os_raw = tokio::task::spawn_blocking(crate::os::read);
        let sample = tokio::task::spawn_blocking(sample_system);
        let (installed, (accel, device_sources)) = tokio::join!(
            async {
                self.cim
                    .scalar(INSTALLED_RAM_SCRIPT)
                    .await
                    .and_then(|text| parse_installed_ram(&text))
            },
            crate::devices::probe(&self.devices),
        );
        let join = |e: tokio::task::JoinError| {
            LarmError::new(
                ErrorCode::Internal,
                "El sondeo de hardware falló",
                "hardware_probe_failed",
            )
            .with_technical(e.to_string())
        };
        let os_raw = os_raw.await.map_err(join)?;
        let sample = sample.await.map_err(join)?;

        let memory = crate::memory::map(&RawMemory {
            total_phys: sample.total_phys,
            avail_phys: sample.avail_phys,
            pagefile_total: sample.pagefile_total,
            installed,
        });
        let snapshot = HardwareSnapshot {
            os: crate::os::map(&os_raw),
            cpu: crate::cpu::map(&sample.cpu, crate::cpu::instruction_sets()),
            ram: memory.ram.clone(),
            devices: accel.devices,
            dependencies: accel.dependencies,
            observed_metrics: ObservedMetrics {
                available_ram_bytes: memory.available,
                cpu_usage_percent: sample.cpu_usage,
            },
        }
        .canonical();
        snapshot.validate().map_err(|detail| {
            LarmError::new(
                ErrorCode::Internal,
                "El sondeo de hardware es incoherente",
                "hardware_probe_failed",
            )
            .with_technical(detail)
        })?;

        let cim_source = self.cim.source();
        let mut sources = json!({
            "os": format!(r"registry:HKLM\{}", crate::os::CURRENT_VERSION_KEY),
            "os.architecture": format!(r"registry:HKLM\{}", crate::os::ENVIRONMENT_KEY),
            "cpu": format!("{SYSINFO}:GetSystemInfo+GetLogicalProcessorInformationEx"),
            "cpu.instruction_sets": "cpuid:is_x86_feature_detected",
            "ram.physical_bytes": format!("{cim_source}:Win32_PhysicalMemory"),
            "ram.usable_bytes": format!("{SYSINFO}:GlobalMemoryStatusEx"),
            "pagefile": format!("{SYSINFO}:GetPerformanceInfo"),
        });
        if let Some(map) = sources.as_object_mut() {
            for (k, v) in device_sources {
                map.insert(k.into(), Value::String(v));
            }
        }
        let mut metadata = Map::new();
        metadata.insert(
            "profiler".into(),
            json!({"name": PROFILER_NAME, "version": PROFILER_VERSION}),
        );
        metadata.insert("sources".into(), sources);
        metadata.insert(
            "pagefile".into(),
            json!({"total_bytes": memory.pagefile_total.map(|b| b.to_string())}),
        );
        metadata.insert(
            "coverage".into(),
            json!({"probed": ["os", "cpu", "ram", "devices", "dependencies"], "pending": []}),
        );
        metadata.insert(
            "accelerators".into(),
            serde_json::to_value(&accel.accelerators).unwrap_or(Value::Array(vec![])),
        );
        let mut unknown = unknowns(&snapshot, memory.physical_unknown.as_ref());
        unknown.extend(accel.unknown);
        metadata.insert("unknown".into(), Value::Array(unknown));
        let evidence_refs = vec![
            format!("{PROFILER_NAME}@{PROFILER_VERSION}"),
            SYSINFO.to_string(),
            cim_source,
        ];
        Ok(HardwareProbe {
            snapshot,
            metadata,
            evidence_refs,
            probed_at,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::cim::CimFailure;

    struct Broken(&'static str);

    #[async_trait]
    impl CimQuery for Broken {
        async fn scalar(&self, _: &str) -> Result<String, CimFailure> {
            match self.0 {
                "garbage" => Ok("no es un número".into()),
                reason => Err(CimFailure {
                    reason: if reason == "timeout" {
                        "wmi_timeout"
                    } else {
                        "wmi_unavailable"
                    },
                    detail: "simulado".into(),
                }),
            }
        }
        fn source(&self) -> String {
            "fake".into()
        }
    }

    #[tokio::test]
    async fn con_wmi_roto_el_sondeo_sigue_y_marca_la_ram_instalada_desconocida() {
        for (mode, reason) in [
            ("timeout", "wmi_timeout"),
            ("absent", "wmi_unavailable"),
            ("garbage", "wmi_unparseable"),
        ] {
            let probe = WindowsProfiler::new(Arc::new(Broken(mode)))
                .probe()
                .await
                .unwrap();
            let s = &probe.snapshot;
            assert_eq!(s.ram.physical_bytes, None, "{mode}");
            assert!(s.ram.usable_bytes.is_some(), "{mode}");
            assert!(s.cpu.logical_cores.is_some(), "{mode}");
            let unknown = probe.metadata["unknown"].as_array().unwrap();
            assert!(
                unknown
                    .iter()
                    .any(|u| u["field"] == "ram.physical_bytes" && u["reason"] == reason),
                "{mode}: {unknown:?}"
            );
            assert_eq!(s.fingerprint().unwrap().len(), 64);
        }
    }
}
