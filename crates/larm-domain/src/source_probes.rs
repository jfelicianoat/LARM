//! P045: sondeos de autenticación de fuentes como jobs durables.

use crate::bridge::{is_uuid_v7, JobPhase, JobReceipt, JobState};
use crate::error::{ErrorCode, LarmError};
use crate::hardware::Probe;
use crate::ports::MutationContext;
use serde::{Deserialize, Serialize};

pub const KIND: &str = "source.auth_probe";
pub const OPERATION: &str = "P045_post__sources_id_probe";

/// Resumen seguro que conserva el resultado contractual sin detalle upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthProbeResult {
    pub authenticated: bool,
    pub auth_required: bool,
    pub license_required: bool,
    pub reason: String,
}

/// Solicitud y resultado del probe; no contiene valores secretos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProbeJob {
    pub id: String,
    pub target: String,
    pub actor_id: String,
    pub correlation_id: String,
    pub request: Probe,
    pub source_row_version: i64,
    pub phase: JobPhase,
    pub version: u8,
    pub result: Option<SourceAuthProbeResult>,
    pub error: Option<LarmError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_pending: Option<bool>,
}

impl SourceProbeJob {
    #[must_use]
    pub fn accepted(&self) -> JobReceipt {
        JobReceipt {
            job_id: self.id.clone(),
            status_url: format!("/api/v1/jobs/{}", self.id),
            state: JobPhase::Queued,
        }
    }

    #[must_use]
    pub fn context(&self) -> MutationContext {
        MutationContext {
            actor_id: self.actor_id.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }

    #[must_use]
    pub fn public_state(&self, recovery_required: bool) -> JobState {
        let mut progress = serde_json::Map::new();
        if recovery_required {
            progress.insert("recovery_required".into(), serde_json::json!(true));
        }
        if self.phase == JobPhase::Failed && self.authority_pending.unwrap_or(false) {
            progress.insert("revalidation_required".into(), serde_json::json!(true));
        }
        JobState {
            job_id: self.id.clone(),
            state: self.phase,
            resource_type: "model_source".into(),
            resource_id: Some(self.target.clone()),
            progress,
            result: self.result.as_ref().map(|r| {
                serde_json::json!({
                    "authenticated": r.authenticated,
                    "auth_required": r.auth_required,
                    "license_required": r.license_required,
                    "reason": r.reason,
                })
            }),
            error: self.error.clone(),
            evidence_refs: vec![],
        }
    }
}

pub fn validate_request(target: &str, request: &Probe) -> Result<(), LarmError> {
    if !is_uuid_v7(target)
        || request
            .resource_id
            .as_deref()
            .is_some_and(|id| id != target)
    {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Identificador de fuente no válido",
            "source_probe_resource_id_mismatch",
        ));
    }
    // No existe aún ningún perfil source de autenticación publicado.
    if request.profile_id.is_some() {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El perfil de sondeo de la fuente no está disponible",
            "source_probe_profile_unavailable",
        ));
    }
    Ok(())
}

/// No permite que un error del adaptador o del almacén de secretos llegue al API.
pub fn public_failure(error: &LarmError, correlation: &str) -> LarmError {
    let code = if matches!(error.code, ErrorCode::Other(_)) {
        ErrorCode::Internal
    } else {
        error.code.clone()
    };
    let mut safe = LarmError::new(
        code,
        "No se pudo completar el sondeo de autenticación de la fuente",
        "source_auth_probe_failed",
    )
    .retryable(error.retryable);
    safe.correlation_id = correlation.into();
    safe
}

#[derive(Debug)]
pub struct SourceProbeFailure {
    pub error: LarmError,
    pub authority_pending: bool,
}

impl SourceProbeFailure {
    #[must_use]
    pub fn before(error: LarmError) -> Self {
        Self {
            error,
            authority_pending: false,
        }
    }

    #[must_use]
    pub fn unconfirmed(error: LarmError) -> Self {
        Self {
            error,
            authority_pending: true,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn request() -> Probe {
        Probe {
            resource_id: None,
            profile_id: None,
        }
    }

    #[test]
    fn request_requires_source_identity_and_rejects_unknown_profile() {
        let source = "0192f000-0000-7000-8000-00000000f201";
        validate_request(source, &request()).unwrap();
        assert_eq!(
            validate_request("not-a-uuid", &request())
                .unwrap_err()
                .details
                .reason,
            "source_probe_resource_id_mismatch"
        );
        let mut mismatch = request();
        mismatch.resource_id = Some("0192f000-0000-7000-8000-00000000f202".into());
        assert_eq!(
            validate_request(source, &mismatch)
                .unwrap_err()
                .details
                .reason,
            "source_probe_resource_id_mismatch"
        );
        let mut unsupported = request();
        unsupported.profile_id = Some("unregistered".into());
        assert_eq!(
            validate_request(source, &unsupported)
                .unwrap_err()
                .details
                .reason,
            "source_probe_profile_unavailable"
        );
    }

    #[test]
    fn public_result_and_failure_never_expose_upstream_detail() {
        let error = LarmError::new(
            ErrorCode::Other("vendor_private_marker".into()),
            "token=must-not-escape",
            "upstream_error_must-not-escape",
        );
        let safe = public_failure(&error, "0192f000-0000-7000-8000-00000000f203");
        let text = serde_json::to_string(&safe).unwrap();
        assert!(!text.contains("must-not-escape"));
        assert_eq!(safe.details.reason, "source_auth_probe_failed");
    }
}
