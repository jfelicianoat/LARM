//! Derivación estricta de availability y elegibilidad (P042).
//!
//! `AVAILABLE` requiere artefactos requeridos con bytes verificados, bundle
//! verificado, todos los gates aprobados y smoke vigente en la misma huella
//! de ejecución. La elegibilidad del Broker añade la cualificación de
//! protocolo y no cambia el lifecycle guardado.

use larm_domain::model::{ModelAvailability, ModelLifecycle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaEvidence {
    OnlineVerified,
    OnlineUnverified,
    Offline,
    Missing,
    Corrupt,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredArtifactEvidence {
    /// Réplicas conocidas del artefacto. Una sola copia online y verificada
    /// basta para satisfacer este requisito.
    pub replicas: Vec<ReplicaEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateEvidence {
    Pass,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintedCheck {
    pub result: GateEvidence,
    pub execution_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilityEvidence {
    pub lifecycle: ModelLifecycle,
    pub required_artifacts: Vec<RequiredArtifactEvidence>,
    pub bundle_verified: bool,
    pub model_gate: GateEvidence,
    pub runtime_gate: GateEvidence,
    pub backend_gate: GateEvidence,
    pub hardware_gate: GateEvidence,
    pub execution_fingerprint: Option<String>,
    pub load_check: FingerprintedCheck,
    pub health_check: FingerprintedCheck,
    pub smoke_check: FingerprintedCheck,
    /// Debe proceder de una observación explícita de degradación, nunca de
    /// metadata descriptiva.
    pub degraded: bool,
    pub protocol_qualified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedAvailability {
    pub availability: ModelAvailability,
    pub broker_eligible: bool,
}

fn lifecycle_is_preparing(lifecycle: ModelLifecycle) -> bool {
    matches!(
        lifecycle,
        ModelLifecycle::Selected
            | ModelLifecycle::DownloadPlanned
            | ModelLifecycle::Downloading
            | ModelLifecycle::Downloaded
            | ModelLifecycle::Verified
            | ModelLifecycle::Prepared
            | ModelLifecycle::TechnicallyValidated
    )
}

fn all_required_artifacts_verified(evidence: &[RequiredArtifactEvidence]) -> bool {
    !evidence.is_empty()
        && evidence
            .iter()
            .all(|artifact| artifact.replicas.contains(&ReplicaEvidence::OnlineVerified))
}

fn has_corrupt_required_artifact(evidence: &[RequiredArtifactEvidence]) -> bool {
    evidence.iter().any(|artifact| {
        !artifact.replicas.contains(&ReplicaEvidence::OnlineVerified)
            && artifact.replicas.contains(&ReplicaEvidence::Corrupt)
    })
}

fn has_unavailable_required_artifact(evidence: &[RequiredArtifactEvidence]) -> bool {
    evidence.iter().any(|artifact| {
        !artifact.replicas.contains(&ReplicaEvidence::OnlineVerified)
            && (artifact.replicas.contains(&ReplicaEvidence::Offline)
                || artifact.replicas.contains(&ReplicaEvidence::Missing))
    })
}

/// Calcula availability sin consultar ni interpretar `metadata`.
#[must_use]
pub fn derive_availability(evidence: &AvailabilityEvidence) -> DerivedAvailability {
    let current_fingerprint = evidence
        .execution_fingerprint
        .as_deref()
        .filter(|fingerprint| !fingerprint.is_empty());
    let checks = [
        &evidence.load_check,
        &evidence.health_check,
        &evidence.smoke_check,
    ];
    let checks_match = current_fingerprint.is_some_and(|fingerprint| {
        checks
            .iter()
            .all(|check| check.execution_fingerprint.as_deref() == Some(fingerprint))
    });
    let checks_pass = checks_match
        && checks
            .iter()
            .all(|check| check.result == GateEvidence::Pass);
    let checks_fail = checks_match
        && checks
            .iter()
            .any(|check| check.result == GateEvidence::Fail);
    let availability = if evidence.lifecycle == ModelLifecycle::Corrupt
        || has_corrupt_required_artifact(&evidence.required_artifacts)
    {
        ModelAvailability::Corrupt
    } else if has_unavailable_required_artifact(&evidence.required_artifacts) {
        ModelAvailability::Offline
    } else if [
        evidence.model_gate,
        evidence.runtime_gate,
        evidence.backend_gate,
        evidence.hardware_gate,
    ]
    .contains(&GateEvidence::Fail)
        || checks_fail
    {
        ModelAvailability::Incompatible
    } else if lifecycle_is_preparing(evidence.lifecycle) {
        ModelAvailability::Preparing
    } else if evidence.lifecycle == ModelLifecycle::Failed
        || evidence.lifecycle == ModelLifecycle::Rejected
        || !all_required_artifacts_verified(&evidence.required_artifacts)
        || !evidence.bundle_verified
        || [
            evidence.model_gate,
            evidence.runtime_gate,
            evidence.backend_gate,
            evidence.hardware_gate,
        ]
        .contains(&GateEvidence::Unknown)
        || !checks_pass
        || !matches!(
            evidence.lifecycle,
            ModelLifecycle::Available | ModelLifecycle::Running
        )
    {
        ModelAvailability::Unknown
    } else if evidence.degraded {
        ModelAvailability::Degraded
    } else {
        ModelAvailability::Available
    };

    let broker_eligible =
        availability == ModelAvailability::Available && evidence.protocol_qualified;
    DerivedAvailability {
        availability,
        broker_eligible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qualified() -> AvailabilityEvidence {
        let fingerprint = "a".repeat(64);
        AvailabilityEvidence {
            lifecycle: ModelLifecycle::Available,
            required_artifacts: vec![RequiredArtifactEvidence {
                replicas: vec![ReplicaEvidence::OnlineVerified],
            }],
            bundle_verified: true,
            model_gate: GateEvidence::Pass,
            runtime_gate: GateEvidence::Pass,
            backend_gate: GateEvidence::Pass,
            hardware_gate: GateEvidence::Pass,
            execution_fingerprint: Some(fingerprint.clone()),
            load_check: FingerprintedCheck {
                result: GateEvidence::Pass,
                execution_fingerprint: Some(fingerprint.clone()),
            },
            health_check: FingerprintedCheck {
                result: GateEvidence::Pass,
                execution_fingerprint: Some(fingerprint.clone()),
            },
            smoke_check: FingerprintedCheck {
                result: GateEvidence::Pass,
                execution_fingerprint: Some(fingerprint),
            },
            degraded: false,
            protocol_qualified: true,
        }
    }

    #[test]
    fn broker_requires_protocol_qualification_after_available() {
        let mut evidence = qualified();
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Available,
                broker_eligible: true,
            }
        );
        evidence.protocol_qualified = false;
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Available,
                broker_eligible: false,
            }
        );
    }

    #[test]
    fn offline_and_corrupt_artifacts_revoke_availability() {
        let mut evidence = qualified();
        evidence.required_artifacts[0].replicas = vec![ReplicaEvidence::Offline];
        assert_eq!(
            derive_availability(&evidence).availability,
            ModelAvailability::Offline
        );
        evidence.required_artifacts[0].replicas = vec![ReplicaEvidence::Corrupt];
        assert_eq!(
            derive_availability(&evidence).availability,
            ModelAvailability::Corrupt
        );
    }

    #[test]
    fn partial_unknown_evidence_never_qualifies() {
        let mut evidence = qualified();
        evidence.hardware_gate = GateEvidence::Unknown;
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Unknown,
                broker_eligible: false,
            }
        );
    }

    #[test]
    fn load_health_and_smoke_must_share_current_execution_fingerprint() {
        let mut evidence = qualified();
        evidence.health_check.execution_fingerprint = Some("b".repeat(64));
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Unknown,
                broker_eligible: false,
            }
        );
        evidence.health_check.execution_fingerprint = evidence.execution_fingerprint.clone();
        evidence.smoke_check.result = GateEvidence::Fail;
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Incompatible,
                broker_eligible: false,
            }
        );
    }

    #[test]
    fn empty_required_artifact_set_cannot_qualify() {
        let mut evidence = qualified();
        evidence.required_artifacts.clear();
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Unknown,
                broker_eligible: false,
            }
        );
    }

    #[test]
    fn explicit_degradation_does_not_become_broker_eligible() {
        let mut evidence = qualified();
        evidence.degraded = true;
        assert_eq!(
            derive_availability(&evidence),
            DerivedAvailability {
                availability: ModelAvailability::Degraded,
                broker_eligible: false,
            }
        );
    }
}
