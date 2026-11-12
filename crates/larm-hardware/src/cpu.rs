//! CPU: fabricante, modelo, núcleos y extensiones del juego de instrucciones.
//!
//! Las extensiones se detectan con CPUID en ejecución
//! (`is_x86_feature_detected!`, que además comprueba que el sistema operativo
//! guarde el estado AVX/AVX-512), nunca a partir del nombre del modelo.

use larm_domain::hardware::CpuInfo;

/// Extensiones detectadas, con los nombres de característica de LLVM/Rust.
#[must_use]
pub fn instruction_sets() -> Vec<String> {
    let mut found = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! detect {
            ($($feature:tt),* $(,)?) => {
                $(if std::arch::is_x86_feature_detected!($feature) {
                    found.push($feature.to_string());
                })*
            };
        }
        detect!(
            "sse3",
            "ssse3",
            "sse4.1",
            "sse4.2",
            "popcnt",
            "aes",
            "sha",
            "pclmulqdq",
            "avx",
            "avx2",
            "fma",
            "f16c",
            "bmi1",
            "bmi2",
            "lzcnt",
            "avxvnni",
            "avx512f",
            "avx512cd",
            "avx512bw",
            "avx512dq",
            "avx512vl",
            "avx512ifma",
            "avx512vbmi",
            "avx512vbmi2",
            "avx512vnni",
            "avx512bitalg",
            "avx512vpopcntdq",
            "avx512bf16",
            "avx512fp16",
        );
    }
    found.sort();
    found
}

/// Datos crudos de la CPU tal como los da el sistema.
#[derive(Debug, Clone, Default)]
pub struct RawCpu {
    pub vendor: String,
    pub brand: String,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
}

fn text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Convierte los datos crudos: texto vacío o recuento cero es desconocido.
#[must_use]
pub fn map(raw: &RawCpu, instruction_sets: Vec<String>) -> CpuInfo {
    let count = |n: usize| u32::try_from(n).ok().filter(|n| *n > 0);
    CpuInfo {
        vendor: text(&raw.vendor),
        model: text(&raw.brand),
        physical_cores: raw.physical_cores.and_then(count),
        logical_cores: count(raw.logical_cores),
        instruction_sets,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn vacio_y_cero_son_desconocidos() {
        let cpu = map(&RawCpu::default(), vec![]);
        assert_eq!(cpu.vendor, None);
        assert_eq!(cpu.model, None);
        assert_eq!(cpu.physical_cores, None);
        assert_eq!(cpu.logical_cores, None);
        let cpu = map(
            &RawCpu {
                vendor: "AuthenticAMD".into(),
                brand: "  AMD RYZEN AI MAX+ 395 w/ Radeon 8060S  ".into(),
                physical_cores: Some(16),
                logical_cores: 32,
            },
            vec!["avx2".into()],
        );
        assert_eq!(
            cpu.model.as_deref(),
            Some("AMD RYZEN AI MAX+ 395 w/ Radeon 8060S")
        );
        assert_eq!(
            (cpu.physical_cores, cpu.logical_cores),
            (Some(16), Some(32))
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn la_deteccion_es_coherente() {
        let isa = instruction_sets();
        let has = |f: &str| isa.iter().any(|i| i == f);
        // Jerarquías que la propia arquitectura garantiza.
        if has("avx2") {
            assert!(has("avx"));
        }
        if has("avx512bw") || has("avx512vl") {
            assert!(has("avx512f"));
        }
        assert!(
            isa.windows(2).all(|w| w[0] < w[1]),
            "ordenadas y sin duplicados"
        );
    }
}
