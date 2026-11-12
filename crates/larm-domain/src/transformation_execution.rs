//! Internal ownership of an admitted P093 attempt. These are not HTTP DTOs;
//! a stored token or a worker receipt does not prove that process IO is closed.
use crate::{
    adapter_updates::is_sha256,
    bridge::is_uuid_v7,
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    transformations::{
        SealedTransformationPlan, TransformationExecutionBinding, TransformationInputIdentity,
        TransformationInputLocation, TransformationJob,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationAttemptFence {
    pub job_id: String,
    pub claim_id: String,
    pub generation: u64,
    pub attempt: u32,
    pub plan_hash: String,
}
impl TransformationAttemptFence {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.job_id)
            || !is_uuid_v7(&self.claim_id)
            || self.generation > i64::MAX as u64
            || self.attempt == 0
            || !is_sha256(&self.plan_hash)
        {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El intento de transformación no es válido",
                "transformation_attempt_fence_invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TransformationClaim {
    pub job: TransformationJob,
    pub plan: SealedTransformationPlan,
    pub fence: TransformationAttemptFence,
    pub context: MutationContext,
    pub input_identity: TransformationInputIdentity,
    pub input_locations: Vec<TransformationInputLocation>,
}

/// A read-only proposal, not a claim or permission to execute. The repository
/// must compare the queued revision again while atomically registering an owner.
#[derive(Debug, Clone)]
pub struct TransformationClaimProposal {
    pub queued_row_version: i64,
    pub claim: TransformationClaim,
}

/// Public routing/identity metadata for a server-created native owner. This
/// record is never process/IO closure evidence; a live guard is still required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationNativeOwner {
    pub schema_version: String,
    pub session_id: String,
    pub public_key: String,
    pub queued_row_version: i64,
    pub fence: TransformationAttemptFence,
    pub adapter_id: String,
    pub binding: TransformationExecutionBinding,
    /// Sesión de arranque del sistema (época del último boot) registrada al
    /// crear la intención. Cero sólo en journals anteriores a la política:
    /// un valor distinto del boot actual prueba que ningún proceso ni handle
    /// de esa sesión sobrevive (reinicio del sistema) y habilita el cierre
    /// sin testigo.
    #[serde(default)]
    pub boot_session: u64,
}
impl TransformationNativeOwner {
    pub fn validate(&self) -> Result<(), LarmError> {
        self.fence.validate()?;
        self.binding
            .validate()
            .map_err(|_| native_owner_invalid())?;
        if self.schema_version != "1.0"
            || !is_uuid_v7(&self.session_id)
            || !is_sha256(&self.public_key)
            || self.queued_row_version < 1
            || self.adapter_id.is_empty()
            || self.adapter_id.len() > 256
            || self.adapter_id.chars().any(char::is_control)
        {
            return Err(native_owner_invalid());
        }
        Ok(())
    }
}
fn native_owner_invalid() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "La identidad nativa del intento no es válida",
        "transformation_native_owner_invalid",
    )
}

#[derive(Debug, Clone)]
pub struct TransformationOwnedClaim {
    pub claim: TransformationClaim,
    pub owner: TransformationNativeOwner,
}

/// Durable routing metadata for startup reconciliation. Neither phase nor
/// presence of this record proves that any native process/IO has closed.
#[derive(Debug, Clone)]
pub struct TransformationNativePreparation {
    pub owner: TransformationNativeOwner,
    pub context: MutationContext,
    pub registered: bool,
}

/// Implemented by a server-owned physical process/IO owner. Cleanup remains
/// possible after qualification is lost. require_closed must inspect retained
/// native ownership, never a boolean, HTTP body, terminal DB flag or timeout.
#[async_trait::async_trait]
pub trait TransformationAttemptGuard: Send + Sync {
    fn fence(&self) -> &TransformationAttemptFence;
    fn binding(&self) -> &TransformationExecutionBinding;
    /// None for legacy in-process ownership. A registered native attempt must
    /// be checked against the exact descriptor persisted with its claim.
    fn native_owner(&self) -> Option<&TransformationNativeOwner> {
        None
    }
    async fn recheck(&self) -> Result<(), LarmError>;
    async fn require_closed(&self) -> Result<(), LarmError>;
}

/// A server-owned, already prepared native helper. Ready may permit its P022
/// handshake, but transformation RPC must remain gated on committed registration.
#[async_trait::async_trait]
pub trait TransformationReadyOwner: TransformationAttemptGuard {
    async fn require_ready(&self) -> Result<(), LarmError>;
}
