//! P026: solicitud pública Probe y trabajo durable. Nunca transporta PASS/suites.
use crate::adapters::AdapterInstallation;
use crate::bridge::{is_uuid_v7, JobPhase, JobReceipt, JobState};
use crate::conformance::{hash, Receipt};
use crate::error::{ErrorCode, LarmError};
use crate::hardware::Probe;
use serde::{Deserialize, Serialize};

pub const KIND: &str = "adapter.probe";
pub const OPERATION: &str = "P026_post__adapters_id_probe";

pub fn validate_request(target: &str, request: &Probe) -> Result<(), LarmError> {
    if !is_uuid_v7(target)
        || request
            .resource_id
            .as_deref()
            .is_some_and(|id| id != target)
    {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Identificador de sondeo no válido",
            "probe_resource_id_mismatch",
        ));
    }
    if request.profile_id.as_deref() == Some("") {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Perfil vacío",
            "probe_profile_invalid",
        ));
    }
    if request
        .profile_id
        .as_deref()
        .is_some_and(|id| id != "conformance.v1")
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "Perfil de sondeo no registrado",
            "probe_profile_unavailable",
        ));
    }
    Ok(())
}
pub fn snapshot(a: &AdapterInstallation) -> Result<String, LarmError> {
    let mut value = serde_json::to_value(a)
        .map_err(|_| crate::conformance::invalid("probe_snapshot_invalid"))?;
    value["row_version"] = serde_json::json!(a.row_version.to_string());
    hash(&value)
}
/// Sólo contiene referencias, snapshot de aceptación y resultados internos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeJob {
    pub id: String,
    pub target: String,
    pub actor_id: String,
    pub correlation_id: String,
    pub request: Probe,
    pub installation_sha256: String,
    pub phase: JobPhase,
    pub version: u8,
    pub receipt: Option<Receipt>,
    pub error: Option<LarmError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_pending: Option<bool>,
}
impl ProbeJob {
    pub fn accepted(&self) -> JobReceipt {
        JobReceipt {
            job_id: self.id.clone(),
            status_url: format!("/api/v1/jobs/{}", self.id),
            state: JobPhase::Queued,
        }
    }
    pub fn context(&self) -> crate::ports::MutationContext {
        crate::ports::MutationContext {
            actor_id: self.actor_id.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }
    pub fn public_state(&self, recovery_required: bool) -> JobState {
        let mut progress = serde_json::Map::new();
        if recovery_required {
            progress.insert("recovery_required".into(), serde_json::json!(true));
        }
        if self.phase == JobPhase::Failed
            && self.authority_pending.unwrap_or(self.receipt.is_none())
        {
            progress.insert("revalidation_required".into(), serde_json::json!(true));
        }
        JobState {
            job_id: self.id.clone(),
            state: self.phase,
            resource_type: "adapter_installation".into(),
            resource_id: Some(self.target.clone()),
            progress,
            result: self
                .receipt
                .as_ref()
                .map(|r| serde_json::json!({"conformance":r})),
            error: self.error.clone(),
            evidence_refs: vec![],
        }
    }
}
/// Un error público conserva la categoría conocida, sin mensajes/payloads privados.
pub fn public_failure(error: &LarmError, correlation: &str) -> LarmError {
    let code = if matches!(error.code, ErrorCode::Other(_)) {
        ErrorCode::Internal
    } else {
        error.code.clone()
    };
    let mut safe = LarmError::new(
        code,
        "No se pudo completar el sondeo del adaptador",
        "adapter_probe_failed",
    )
    .retryable(error.retryable);
    safe.correlation_id = correlation.into();
    safe
}

/// Error del ejecutor interno: distingue rechazo previo de commit no confirmado.
/// No es DTO ni acepta datos del cliente como autoridad.
#[derive(Debug)]
pub struct ProbeFailure {
    pub error: LarmError,
    pub authority_pending: bool,
}
impl ProbeFailure {
    pub fn before(error: LarmError) -> Self {
        Self {
            error,
            authority_pending: false,
        }
    }
    pub fn unconfirmed(error: LarmError) -> Self {
        Self {
            error,
            authority_pending: true,
        }
    }
}
