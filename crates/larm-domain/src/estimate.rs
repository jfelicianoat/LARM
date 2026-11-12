//! StorageEstimate (P015; spec §31, §33, §45, §114, §137; contrato 02).
//!
//! Una operación (descarga, transformación, copia) se describe como fases;
//! en cada fase hay ficheros vivos a la vez en uno o varios volúmenes. Los
//! que ya existen en disco (fuente local, CAS) no son asignación nueva.
//!
//! - `peak_extra(v)` = máximo, entre fases, de los bytes **nuevos** vivos a
//!   la vez en el volumen `v`;
//! - margen(v) = `max(minimum_margin, ceil(peak_extra(v) · margin_bp / 10000))`;
//! - `required_free(v)` = `peak_extra(v) + margen(v)`;
//! - `usable_free(v)` = libre del SO − mínimo de política del volumen −
//!   reservas de otros trabajos (nunca negativo; la propia reserva no se
//!   descuenta dos veces: el SO ya refleja lo creado).
//!
//! Toda la aritmética es comprobada: un desbordamiento es un error, nunca un
//! número truncado. Un tamaño UNKNOWN sin cota superior bloquea salvo que el
//! operador dé un presupuesto duro explícito.

use serde::{Deserialize, Serialize};

use crate::units::DecimalU64;

/// Confianza de un tamaño (spec §31). Orden: EXACT es la mejor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Confidence {
    Exact,
    High,
    Estimated,
    Unknown,
}

/// Forma declarativa del estimate que acompaña un LaunchPlan (schema
/// `MemoryEstimate`). El supervisor conserva la evidencia del adaptador, pero
/// no usa un estimate `UNKNOWN` como permiso de admisión.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMemoryEstimate {
    #[serde(deserialize_with = "Option::deserialize")]
    pub weights_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub kv_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub activation_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub workspace_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub overhead_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub total_bytes: Option<DecimalU64>,
    pub per_pool: Vec<RuntimePoolMemoryEstimate>,
    pub confidence: Confidence,
    pub evidence_refs: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub multimodal_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub temporary_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub safety_margin_bytes: Option<DecimalU64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePoolMemoryEstimate {
    pub pool_key: String,
    pub required_bytes: DecimalU64,
}

impl RuntimeMemoryEstimate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.per_pool.iter().any(|pool| pool.pool_key.is_empty()) {
            return Err("memory_estimate_pool_key_invalid");
        }
        Ok(())
    }
}

/// Papel de un fichero en la operación (para el desglose del estimate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ItemKind {
    /// Bytes descargados (incluye ficheros comprimidos mientras existan).
    Download,
    /// Artefacto final (lo que queda tras la operación).
    FinalArtifact,
    /// Instalación (runtime extraído, por ejemplo).
    Installed,
    /// Caché derivada (índices, conversiones reutilizables).
    DerivedCache,
    /// Temporal que desaparece al terminar.
    Temporary,
}

/// Tamaño con su confianza: `bytes` es la mejor estimación y `upper_bound`
/// la cota superior (igual a `bytes` si es EXACT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub bytes: Option<u64>,
    pub upper_bound: Option<u64>,
    pub confidence: Confidence,
}

impl Size {
    #[must_use]
    pub fn exact(bytes: u64) -> Self {
        Self {
            bytes: Some(bytes),
            upper_bound: Some(bytes),
            confidence: Confidence::Exact,
        }
    }
    #[must_use]
    pub fn estimated(bytes: u64, upper_bound: u64, confidence: Confidence) -> Self {
        Self {
            bytes: Some(bytes),
            upper_bound: Some(upper_bound.max(bytes)),
            confidence,
        }
    }
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            bytes: None,
            upper_bound: None,
            confidence: Confidence::Unknown,
        }
    }
    /// Lo que cuenta para reservar: la cota superior.
    fn reserve_bytes(self) -> Option<u64> {
        self.upper_bound.or(match self.confidence {
            Confidence::Exact => self.bytes,
            _ => None,
        })
    }
}

/// Fichero vivo durante una fase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseItem {
    pub name: String,
    pub kind: ItemKind,
    pub volume_key: String,
    pub size: Size,
    /// Ya está en disco antes de empezar (no es asignación nueva).
    pub existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub items: Vec<PhaseItem>,
}

/// Estado de un volumen candidato en el momento del cálculo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSnapshot {
    pub volume_key: String,
    /// Libre del SO para el usuario del daemon.
    pub os_free: u64,
    /// Mayor reserva entre las raíces del volumen y el mínimo global.
    pub policy_minimum: u64,
    /// Restante de las reservas de **otros** trabajos sobre el volumen.
    pub other_reservations: u64,
}

/// Política de margen (`Settings.storage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarginPolicy {
    pub minimum_margin_bytes: u64,
    pub margin_basis_points: u64,
}

impl Default for MarginPolicy {
    fn default() -> Self {
        Self {
            minimum_margin_bytes: 1 << 30,
            margin_basis_points: 500,
        }
    }
}

/// Desglose por volumen (schema `StorageEstimate.per_volume`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeEstimate {
    pub volume_key: String,
    pub peak_extra_bytes: DecimalU64,
    pub margin_bytes: DecimalU64,
    pub other_reservations_bytes: DecimalU64,
    pub usable_free_bytes: DecimalU64,
    pub eligible: bool,
    pub reason: String,
}

/// `StorageEstimate` (schema cerrado de `larm.schema.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageEstimate {
    pub download_bytes: Option<DecimalU64>,
    pub final_artifact_bytes: Option<DecimalU64>,
    pub installed_bytes: Option<DecimalU64>,
    pub derived_cache_bytes: Option<DecimalU64>,
    pub temporary_peak_bytes: Option<DecimalU64>,
    pub existing_bytes: Option<DecimalU64>,
    pub peak_extra_bytes: Option<DecimalU64>,
    pub required_free_bytes: Option<DecimalU64>,
    pub upper_bound_bytes: Option<DecimalU64>,
    pub confidence: Confidence,
    pub reason: String,
    pub evidence_refs: Vec<String>,
    pub per_volume: Vec<VolumeEstimate>,
}

/// Fallo del cálculo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EstimateError {
    /// Una suma supera `u64` o el máximo representable (i64::MAX).
    Overflow(String),
    /// Falta el estado de un volumen implicado.
    MissingVolume(String),
}

const MAX: u64 = crate::units::DECIMAL_U64_MAX;

fn add(a: u64, b: u64, what: &str) -> Result<u64, EstimateError> {
    a.checked_add(b)
        .filter(|v| *v <= MAX)
        .ok_or_else(|| EstimateError::Overflow(format!("{what}: {a} + {b} supera el máximo")))
}

fn dec(v: u64) -> Result<DecimalU64, EstimateError> {
    DecimalU64::new(v).ok_or_else(|| EstimateError::Overflow(format!("{v} supera el máximo")))
}

/// Margen de un volumen.
///
/// # Errors
/// Desbordamiento.
pub fn margin(peak_extra: u64, policy: MarginPolicy) -> Result<u64, EstimateError> {
    let scaled = u128::from(peak_extra) * u128::from(policy.margin_basis_points);
    let ceil = scaled.div_ceil(10_000);
    let ceil = u64::try_from(ceil).map_err(|_| EstimateError::Overflow("margen".into()))?;
    Ok(ceil.max(policy.minimum_margin_bytes))
}

/// Presupuesto duro del operador para tamaños UNKNOWN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardBudget(pub u64);

/// Calcula el estimate. `budget`, si existe, acota cada fichero UNKNOWN
/// (nunca un UNKNOWN pasa sin cota).
///
/// # Errors
/// Desbordamiento o un volumen sin estado.
pub fn estimate(
    phases: &[Phase],
    volumes: &[VolumeSnapshot],
    policy: MarginPolicy,
    budget: Option<HardBudget>,
) -> Result<StorageEstimate, EstimateError> {
    use std::collections::BTreeMap;
    let items = phases.iter().flat_map(|p| p.items.iter());
    let confidence = items
        .clone()
        .map(|i| i.size.confidence)
        .max()
        .unwrap_or(Confidence::Exact);
    let unbounded: Vec<&PhaseItem> = items
        .clone()
        .filter(|i| i.size.reserve_bytes().is_none())
        .collect();
    if !unbounded.is_empty() && budget.is_none() {
        let names: Vec<&str> = unbounded.iter().map(|i| i.name.as_str()).collect();
        return Ok(StorageEstimate {
            download_bytes: None,
            final_artifact_bytes: None,
            installed_bytes: None,
            derived_cache_bytes: None,
            temporary_peak_bytes: None,
            existing_bytes: None,
            peak_extra_bytes: None,
            required_free_bytes: None,
            upper_bound_bytes: None,
            confidence: Confidence::Unknown,
            reason: format!(
                "tamaño desconocido y sin cota superior: {}; hace falta una cota o un presupuesto duro explícito",
                names.join(", ")
            ),
            evidence_refs: vec![],
            per_volume: volumes
                .iter()
                .map(|v| VolumeEstimate {
                    volume_key: v.volume_key.clone(),
                    peak_extra_bytes: DecimalU64::default(),
                    margin_bytes: DecimalU64::default(),
                    other_reservations_bytes: DecimalU64::new(v.other_reservations).unwrap_or_default(),
                    usable_free_bytes: DecimalU64::new(usable_free(v)).unwrap_or_default(),
                    eligible: false,
                    reason: "size_unknown".into(),
                })
                .collect(),
        });
    }
    let size_of = |i: &PhaseItem| -> u64 {
        i.size
            .reserve_bytes()
            .unwrap_or_else(|| budget.map_or(0, |b| b.0))
    };

    // Picos por volumen (total y nuevo) y temporales, fase a fase.
    let mut peak_extra: BTreeMap<&str, u64> = BTreeMap::new();
    let mut temporary_peak = 0u64;
    for phase in phases {
        let mut live: BTreeMap<&str, u64> = BTreeMap::new();
        let mut temporary = 0u64;
        for item in &phase.items {
            let bytes = size_of(item);
            if !item.existing {
                let slot = live.entry(item.volume_key.as_str()).or_default();
                *slot = add(*slot, bytes, &item.name)?;
            }
            if item.kind == ItemKind::Temporary {
                temporary = add(temporary, bytes, "temporales")?;
            }
        }
        for (volume, bytes) in live {
            let slot = peak_extra.entry(volume).or_default();
            *slot = (*slot).max(bytes);
        }
        temporary_peak = temporary_peak.max(temporary);
    }

    // Totales por papel: cada fichero cuenta una vez (por nombre y volumen).
    let mut seen = std::collections::BTreeSet::new();
    let (mut download, mut final_bytes, mut installed, mut cache, mut existing) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    for item in phases.iter().flat_map(|p| p.items.iter()) {
        if !seen.insert((item.name.as_str(), item.volume_key.as_str())) {
            continue;
        }
        let bytes = size_of(item);
        if item.existing {
            existing = add(existing, bytes, "existentes")?;
            continue;
        }
        match item.kind {
            ItemKind::Download => download = add(download, bytes, "descarga")?,
            ItemKind::FinalArtifact => final_bytes = add(final_bytes, bytes, "final")?,
            ItemKind::Installed => installed = add(installed, bytes, "instalado")?,
            ItemKind::DerivedCache => cache = add(cache, bytes, "caché")?,
            ItemKind::Temporary => {}
        }
    }

    let mut per_volume = Vec::new();
    let (mut total_extra, mut total_required) = (0u64, 0u64);
    for (volume, extra) in &peak_extra {
        let snapshot = volumes
            .iter()
            .find(|v| v.volume_key == *volume)
            .ok_or_else(|| EstimateError::MissingVolume((*volume).to_string()))?;
        let m = margin(*extra, policy)?;
        let required = add(*extra, m, "requerido")?;
        let usable = usable_free(snapshot);
        let eligible = usable >= required;
        per_volume.push(VolumeEstimate {
            volume_key: (*volume).to_string(),
            peak_extra_bytes: dec(*extra)?,
            margin_bytes: dec(m)?,
            other_reservations_bytes: dec(snapshot.other_reservations)?,
            usable_free_bytes: dec(usable)?,
            eligible,
            reason: if eligible {
                "fits".into()
            } else {
                format!("insufficient_space: requiere {required} y hay {usable} utilizables")
            },
        });
        total_extra = add(total_extra, *extra, "pico")?;
        total_required = add(total_required, required, "requerido total")?;
    }
    let budget_note = if unbounded.is_empty() {
        ""
    } else {
        "; tamaños desconocidos acotados por el presupuesto duro del operador"
    };
    let reason = if per_volume.iter().all(|v| v.eligible) {
        format!("cabe en todos los volúmenes{budget_note}")
    } else {
        format!("espacio insuficiente en algún volumen{budget_note}")
    };
    let opt = |v: u64| dec(v).map(Some);
    Ok(StorageEstimate {
        download_bytes: opt(download)?,
        final_artifact_bytes: opt(final_bytes)?,
        installed_bytes: opt(installed)?,
        derived_cache_bytes: opt(cache)?,
        temporary_peak_bytes: opt(temporary_peak)?,
        existing_bytes: opt(existing)?,
        peak_extra_bytes: opt(total_extra)?,
        required_free_bytes: opt(total_required)?,
        upper_bound_bytes: opt(add(total_extra, 0, "cota")?)?,
        confidence,
        reason,
        evidence_refs: vec![],
        per_volume,
    })
}

/// Libre utilizable de un volumen (nunca negativo).
#[must_use]
pub fn usable_free(v: &VolumeSnapshot) -> u64 {
    v.os_free
        .saturating_sub(v.policy_minimum)
        .saturating_sub(v.other_reservations)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn item(name: &str, kind: ItemKind, vol: &str, size: Size, existing: bool) -> PhaseItem {
        PhaseItem {
            name: name.into(),
            kind,
            volume_key: vol.into(),
            size,
            existing,
        }
    }

    fn vol(key: &str, free: u64) -> VolumeSnapshot {
        VolumeSnapshot {
            volume_key: key.into(),
            os_free: free,
            policy_minimum: 0,
            other_reservations: 0,
        }
    }

    #[test]
    fn estimate_de_launch_plan_exige_cadenas_decimales_y_campos_completos() {
        let value = serde_json::json!({
            "weights_bytes": null,
            "kv_bytes": "10",
            "activation_bytes": null,
            "workspace_bytes": null,
            "overhead_bytes": null,
            "total_bytes": null,
            "per_pool": [{"pool_key": "gpu:0", "required_bytes": "10"}],
            "confidence": "ESTIMATED",
            "evidence_refs": [],
            "multimodal_bytes": null,
            "temporary_bytes": null,
            "safety_margin_bytes": null
        });
        let estimate: RuntimeMemoryEstimate = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(estimate.kv_bytes.unwrap().get(), 10);
        assert!(estimate.validate().is_ok());

        let mut numeric = value.clone();
        numeric["kv_bytes"] = serde_json::json!(10);
        assert!(serde_json::from_value::<RuntimeMemoryEstimate>(numeric).is_err());

        let mut incomplete = value;
        incomplete.as_object_mut().unwrap().remove("confidence");
        assert!(serde_json::from_value::<RuntimeMemoryEstimate>(incomplete).is_err());
    }

    /// Transformación del contrato 02: fuente 30 + temporal 18 + salida 9.
    fn transform(source_exists: bool) -> Vec<Phase> {
        let source = item(
            "fuente.safetensors",
            ItemKind::Download,
            "v",
            Size::exact(30 * GB),
            source_exists,
        );
        vec![
            Phase {
                name: "convertir".into(),
                items: vec![
                    source.clone(),
                    item(
                        "temp.gguf",
                        ItemKind::Temporary,
                        "v",
                        Size::exact(18 * GB),
                        false,
                    ),
                    item(
                        "final.gguf",
                        ItemKind::FinalArtifact,
                        "v",
                        Size::exact(9 * GB),
                        false,
                    ),
                ],
            },
            Phase {
                name: "terminar".into(),
                items: vec![
                    source,
                    item(
                        "final.gguf",
                        ItemKind::FinalArtifact,
                        "v",
                        Size::exact(9 * GB),
                        false,
                    ),
                ],
            },
        ]
    }

    #[test]
    fn ejemplo_del_contrato_57_total_27_extra() {
        let e = estimate(
            &transform(true),
            &[vol("v", 1000 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(e.existing_bytes.unwrap().get(), 30 * GB);
        assert_eq!(e.peak_extra_bytes.unwrap().get(), 27 * GB, "extra 27");
        assert_eq!(
            e.existing_bytes.unwrap().get() + e.peak_extra_bytes.unwrap().get(),
            57 * GB,
            "total 57"
        );
        assert_eq!(e.temporary_peak_bytes.unwrap().get(), 18 * GB);
        assert_eq!(e.confidence, Confidence::Exact);
        // Si la fuente se descarga, todo es nuevo: extra 57.
        let fresh = estimate(
            &transform(false),
            &[vol("v", 1000 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(fresh.peak_extra_bytes.unwrap().get(), 57 * GB);
        assert_eq!(fresh.download_bytes.unwrap().get(), 30 * GB);
    }

    #[test]
    fn margen_y_requerido() {
        let p = MarginPolicy::default();
        assert_eq!(margin(10 * GB, p).unwrap(), 1 << 30, "mínimo 1 GiB");
        assert_eq!(margin(100 * GB, p).unwrap(), 5 * GB, "5 %");
        assert_eq!(
            margin(
                1,
                MarginPolicy {
                    minimum_margin_bytes: 0,
                    margin_basis_points: 500
                }
            )
            .unwrap(),
            1,
            "redondeo hacia arriba"
        );
        let e = estimate(&transform(true), &[vol("v", 1000 * GB)], p, None).unwrap();
        assert_eq!(
            e.required_free_bytes.unwrap().get(),
            27 * GB + (27 * GB * 500).div_ceil(10_000)
        );
    }

    #[test]
    fn espacio_insuficiente_y_reserva_respetada() {
        let mut v = vol("v", 30 * GB);
        let e = estimate(
            &transform(true),
            std::slice::from_ref(&v),
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert!(e.per_volume[0].eligible, "27 + 1,35 caben en 30");
        // La reserva de la ubicación (mínimo de política) se respeta.
        v.policy_minimum = 5 * GB;
        let e = estimate(
            &transform(true),
            std::slice::from_ref(&v),
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert!(!e.per_volume[0].eligible);
        assert!(e.per_volume[0].reason.starts_with("insufficient_space"));
        assert_eq!(e.per_volume[0].usable_free_bytes.get(), 25 * GB);
        // Las reservas de otros trabajos también.
        v.policy_minimum = 0;
        // Requerido = 27 GB + 1,35 GB de margen = 28,35 GB.
        v.other_reservations = GB;
        let e = estimate(
            &transform(true),
            std::slice::from_ref(&v),
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            (
                e.per_volume[0].usable_free_bytes.get(),
                e.per_volume[0].eligible
            ),
            (29 * GB, true)
        );
        v.other_reservations = 2 * GB;
        let e = estimate(
            &transform(true),
            std::slice::from_ref(&v),
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            (
                e.per_volume[0].usable_free_bytes.get(),
                e.per_volume[0].eligible
            ),
            (28 * GB, false)
        );
        v.other_reservations = 40 * GB;
        let e = estimate(
            &transform(true),
            std::slice::from_ref(&v),
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(e.per_volume[0].usable_free_bytes.get(), 0, "nunca negativo");
    }

    #[test]
    fn estimado_y_rango_usan_la_cota_superior() {
        let phases = vec![Phase {
            name: "descargar".into(),
            items: vec![
                item(
                    "a.gguf",
                    ItemKind::Download,
                    "v",
                    Size::estimated(10 * GB, 12 * GB, Confidence::High),
                    false,
                ),
                item("b.json", ItemKind::Download, "v", Size::exact(GB), false),
            ],
        }];
        let e = estimate(
            &phases,
            &[vol("v", 100 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(e.confidence, Confidence::High, "la peor confianza manda");
        assert_eq!(
            e.peak_extra_bytes.unwrap().get(),
            13 * GB,
            "cota superior, no la estimación"
        );
    }

    #[test]
    fn desconocido_bloquea_salvo_presupuesto_duro() {
        let phases = vec![Phase {
            name: "descargar".into(),
            items: vec![item(
                "x.bin",
                ItemKind::Download,
                "v",
                Size::unknown(),
                false,
            )],
        }];
        let e = estimate(
            &phases,
            &[vol("v", 100 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(e.confidence, Confidence::Unknown);
        assert!(e.required_free_bytes.is_none() && e.per_volume.iter().all(|v| !v.eligible));
        assert!(e.reason.contains("x.bin"));
        let e = estimate(
            &phases,
            &[vol("v", 100 * GB)],
            MarginPolicy::default(),
            Some(HardBudget(20 * GB)),
        )
        .unwrap();
        assert_eq!(
            e.confidence,
            Confidence::Unknown,
            "sigue siendo desconocido"
        );
        assert_eq!(e.peak_extra_bytes.unwrap().get(), 20 * GB);
        assert!(e.per_volume[0].eligible && e.reason.contains("presupuesto duro"));
    }

    #[test]
    fn copia_a_otro_volumen_y_desbordamiento() {
        // La fuente se mantiene en v1 y el destino reserva íntegro en v2.
        let phases = vec![Phase {
            name: "copiar".into(),
            items: vec![
                item(
                    "m.gguf",
                    ItemKind::FinalArtifact,
                    "v1",
                    Size::exact(50 * GB),
                    true,
                ),
                item(
                    "m.gguf",
                    ItemKind::FinalArtifact,
                    "v2",
                    Size::exact(50 * GB),
                    false,
                ),
            ],
        }];
        let e = estimate(
            &phases,
            &[vol("v1", 0), vol("v2", 100 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(e.per_volume.len(), 1);
        assert_eq!(
            (
                e.per_volume[0].volume_key.as_str(),
                e.per_volume[0].peak_extra_bytes.get()
            ),
            ("v2", 50 * GB)
        );
        let huge = vec![Phase {
            name: "x".into(),
            items: vec![
                item(
                    "a",
                    ItemKind::Download,
                    "v",
                    Size::exact(u64::MAX / 2),
                    false,
                ),
                item(
                    "b",
                    ItemKind::Download,
                    "v",
                    Size::exact(u64::MAX / 2),
                    false,
                ),
            ],
        }];
        assert!(matches!(
            estimate(&huge, &[vol("v", 1)], MarginPolicy::default(), None),
            Err(EstimateError::Overflow(_))
        ));
        assert!(matches!(
            estimate(&transform(true), &[], MarginPolicy::default(), None),
            Err(EstimateError::MissingVolume(_))
        ));
    }

    #[test]
    fn serializa_con_la_forma_del_schema() {
        let e = estimate(
            &transform(true),
            &[vol("v", 1000 * GB)],
            MarginPolicy::default(),
            None,
        )
        .unwrap();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["peak_extra_bytes"], "27000000000");
        assert_eq!(v["confidence"], "EXACT");
        assert_eq!(v["per_volume"][0]["eligible"], true);
        let back: StorageEstimate = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }
}
