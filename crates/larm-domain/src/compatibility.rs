//! Contrato de compatibilidad (`domain.schema.json#/$defs/compatibility_record`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bridge::is_uuid_v7;

/// Gates mínimos del contrato 04 que deben materializarse en todo registro.
pub const REQUIRED_HARD_GATE_NAMES: [&str; 13] = [
    "format",
    "architecture",
    "ops",
    "quantization",
    "isa",
    "os",
    "drivers",
    "execution_provider",
    "license_trust",
    "bundle_integrity",
    "memory",
    "protocol",
    "backend_operable",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateStatus {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "FAIL")]
    Fail,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl GateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardGate {
    pub name: String,
    pub status: GateStatus,
    pub reason: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityStatus {
    #[serde(rename = "COMPATIBLE")]
    Compatible,
    #[serde(rename = "COMPATIBLE_WITH_LIMITATIONS")]
    CompatibleWithLimitations,
    #[serde(rename = "EXPERIMENTAL")]
    Experimental,
    #[serde(rename = "CONVERSION_REQUIRED")]
    ConversionRequired,
    #[serde(rename = "RUNTIME_REQUIRED")]
    RuntimeRequired,
    #[serde(rename = "BACKEND_REQUIRED")]
    BackendRequired,
    #[serde(rename = "INCOMPATIBLE")]
    Incompatible,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl CompatibilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "COMPATIBLE",
            Self::CompatibleWithLimitations => "COMPATIBLE_WITH_LIMITATIONS",
            Self::Experimental => "EXPERIMENTAL",
            Self::ConversionRequired => "CONVERSION_REQUIRED",
            Self::RuntimeRequired => "RUNTIME_REQUIRED",
            Self::BackendRequired => "BACKEND_REQUIRED",
            Self::Incompatible => "INCOMPATIBLE",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Representación exacta de la entidad normativa. No incorpora campos de
/// motor ni timestamps alternativos que no estén en el schema publicado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityRecord {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub runtime_version_id: String,
    pub backend_id: String,
    pub hardware_fingerprint: String,
    pub launch_profile_hash: String,
    pub status: CompatibilityStatus,
    pub hard_gates: Vec<HardGate>,
    pub limitations: Vec<String>,
    pub technical_fit: Option<f64>,
    pub evidence_refs: Vec<String>,
}

impl CompatibilityRecord {
    /// Valida los invariantes de identidad y coherencia que acompañan al
    /// schema JSON cerrado antes de persistir o publicar el registro.
    pub fn validate(&self) -> Result<(), CompatibilityValidationError> {
        for (field, value) in [
            ("id", self.id.as_str()),
            ("variant_id", self.variant_id.as_str()),
            ("runtime_version_id", self.runtime_version_id.as_str()),
            ("backend_id", self.backend_id.as_str()),
        ] {
            if !is_uuid_v7(value) {
                return Err(CompatibilityValidationError::InvalidUuid(field));
            }
        }
        for (field, value) in [
            ("hardware_fingerprint", self.hardware_fingerprint.as_str()),
            ("launch_profile_hash", self.launch_profile_hash.as_str()),
        ] {
            if !is_sha256(value) {
                return Err(CompatibilityValidationError::InvalidSha256(field));
            }
        }
        if self.row_version < 1 {
            return Err(CompatibilityValidationError::InvalidRowVersion);
        }
        if !is_contract_timestamp(&self.created_at) || !is_contract_timestamp(&self.updated_at) {
            return Err(CompatibilityValidationError::InvalidTimestamp);
        }
        if self
            .technical_fit
            .is_some_and(|score| !score.is_finite() || !(0.0..=100.0).contains(&score))
        {
            return Err(CompatibilityValidationError::InvalidTechnicalFit);
        }
        let mut names = std::collections::BTreeSet::new();
        for gate in &self.hard_gates {
            if gate.name.trim().is_empty() || gate.reason.trim().is_empty() {
                return Err(CompatibilityValidationError::InvalidGate);
            }
            if gate.status != GateStatus::Unknown && gate.evidence_refs.is_empty() {
                return Err(CompatibilityValidationError::GateEvidenceRequired);
            }
            if !names.insert(gate.name.as_str()) {
                return Err(CompatibilityValidationError::DuplicateGate);
            }
        }
        if self
            .limitations
            .iter()
            .any(|limitation| limitation.trim().is_empty())
        {
            return Err(CompatibilityValidationError::InvalidLimitation);
        }
        if REQUIRED_HARD_GATE_NAMES
            .iter()
            .any(|required| !names.contains(*required))
        {
            return Err(CompatibilityValidationError::MissingRequiredGate);
        }

        let failed = self
            .hard_gates
            .iter()
            .any(|gate| gate.status == GateStatus::Fail);
        let unknown = self
            .hard_gates
            .iter()
            .any(|gate| gate.status == GateStatus::Unknown);
        if self.technical_fit.is_some() && (failed || unknown) {
            return Err(CompatibilityValidationError::ScoreWithoutAllPass);
        }
        match self.status {
            CompatibilityStatus::Compatible
                if failed || unknown || !self.limitations.is_empty() =>
            {
                return Err(CompatibilityValidationError::StatusContradictsGates);
            }
            CompatibilityStatus::CompatibleWithLimitations
                if failed || unknown || self.limitations.is_empty() =>
            {
                return Err(CompatibilityValidationError::StatusContradictsGates);
            }
            CompatibilityStatus::Incompatible if !failed => {
                return Err(CompatibilityValidationError::StatusContradictsGates);
            }
            CompatibilityStatus::Unknown if failed || !unknown => {
                return Err(CompatibilityValidationError::StatusContradictsGates);
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CompatibilityValidationError {
    #[error("{0} debe ser UUIDv7 en minúsculas")]
    InvalidUuid(&'static str),
    #[error("{0} debe ser SHA-256 hexadecimal en minúsculas")]
    InvalidSha256(&'static str),
    #[error("row_version debe ser al menos 1")]
    InvalidRowVersion,
    #[error("timestamps deben usar UTC con milisegundos y Z")]
    InvalidTimestamp,
    #[error("technical_fit debe ser finito y estar entre 0 y 100")]
    InvalidTechnicalFit,
    #[error("cada gate requiere name y reason no vacíos")]
    InvalidGate,
    #[error("hard_gates no puede repetir nombres")]
    DuplicateGate,
    #[error("limitations no puede contener texto vacío")]
    InvalidLimitation,
    #[error("un gate PASS/FAIL requiere evidencia")]
    GateEvidenceRequired,
    #[error("el registro omite una puerta obligatoria")]
    MissingRequiredGate,
    #[error("technical_fit requiere todas las puertas PASS")]
    ScoreWithoutAllPass,
    #[error("status contradice el resultado de hard_gates")]
    StatusContradictsGates,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_contract_timestamp(value: &str) -> bool {
    value.len() == 24
        && chrono::DateTime::parse_from_rfc3339(value).is_ok_and(|parsed| {
            crate::bridge::timestamp(parsed.with_timezone(&chrono::Utc)) == value
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn record() -> CompatibilityRecord {
        let ids: Vec<String> = (0..4).map(|_| Uuid::now_v7().to_string()).collect();
        CompatibilityRecord {
            id: ids[0].clone(),
            created_at: "2026-10-22T12:00:00.000Z".into(),
            updated_at: "2026-10-22T12:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            variant_id: ids[1].clone(),
            runtime_version_id: ids[2].clone(),
            backend_id: ids[3].clone(),
            hardware_fingerprint: "a".repeat(64),
            launch_profile_hash: "b".repeat(64),
            status: CompatibilityStatus::Compatible,
            hard_gates: REQUIRED_HARD_GATE_NAMES
                .iter()
                .map(|name| HardGate {
                    name: (*name).into(),
                    status: GateStatus::Pass,
                    reason: format!("{name} confirmado"),
                    evidence_refs: vec![format!("probe:{name}")],
                })
                .collect(),
            limitations: vec![],
            technical_fit: None,
            evidence_refs: REQUIRED_HARD_GATE_NAMES
                .iter()
                .map(|name| format!("probe:{name}"))
                .collect(),
        }
    }

    #[test]
    fn compatibility_record_uses_exact_schema_fields_and_round_trips() {
        let record = record();
        record.validate().unwrap();
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 15);
        assert_eq!(value["status"], json!("COMPATIBLE"));
        assert!(serde_json::from_value::<CompatibilityRecord>(value).is_ok());
    }

    #[test]
    fn compatibility_record_rejects_invalid_identity_hash_timestamp_and_score() {
        let mut value = record();
        value.id = "not-v7".into();
        assert_eq!(
            value.validate(),
            Err(CompatibilityValidationError::InvalidUuid("id"))
        );

        let mut value = record();
        value.hardware_fingerprint = "G".repeat(64);
        assert_eq!(
            value.validate(),
            Err(CompatibilityValidationError::InvalidSha256(
                "hardware_fingerprint"
            ))
        );

        let mut value = record();
        value.created_at = "2026-10-22T12:00:00Z".into();
        assert_eq!(
            value.validate(),
            Err(CompatibilityValidationError::InvalidTimestamp)
        );

        let mut value = record();
        value.technical_fit = Some(40.0);
        let architecture = value
            .hard_gates
            .iter_mut()
            .find(|gate| gate.name == "architecture")
            .unwrap();
        architecture.status = GateStatus::Unknown;
        architecture.evidence_refs.clear();
        assert_eq!(
            value.validate(),
            Err(CompatibilityValidationError::ScoreWithoutAllPass)
        );
    }

    #[test]
    fn compatibility_record_rejects_unknown_fields_and_duplicate_gates() {
        let mut value = serde_json::to_value(record()).unwrap();
        value["engine_version"] = json!("1.0");
        assert!(serde_json::from_value::<CompatibilityRecord>(value).is_err());

        let mut value = record();
        value.hard_gates.push(value.hard_gates[0].clone());
        assert_eq!(
            value.validate(),
            Err(CompatibilityValidationError::DuplicateGate)
        );
    }
}
