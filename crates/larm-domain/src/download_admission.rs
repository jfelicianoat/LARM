//! P119: política opt-in de descargas automáticas y evaluador determinista de
//! admisión. La política es opt-in del operador (allowlists de
//! fuente/capability/license/trust, cuota diaria de bytes y destino); la
//! evaluación es pura y determinista: una detección sólo se convierte en
//! auto-job BACKGROUND si supera las seis reglas; cualquier fallo produce una
//! admisión BLOCKED con motivo estable y SIN job. El watcher por defecto sólo
//! notifica.
use crate::bridge::{is_uuid_v7, timestamp};
use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Política de admisión de descargas no válida",
        reason,
    )
}

/// Estados de `Trust` que la política acepta como mínimo, en orden de
/// excelencia (§ de linaje): el trust de la variante debe alcanzarlo.
pub const TRUST_LADDER: [&str; 3] = ["CANDIDATE", "TECHNICALLY_APPROVED", "EVALUATION_APPROVED"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadAdmissionPolicy {
    pub enabled: bool,
    pub source_allowlist: Vec<String>,
    pub capability_allowlist: Vec<String>,
    pub license_allowlist: Vec<String>,
    pub min_trust: String,
    /// Cuota diaria de bytes agregada; `None` sin límite.
    #[serde(deserialize_with = "Option::deserialize")]
    pub daily_quota_bytes: Option<Decimal>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub storage_location_id: Option<String>,
}

/// Decimal contractual como string (`DecimalU64`).
pub type Decimal = String;

impl DownloadAdmissionPolicy {
    pub fn validate(&self) -> Result<(), LarmError> {
        for source in &self.source_allowlist {
            if !is_uuid_v7(source) {
                return Err(invalid("admission_policy_source_invalid"));
            }
        }
        for capability in &self.capability_allowlist {
            if capability.is_empty() || capability.len() > 256 {
                return Err(invalid("admission_policy_capability_invalid"));
            }
        }
        for license in &self.license_allowlist {
            if license.is_empty() || license.len() > 256 {
                return Err(invalid("admission_policy_license_invalid"));
            }
        }
        if !TRUST_LADDER.contains(&self.min_trust.as_str()) {
            return Err(invalid("admission_policy_min_trust_invalid"));
        }
        if let Some(quota) = &self.daily_quota_bytes {
            if quota.is_empty()
                || quota.len() > 19
                || !quota.bytes().all(|byte| byte.is_ascii_digit())
                || quota
                    .parse::<u128>()
                    .map_err(|_| invalid("admission_policy_quota_invalid"))?
                    > u64::MAX as u128
            {
                return Err(invalid("admission_policy_quota_invalid"));
            }
        }
        Ok(())
    }
}

/// Candidato de admisión: lo que el watcher observa de la revisión detectada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionCandidate {
    pub variant_id: String,
    pub source_id: String,
    pub capability: String,
    pub license_id: Option<String>,
    pub trust: String,
    pub estimated_bytes: u64,
}

/// Evaluación determinista. `daily_used_bytes` agrega las admisiones
/// ADMITTED del mismo `day_key`; `day_key` es la fecha UTC del intento.
pub fn evaluate_admission(
    policy: &DownloadAdmissionPolicy,
    candidate: &AdmissionCandidate,
    daily_used_bytes: u64,
    day_key: &str,
) -> Result<AdmissionDecision, LarmError> {
    policy.validate()?;
    let deny = |reason: &str| AdmissionDecision {
        decision: "BLOCKED",
        reason: reason.to_string(),
        priority: "BACKGROUND",
        day_key: day_key.to_string(),
    };
    if !policy.enabled {
        return Ok(deny("admission_policy_disabled"));
    }
    if !policy
        .source_allowlist
        .iter()
        .any(|allowed| allowed == &candidate.source_id)
    {
        return Ok(deny("admission_source_not_allowlisted"));
    }
    if !policy
        .capability_allowlist
        .iter()
        .any(|allowed| allowed == &candidate.capability)
    {
        return Ok(deny("admission_capability_not_allowlisted"));
    }
    match (&candidate.license_id, policy.license_allowlist.as_slice()) {
        (Some(license), allowlist) if allowlist.iter().any(|allowed| allowed == license) => {}
        (Some(_), _) => return Ok(deny("admission_license_not_allowlisted")),
        (None, []) => {}
        (None, _) => return Ok(deny("admission_license_missing")),
    }
    let candidate_rank = TRUST_LADDER
        .iter()
        .position(|level| *level == candidate.trust.as_str())
        .ok_or_else(|| invalid("admission_trust_invalid"))?;
    let Some(minimum_rank) = TRUST_LADDER
        .iter()
        .position(|level| *level == policy.min_trust.as_str())
    else {
        return Err(invalid("admission_policy_min_trust_invalid"));
    };
    if candidate_rank < minimum_rank {
        return Ok(deny("admission_trust_below_minimum"));
    }
    if let Some(quota) = &policy.daily_quota_bytes {
        let quota_bytes: u64 = quota
            .parse()
            .map_err(|_| invalid("admission_policy_quota_invalid"))?;
        if daily_used_bytes.saturating_add(candidate.estimated_bytes) > quota_bytes {
            return Ok(deny("admission_quota_exhausted"));
        }
    }
    Ok(AdmissionDecision {
        decision: "ADMITTED",
        reason: "admission_authorized".into(),
        priority: "BACKGROUND",
        day_key: day_key.to_string(),
    })
}

/// Decisión de admisión derivada de la evaluación pura.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionDecision {
    pub decision: &'static str,
    pub reason: String,
    /// Las admitidas entran siempre BACKGROUND (nunca evita presupuestos
    /// eligiendo prioridad).
    pub priority: &'static str,
    pub day_key: String,
}

/// Registro del historial (DTO sellado `download_admission`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadAdmission {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub source_id: String,
    pub estimated_bytes: String,
    pub decision: String,
    pub reason: String,
    pub policy_row_version: i64,
    pub day_key: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub job_id: Option<String>,
}

impl DownloadAdmission {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.variant_id) {
            return Err(invalid("admission_variant_invalid"));
        }
        if self.source_id.is_empty() || self.source_id.len() > 256 {
            return Err(invalid("admission_source_invalid"));
        }
        if self.estimated_bytes.is_empty()
            || !self
                .estimated_bytes
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid("admission_bytes_invalid"));
        }
        if !matches!(self.decision.as_str(), "ADMITTED" | "BLOCKED" | "REVOKED") {
            return Err(invalid("admission_decision_invalid"));
        }
        if self.reason.is_empty() {
            return Err(invalid("admission_reason_invalid"));
        }
        Ok(())
    }
}

/// Registro la marca de tiempo del dominio (helper de bridge reexportado).
#[must_use]
pub fn admission_now() -> String {
    timestamp(chrono::Utc::now())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn policy() -> DownloadAdmissionPolicy {
        DownloadAdmissionPolicy {
            enabled: true,
            source_allowlist: vec!["0192aaaa-0000-7000-8000-00000000aa01".into()],
            capability_allowlist: vec!["vendor.future".into()],
            license_allowlist: vec!["apache-2.0".into()],
            min_trust: "TECHNICALLY_APPROVED".into(),
            daily_quota_bytes: Some("1000".into()),
            storage_location_id: Some("0192aaaa-0000-7000-8000-00000000aa02".into()),
        }
    }

    fn candidate() -> AdmissionCandidate {
        AdmissionCandidate {
            variant_id: "0192aaaa-0000-7000-8000-00000000aa03".into(),
            source_id: "0192aaaa-0000-7000-8000-00000000aa01".into(),
            capability: "vendor.future".into(),
            license_id: Some("apache-2.0".into()),
            trust: "TECHNICALLY_APPROVED".into(),
            estimated_bytes: 400,
        }
    }

    #[test]
    fn candidato_en_allowlist_se_admite_como_background() {
        let decision = evaluate_admission(&policy(), &candidate(), 0, "2026-11-16").unwrap();
        assert_eq!(decision.decision, "ADMITTED");
        assert_eq!(decision.priority, "BACKGROUND");
        assert_eq!(decision.reason, "admission_authorized");
    }

    #[test]
    fn politica_deshabilitada_solo_notifica() {
        let mut policy_fixture = policy();
        policy_fixture.enabled = false;
        let policy = policy_fixture;
        let decision = evaluate_admission(&policy, &candidate(), 0, "2026-11-16").unwrap();
        assert_eq!(decision.decision, "BLOCKED");
        assert_eq!(decision.reason, "admission_policy_disabled");
    }

    #[test]
    fn fuente_fuera_de_allowlist_bloquea() {
        let mut policy_fixture = policy();
        policy_fixture.source_allowlist = vec!["0192bbbb-0000-7000-8000-00000000aa07".into()];
        let policy = policy_fixture;
        let decision = evaluate_admission(&policy, &candidate(), 0, "2026-11-16").unwrap();
        assert_eq!(decision.reason, "admission_source_not_allowlisted");
    }

    #[test]
    fn capability_y_license_separados_bloquean() {
        let mut policy_fixture = policy();
        policy_fixture.capability_allowlist = vec!["vendor.other".into()];
        let policy_no_cap = &policy_fixture;
        assert_eq!(
            evaluate_admission(policy_no_cap, &candidate(), 0, "d")
                .unwrap()
                .reason,
            "admission_capability_not_allowlisted"
        );
        let mut policy_fixture = policy();
        policy_fixture.license_allowlist = vec!["mit".into()];
        let policy_no_lic = &policy_fixture;
        assert_eq!(
            evaluate_admission(policy_no_lic, &candidate(), 0, "d")
                .unwrap()
                .reason,
            "admission_license_not_allowlisted"
        );
        let mut candidate_fixture = candidate();
        candidate_fixture.license_id = None;
        let candidate_no_license = &candidate_fixture;
        let mut policy_fixture = policy();
        policy_fixture.license_allowlist = vec!["mit".into()];
        assert_eq!(
            evaluate_admission(&policy_fixture, candidate_no_license, 0, "d")
                .unwrap()
                .reason,
            "admission_license_missing"
        );
    }

    #[test]
    fn trust_por_debajo_del_minimo_bloquea_y_desconocido_es_error() {
        let mut candidate_fixture = candidate();
        candidate_fixture.trust = "CANDIDATE".into();
        assert_eq!(
            evaluate_admission(&policy(), &candidate_fixture, 0, "d")
                .unwrap()
                .reason,
            "admission_trust_below_minimum"
        );
        let mut candidate_fixture = candidate();
        candidate_fixture.trust = "EVALUATION_APPROVED".into();
        assert_eq!(
            evaluate_admission(&policy(), &candidate_fixture, 0, "d")
                .unwrap()
                .decision,
            "ADMITTED"
        );
        let mut candidate_fixture = candidate();
        candidate_fixture.trust = "DESCONOCIDO".into();
        assert!(evaluate_admission(&policy(), &candidate_fixture, 0, "d").is_err());
    }

    #[test]
    fn cuota_diaria_agregada_bloquea_el_excedente() {
        assert_eq!(
            evaluate_admission(&policy(), &candidate(), 400, "2026-11-16")
                .unwrap()
                .decision,
            "ADMITTED"
        );
        let decision = evaluate_admission(&policy(), &candidate(), 601, "2026-11-16").unwrap();
        assert_eq!(decision.reason, "admission_quota_exhausted");
    }

    #[test]
    fn politica_invalida_falla_cerrado() {
        let mut invalid_trust = policy();
        invalid_trust.min_trust = "READY".into();
        assert!(evaluate_admission(&invalid_trust, &candidate(), 0, "d").is_err());
        let mut invalid_quota = policy();
        invalid_quota.daily_quota_bytes = Some("-5".into());
        assert!(evaluate_admission(&invalid_quota, &candidate(), 0, "d").is_err());
    }

    #[test]
    fn download_admission_valida_campos_sellados() {
        let mut admission = DownloadAdmission {
            id: "0192aaaa-0000-7000-8000-00000000aa05".into(),
            created_at: "2026-11-16T00:00:00.000Z".into(),
            updated_at: "2026-11-16T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            variant_id: "0192aaaa-0000-7000-8000-00000000aa03".into(),
            source_id: "0192aaaa-0000-7000-8000-00000000aa01".into(),
            estimated_bytes: "400".into(),
            decision: "ADMITTED".into(),
            reason: "admission_authorized".into(),
            policy_row_version: 1,
            day_key: "2026-11-16".into(),
            job_id: Some("0192aaaa-0000-7000-8000-00000000aa06".into()),
        };
        admission.validate().unwrap();
        admission.decision = "READY".into();
        assert!(admission.validate().is_err());
    }
}
