//! Shared sealing of persisted download records; preserves the original
//! preflight byte representation, including the explicit legacy candidate mode.
use super::{
    DownloadArtifactEstimate, DownloadDestinationOption, DownloadPlanRecord, DownloadPlanRequest,
};
use crate::bridge::{is_uuid_v7, ApiError};
use sha2::{Digest, Sha256};

fn invalid(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new("VALIDATION_FAILED", message, reason)
}

pub struct PlanHashInput<'a> {
    pub acquisition: Option<&'a crate::acquisition::AcquisitionPlanBinding>,
    pub request: &'a DownloadPlanRequest,
    pub variant_id: &'a str,
    pub revision_id: &'a str,
    pub revision_key: &'a str,
    pub manifest_hash: &'a str,
    pub artifact_estimates: &'a [DownloadArtifactEstimate],
    pub destination_options: &'a [DownloadDestinationOption],
    pub source_candidates: &'a [crate::download::DownloadSourceCandidate],
    pub include_source_candidates: bool,
    pub observed_at: &'a str,
    pub expires_at: &'a str,
    pub source_id: &'a str,
}

pub fn plan_hash(input: PlanHashInput<'_>) -> Result<String, ApiError> {
    let mut optional_artifact_ids = input.request.optional_artifact_ids.clone();
    optional_artifact_ids.sort();
    let mut hash_basis = serde_json::json!({
        "variant_id": input.variant_id,
        "revision_id": input.revision_id,
        "revision_key": input.revision_key,
        "manifest_hash": input.manifest_hash,
        "optional_artifact_ids": optional_artifact_ids,
        "artifact_estimates": input.artifact_estimates,
        "destination_options": input.destination_options,
        "selected_storage_location_id": input.request.storage_location_id,
        "source_id": input.source_id,
        "hard_budget_bytes": input.request.hard_budget_bytes,
        "priority": input.request.priority,
        "observed_at": input.observed_at,
        "expires_at": input.expires_at,
    });
    // Legacy, already-issued plans did not contain this member. Activation
    // accepts their old hash only when the stored candidate list is empty.
    if input.include_source_candidates || !input.source_candidates.is_empty() {
        hash_basis["source_candidates"] = serde_json::json!(input.source_candidates);
    }
    if let Some(binding) = input.acquisition {
        if !is_uuid_v7(&binding.manifest_id)
            || !crate::adapter_updates::is_sha256(&binding.snapshot_hash)
            || !crate::adapter_updates::is_sha256(&binding.upstream_manifest_hash)
            || binding.upstream_manifest_hash != input.manifest_hash
        {
            return Err(invalid(
                "acquisition_binding_invalid",
                "El vínculo del manifiesto no es válido",
            ));
        }
        hash_basis["acquisition"] = serde_json::json!(binding);
    }
    let hash_bytes = serde_json::to_vec(&hash_basis).map_err(|error| {
        ApiError::new(
            "INTERNAL",
            "No se pudo sellar el preflight",
            "preview_hash_failed",
        )
        .with_technical(error.to_string())
    })?;
    Ok(hex::encode(Sha256::digest(hash_bytes)))
}

impl DownloadPlanRecord {
    /// Recomputes the seal without expiring historical completed-job evidence.
    /// Callers separately check expiry when admitting a new job.
    pub fn sealed_hash(&self, include_source_candidates: bool) -> Result<String, ApiError> {
        let source_id = self.response.source_id.as_deref().ok_or_else(|| {
            invalid(
                "download_plan_source_missing",
                "El plan no conserva su fuente",
            )
        })?;
        plan_hash(PlanHashInput {
            acquisition: self.response.acquisition.as_ref(),
            request: &self.request,
            variant_id: &self.response.variant_id,
            revision_id: &self.revision_id,
            revision_key: &self.revision_key,
            manifest_hash: &self.manifest_hash,
            artifact_estimates: &self.response.artifact_estimates,
            destination_options: &self.response.destination_options,
            source_candidates: &self.response.source_candidates,
            include_source_candidates,
            observed_at: &self.storage_observed_at,
            expires_at: &self.response.expires_at,
            source_id,
        })
    }
}
