//! Perfil de hardware (spec §22–§23, contrato 04): schema cerrado
//! `domain.schema.json#/$defs/hardware_profile` y su `hardware_fingerprint`.
//!
//! Un componente que no se pudo observar queda `null` (UNKNOWN) con su motivo en
//! `metadata.unknown`; nunca se rellena por conjetura. Sólo el hardware estable
//! entra en la huella: `observed_metrics`, LUID de sesión, `evidence_refs` y
//! marcas de tiempo quedan fuera.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::units::DecimalU64;

/// Pool de la RAM del sistema. Una GPU de memoria unificada lo cita en
/// `unified_pool_id` para que la admisión no cuente dos veces esa memoria.
pub const SYSTEM_RAM_POOL: &str = "system-ram";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsInfo {
    pub edition: Option<String>,
    pub build: Option<String>,
    pub architecture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuInfo {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub physical_cores: Option<u32>,
    pub logical_cores: Option<u32>,
    /// Extensiones detectadas en ejecución (CPUID), con los nombres de
    /// característica de LLVM/Rust (`avx2`, `avx512f`…). Lista vacía = no se
    /// detectó ninguna, no «desconocido».
    pub instruction_sets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryInfo {
    /// RAM instalada (módulos físicos).
    pub physical_bytes: Option<DecimalU64>,
    /// RAM que ve Windows. Con un carve-out de VRAM es menor que la
    /// instalada; la memoria compartida de la GPU no se suma.
    pub usable_bytes: Option<DecimalU64>,
    pub pool_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DeviceKind {
    #[serde(rename = "GPU")]
    Gpu,
    #[serde(rename = "NPU")]
    Npu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub kind: DeviceKind,
    pub vendor: Option<String>,
    pub device_id: Option<String>,
    pub luid: Option<String>,
    pub name: Option<String>,
    pub driver_version: Option<String>,
    pub dedicated_memory_bytes: Option<DecimalU64>,
    pub shared_memory_bytes: Option<DecimalU64>,
    pub unified_pool_id: Option<String>,
    pub backends: Vec<String>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedMetrics {
    pub available_ram_bytes: Option<DecimalU64>,
    pub cpu_usage_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareSnapshot {
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub ram: MemoryInfo,
    pub devices: Vec<DeviceInfo>,
    pub dependencies: Vec<Dependency>,
    pub observed_metrics: ObservedMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareProfile {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub hardware_fingerprint: String,
    pub snapshot: HardwareSnapshot,
    pub evidence_refs: Vec<String>,
    pub probed_at: String,
}

/// Resultado de un sondeo antes de persistirlo: el repositorio asigna id,
/// fechas, versión y huella.
#[derive(Debug, Clone, PartialEq)]
pub struct HardwareProbe {
    pub snapshot: HardwareSnapshot,
    /// `profiler {name, version}`, `sources`, `pagefile` y `unknown`.
    pub metadata: Map<String, Value>,
    pub evidence_refs: Vec<String>,
    pub probed_at: String,
}

/// Cuerpo de `POST /hardware/refresh` (schema `Probe`): ambos campos son
/// obligatorios y anulables. En hardware, `resource_id` es el perfil que se
/// quiere refrescar (o `null`) y `profile_id` no aplica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    #[serde(deserialize_with = "required_probe_reference")]
    pub resource_id: Option<String>,
    #[serde(deserialize_with = "required_probe_reference")]
    pub profile_id: Option<String>,
}
fn required_probe_reference<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}

impl Probe {
    /// Exige las dos claves (el schema las marca `required` aunque admitan
    /// `null`) y rechaza cualquier otra.
    ///
    /// # Errors
    /// Texto del motivo.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("el cuerpo no es un objeto")?;
        for key in ["resource_id", "profile_id"] {
            if !object.contains_key(key) {
                return Err(format!("falta {key} (obligatorio aunque sea null)"));
            }
        }
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())
    }
}

/// Versión del algoritmo de `hardware_fingerprint` (P013). Cambiar qué entra
/// en la huella o cómo se canoniza exige subirla.
pub const FINGERPRINT_VERSION: u32 = 1;

/// Cambio de un componente estable entre dos configuraciones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FingerprintChange {
    /// Ruta del componente: `os.build`, `devices[GPU:PCI\…].driver_version`,
    /// `dependencies[directml.dll].version`…
    pub field: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// Vista estable (lo que entra en la huella) aplanada en rutas → valor.
/// Los dispositivos se identifican por `kind:device_id` y las dependencias
/// por nombre, de modo que reordenarlos no produce cambios.
fn stable_paths(snapshot: &HardwareSnapshot) -> std::collections::BTreeMap<String, Value> {
    let s = snapshot.clone().canonical();
    let mut out = std::collections::BTreeMap::new();
    let mut put = |prefix: String, value: Value| {
        if let Value::Object(map) = value {
            for (k, v) in map {
                out.insert(format!("{prefix}.{k}"), v);
            }
        }
    };
    put("os".into(), serde_json::to_value(&s.os).unwrap_or_default());
    put(
        "cpu".into(),
        serde_json::to_value(&s.cpu).unwrap_or_default(),
    );
    put(
        "ram".into(),
        serde_json::to_value(&s.ram).unwrap_or_default(),
    );
    for d in &s.devices {
        let kind = if d.kind == DeviceKind::Gpu {
            "GPU"
        } else {
            "NPU"
        };
        let id = format!(
            "devices[{kind}:{}]",
            d.device_id.clone().unwrap_or_else(|| "desconocido".into())
        );
        let mut v = serde_json::to_value(d).unwrap_or_default();
        if let Some(map) = v.as_object_mut() {
            map.remove("luid");
            map.remove("evidence_refs");
        }
        put(id, v);
    }
    for dep in &s.dependencies {
        let mut v = serde_json::to_value(dep).unwrap_or_default();
        if let Some(map) = v.as_object_mut() {
            map.remove("name");
        }
        put(format!("dependencies[{}]", dep.name), v);
    }
    out
}

/// Diferencias entre los componentes estables de dos snapshots (P013: «diff
/// respecto al snapshot anterior»). Vacío si la huella es la misma.
#[must_use]
pub fn fingerprint_diff(
    before: &HardwareSnapshot,
    after: &HardwareSnapshot,
) -> Vec<FingerprintChange> {
    let (a, b) = (stable_paths(before), stable_paths(after));
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| FingerprintChange {
            field: k.clone(),
            before: a.get(k).cloned(),
            after: b.get(k).cloned(),
        })
        .collect()
}

/// Clase de operación de un refresco de hardware en `operation_journal`.
pub const REFRESH_JOB_KIND: &str = "hardware.refresh";

impl HardwareSnapshot {
    /// Orden canónico del contrato 04: `instruction_sets`; `devices` por
    /// kind+device_id+vendor; `dependencies` por name+version+hash; `backends`.
    #[must_use]
    pub fn canonical(mut self) -> Self {
        self.cpu.instruction_sets.sort();
        self.cpu.instruction_sets.dedup();
        for device in &mut self.devices {
            device.backends.sort();
            device.backends.dedup();
        }
        self.devices.sort_by(|a, b| {
            (a.kind, &a.device_id, &a.vendor).cmp(&(b.kind, &b.device_id, &b.vendor))
        });
        self.dependencies
            .sort_by(|a, b| (&a.name, &a.version, &a.hash).cmp(&(&b.name, &b.version, &b.hash)));
        self
    }

    /// `hardware_fingerprint`: sha256 del JSON RFC 8785 del snapshot canónico
    /// sin `observed_metrics` ni, por dispositivo, `luid` y `evidence_refs`.
    ///
    /// # Errors
    /// Texto si el snapshot no se puede serializar de forma canónica.
    pub fn fingerprint(&self) -> Result<String, String> {
        let canonical = self.clone().canonical();
        let mut value = serde_json::to_value(&canonical).map_err(|e| e.to_string())?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| "el snapshot no es un objeto".to_string())?;
        object.remove("observed_metrics");
        if let Some(Value::Array(devices)) = object.get_mut("devices") {
            for device in devices.iter_mut().filter_map(Value::as_object_mut) {
                device.remove("luid");
                device.remove("evidence_refs");
            }
        }
        crate::canonical::sha256_hex(&value)
    }

    /// Invariantes que un sondeo no puede violar: disponible ≤ usable ≤
    /// instalada, núcleos físicos ≤ lógicos y porcentaje en 0..=100.
    ///
    /// # Errors
    /// Primera incoherencia encontrada.
    pub fn validate(&self) -> Result<(), String> {
        let ram = &self.ram;
        if let (Some(physical), Some(usable)) = (ram.physical_bytes, ram.usable_bytes) {
            if usable > physical {
                return Err(format!(
                    "usable_bytes {usable} supera physical_bytes {physical}"
                ));
            }
        }
        if let (Some(usable), Some(available)) =
            (ram.usable_bytes, self.observed_metrics.available_ram_bytes)
        {
            if available > usable {
                return Err(format!(
                    "available_ram_bytes {available} supera usable_bytes {usable}"
                ));
            }
        }
        if let (Some(physical), Some(logical)) = (self.cpu.physical_cores, self.cpu.logical_cores) {
            if physical > logical {
                return Err(format!(
                    "physical_cores {physical} supera logical_cores {logical}"
                ));
            }
        }
        if let Some(usage) = self.observed_metrics.cpu_usage_percent {
            if !(0.0..=100.0).contains(&usage) {
                return Err(format!("cpu_usage_percent {usage} fuera de 0..=100"));
            }
        }
        if ram.pool_id.is_empty() {
            return Err("pool_id vacío".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn bytes(n: u64) -> Option<DecimalU64> {
        DecimalU64::new(n)
    }

    fn gpu(id: &str, luid: &str) -> DeviceInfo {
        DeviceInfo {
            kind: DeviceKind::Gpu,
            vendor: Some("1002".into()),
            device_id: Some(id.into()),
            luid: Some(luid.into()),
            name: None,
            driver_version: Some("32.0.21025.1004".into()),
            dedicated_memory_bytes: bytes(1 << 30),
            shared_memory_bytes: None,
            unified_pool_id: Some(SYSTEM_RAM_POOL.into()),
            backends: vec!["vulkan".into(), "directml".into()],
            evidence_refs: vec![format!("ev-{luid}")],
        }
    }

    fn sample() -> HardwareSnapshot {
        HardwareSnapshot {
            os: OsInfo {
                edition: Some("Professional".into()),
                build: Some("26200.6899".into()),
                architecture: Some("AMD64".into()),
            },
            cpu: CpuInfo {
                vendor: Some("AuthenticAMD".into()),
                model: Some("AMD RYZEN AI MAX+ 395".into()),
                physical_cores: Some(16),
                logical_cores: Some(32),
                instruction_sets: vec!["avx2".into(), "avx".into(), "avx512f".into()],
            },
            ram: MemoryInfo {
                physical_bytes: bytes(128 << 30),
                usable_bytes: bytes(64 << 30),
                pool_id: SYSTEM_RAM_POOL.into(),
            },
            devices: vec![gpu("150e", "0-1"), gpu("1506", "0-2")],
            dependencies: vec![],
            observed_metrics: ObservedMetrics {
                available_ram_bytes: bytes(40 << 30),
                cpu_usage_percent: Some(12.5),
            },
        }
    }

    #[test]
    fn la_huella_ignora_metricas_luid_evidencias_y_orden() {
        let base = sample().fingerprint().unwrap();
        assert_eq!(base.len(), 64);
        let mut other = sample();
        other.observed_metrics = ObservedMetrics {
            available_ram_bytes: bytes(1),
            cpu_usage_percent: None,
        };
        other.devices.reverse();
        for d in &mut other.devices {
            d.luid = Some("9-9".into());
            d.evidence_refs = vec!["otra".into()];
            d.backends.reverse();
        }
        other.cpu.instruction_sets.reverse();
        assert_eq!(other.fingerprint().unwrap(), base);
    }

    #[test]
    fn la_huella_cambia_con_driver_build_ram_o_isa() {
        let base = sample().fingerprint().unwrap();
        type Mutation = Box<dyn Fn(&mut HardwareSnapshot)>;
        let mutations: Vec<Mutation> = vec![
            Box::new(|s| s.devices[0].driver_version = Some("32.0.21025.2000".into())),
            Box::new(|s| s.os.build = Some("26200.7000".into())),
            Box::new(|s| s.ram.usable_bytes = bytes(96 << 30)),
            Box::new(|s| s.cpu.instruction_sets.retain(|i| i != "avx512f")),
            Box::new(|s| s.cpu.model = None),
        ];
        for (i, mutate) in mutations.iter().enumerate() {
            let mut s = sample();
            mutate(&mut s);
            assert_ne!(s.fingerprint().unwrap(), base, "mutación {i}");
        }
    }

    #[test]
    fn p013_oraculos_de_la_huella_y_diff() {
        // Mismo input => mismo hash; orden distinto => mismo hash.
        let base = sample();
        assert_eq!(base.fingerprint().unwrap(), sample().fingerprint().unwrap());
        let mut shuffled = sample();
        shuffled.devices.reverse();
        shuffled.cpu.instruction_sets.reverse();
        assert_eq!(shuffled.fingerprint().unwrap(), base.fingerprint().unwrap());
        assert!(
            fingerprint_diff(&base, &shuffled).is_empty(),
            "reordenar no es un cambio"
        );
        // Cambio de RAM libre => mismo hash y sin diff.
        let mut free = sample();
        free.observed_metrics.available_ram_bytes = bytes(1);
        assert_eq!(free.fingerprint().unwrap(), base.fingerprint().unwrap());
        assert!(fingerprint_diff(&base, &free).is_empty());
        // Cambio de driver => hash distinto y diff que lo nombra.
        let mut driver = sample();
        driver.devices[1].driver_version = Some("32.0.22000.1".into());
        assert_ne!(driver.fingerprint().unwrap(), base.fingerprint().unwrap());
        let diff = fingerprint_diff(&base, &driver);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].field, "devices[GPU:1506].driver_version");
        assert_eq!(
            diff[0].before,
            Some(Value::String("32.0.21025.1004".into()))
        );
        // Un dispositivo nuevo aparece como componentes añadidos.
        let mut added = sample();
        added.devices.push(gpu("17f0", "0-3"));
        let diff = fingerprint_diff(&base, &added);
        assert!(diff
            .iter()
            .all(|c| c.field.starts_with("devices[GPU:17f0]") && c.before.is_none()));
        assert!(!diff.is_empty());
        assert_eq!(FINGERPRINT_VERSION, 1);
    }

    #[test]
    fn serializa_con_la_forma_del_schema() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(value["ram"]["physical_bytes"], "137438953472");
        assert_eq!(value["devices"][0]["kind"], "GPU");
        assert_eq!(value["cpu"]["physical_cores"], 16);
        let back: HardwareSnapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, sample());
        let mut extra = value;
        extra["cpu"]["threads"] = 32.into();
        assert!(serde_json::from_value::<HardwareSnapshot>(extra).is_err());
    }

    #[test]
    fn validate_rechaza_incoherencias() {
        assert!(sample().validate().is_ok());
        let mut s = sample();
        s.ram.usable_bytes = bytes(256 << 30);
        assert!(s.validate().unwrap_err().contains("usable_bytes"));
        let mut s = sample();
        s.observed_metrics.available_ram_bytes = bytes(65 << 30);
        assert!(s.validate().unwrap_err().contains("available_ram_bytes"));
        let mut s = sample();
        s.cpu.physical_cores = Some(64);
        assert!(s.validate().unwrap_err().contains("physical_cores"));
        let mut s = sample();
        s.observed_metrics.cpu_usage_percent = Some(101.0);
        assert!(s.validate().is_err());
        // Desconocido no es incoherente.
        let mut s = sample();
        s.ram.physical_bytes = None;
        s.cpu.physical_cores = None;
        assert!(s.validate().is_ok());
    }
}
