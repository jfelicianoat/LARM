//! Cola durable de verificación de variantes (P042).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::artifacts::ArtifactState;
use crate::bridge::{ApiError, JobReceipt};
use crate::model::BundleState;
use crate::ports::{JobSource, MutationContext};

pub const VERIFY_JOB_KIND: &str = "model.verify";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationClaim {
    pub job_id: String,
    pub variant_id: String,
    pub actor_id: String,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationVerification {
    pub id: String,
    pub expected_row_version: i64,
    pub state: ArtifactState,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub verified_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCommit {
    pub variant_id: String,
    pub expected_variant_row_version: i64,
    pub bundle_id: String,
    pub expected_bundle_row_version: i64,
    pub bundle_state: BundleState,
    pub evidence: Value,
    pub result: Value,
    pub locations: Vec<LocationVerification>,
}

#[async_trait::async_trait]
pub trait ModelVerificationSubmitter: Send + Sync {
    async fn enqueue_verification(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError>;
}

#[async_trait::async_trait]
pub trait ModelVerificationJobs: ModelVerificationSubmitter + JobSource {
    async fn claim_verification(&self) -> Result<Option<VerificationClaim>, ApiError>;
    /// Actualiza ubicaciones, bundle, variante, journal y outbox en una
    /// transacción. Todos los row_version se comprueban antes de confirmar.
    async fn complete_verification(
        &self,
        claim: &VerificationClaim,
        commit: &VerificationCommit,
    ) -> Result<(), ApiError>;
    async fn fail_verification(
        &self,
        claim: &VerificationClaim,
        error: &ApiError,
    ) -> Result<(), ApiError>;
    async fn recover_verifications(&self) -> Result<u64, ApiError>;
}
