//! Configuración interna del host; no es un veredicto ni un DTO de Probe.
use crate::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use crate::approval::ApprovalRecord;
use crate::conformance::hash;
use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};

pub const KIND: &str = "adapter.conformance.profile";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceProfile {
    pub id: String,
    pub installation_id: String,
    pub actor_id: String,
    pub manifest_sha256: String,
    /// Datos de una de las recetas compiladas. El host valida su forma cerrada.
    pub configuration: serde_json::Value,
    /// Snapshot anterior al consumo; la DB exige el consumo correspondiente.
    pub approval: ApprovalRecord,
    pub created_at: String,
}
pub fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Perfil de conformidad no válido",
        reason,
    )
}
impl ConformanceProfile {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !crate::bridge::is_uuid_v7(&self.id)
            || !crate::bridge::is_uuid_v7(&self.installation_id)
            || self.actor_id.is_empty()
            || self.actor_id.len() > 256
            || self.actor_id.chars().any(char::is_control)
            || !crate::adapter_updates::is_sha256(&self.manifest_sha256)
            || !self.configuration.is_object()
            || self.approval.actor_id != self.actor_id
            || !crate::bridge::is_uuid_v7(&self.approval.id)
            || self.approval.consumed_at.is_some()
            || chrono::DateTime::parse_from_rfc3339(&self.created_at).is_err()
        {
            return Err(invalid("conformance_profile_invalid"));
        }
        hash(self)?;
        Ok(())
    }
    pub fn check_installation(&self, a: &AdapterInstallation) -> Result<(), LarmError> {
        self.validate()?;
        if a.id != self.installation_id
            || hash(&a.manifest)? != self.manifest_sha256
            || self.approval.code_hash.as_deref() != Some(&a.package_hash)
            || self.approval.permissions.get("publisher") != a.manifest.get("publisher")
            || self.approval.permissions.get("permissions") != a.manifest.get("permissions")
            || a.trust == AdapterTrust::Revoked
            || !matches!(a.state, AdapterState::Installed | AdapterState::Validated)
        {
            return Err(invalid("conformance_profile_subject_changed"));
        }
        Ok(())
    }
}
