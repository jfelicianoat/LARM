//! Tipos y transiciones durables de descargas (P051; contratos 01 y 02).

use crate::error::{ErrorCode, LarmError};
use crate::estimate::StorageEstimate;
use crate::model::ArtifactSizeEstimate;
use crate::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[path = "download_seal.rs"]
mod seal;
pub use seal::{plan_hash, PlanHashInput};

/// Prioridad persistida y aprobada dentro del plan de descarga.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DownloadPriority {
    Background,
    Normal,
    High,
}

/// Petición de preflight multi-destino (P052, contrato 3.1.0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPlanRequest {
    pub variant_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub storage_location_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_budget_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_id: Option<String>,
    pub priority: DownloadPriority,
    #[serde(default)]
    pub optional_artifact_ids: Vec<String>,
}

/// Tamaño y selección mostrados para un artefacto del bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadArtifactEstimate {
    pub artifact_id: String,
    pub relative_path: String,
    pub required: bool,
    pub selected: bool,
    pub size_estimate: ArtifactSizeEstimate,
    #[serde(deserialize_with = "Option::deserialize")]
    pub sha256: Option<String>,
}

/// Un destino comparado en el preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadDestinationOption {
    pub storage_location_id: String,
    pub selected: bool,
    pub eligible: bool,
    pub reason: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub storage_estimate: Option<StorageEstimate>,
}

/// Correspondencia de un artefacto del plan con su identidad en un mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadMirrorArtifact {
    pub target_artifact_id: String,
    pub source_artifact_id: String,
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: DecimalU64,
}

/// Fuente cualificada que puede entregar el bundle completo bajo la misma
/// identidad byte a byte del plan confirmado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadSourceCandidate {
    pub source_id: String,
    pub variant_id: String,
    pub revision_id: String,
    pub revision_key: String,
    pub manifest_hash: String,
    pub source_priority: u64,
    pub confidence: crate::model::ArtifactSizeConfidence,
    pub authenticated: bool,
    pub https: bool,
    pub artifacts: Vec<DownloadMirrorArtifact>,
}

/// Respuesta del preflight P052. Los campos de plan se completan cuando se
/// elige y persiste un destino elegible; el plan todavía no crea una descarga.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPlanResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<crate::acquisition::AcquisitionPlanBinding>,
    pub preview_id: String,
    pub preview_hash: String,
    pub variant_id: String,
    pub optional_artifact_ids: Vec<String>,
    pub artifact_estimates: Vec<DownloadArtifactEstimate>,
    pub destination_options: Vec<DownloadDestinationOption>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub selected_storage_location_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub plan_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub plan_hash: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub storage_estimate: Option<StorageEstimate>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_candidates: Vec<DownloadSourceCandidate>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_budget_bytes: Option<DecimalU64>,
    pub priority: DownloadPriority,
    pub expires_at: String,
    pub operator_confirmation_required: bool,
}

/// Payload conservado para revalidar y consumir un plan de descarga. La
/// instantánea de espacio se guarda junto con la respuesta, sin exponer datos
/// internos adicionales en el DTO HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPlanRecord {
    pub request: DownloadPlanRequest,
    pub response: DownloadPlanResponse,
    /// Referencias inmutables incluidas en el hash del preflight y verificadas
    /// de nuevo antes de consumirlo.
    pub revision_id: String,
    pub revision_key: String,
    pub manifest_hash: String,
    pub storage_observed_at: String,
}

impl DownloadPriority {
    /// Orden de cola: HIGH, NORMAL y BACKGROUND.
    #[must_use]
    pub const fn queue_rank(self) -> u8 {
        match self {
            Self::Background => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }
}

/// Estados durables del schema `download_job`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DownloadState {
    Planned,
    Queued,
    Running,
    Pausing,
    Paused,
    Verifying,
    Completed,
    Cancelling,
    Cancelled,
    Failed,
}

impl DownloadState {
    /// Transición ordinaria permitida por el contrato 01.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::Queued)
                | (Self::Queued, Self::Running)
                | (Self::Running, Self::Pausing)
                | (Self::Running, Self::Verifying)
                | (Self::Pausing, Self::Paused)
                | (Self::Paused, Self::Queued)
                | (Self::Verifying, Self::Completed)
                | (Self::Cancelling, Self::Cancelled)
                | (Self::Planned, Self::Cancelling)
                | (Self::Queued, Self::Cancelling)
                | (Self::Running, Self::Cancelling)
                | (Self::Pausing, Self::Cancelling)
                | (Self::Paused, Self::Cancelling)
                | (Self::Verifying, Self::Cancelling)
                | (Self::Failed, Self::Cancelling)
                | (Self::Planned, Self::Failed)
                | (Self::Queued, Self::Failed)
                | (Self::Running, Self::Failed)
                | (Self::Pausing, Self::Failed)
                | (Self::Paused, Self::Failed)
                | (Self::Verifying, Self::Failed)
        )
    }

    /// `FAILED` sólo vuelve a `QUEUED` con preflight nuevo y attempt + 1.
    #[must_use]
    pub fn can_retry(
        self,
        next: Self,
        attempt: u32,
        next_attempt: u32,
        preflight_valid: bool,
    ) -> bool {
        matches!((self, next), (Self::Failed, Self::Queued))
            && preflight_valid
            && attempt.checked_add(1) == Some(next_attempt)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

/// Registro público estricto `download_job`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadJob {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub storage_location_id: String,
    pub plan_hash: String,
    pub state: DownloadState,
    pub attempt: u32,
    pub progress: Map<String, Value>,
    pub error: Option<Map<String, Value>>,
    pub priority: DownloadPriority,
}

impl DownloadJob {
    /// Comprueba identidades y versión antes de persistir o devolver el registro.
    ///
    /// # Errors
    /// Devuelve `VALIDATION_FAILED` si la forma viola el schema del contrato.
    pub fn validate(&self) -> Result<(), LarmError> {
        if !crate::bridge::is_uuid_v7(&self.id)
            || !crate::bridge::is_uuid_v7(&self.variant_id)
            || !crate::bridge::is_uuid_v7(&self.storage_location_id)
            || !crate::adapter_updates::is_sha256(&self.plan_hash)
            || self.row_version < 1
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Registro de descarga no válido",
                "download_job_invalid",
            ));
        }
        Ok(())
    }
}

/// Petición de ejecución posterior a la confirmación explícita del plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutePlan {
    pub plan_id: String,
    pub plan_hash: String,
    pub operator_confirmed: bool,
}

/// Petición contractual común de pausa, reanudación y cancelación.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadStop {
    pub reason: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
}

/// Orden sobre un job existente; el cuerpo HTTP sigue siendo `Stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCommand {
    Pause,
    Resume,
    Cancel,
}

impl DownloadCommand {
    #[must_use]
    pub const fn operation_id(self) -> &'static str {
        match self {
            Self::Pause => "P051_post__downloads_id_pause",
            Self::Resume => "P051_post__downloads_id_resume",
            Self::Cancel => "P051_post__downloads_id_cancel",
        }
    }
}

/// La respuesta de control se confirma junto al estado, journal y outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadControlIdempotency {
    pub key: String,
    pub body_hash: String,
}

/// Cambio CAS de prioridad en un job persistido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPriorityUpdate {
    pub priority: DownloadPriority,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn prioridades_ordenan_sin_confundir_el_orden_de_cola() {
        assert!(DownloadPriority::High.queue_rank() > DownloadPriority::Normal.queue_rank());
        assert!(DownloadPriority::Normal.queue_rank() > DownloadPriority::Background.queue_rank());
        assert_eq!(
            serde_json::to_value(DownloadPriority::High).unwrap(),
            json!("HIGH")
        );
    }

    #[test]
    fn transiciones_durables_y_estados_terminales_son_estrictos() {
        assert!(DownloadState::Planned.can_transition_to(DownloadState::Queued));
        assert!(DownloadState::Running.can_transition_to(DownloadState::Pausing));
        assert!(DownloadState::Paused.can_transition_to(DownloadState::Queued));
        assert!(DownloadState::Verifying.can_transition_to(DownloadState::Completed));
        assert!(!DownloadState::Running.can_transition_to(DownloadState::Completed));
        assert!(!DownloadState::Completed.can_transition_to(DownloadState::Queued));
        assert!(!DownloadState::Cancelled.can_transition_to(DownloadState::Failed));
    }

    #[test]
    fn fallo_requiere_preflight_nuevo_y_attempt_incrementado() {
        assert!(!DownloadState::Failed.can_retry(DownloadState::Queued, 2, 3, false));
        assert!(!DownloadState::Failed.can_retry(DownloadState::Queued, 2, 2, true));
        assert!(DownloadState::Failed.can_retry(DownloadState::Queued, 2, 3, true));
        assert!(!DownloadState::Running.can_retry(DownloadState::Queued, 2, 3, true));
    }

    #[test]
    fn dto_rechaza_campos_no_contractuales() {
        let invalid = json!({"priority":"HIGH","unexpected":true});
        assert!(serde_json::from_value::<DownloadPriorityUpdate>(invalid).is_err());
    }
}
