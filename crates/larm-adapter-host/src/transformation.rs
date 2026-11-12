//! Qualified transformation RPC. Core still owns admission, reservations,
//! output verification and publication; a worker reply grants none of these.
use crate::{policy::GuardedWorker, worker::WorkerState};
pub use larm_adapter_sdk::source::{ArtifactManifest, SourceArtifact};
use larm_adapter_sdk::{
    permissions::Access,
    protocol::{Progress, Request},
};
use larm_domain::{
    adapter_updates::is_sha256,
    bridge::{is_uuid_v7, JobReceipt},
    error::{ErrorCode, LarmError},
    ports::AdapterQualificationGuard,
    transformation_execution::{
        TransformationAttemptFence, TransformationAttemptGuard, TransformationClaim,
    },
    transformations::{
        ExecuteTransformationPlan, TransformationExecutionBinding, TransformationPlanResponse,
        TransformationRequest,
    },
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationDescription {
    pub formats_input: Vec<String>,
    pub formats_output: Vec<String>,
    pub parameter_schema: Map<String, Value>,
    pub tool_version: String,
    pub pause_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub adapter_id: String,
    pub installation_id: String,
    pub run_id: String,
    pub run_sha256: String,
    pub profile_sha256: String,
}
impl SessionIdentity {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.adapter_id.is_empty()
            || !is_uuid_v7(&self.installation_id)
            || !is_uuid_v7(&self.run_id)
            || !is_sha256(&self.run_sha256)
            || !is_sha256(&self.profile_sha256)
        {
            return Err(invalid_request());
        }
        Ok(())
    }
}

pub struct TransformationWorker {
    worker: GuardedWorker,
    qualification: Arc<dyn AdapterQualificationGuard>,
    identity: SessionIdentity,
    attempt: OnceLock<(TransformationAttemptFence, TransformationExecutionBinding)>,
}

/// A verify reply observed by the qualified host for one bound Core attempt.
/// It is not deserializable and is not evidence of physical output integrity.
pub struct TransformationOutputManifest {
    fence: TransformationAttemptFence,
    binding: TransformationExecutionBinding,
    worker_job_id: String,
    manifest: ArtifactManifest,
}
impl TransformationOutputManifest {
    /// Crate-private boundary for a typed Verify reply observed through a
    /// qualified host channel. Callers must bind and recheck that channel;
    /// arbitrary deserialized manifests cannot mint this public opaque type.
    pub(crate) fn observed(
        fence: TransformationAttemptFence,
        binding: TransformationExecutionBinding,
        worker_job_id: String,
        manifest: ArtifactManifest,
    ) -> Result<Self, LarmError> {
        fence.validate()?;
        binding.validate().map_err(|_| invalid_request())?;
        if !is_uuid_v7(&worker_job_id) {
            return Err(invalid_request());
        }
        manifest.validate()?;
        Ok(Self {
            fence,
            binding,
            worker_job_id,
            manifest,
        })
    }

    pub fn fence(&self) -> &TransformationAttemptFence {
        &self.fence
    }
    pub fn binding(&self) -> &TransformationExecutionBinding {
        &self.binding
    }
    pub fn worker_job_id(&self) -> &str {
        &self.worker_job_id
    }
    pub fn manifest(&self) -> &ArtifactManifest {
        &self.manifest
    }
}

/// Borrowing keeps the real worker/IO owner alive. A worker can bind only one
/// Core claim for its lifetime; it cannot be relabelled after a restart.
pub struct TransformationClaimGuard<'a> {
    worker: &'a TransformationWorker,
    fence: &'a TransformationAttemptFence,
    binding: &'a TransformationExecutionBinding,
}
#[async_trait::async_trait]
impl TransformationAttemptGuard for TransformationClaimGuard<'_> {
    fn fence(&self) -> &TransformationAttemptFence {
        self.fence
    }
    fn binding(&self) -> &TransformationExecutionBinding {
        self.binding
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.worker.recheck().await
    }
    async fn require_closed(&self) -> Result<(), LarmError> {
        if !self.worker.io_drained() {
            return Err(error(
                ErrorCode::Conflict,
                "transformation_attempt_io_not_drained",
            ));
        }
        Ok(())
    }
}
fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo completar la operación de transformación",
        reason,
    )
}
fn invalid_request() -> LarmError {
    error(
        ErrorCode::ValidationFailed,
        "transformation_request_invalid",
    )
}

fn validate_plan(
    request: &TransformationRequest,
    reply: &TransformationPlanResponse,
) -> Result<(), LarmError> {
    let resources = reply.resource_ids.iter().collect::<BTreeSet<_>>();
    if !is_uuid_v7(&reply.plan_id)
        || !is_sha256(&reply.plan_hash)
        || reply.operation.trim().is_empty()
        || resources.len() != reply.resource_ids.len()
        || !resources.contains(&request.input_variant_id)
        || !resources.contains(&request.storage_location_id)
        || reply.steps.is_empty()
        || reply.steps.iter().any(|s| {
            s.phase.trim().is_empty()
                || !s.request.is_object()
                || s.adapter_id
                    .as_deref()
                    .is_some_and(|id| id != request.adapter_id)
        })
        || reply.expires_at.len() != 24
        || !reply.expires_at.ends_with('Z')
        || !chrono::DateTime::parse_from_rfc3339(&reply.expires_at)
            .is_ok_and(|at| at > chrono::Utc::now())
    {
        return Err(error(
            ErrorCode::ProtocolError,
            "transformation_plan_reply_invalid",
        ));
    }
    Ok(())
}

impl TransformationWorker {
    pub fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    pub fn pid(&self) -> u32 {
        self.worker.pid()
    }
    pub(crate) fn from_owned(
        worker: GuardedWorker,
        qualification: Arc<dyn AdapterQualificationGuard>,
        identity: SessionIdentity,
    ) -> Self {
        Self {
            worker,
            qualification,
            identity,
            attempt: OnceLock::new(),
        }
    }

    pub fn claim_guard(
        &self,
        claim: &TransformationClaim,
    ) -> Result<TransformationClaimGuard<'_>, LarmError> {
        claim.fence.validate()?;
        claim.plan.verify_seal().map_err(|_| invalid_request())?;
        claim.job.validate().map_err(|_| invalid_request())?;
        let binding = claim
            .plan
            .execution_binding
            .as_ref()
            .ok_or_else(invalid_request)?;
        if binding.adapter_installation_id != self.identity.installation_id
            || binding.conformance_run_id != self.identity.run_id
            || binding.conformance_run_sha256 != self.identity.run_sha256
            || binding.profile_sha256 != self.identity.profile_sha256
            || claim.job.id != claim.fence.job_id
            || claim.job.plan_hash != claim.fence.plan_hash
            || claim.job.attempt != claim.fence.attempt
            || claim.job.adapter_id != self.identity.installation_id
            || claim.plan.request.adapter_id != self.identity.adapter_id
            || claim.plan.response.plan_hash != claim.fence.plan_hash
            || claim.job.metadata.get("attempt_execution") != Some(&json!(claim.fence))
        {
            return Err(error(
                ErrorCode::StaleRevision,
                "transformation_attempt_session_mismatch",
            ));
        }
        let (fence, bound) = self
            .attempt
            .get_or_init(|| (claim.fence.clone(), binding.clone()));
        if fence != &claim.fence || bound != binding {
            return Err(error(
                ErrorCode::StaleRevision,
                "transformation_attempt_session_already_bound",
            ));
        }
        Ok(TransformationClaimGuard {
            worker: self,
            fence,
            binding: bound,
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        id: &str,
        method: &str,
        params: Value,
        access: &Access,
        timeout: Option<Duration>,
        resource: Option<&str>,
    ) -> Result<T, LarmError> {
        if !is_uuid_v7(id) {
            return Err(error(ErrorCode::ValidationFailed, "request_id_invalid"));
        }
        self.qualification.recheck().await?;
        let result = self
            .worker
            .call_request(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: method.into(),
                    params,
                },
                access,
                timeout,
                resource,
            )
            .await;
        let current = self.qualification.recheck().await;
        let value = match result {
            Err(original) => return Err(original),
            Ok(value) => {
                current?;
                value
            }
        };
        serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "transformation_reply_invalid"))
    }

    pub async fn describe(
        &self,
        id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationDescription, LarmError> {
        self.call(
            id,
            "transformation.describe",
            json!({}),
            access,
            timeout,
            None,
        )
        .await
    }

    pub async fn plan(
        &self,
        id: &str,
        request: &TransformationRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationPlanResponse, LarmError> {
        request.validate().map_err(|_| invalid_request())?;
        if request.adapter_id != self.identity.adapter_id {
            return Err(error(
                ErrorCode::Forbidden,
                "transformation_adapter_identity_mismatch",
            ));
        }
        let reply = self
            .call(
                id,
                "transformation.plan",
                serde_json::to_value(request).map_err(|_| invalid_request())?,
                access,
                timeout,
                None,
            )
            .await?;
        validate_plan(request, &reply)?;
        Ok(reply)
    }

    /// The worker plan remains distinct from Core's operator-confirmed plan.
    /// Both identities are sealed by Core; this binding comes from the host.
    pub fn execution_binding(
        &self,
        plan: &TransformationPlanResponse,
    ) -> Result<TransformationExecutionBinding, LarmError> {
        let binding = TransformationExecutionBinding {
            adapter_installation_id: self.identity.installation_id.clone(),
            conformance_run_id: self.identity.run_id.clone(),
            conformance_run_sha256: self.identity.run_sha256.clone(),
            profile_sha256: self.identity.profile_sha256.clone(),
            worker_plan_id: plan.plan_id.clone(),
            worker_plan_hash: plan.plan_hash.clone(),
        };
        binding.validate().map_err(|_| invalid_request())?;
        Ok(binding)
    }

    pub async fn execute(
        &self,
        id: &str,
        request: &ExecuteTransformationPlan,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<JobReceipt, LarmError> {
        if !is_uuid_v7(&request.plan_id)
            || !is_sha256(&request.plan_hash)
            || request
                .approval_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
        {
            return Err(invalid_request());
        }
        let receipt: JobReceipt = self
            .call(
                id,
                "transformation.execute",
                serde_json::to_value(request).map_err(|_| invalid_request())?,
                access,
                timeout,
                Some(&request.plan_id),
            )
            .await?;
        if !is_uuid_v7(&receipt.job_id) {
            return Err(error(
                ErrorCode::ProtocolError,
                "transformation_job_reply_invalid",
            ));
        }
        Ok(receipt)
    }

    pub async fn verify(
        &self,
        id: &str,
        job_id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<ArtifactManifest, LarmError> {
        if !is_uuid_v7(job_id) {
            return Err(invalid_request());
        }
        let manifest: ArtifactManifest = self
            .call(
                id,
                "transformation.verify",
                json!({"job_id":job_id}),
                access,
                timeout,
                Some(job_id),
            )
            .await?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub async fn recheck(&self) -> Result<(), LarmError> {
        self.qualification.recheck().await
    }

    pub async fn verify_claim_output(
        &self,
        claim: &TransformationClaim,
        id: &str,
        worker_job_id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationOutputManifest, LarmError> {
        let guard = self.claim_guard(claim)?;
        guard.recheck().await?;
        let manifest = self.verify(id, worker_job_id, access, timeout).await?;
        guard.recheck().await?;
        TransformationOutputManifest::observed(
            guard.fence().clone(),
            guard.binding().clone(),
            worker_job_id.into(),
            manifest,
        )
    }
    pub async fn cancel(&self, id: &str) -> Result<bool, LarmError> {
        if !is_uuid_v7(id) {
            return Err(invalid_request());
        }
        self.worker.cancel(id).await
    }
    /// Cleanup does not require a still-valid qualification. Wait for process
    /// and IO drainage before releasing Core resources or accepting outputs.
    pub async fn close(&self) -> Result<(), LarmError> {
        self.worker.stop_and_wait().await
    }
    pub fn progress(&self) -> tokio::sync::broadcast::Receiver<Progress> {
        self.worker.subscribe_progress()
    }
    pub fn io_drained(&self) -> bool {
        self.worker.io_drained()
    }
    pub fn state(&self) -> WorkerState {
        self.worker.state()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn request() -> TransformationRequest {
        TransformationRequest {
            input_variant_id: uuid::Uuid::now_v7().to_string(),
            adapter_id: "fixture.transform".into(),
            tool_version: "1".into(),
            target_format: "fixture".into(),
            parameters: json!({}),
            storage_location_id: uuid::Uuid::now_v7().to_string(),
            hard_budget_bytes: None,
        }
    }
    fn reply(request: &TransformationRequest) -> TransformationPlanResponse {
        serde_json::from_value(json!({
            "plan_id":uuid::Uuid::now_v7().to_string(),"plan_hash":"a".repeat(64),
            "operation":"transformation.execute", "resource_ids":[request.input_variant_id,request.storage_location_id],
            "steps":[{"phase":"fixture","adapter_id":request.adapter_id,"request":{}}],
            "expires_at":larm_domain::bridge::timestamp(chrono::Utc::now()+chrono::Duration::minutes(5)),
            "approval_required":true,"storage_estimate":{
                "download_bytes":null,"final_artifact_bytes":null,"installed_bytes":null,"derived_cache_bytes":null,
                "temporary_peak_bytes":null,"existing_bytes":null,"peak_extra_bytes":null,"required_free_bytes":null,
                "upper_bound_bytes":null,"confidence":"UNKNOWN","reason":"fixture not observed","evidence_refs":[],"per_volume":[]
            }
        })).unwrap()
    }
    #[test]
    fn typed_plan_does_not_turn_unknown_estimates_into_admission() {
        let request = request();
        let reply = reply(&request);
        validate_plan(&request, &reply).unwrap();
        assert!(reply.approval_required);
        assert!(reply.storage_estimate.required_free_bytes.is_none());
        assert!(reply.storage_estimate.per_volume.is_empty());
    }
    #[test]
    fn plan_cannot_change_resources_adapter_operation_or_expiry() {
        let request = request();
        for fault in 0..8 {
            let mut reply = reply(&request);
            match fault {
                0 => {
                    reply.resource_ids.remove(0);
                }
                1 => reply.resource_ids.push(request.input_variant_id.clone()),
                2 => reply.steps[0].adapter_id = Some("other.adapter".into()),
                3 => reply.expires_at = "2000-01-01T00:00:00.000Z".into(),
                4 => reply.plan_hash = "A".repeat(64),
                5 => reply.operation.clear(),
                6 => reply.steps.clear(),
                _ => reply.steps[0].request = Value::Null,
            }
            assert_eq!(
                validate_plan(&request, &reply).unwrap_err().code,
                ErrorCode::ProtocolError,
                "fault {fault}"
            );
        }
    }
    #[test]
    fn description_has_a_closed_shape_and_preserves_unsupported_pause() {
        let value = json!({"formats_input":["BINARY"],"formats_output":["BASE64"],"parameter_schema":{},"tool_version":"1","pause_supported":false});
        let description: TransformationDescription = serde_json::from_value(value.clone()).unwrap();
        assert!(!description.pause_supported);
        let mut extra = value.clone();
        extra["approved"] = json!(true);
        assert!(serde_json::from_value::<TransformationDescription>(extra).is_err());
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("pause_supported");
        assert!(serde_json::from_value::<TransformationDescription>(missing).is_err());
    }
}
