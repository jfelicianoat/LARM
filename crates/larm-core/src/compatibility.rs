//! Evaluación determinista de hard gates P090 antes de cualquier scoring.

pub mod score;

use std::collections::{BTreeMap, BTreeSet};

use larm_domain::{
    canonical,
    compatibility::{
        CompatibilityRecord, CompatibilityStatus, CompatibilityValidationError, GateStatus,
        HardGate,
    },
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Puertas mínimas de contrato 04. `backend_operable` se exige porque la
/// presencia de una fila o dispositivo no demuestra que el backend funcione.
pub use larm_domain::compatibility::REQUIRED_HARD_GATE_NAMES as REQUIRED_HARD_GATES;

const MISSING_GATE_REASON: &str =
    "No hay evidencia suficiente para evaluar esta puerta obligatoria.";

/// Tupla completa usada para invalidar la caché cuando cambia cualquier
/// dimensión contractual de compatibilidad.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityCacheKey {
    pub variant_id: String,
    pub runtime_version_id: String,
    pub backend_id: String,
    pub hardware_fingerprint: String,
    pub launch_profile_hash: String,
}

impl CompatibilityCacheKey {
    pub fn new(
        variant_id: impl Into<String>,
        runtime_version_id: impl Into<String>,
        backend_id: impl Into<String>,
        hardware_fingerprint: impl Into<String>,
        launch_profile_hash: impl Into<String>,
    ) -> Result<Self, CompatibilityEngineError> {
        let key = Self {
            variant_id: variant_id.into(),
            runtime_version_id: runtime_version_id.into(),
            backend_id: backend_id.into(),
            hardware_fingerprint: hardware_fingerprint.into(),
            launch_profile_hash: launch_profile_hash.into(),
        };
        for (field, value) in [
            ("variant_id", key.variant_id.as_str()),
            ("runtime_version_id", key.runtime_version_id.as_str()),
            ("backend_id", key.backend_id.as_str()),
        ] {
            if !larm_domain::bridge::is_uuid_v7(value) {
                return Err(CompatibilityEngineError::InvalidCacheIdentity(field));
            }
        }
        for (field, value) in [
            ("hardware_fingerprint", key.hardware_fingerprint.as_str()),
            ("launch_profile_hash", key.launch_profile_hash.as_str()),
        ] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(CompatibilityEngineError::InvalidCacheIdentity(field));
            }
        }
        Ok(key)
    }

    /// Huella determinista de la tupla para almacenamiento/indexación. La
    /// igualdad de caché continúa definida por los cinco campos completos.
    pub fn fingerprint(&self) -> Result<String, CompatibilityEngineError> {
        let value =
            serde_json::to_value(self).map_err(|_| CompatibilityEngineError::Canonicalization)?;
        canonical::sha256_hex(&value).map_err(|_| CompatibilityEngineError::Canonicalization)
    }
}

/// Cache local a la sesión. Las cinco dimensiones forman la clave completa;
/// cambiar cualquiera produce un miss y no reutiliza la evaluación anterior.
#[derive(Debug, Default)]
pub struct CompatibilityCache {
    entries: RwLock<BTreeMap<CompatibilityCacheKey, CompatibilityAssessment>>,
}

impl CompatibilityCache {
    #[must_use]
    pub fn get(&self, key: &CompatibilityCacheKey) -> Option<CompatibilityAssessment> {
        self.entries.read().get(key).cloned()
    }

    pub fn insert(&self, key: CompatibilityCacheKey, value: CompatibilityAssessment) {
        self.entries.write().insert(key, value);
    }

    pub fn clear(&self) {
        self.entries.write().clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityAssessment {
    pub status: CompatibilityStatus,
    pub hard_gates: Vec<HardGate>,
    pub limitations: Vec<String>,
    /// P090 no calcula score: P091 sólo podrá hacerlo después de hard gates PASS.
    pub technical_fit: Option<f64>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompatibilityEngineError {
    #[error("gate con name o reason vacío")]
    InvalidGate,
    #[error("el gate {0} requiere evidencia para PASS o FAIL")]
    EvidenceRequired(String),
    #[error("limitación vacía")]
    InvalidLimitation,
    #[error("identidad de caché inválida: {0}")]
    InvalidCacheIdentity(&'static str),
    #[error("no se pudo canonicalizar la tupla de compatibilidad")]
    Canonicalization,
    #[error("registro de compatibilidad inválido: {0}")]
    InvalidRecord(#[from] CompatibilityValidationError),
}

/// Combina observaciones del adapter y puertas derivadas de modelo/hardware.
/// Los conflictos se resuelven de forma conservadora: FAIL > UNKNOWN > PASS.
/// Las puertas obligatorias ausentes se materializan como UNKNOWN.
pub fn evaluate(
    observations: &[HardGate],
    limitations: &[String],
) -> Result<CompatibilityAssessment, CompatibilityEngineError> {
    let mut grouped = BTreeMap::<String, Vec<HardGate>>::new();
    for gate in observations {
        if gate.name.trim().is_empty() || gate.reason.trim().is_empty() {
            return Err(CompatibilityEngineError::InvalidGate);
        }
        if gate.status != GateStatus::Unknown && gate.evidence_refs.is_empty() {
            return Err(CompatibilityEngineError::EvidenceRequired(
                gate.name.clone(),
            ));
        }
        grouped
            .entry(gate.name.clone())
            .or_default()
            .push(gate.clone());
    }

    for name in REQUIRED_HARD_GATES {
        grouped.entry(name.to_owned()).or_insert_with(|| {
            vec![HardGate {
                name: name.to_owned(),
                status: GateStatus::Unknown,
                reason: MISSING_GATE_REASON.to_owned(),
                evidence_refs: Vec::new(),
            }]
        });
    }

    let hard_gates: Vec<HardGate> = grouped
        .into_iter()
        .map(|(name, gates)| merge_gate(name, gates))
        .collect();
    let mut normalized_limitations = BTreeSet::new();
    for limitation in limitations {
        let normalized = limitation.trim();
        if normalized.is_empty() {
            return Err(CompatibilityEngineError::InvalidLimitation);
        }
        normalized_limitations.insert(normalized.to_owned());
    }
    let limitations: Vec<String> = normalized_limitations.into_iter().collect();

    let status = if hard_gates
        .iter()
        .any(|gate| gate.status == GateStatus::Fail)
    {
        CompatibilityStatus::Incompatible
    } else if hard_gates
        .iter()
        .any(|gate| gate.status == GateStatus::Unknown)
    {
        CompatibilityStatus::Unknown
    } else if limitations.is_empty() {
        CompatibilityStatus::Compatible
    } else {
        CompatibilityStatus::CompatibleWithLimitations
    };
    let evidence_refs = hard_gates
        .iter()
        .flat_map(|gate| gate.evidence_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(CompatibilityAssessment {
        status,
        hard_gates,
        limitations,
        technical_fit: None,
        evidence_refs,
    })
}

/// Materializa la forma contractual después de evaluar todas las hard gates.
/// Identificadores, timestamps y metadata proceden de la capa que persiste.
pub fn to_record(
    id: String,
    created_at: String,
    updated_at: String,
    row_version: i64,
    metadata: Map<String, Value>,
    key: CompatibilityCacheKey,
    assessment: CompatibilityAssessment,
) -> Result<CompatibilityRecord, CompatibilityEngineError> {
    let record = CompatibilityRecord {
        id,
        created_at,
        updated_at,
        row_version,
        metadata,
        variant_id: key.variant_id,
        runtime_version_id: key.runtime_version_id,
        backend_id: key.backend_id,
        hardware_fingerprint: key.hardware_fingerprint,
        launch_profile_hash: key.launch_profile_hash,
        status: assessment.status,
        hard_gates: assessment.hard_gates,
        limitations: assessment.limitations,
        technical_fit: assessment.technical_fit,
        evidence_refs: assessment.evidence_refs,
    };
    record.validate()?;
    Ok(record)
}

fn merge_gate(name: String, gates: Vec<HardGate>) -> HardGate {
    let status = if gates.iter().any(|gate| gate.status == GateStatus::Fail) {
        GateStatus::Fail
    } else if gates.iter().any(|gate| gate.status == GateStatus::Unknown) {
        GateStatus::Unknown
    } else {
        GateStatus::Pass
    };
    let reason = gates
        .iter()
        .map(|gate| gate.reason.trim())
        .filter(|reason| !reason.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ");
    let evidence_refs = gates
        .iter()
        .flat_map(|gate| gate.evidence_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    HardGate {
        name,
        status,
        reason,
        evidence_refs,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use uuid::Uuid;

    fn complete_gates() -> Vec<HardGate> {
        REQUIRED_HARD_GATES
            .iter()
            .map(|name| HardGate {
                name: (*name).into(),
                status: GateStatus::Pass,
                reason: format!("{name} confirmado"),
                evidence_refs: vec![format!("fixture:{name}")],
            })
            .collect()
    }

    fn set(gates: &mut [HardGate], name: &str, status: GateStatus, reason: &str) {
        let gate = gates.iter_mut().find(|gate| gate.name == name).unwrap();
        gate.status = status;
        gate.reason = reason.into();
        gate.evidence_refs = vec![format!("fixture:{name}:negative")];
    }

    #[test]
    fn format_fail_is_incompatible_and_never_scored() {
        let mut gates = complete_gates();
        set(
            &mut gates,
            "format",
            GateStatus::Fail,
            "formato no soportado",
        );
        let result = evaluate(&gates, &[]).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Incompatible);
        assert_eq!(result.technical_fit, None);
    }

    #[test]
    fn architecture_unknown_stays_unknown() {
        let mut gates = complete_gates();
        let arch = gates
            .iter_mut()
            .find(|gate| gate.name == "architecture")
            .unwrap();
        arch.status = GateStatus::Unknown;
        arch.reason = "la arquitectura no está observada".into();
        arch.evidence_refs.clear();
        let result = evaluate(&gates, &[]).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Unknown);
        assert_eq!(result.technical_fit, None);
    }

    #[test]
    fn missing_backend_is_fail_closed() {
        let mut gates = complete_gates();
        set(
            &mut gates,
            "backend_operable",
            GateStatus::Fail,
            "backend inexistente",
        );
        let result = evaluate(&gates, &[]).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Incompatible);
    }

    #[test]
    fn insufficient_memory_and_incomplete_bundle_are_incompatible() {
        for (name, reason) in [
            ("memory", "memoria insuficiente"),
            ("bundle_integrity", "falta un artefacto requerido"),
        ] {
            let mut gates = complete_gates();
            set(&mut gates, name, GateStatus::Fail, reason);
            assert_eq!(
                evaluate(&gates, &[]).unwrap().status,
                CompatibilityStatus::Incompatible
            );
        }
    }

    #[test]
    fn missing_required_gate_becomes_unknown_and_extra_gates_are_retained() {
        let gates: Vec<_> = complete_gates()
            .into_iter()
            .filter(|gate| gate.name != "protocol")
            .chain([HardGate {
                name: "custom_pipeline".into(),
                status: GateStatus::Pass,
                reason: "pipeline observado".into(),
                evidence_refs: vec!["fixture:pipeline".into()],
            }])
            .collect();
        let result = evaluate(&gates, &[]).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Unknown);
        let protocol = result
            .hard_gates
            .iter()
            .find(|gate| gate.name == "protocol")
            .unwrap();
        assert_eq!(protocol.status, GateStatus::Unknown);
        assert!(result
            .hard_gates
            .iter()
            .any(|gate| gate.name == "custom_pipeline"));
    }

    #[test]
    fn conflicts_and_limitations_are_resolved_deterministically() {
        let mut gates = complete_gates();
        gates.push(HardGate {
            name: "format".into(),
            status: GateStatus::Unknown,
            reason: "adapter sin probe".into(),
            evidence_refs: vec![],
        });
        let first = evaluate(
            &gates,
            &["sin GPU dedicada".into(), "sin GPU dedicada".into()],
        )
        .unwrap();
        let mut shuffled = gates.clone();
        shuffled.reverse();
        let second = evaluate(&shuffled, &["sin GPU dedicada".into()]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.status, CompatibilityStatus::Unknown);
        assert_eq!(first.limitations, vec!["sin GPU dedicada"]);
        assert_eq!(first.technical_fit, None);
        assert_eq!(
            first
                .hard_gates
                .iter()
                .find(|gate| gate.name == "format")
                .unwrap()
                .status,
            GateStatus::Unknown
        );
    }

    #[test]
    fn cache_key_binds_all_five_dimensions() {
        let ids: Vec<_> = (0..3).map(|_| Uuid::now_v7().to_string()).collect();
        let key =
            CompatibilityCacheKey::new(&ids[0], &ids[1], &ids[2], "a".repeat(64), "b".repeat(64))
                .unwrap();
        assert_eq!(key.fingerprint().unwrap(), key.fingerprint().unwrap());
        let changed =
            CompatibilityCacheKey::new(&ids[0], &ids[1], &ids[2], "c".repeat(64), "b".repeat(64))
                .unwrap();
        assert_ne!(key, changed);
        assert_ne!(key.fingerprint().unwrap(), changed.fingerprint().unwrap());
        assert_eq!(
            serde_json::to_value(key)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            serde_json::to_value(GateStatus::Unknown).unwrap(),
            "UNKNOWN"
        );
    }

    #[test]
    fn cache_lookup_misses_after_any_tuple_change_and_clear_removes_records() {
        let ids: Vec<_> = (0..4).map(|_| Uuid::now_v7().to_string()).collect();
        let key =
            CompatibilityCacheKey::new(&ids[0], &ids[1], &ids[2], "a".repeat(64), "b".repeat(64))
                .unwrap();
        let changed_profile =
            CompatibilityCacheKey::new(&ids[0], &ids[1], &ids[2], "a".repeat(64), "c".repeat(64))
                .unwrap();
        let cache = CompatibilityCache::default();
        let assessment = evaluate(&complete_gates(), &[]).unwrap();
        cache.insert(key.clone(), assessment.clone());
        assert_eq!(cache.get(&key), Some(assessment));
        assert_eq!(cache.get(&changed_profile), None);
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn assessment_materializes_the_normative_record_without_extra_fields() {
        let ids: Vec<_> = (0..4).map(|_| Uuid::now_v7().to_string()).collect();
        let key =
            CompatibilityCacheKey::new(&ids[1], &ids[2], &ids[3], "a".repeat(64), "b".repeat(64))
                .unwrap();
        let record = to_record(
            ids[0].clone(),
            "2026-10-22T12:00:00.000Z".into(),
            "2026-10-22T12:00:00.000Z".into(),
            1,
            Map::new(),
            key,
            evaluate(&complete_gates(), &[]).unwrap(),
        )
        .unwrap();
        assert_eq!(record.status, CompatibilityStatus::Compatible);
        assert_eq!(record.hard_gates.len(), REQUIRED_HARD_GATES.len());
        assert_eq!(
            serde_json::to_value(record)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            15
        );
    }

    #[test]
    fn pass_fail_require_evidence_and_blank_inputs_are_rejected() {
        let gate = HardGate {
            name: "format".into(),
            status: GateStatus::Pass,
            reason: "ok".into(),
            evidence_refs: vec![],
        };
        assert!(matches!(
            evaluate(&[gate], &[]),
            Err(CompatibilityEngineError::EvidenceRequired(_))
        ));
        assert_eq!(
            evaluate(&[], &["  ".into()]),
            Err(CompatibilityEngineError::InvalidLimitation)
        );
    }
}
