//! Dominio puro del pipeline de transformaciones (P093).
#![forbid(unsafe_code)]

use crate::{
    bridge::is_uuid_v7, canonical, estimate::StorageEstimate, model::bundle_path,
    ports::MutationContext, units::DecimalU64,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use thiserror::Error;

pub use crate::transformation_output::TransformationOutputPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransformationState {
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
    FailedRecoverable,
}

impl TransformationState {
    /// Transiciones ordinarias del contrato 01. Los reintentos pasan por
    /// `retry_with_preflight` para exigir hash de entrada y plan nuevos.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use TransformationState as S;
        matches!(
            (self, next),
            (S::Planned, S::Queued)
                | (S::Planned, S::Cancelling)
                | (S::Queued, S::Running)
                | (S::Queued, S::Cancelling)
                | (S::Queued, S::Failed)
                | (S::Running, S::Pausing)
                | (S::Running, S::Verifying)
                | (S::Running, S::Cancelling)
                | (S::Running, S::Failed)
                | (S::Running, S::FailedRecoverable)
                | (S::Pausing, S::Paused)
                | (S::Pausing, S::Cancelling)
                | (S::Pausing, S::Failed)
                | (S::Pausing, S::FailedRecoverable)
                | (S::Paused, S::Queued)
                | (S::Paused, S::Cancelling)
                | (S::Paused, S::Failed)
                | (S::Paused, S::FailedRecoverable)
                | (S::Verifying, S::Completed)
                | (S::Verifying, S::Cancelling)
                | (S::Verifying, S::Failed)
                | (S::Verifying, S::FailedRecoverable)
                | (S::Cancelling, S::Cancelled)
        )
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    #[must_use]
    pub const fn is_interrupted(self) -> bool {
        matches!(self, Self::Running | Self::Pausing | Self::Verifying)
    }
}

/// Entrada contractual de `POST /api/v1/transform-plans`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationRequest {
    pub input_variant_id: String,
    pub adapter_id: String,
    pub tool_version: String,
    pub target_format: String,
    pub parameters: Value,
    pub storage_location_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_budget_bytes: Option<DecimalU64>,
}

/// Confirmación explícita del plan sellado, según `ExecutePlan` del contrato.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteTransformationPlan {
    pub plan_id: String,
    pub plan_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
}

/// Clave que debe participar en la aceptación durable del job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformationActivationIdempotency {
    pub key: String,
    pub body_hash: String,
}

/// Internal route/header binding for the explicitly authorized restart. The
/// wire body remains the closed ExecutePlan DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformationRestart {
    pub job_id: String,
    pub if_match: u64,
}

impl TransformationRequest {
    pub fn validate(&self) -> Result<(), TransformError> {
        if !is_uuid_v7(&self.input_variant_id)
            || !is_uuid_v7(&self.storage_location_id)
            || self.adapter_id.trim().is_empty()
            || self.tool_version.trim().is_empty()
            || self.target_format.trim().is_empty()
            || !self.parameters.is_object()
        {
            return Err(TransformError::InvalidRequest);
        }
        Ok(())
    }
}

/// Descriptor inmutable del input que ha sido leído y verificado antes del
/// preflight. Las rutas son relativas al bundle y los tamaños son observados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputArtifact {
    pub artifact_id: String,
    pub relative_path: String,
    pub size_bytes: DecimalU64,
    pub sha256: String,
}

/// Internal catalogue identity of a physically pinned input. Logical bundle
/// filenames and the location's physical relative path need not be equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationInputLocation {
    pub artifact_id: String,
    pub artifact_location_id: String,
    pub storage_location_id: String,
    pub canonical_storage_path: String,
    pub relative_path: String,
    pub volume_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationInputIdentity {
    pub variant_id: String,
    pub revision_id: String,
    pub variant_bundle_hash: String,
    pub bundle_id: String,
    pub bundle_row_version: i64,
    pub bundle_manifest_hash: String,
}

/// Live server-owned read guard. Implementations retain the physical path/file
/// owners until admission ends and recheck their identity without releasing
/// ownership. Neither an HTTP body nor an old stored fingerprint implements it.
#[async_trait::async_trait]
pub trait TransformationInputGuard: Send + Sync {
    fn identity(&self) -> &TransformationInputIdentity;
    fn artifacts(&self) -> &[InputArtifact];
    fn locations(&self) -> &[TransformationInputLocation];
    async fn recheck(&self) -> Result<(), crate::error::LarmError>;
}

/// Fresh server-side observations for initial job admission, not a wire DTO.
pub struct TransformationAdmission<'a> {
    pub input: &'a dyn TransformationInputGuard,
    pub space: &'a dyn TransformationSpaceObserver,
}

pub struct TransformationSpaceSnapshot {
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub demands: Vec<crate::ports::VolumeDemand>,
    pub margin_policy: crate::estimate::MarginPolicy,
    pub minimum_free_bytes: u64,
}

/// Sample after the potentially expensive qualification work, immediately
/// before transactional reservation. Cached preview free bytes are not input.
#[async_trait::async_trait]
pub trait TransformationSpaceObserver: Send + Sync {
    async fn observe(&self) -> Result<TransformationSpaceSnapshot, crate::error::LarmError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub phase: String,
    pub adapter_id: Option<String>,
    pub request: Value,
}

/// Campos del Plan devuelto por `transformation.plan`; el core asigna el ID y
/// sella el contenido después de incorporar el snapshot de los inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDraft {
    pub operation: String,
    pub resource_ids: Vec<String>,
    pub storage_estimate: StorageEstimate,
    pub steps: Vec<PlanStep>,
    pub expires_at: String,
    pub approval_required: bool,
}

/// DTO de salida exacto del schema `Plan`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationPlanResponse {
    pub plan_id: String,
    pub plan_hash: String,
    pub operation: String,
    pub resource_ids: Vec<String>,
    pub storage_estimate: StorageEstimate,
    pub steps: Vec<PlanStep>,
    pub expires_at: String,
    pub approval_required: bool,
}

/// Registro interno sellado que liga la petición, todos los hashes del input
/// y el plan que se mostró al operador.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedTransformationPlan {
    pub request: TransformationRequest,
    pub input_artifacts: Vec<InputArtifact>,
    pub response: TransformationPlanResponse,
    /// Internal execution identity measured by the host. Legacy recovery
    /// records may omit it; new admission requires a bound plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_binding: Option<TransformationExecutionBinding>,
    /// Private Core publication bounds. Legacy plans remain readable but
    /// cannot authorize managed output publication without this policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_policy: Option<TransformationOutputPolicy>,
}

/// Private host binding, never accepted from an HTTP Transform/ExecutePlan.
/// A structural check is not qualification: the host must recheck the
/// recorded installation/profile and qualification before every worker IO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationExecutionBinding {
    pub adapter_installation_id: String,
    pub conformance_run_id: String,
    pub conformance_run_sha256: String,
    pub profile_sha256: String,
    pub worker_plan_id: String,
    pub worker_plan_hash: String,
}

impl TransformationExecutionBinding {
    pub fn validate(&self) -> Result<(), TransformError> {
        if ![
            &self.adapter_installation_id,
            &self.conformance_run_id,
            &self.worker_plan_id,
        ]
        .into_iter()
        .all(|id| is_uuid_v7(id))
            || ![
                &self.conformance_run_sha256,
                &self.profile_sha256,
                &self.worker_plan_hash,
            ]
            .into_iter()
            .all(|hash| crate::adapter_updates::is_sha256(hash))
        {
            return Err(TransformError::InvalidExecutionBinding);
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransformError {
    #[error("La petición de transformación no cumple el contrato")]
    InvalidRequest,
    #[error("El snapshot de archivos de entrada no es válido")]
    InvalidInputSnapshot,
    #[error("El plan de transformación no corresponde a los recursos solicitados")]
    InvalidPlan,
    #[error("La identidad de ejecución de la transformación no es válida")]
    InvalidExecutionBinding,
    #[error("El hash del plan no coincide con su contenido")]
    PlanSealMismatch,
    #[error("La transición del trabajo no está permitida")]
    InvalidTransition,
    #[error("El trabajo de transformación no es válido")]
    InvalidJob,
    #[error("No se puede reiniciar sin un input íntegro y un preflight nuevo")]
    RestartPrecondition,
    #[error("Se agotó el contador de revisión o intento")]
    CounterOverflow,
    #[error("No se pudo canonicalizar el plan")]
    Canonicalization,
}

fn valid_timestamp(value: &str) -> bool {
    value.len() == 24 && value.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn validate_input_artifacts(artifacts: &[InputArtifact]) -> Result<(), TransformError> {
    if artifacts.is_empty() {
        return Err(TransformError::InvalidInputSnapshot);
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for artifact in artifacts {
        if !is_uuid_v7(&artifact.artifact_id)
            || !crate::adapter_updates::is_sha256(&artifact.sha256)
            || bundle_path(&artifact.relative_path).is_err()
            || !ids.insert(artifact.artifact_id.as_str())
            || !paths.insert(artifact.relative_path.to_lowercase())
        {
            return Err(TransformError::InvalidInputSnapshot);
        }
    }
    Ok(())
}

fn sorted_artifacts(artifacts: &[InputArtifact]) -> Vec<InputArtifact> {
    let mut result = artifacts.to_vec();
    result.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    result
}

fn hash_artifacts(artifacts: &[InputArtifact]) -> Result<String, TransformError> {
    validate_input_artifacts(artifacts)?;
    let value = serde_json::to_value(sorted_artifacts(artifacts))
        .map_err(|_| TransformError::Canonicalization)?;
    canonical::sha256_hex(&value).map_err(|_| TransformError::Canonicalization)
}

fn seal_payload(
    plan_id: &str,
    request: &TransformationRequest,
    input_artifacts: &[InputArtifact],
    draft: &PlanDraft,
    binding: Option<&TransformationExecutionBinding>,
    output_policy: Option<&TransformationOutputPolicy>,
) -> Result<Value, TransformError> {
    let mut payload = json!({
        "plan_id": plan_id,
        "request": request,
        "input_artifacts": sorted_artifacts(input_artifacts),
        "operation": draft.operation,
        "resource_ids": draft.resource_ids,
        "storage_estimate": draft.storage_estimate,
        "steps": draft.steps,
        "expires_at": draft.expires_at,
        "approval_required": draft.approval_required,
    });
    if let Some(binding) = binding {
        binding.validate()?;
        payload["execution_binding"] =
            serde_json::to_value(binding).map_err(|_| TransformError::Canonicalization)?;
    }
    if let Some(policy) = output_policy {
        policy.validate_estimate(&draft.storage_estimate)?;
        payload["output_policy"] =
            serde_json::to_value(policy).map_err(|_| TransformError::Canonicalization)?;
    }
    Ok(payload)
}

fn validate_draft(
    request: &TransformationRequest,
    input_artifacts: &[InputArtifact],
    draft: &PlanDraft,
) -> Result<(), TransformError> {
    request.validate()?;
    validate_input_artifacts(input_artifacts)?;
    let resources: BTreeSet<&str> = draft.resource_ids.iter().map(String::as_str).collect();
    if draft.operation.trim().is_empty()
        || draft.steps.is_empty()
        || draft.steps.iter().any(|step| {
            step.phase.trim().is_empty()
                || !step.request.is_object()
                || step
                    .adapter_id
                    .as_ref()
                    .is_some_and(|id| id.trim().is_empty())
        })
        || resources.len() != draft.resource_ids.len()
        || !resources.contains(request.input_variant_id.as_str())
        || !resources.contains(request.storage_location_id.as_str())
        || !valid_timestamp(&draft.expires_at)
    {
        return Err(TransformError::InvalidPlan);
    }
    Ok(())
}

/// Sella una respuesta de herramienta junto al snapshot exacto que se
/// transformará. El adapter no puede cambiar input, destino o parámetros sin
/// invalidar el hash que la UI confirma.
pub fn seal_plan(
    plan_id: impl Into<String>,
    request: TransformationRequest,
    input_artifacts: Vec<InputArtifact>,
    draft: PlanDraft,
) -> Result<SealedTransformationPlan, TransformError> {
    let plan_id = plan_id.into();
    if !is_uuid_v7(&plan_id) {
        return Err(TransformError::InvalidPlan);
    }
    validate_draft(&request, &input_artifacts, &draft)?;
    let payload = seal_payload(&plan_id, &request, &input_artifacts, &draft, None, None)?;
    let plan_hash =
        canonical::sha256_hex(&payload).map_err(|_| TransformError::Canonicalization)?;
    Ok(SealedTransformationPlan {
        request,
        input_artifacts: sorted_artifacts(&input_artifacts),
        execution_binding: None,
        output_policy: None,
        response: TransformationPlanResponse {
            plan_id,
            plan_hash,
            operation: draft.operation,
            resource_ids: draft.resource_ids,
            storage_estimate: draft.storage_estimate,
            steps: draft.steps,
            expires_at: draft.expires_at,
            approval_required: draft.approval_required,
        },
    })
}

/// Seal the private worker/qualification identity into the operator's plan
/// digest without adding fields to the public Plan response.
pub fn seal_bound_plan(
    plan_id: impl Into<String>,
    request: TransformationRequest,
    input_artifacts: Vec<InputArtifact>,
    draft: PlanDraft,
    binding: TransformationExecutionBinding,
) -> Result<SealedTransformationPlan, TransformError> {
    binding.validate()?;
    let mut plan = seal_plan(plan_id, request, input_artifacts, draft.clone())?;
    let payload = seal_payload(
        &plan.response.plan_id,
        &plan.request,
        &plan.input_artifacts,
        &draft,
        Some(&binding),
        None,
    )?;
    plan.response.plan_hash =
        canonical::sha256_hex(&payload).map_err(|_| TransformError::Canonicalization)?;
    plan.execution_binding = Some(binding);
    Ok(plan)
}

/// Seal Core's managed-output budget together with the measured worker
/// identity. The public Plan/ExecutePlan schemas remain unchanged.
pub fn seal_publication_plan(
    plan_id: impl Into<String>,
    request: TransformationRequest,
    input_artifacts: Vec<InputArtifact>,
    draft: PlanDraft,
    binding: TransformationExecutionBinding,
    output_policy: TransformationOutputPolicy,
) -> Result<SealedTransformationPlan, TransformError> {
    let mut plan = seal_bound_plan(plan_id, request, input_artifacts, draft.clone(), binding)?;
    let payload = seal_payload(
        &plan.response.plan_id,
        &plan.request,
        &plan.input_artifacts,
        &draft,
        plan.execution_binding.as_ref(),
        Some(&output_policy),
    )?;
    plan.response.plan_hash =
        canonical::sha256_hex(&payload).map_err(|_| TransformError::Canonicalization)?;
    plan.output_policy = Some(output_policy);
    Ok(plan)
}

impl SealedTransformationPlan {
    pub fn verify_seal(&self) -> Result<(), TransformError> {
        let draft = PlanDraft {
            operation: self.response.operation.clone(),
            resource_ids: self.response.resource_ids.clone(),
            storage_estimate: self.response.storage_estimate.clone(),
            steps: self.response.steps.clone(),
            expires_at: self.response.expires_at.clone(),
            approval_required: self.response.approval_required,
        };
        if !is_uuid_v7(&self.response.plan_id) {
            return Err(TransformError::InvalidPlan);
        }
        validate_draft(&self.request, &self.input_artifacts, &draft)?;
        let payload = seal_payload(
            &self.response.plan_id,
            &self.request,
            &self.input_artifacts,
            &draft,
            self.execution_binding.as_ref(),
            self.output_policy.as_ref(),
        )?;
        let expected =
            canonical::sha256_hex(&payload).map_err(|_| TransformError::Canonicalization)?;
        if expected == self.response.plan_hash {
            Ok(())
        } else {
            Err(TransformError::PlanSealMismatch)
        }
    }

    pub fn input_fingerprint(&self) -> Result<String, TransformError> {
        hash_artifacts(&self.input_artifacts)
    }
}

/// Prueba opaca que solo se obtiene al cotejar una nueva lectura física del
/// manifiesto y hashes con el snapshot original. El consumidor debe calcular
/// esos hashes después del crash, no reutilizar el valor guardado en SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputIntegrityProof {
    fingerprint_sha256: String,
}

pub fn verify_observed_input_hashes(
    expected: &[InputArtifact],
    observed: &[InputArtifact],
) -> Result<InputIntegrityProof, TransformError> {
    let expected_hash = hash_artifacts(expected)?;
    let observed_hash = hash_artifacts(observed)?;
    if expected_hash != observed_hash {
        return Err(TransformError::RestartPrecondition);
    }
    Ok(InputIntegrityProof {
        fingerprint_sha256: expected_hash,
    })
}

/// DTO de `transformation_job` según el schema del dominio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationJob {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub input_variant_id: String,
    pub output_variant_id: Option<String>,
    pub adapter_id: String,
    pub tool_version: String,
    pub parameters: Value,
    pub plan_hash: String,
    pub state: TransformationState,
    pub attempt: u32,
    pub error: Option<Value>,
}

impl TransformationJob {
    pub fn from_plan(
        id: impl Into<String>,
        resolved_adapter_id: impl Into<String>,
        plan: &SealedTransformationPlan,
        created_at: impl Into<String>,
        ctx: &MutationContext,
    ) -> Result<Self, TransformError> {
        plan.verify_seal()?;
        let id = id.into();
        let resolved_adapter_id = resolved_adapter_id.into();
        let created_at = created_at.into();
        if !is_uuid_v7(&id)
            || !is_uuid_v7(&resolved_adapter_id)
            || !valid_timestamp(&created_at)
            || ctx.actor_id.trim().is_empty()
            || !is_uuid_v7(&ctx.correlation_id)
        {
            return Err(TransformError::InvalidJob);
        }
        let input_fingerprint = plan.input_fingerprint()?;
        let metadata_value = json!({
            "actor_id": ctx.actor_id,
            "correlation_id": ctx.correlation_id,
            "input_artifacts": plan.input_artifacts,
            "input_fingerprint_sha256": input_fingerprint,
            "request": plan.request,
            "plan_snapshot": plan.response,
            "attempt_history": [],
        });
        let mut metadata = metadata_value
            .as_object()
            .cloned()
            .ok_or(TransformError::InvalidJob)?;
        if let Some(binding) = &plan.execution_binding {
            metadata.insert(
                "execution_binding".into(),
                serde_json::to_value(binding).map_err(|_| TransformError::InvalidJob)?,
            );
        }
        if let Some(policy) = &plan.output_policy {
            metadata.insert(
                "output_policy".into(),
                serde_json::to_value(policy).map_err(|_| TransformError::InvalidJob)?,
            );
        }
        let job = Self {
            id,
            created_at: created_at.clone(),
            updated_at: created_at,
            row_version: 1,
            metadata,
            input_variant_id: plan.request.input_variant_id.clone(),
            output_variant_id: None,
            adapter_id: resolved_adapter_id,
            tool_version: plan.request.tool_version.clone(),
            parameters: plan.request.parameters.clone(),
            plan_hash: plan.response.plan_hash.clone(),
            state: TransformationState::Queued,
            attempt: 0,
            error: None,
        };
        job.validate()?;
        Ok(job)
    }

    pub fn validate(&self) -> Result<(), TransformError> {
        let fingerprint = self
            .metadata
            .get("input_fingerprint_sha256")
            .and_then(Value::as_str);
        let history_is_array = self
            .metadata
            .get("attempt_history")
            .is_some_and(Value::is_array);
        let actor_id = self
            .metadata
            .get("actor_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        let correlation_id = self
            .metadata
            .get("correlation_id")
            .and_then(Value::as_str)
            .is_some_and(is_uuid_v7);
        let input_artifacts = self
            .metadata
            .get("input_artifacts")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<InputArtifact>>(value).ok());
        let request = self
            .metadata
            .get("request")
            .cloned()
            .and_then(|value| serde_json::from_value::<TransformationRequest>(value).ok());
        let plan_snapshot = self
            .metadata
            .get("plan_snapshot")
            .cloned()
            .and_then(|value| serde_json::from_value::<TransformationPlanResponse>(value).ok());
        let binding = self
            .metadata
            .get("execution_binding")
            .map(|value| serde_json::from_value::<TransformationExecutionBinding>(value.clone()))
            .transpose();
        let binding_matches = binding.as_ref().is_ok_and(|binding| {
            binding.as_ref().is_none_or(|binding| {
                binding.validate().is_ok() && binding.adapter_installation_id == self.adapter_id
            })
        });
        let output_policy = self
            .metadata
            .get("output_policy")
            .map(|value| serde_json::from_value::<TransformationOutputPolicy>(value.clone()))
            .transpose();
        let input_matches = input_artifacts
            .as_ref()
            .and_then(|artifacts| hash_artifacts(artifacts).ok())
            .as_deref()
            == fingerprint;
        let request_matches = request.as_ref().is_some_and(|request| {
            request.validate().is_ok()
                && request.input_variant_id == self.input_variant_id
                && request.tool_version == self.tool_version
                && request.parameters == self.parameters
        });
        let plan_matches = plan_snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.plan_hash == self.plan_hash
                && request.as_ref().is_some_and(|request| {
                    input_artifacts.as_ref().is_some_and(|artifacts| {
                        SealedTransformationPlan {
                            request: request.clone(),
                            input_artifacts: artifacts.clone(),
                            response: snapshot.clone(),
                            execution_binding: binding.as_ref().ok().cloned().flatten(),
                            output_policy: output_policy.as_ref().ok().cloned().flatten(),
                        }
                        .verify_seal()
                        .is_ok()
                            && output_policy.is_ok()
                    })
                })
        });
        if !is_uuid_v7(&self.id)
            || !is_uuid_v7(&self.input_variant_id)
            || self
                .output_variant_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
            || !is_uuid_v7(&self.adapter_id)
            || self.row_version < 1
            || self.tool_version.trim().is_empty()
            || !self.parameters.is_object()
            || !crate::adapter_updates::is_sha256(&self.plan_hash)
            || !valid_timestamp(&self.created_at)
            || !valid_timestamp(&self.updated_at)
            || !fingerprint.is_some_and(crate::adapter_updates::is_sha256)
            || !history_is_array
            || !actor_id
            || !correlation_id
            || !input_matches
            || !request_matches
            || !plan_matches
            || !binding_matches
            || self.error.as_ref().is_some_and(|error| !error.is_object())
        {
            return Err(TransformError::InvalidJob);
        }
        Ok(())
    }

    pub fn transition(
        &mut self,
        next: TransformationState,
        updated_at: impl Into<String>,
    ) -> Result<(), TransformError> {
        let updated_at = updated_at.into();
        if !valid_timestamp(&updated_at) || !self.state.can_transition_to(next) {
            return Err(TransformError::InvalidTransition);
        }
        self.row_version = self
            .row_version
            .checked_add(1)
            .ok_or(TransformError::CounterOverflow)?;
        self.updated_at = updated_at;
        self.state = next;
        self.validate()
    }

    /// Recuperación H07: nunca publica output parcial y conserva la causa y el
    /// snapshot de input para que el reinicio pueda verificarlo de nuevo.
    pub fn mark_interrupted_recoverable(
        &mut self,
        normalized_error: Value,
        updated_at: impl Into<String>,
    ) -> Result<(), TransformError> {
        if !self.state.is_interrupted() || !normalized_error.is_object() {
            return Err(TransformError::InvalidTransition);
        }
        let updated_at = updated_at.into();
        if !valid_timestamp(&updated_at) {
            return Err(TransformError::InvalidJob);
        }
        self.row_version = self
            .row_version
            .checked_add(1)
            .ok_or(TransformError::CounterOverflow)?;
        self.updated_at = updated_at;
        self.state = TransformationState::FailedRecoverable;
        self.output_variant_id = None;
        self.error = Some(normalized_error);
        self.validate()
    }

    /// Reinicio H07 tras nuevo preflight. `InputIntegrityProof` debe proceder
    /// de los hashes físicos actuales de la fuente; el plan nuevo se vuelve a
    /// sellar y el historial del intento fallido se conserva en metadata.
    pub fn retry_with_preflight(
        &mut self,
        plan: &SealedTransformationPlan,
        input_proof: &InputIntegrityProof,
        resolved_adapter_id: impl Into<String>,
        updated_at: impl Into<String>,
    ) -> Result<(), TransformError> {
        let mut next = self.clone();
        next.retry_in_place(
            plan,
            input_proof,
            resolved_adapter_id.into(),
            updated_at.into(),
        )?;
        *self = next;
        Ok(())
    }

    fn retry_in_place(
        &mut self,
        plan: &SealedTransformationPlan,
        input_proof: &InputIntegrityProof,
        resolved_adapter_id: String,
        updated_at: String,
    ) -> Result<(), TransformError> {
        if !matches!(
            self.state,
            TransformationState::Failed | TransformationState::FailedRecoverable
        ) || self.output_variant_id.is_some()
        {
            return Err(TransformError::RestartPrecondition);
        }
        plan.verify_seal()?;
        let stored_fingerprint = self
            .metadata
            .get("input_fingerprint_sha256")
            .and_then(Value::as_str)
            .ok_or(TransformError::InvalidJob)?;
        let new_fingerprint = plan.input_fingerprint()?;
        if plan.request.input_variant_id != self.input_variant_id
            || input_proof.fingerprint_sha256 != stored_fingerprint
            || new_fingerprint != stored_fingerprint
            || plan.response.plan_hash == self.plan_hash
            || !is_uuid_v7(&resolved_adapter_id)
            || !valid_timestamp(&updated_at)
            || (self.metadata.contains_key("execution_binding") && plan.execution_binding.is_none())
            || (self.metadata.contains_key("output_policy") && plan.output_policy.is_none())
        {
            return Err(TransformError::RestartPrecondition);
        }
        let next_attempt = self
            .attempt
            .checked_add(1)
            .ok_or(TransformError::CounterOverflow)?;
        let previous = json!({
            "attempt": self.attempt,
            "state": self.state,
            "plan_hash": self.plan_hash,
            "error": self.error,
            "updated_at": self.updated_at,
            "request": self.metadata.get("request"),
            "plan_snapshot": self.metadata.get("plan_snapshot"),
            "execution_binding": self.metadata.get("execution_binding"),
            "output_policy": self.metadata.get("output_policy"),
            "guard_generation": self.metadata.get("guard_generation"),
            "resource_locks": self.metadata.get("resource_locks"),
            "input_location_ids": self.metadata.get("input_location_ids"),
            "input_identity": self.metadata.get("input_identity"),
            "input_locations": self.metadata.get("input_locations"),
            "attempt_execution": self.metadata.get("attempt_execution"),
            "attempt_closed": self.metadata.get("attempt_closed"),
            "native_owner": self.metadata.get("native_owner"),
            "publication": self.metadata.get("publication"),
            "publication_sha256": self.metadata.get("publication_sha256"),
            "planned_output_variant_id": self.metadata.get("planned_output_variant_id"),
            "correlation_id": self.metadata.get("correlation_id"),
        });
        let history = self
            .metadata
            .get_mut("attempt_history")
            .and_then(Value::as_array_mut)
            .ok_or(TransformError::InvalidJob)?;
        history.push(previous);
        self.metadata.remove("attempt_execution");
        self.metadata.remove("attempt_closed");
        self.metadata.remove("native_owner");
        self.metadata.remove("publication");
        self.metadata.remove("publication_sha256");
        self.metadata.insert(
            "request".into(),
            serde_json::to_value(&plan.request).map_err(|_| TransformError::InvalidJob)?,
        );
        self.metadata.insert(
            "plan_snapshot".into(),
            serde_json::to_value(&plan.response).map_err(|_| TransformError::InvalidJob)?,
        );
        if let Some(binding) = &plan.execution_binding {
            self.metadata.insert(
                "execution_binding".into(),
                serde_json::to_value(binding).map_err(|_| TransformError::InvalidJob)?,
            );
        } else {
            self.metadata.remove("execution_binding");
        }
        if let Some(policy) = &plan.output_policy {
            self.metadata.insert(
                "output_policy".into(),
                serde_json::to_value(policy).map_err(|_| TransformError::InvalidJob)?,
            );
        } else {
            self.metadata.remove("output_policy");
        }
        self.attempt = next_attempt;
        self.row_version = self
            .row_version
            .checked_add(1)
            .ok_or(TransformError::CounterOverflow)?;
        self.updated_at = updated_at;
        self.adapter_id = resolved_adapter_id;
        self.tool_version.clone_from(&plan.request.tool_version);
        self.parameters.clone_from(&plan.request.parameters);
        self.plan_hash.clone_from(&plan.response.plan_hash);
        self.state = TransformationState::Queued;
        self.error = None;
        self.output_variant_id = None;
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{
        estimate::{Confidence, VolumeEstimate},
        units::DecimalU64,
    };

    const VARIANT: &str = "0192aaaa-0000-7000-8000-000000000001";
    const ARTIFACT: &str = "0192aaaa-0000-7000-8000-000000000002";
    const DESTINATION: &str = "0192aaaa-0000-7000-8000-000000000003";
    const ADAPTER: &str = "0192aaaa-0000-7000-8000-000000000004";
    const PLAN: &str = "0192aaaa-0000-7000-8000-000000000005";
    const JOB: &str = "0192aaaa-0000-7000-8000-000000000006";
    const TOOL: &str = "0192aaaa-0000-7000-8000-000000000007";
    const NOW: &str = "2026-10-23T12:00:00.000Z";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "0192aaaa-0000-7000-8000-000000000009".into(),
            correlation_id: "0192aaaa-0000-7000-8000-000000000010".into(),
        }
    }

    fn request() -> TransformationRequest {
        TransformationRequest {
            input_variant_id: VARIANT.into(),
            adapter_id: "transformer-adapter".into(),
            tool_version: "2.4.1".into(),
            target_format: "GGUF".into(),
            parameters: json!({"quantization":"Q4_K_M"}),
            storage_location_id: DESTINATION.into(),
            hard_budget_bytes: Some(DecimalU64::new(1000).unwrap()),
        }
    }

    fn input() -> Vec<InputArtifact> {
        vec![InputArtifact {
            artifact_id: ARTIFACT.into(),
            relative_path: "model.safetensors".into(),
            size_bytes: DecimalU64::new(800).unwrap(),
            sha256: HASH.into(),
        }]
    }

    fn estimate() -> StorageEstimate {
        StorageEstimate {
            download_bytes: None,
            final_artifact_bytes: DecimalU64::new(700),
            installed_bytes: None,
            derived_cache_bytes: None,
            temporary_peak_bytes: DecimalU64::new(1200),
            existing_bytes: DecimalU64::new(800),
            peak_extra_bytes: DecimalU64::new(1900),
            required_free_bytes: DecimalU64::new(2000),
            upper_bound_bytes: DecimalU64::new(1900),
            confidence: Confidence::Estimated,
            reason: "preflight de fixture".into(),
            evidence_refs: vec!["fixture:transform-plan".into()],
            per_volume: vec![VolumeEstimate {
                volume_key: "volume-test".into(),
                peak_extra_bytes: DecimalU64::new(1900).unwrap(),
                margin_bytes: DecimalU64::new(100).unwrap(),
                other_reservations_bytes: DecimalU64::new(0).unwrap(),
                usable_free_bytes: DecimalU64::new(3000).unwrap(),
                eligible: true,
                reason: "fits".into(),
            }],
        }
    }

    fn draft() -> PlanDraft {
        PlanDraft {
            operation: "transform".into(),
            resource_ids: vec![VARIANT.into(), DESTINATION.into()],
            storage_estimate: estimate(),
            steps: vec![PlanStep {
                phase: "convert".into(),
                adapter_id: Some("transformer-adapter".into()),
                request: json!({"tool_version":"2.4.1","target_format":"GGUF"}),
            }],
            expires_at: "2026-10-23T12:15:00.000Z".into(),
            approval_required: false,
        }
    }

    fn plan() -> SealedTransformationPlan {
        seal_plan(PLAN, request(), input(), draft()).unwrap()
    }

    fn execution_binding() -> TransformationExecutionBinding {
        TransformationExecutionBinding {
            adapter_installation_id: TOOL.into(),
            conformance_run_id: "0192aaaa-0000-7000-8000-000000000011".into(),
            conformance_run_sha256: "b".repeat(64),
            profile_sha256: "c".repeat(64),
            worker_plan_id: "0192aaaa-0000-7000-8000-000000000012".into(),
            worker_plan_hash: "d".repeat(64),
        }
    }

    fn output_policy() -> TransformationOutputPolicy {
        TransformationOutputPolicy {
            worker_volume_key: "volume-test".into(),
            cas_volume_key: "volume-test".into(),
            max_total_bytes: DecimalU64::new(700).unwrap(),
            max_file_bytes: DecimalU64::new(600).unwrap(),
            max_artifacts: 3,
            worker_control_bytes: DecimalU64::new(100).unwrap(),
            control_bytes: DecimalU64::new(10).unwrap(),
        }
    }

    #[test]
    fn publication_policy_is_sealed_and_tampering_never_authorizes_extra_output() {
        let plan = seal_publication_plan(
            PLAN,
            request(),
            input(),
            draft(),
            execution_binding(),
            output_policy(),
        )
        .unwrap();
        plan.verify_seal().unwrap();
        assert_ne!(
            plan.response.plan_hash,
            seal_bound_plan(PLAN, request(), input(), draft(), execution_binding())
                .unwrap()
                .response
                .plan_hash
        );
        let public = serde_json::to_value(&plan.response).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 8);
        assert!(public.get("output_policy").is_none());
        for field in [
            "worker_volume_key",
            "cas_volume_key",
            "max_total_bytes",
            "max_file_bytes",
            "max_artifacts",
            "worker_control_bytes",
            "control_bytes",
        ] {
            let mut value = serde_json::to_value(&plan).unwrap();
            value["output_policy"][field] = match field {
                "worker_volume_key" | "cas_volume_key" => json!("other-volume"),
                "max_artifacts" => json!(2),
                "max_total_bytes" => json!("699"),
                "max_file_bytes" => json!("599"),
                _ => json!("9"),
            };
            assert!(
                serde_json::from_value::<SealedTransformationPlan>(value)
                    .unwrap()
                    .verify_seal()
                    .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn job_preserves_publication_policy_and_cannot_restart_with_a_downgraded_legacy_plan() {
        let plan = seal_publication_plan(
            PLAN,
            request(),
            input(),
            draft(),
            execution_binding(),
            output_policy(),
        )
        .unwrap();
        let mut job = TransformationJob::from_plan(JOB, TOOL, &plan, NOW, &context()).unwrap();
        assert_eq!(job.metadata["output_policy"], json!(output_policy()));
        job.metadata["output_policy"]["max_artifacts"] = json!(1);
        assert!(job.validate().is_err());
        job.metadata["output_policy"] = json!(output_policy());
        job.state = TransformationState::FailedRecoverable;
        job.validate().unwrap();
        let old = job.clone();
        let legacy = seal_bound_plan(
            "0192aaaa-0000-7000-8000-000000000017",
            request(),
            input(),
            draft(),
            execution_binding(),
        )
        .unwrap();
        let proof = verify_observed_input_hashes(&input(), &input()).unwrap();
        assert!(job
            .retry_with_preflight(&legacy, &proof, TOOL, NOW)
            .is_err());
        assert_eq!(job, old);
        let next = seal_publication_plan(
            "0192aaaa-0000-7000-8000-000000000018",
            request(),
            input(),
            draft(),
            execution_binding(),
            output_policy(),
        )
        .unwrap();
        job.retry_with_preflight(&next, &proof, TOOL, NOW).unwrap();
        assert_eq!(job.metadata["output_policy"], json!(output_policy()));
        assert_eq!(
            job.metadata["attempt_history"][0]["output_policy"],
            json!(output_policy())
        );
    }

    #[test]
    fn bound_plan_seals_every_worker_identity_component_without_changing_public_dto() {
        let bound =
            seal_bound_plan(PLAN, request(), input(), draft(), execution_binding()).unwrap();
        bound.verify_seal().unwrap();
        assert_ne!(bound.response.plan_hash, plan().response.plan_hash);
        let public = serde_json::to_value(&bound.response).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 8);
        assert!(public.get("execution_binding").is_none());
        for field in [
            "adapter_installation_id",
            "conformance_run_id",
            "worker_plan_id",
            "conformance_run_sha256",
            "profile_sha256",
            "worker_plan_hash",
        ] {
            let mut document = serde_json::to_value(&bound).unwrap();
            document["execution_binding"][field] = if field.ends_with("id") {
                json!("0192aaaa-0000-7000-8000-000000000013")
            } else {
                json!("e".repeat(64))
            };
            let changed: SealedTransformationPlan = serde_json::from_value(document).unwrap();
            assert_eq!(
                changed.verify_seal(),
                Err(TransformError::PlanSealMismatch),
                "{field}"
            );
        }
    }

    #[test]
    fn legacy_plan_without_worker_binding_remains_readable_for_recovery() {
        let legacy = plan();
        assert_eq!(
            legacy.response.plan_hash,
            "dedef807a44268c91f47c0a8e91e5edced2dc1cf648fc9d92b15811e8e828ed9"
        );
        let document = serde_json::to_value(&legacy).unwrap();
        assert!(document.get("execution_binding").is_none());
        let decoded: SealedTransformationPlan = serde_json::from_value(document).unwrap();
        decoded.verify_seal().unwrap();
        assert_eq!(decoded, legacy);
    }

    #[test]
    fn malformed_execution_binding_is_rejected_not_treated_as_qualification() {
        let mut binding = serde_json::to_value(execution_binding()).unwrap();
        binding["approved"] = json!(true);
        assert!(serde_json::from_value::<TransformationExecutionBinding>(binding).is_err());
        let mut invalid = execution_binding();
        invalid.worker_plan_hash = "not-a-hash".into();
        assert_eq!(
            seal_bound_plan(PLAN, request(), input(), draft(), invalid).unwrap_err(),
            TransformError::InvalidExecutionBinding
        );
        let mut invalid = execution_binding();
        invalid.adapter_installation_id = "not-an-id".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn bound_job_retains_identity_and_rejects_a_different_resolved_adapter() {
        let bound =
            seal_bound_plan(PLAN, request(), input(), draft(), execution_binding()).unwrap();
        let job = TransformationJob::from_plan(JOB, TOOL, &bound, NOW, &context()).unwrap();
        job.validate().unwrap();
        assert_eq!(
            job.metadata["execution_binding"],
            serde_json::to_value(execution_binding()).unwrap()
        );
        assert_eq!(
            TransformationJob::from_plan(JOB, ADAPTER, &bound, NOW, &context()).unwrap_err(),
            TransformError::InvalidJob
        );
        let mut malformed = job;
        malformed
            .metadata
            .insert("execution_binding".into(), json!({"approved":true}));
        assert_eq!(malformed.validate(), Err(TransformError::InvalidJob));
    }

    #[test]
    fn retry_overflow_does_not_mutate_the_failed_job_or_its_history() {
        let mut job = TransformationJob::from_plan(JOB, TOOL, &plan(), NOW, &context()).unwrap();
        job.transition(TransformationState::Running, NOW).unwrap();
        job.mark_interrupted_recoverable(json!({"code":"TOOL_CRASH"}), NOW)
            .unwrap();
        job.row_version = i64::MAX;
        let before = job.clone();
        let new_plan = seal_plan(
            "0192aaaa-0000-7000-8000-000000000014",
            request(),
            input(),
            draft(),
        )
        .unwrap();
        let proof = verify_observed_input_hashes(&input(), &input()).unwrap();
        assert_eq!(
            job.retry_with_preflight(&new_plan, &proof, TOOL, NOW),
            Err(TransformError::CounterOverflow)
        );
        assert_eq!(job, before);
    }

    #[test]
    fn bound_retry_keeps_the_new_identity_and_cannot_downgrade_to_an_unbound_plan() {
        let first =
            seal_bound_plan(PLAN, request(), input(), draft(), execution_binding()).unwrap();
        let mut job = TransformationJob::from_plan(JOB, TOOL, &first, NOW, &context()).unwrap();
        job.transition(TransformationState::Running, NOW).unwrap();
        job.mark_interrupted_recoverable(json!({"code":"TOOL_CRASH"}), NOW)
            .unwrap();
        let before = job.clone();
        let proof = verify_observed_input_hashes(&input(), &input()).unwrap();
        let id = "0192aaaa-0000-7000-8000-000000000014";
        let legacy = seal_plan(id, request(), input(), draft()).unwrap();
        assert_eq!(
            job.retry_with_preflight(&legacy, &proof, TOOL, NOW),
            Err(TransformError::RestartPrecondition)
        );
        assert_eq!(job, before);
        let mut binding = execution_binding();
        binding.worker_plan_id = "0192aaaa-0000-7000-8000-000000000015".into();
        let next = seal_bound_plan(id, request(), input(), draft(), binding.clone()).unwrap();
        assert!(job
            .retry_with_preflight(&next, &proof, ADAPTER, NOW)
            .is_err());
        assert_eq!(job, before);
        job.retry_with_preflight(&next, &proof, TOOL, NOW).unwrap();
        assert_eq!(job.attempt, before.attempt + 1);
        assert_eq!(
            job.metadata["execution_binding"],
            serde_json::to_value(binding).unwrap()
        );
        assert_eq!(job.metadata["attempt_history"].as_array().unwrap().len(), 1);
        assert_eq!(job.state, TransformationState::Queued);
        job.validate().unwrap();
    }

    #[test]
    fn plan_sella_input_herramienta_parametros_storage_y_steps_de_forma_determinista() {
        let first = plan();
        first.verify_seal().unwrap();
        let mut reordered = input();
        reordered.reverse();
        let same = seal_plan(PLAN, request(), reordered, draft()).unwrap();
        assert_eq!(first.response.plan_hash, same.response.plan_hash);

        let mut changed = request();
        changed.parameters = json!({"quantization":"Q8_0"});
        let changed = seal_plan(PLAN, changed, input(), draft()).unwrap();
        assert_ne!(first.response.plan_hash, changed.response.plan_hash);

        let mut changed_input = input();
        changed_input[0].sha256 =
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
        let changed_input = seal_plan(PLAN, request(), changed_input, draft()).unwrap();
        assert_ne!(first.response.plan_hash, changed_input.response.plan_hash);
    }

    #[test]
    fn plan_rechaza_snapshot_duplicado_path_inseguro_y_recursos_ajenos() {
        let mut duplicate = input();
        duplicate.push(duplicate[0].clone());
        assert_eq!(
            seal_plan(PLAN, request(), duplicate, draft()).unwrap_err(),
            TransformError::InvalidInputSnapshot
        );
        let mut unsafe_path = input();
        unsafe_path[0].relative_path = "../outside/model.bin".into();
        assert_eq!(
            seal_plan(PLAN, request(), unsafe_path, draft()).unwrap_err(),
            TransformError::InvalidInputSnapshot
        );
        let mut unrelated = draft();
        unrelated.resource_ids = vec![DESTINATION.into()];
        assert_eq!(
            seal_plan(PLAN, request(), input(), unrelated).unwrap_err(),
            TransformError::InvalidPlan
        );
    }

    #[test]
    fn cambio_despues_del_sellado_se_detecta_antes_de_aceptar_el_job() {
        let mut tampered = plan();
        tampered.response.storage_estimate.peak_extra_bytes = DecimalU64::new(1);
        assert_eq!(
            tampered.verify_seal(),
            Err(TransformError::PlanSealMismatch)
        );
    }

    #[test]
    fn crash_persiste_como_fallo_recuperable_sin_publicar_salida() {
        let mut job = TransformationJob::from_plan(JOB, ADAPTER, &plan(), NOW, &context()).unwrap();
        job.transition(TransformationState::Running, NOW).unwrap();
        assert!(job
            .state
            .can_transition_to(TransformationState::FailedRecoverable));
        job.mark_interrupted_recoverable(json!({"code":"TOOL_CRASH","retryable":true}), NOW)
            .unwrap();
        assert_eq!(job.state, TransformationState::FailedRecoverable);
        assert_eq!(job.output_variant_id, None);
        assert_eq!(job.error.as_ref().unwrap()["code"], "TOOL_CRASH");
        assert_eq!(
            job.metadata["input_fingerprint_sha256"],
            plan().input_fingerprint().unwrap()
        );
    }

    #[test]
    fn reinicio_exige_hashes_de_input_intactos_y_nuevo_preflight_y_conserva_diagnostico() {
        let original = input();
        let mut job = TransformationJob::from_plan(JOB, ADAPTER, &plan(), NOW, &context()).unwrap();
        job.transition(TransformationState::Running, NOW).unwrap();
        job.mark_interrupted_recoverable(json!({"code":"TOOL_CRASH"}), NOW)
            .unwrap();

        let mut corrupt = original.clone();
        corrupt[0].sha256 =
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
        assert_eq!(
            verify_observed_input_hashes(&original, &corrupt).unwrap_err(),
            TransformError::RestartPrecondition
        );
        let proof = verify_observed_input_hashes(&original, &original).unwrap();
        assert_eq!(
            job.retry_with_preflight(&plan(), &proof, TOOL, NOW),
            Err(TransformError::RestartPrecondition)
        );

        let changed = seal_plan(
            "0192aaaa-0000-7000-8000-000000000008",
            request(),
            original,
            {
                let mut new_draft = draft();
                new_draft.expires_at = "2026-10-23T12:20:00.000Z".into();
                new_draft
            },
        )
        .unwrap();
        job.retry_with_preflight(&changed, &proof, TOOL, NOW)
            .unwrap();
        assert_eq!(job.state, TransformationState::Queued);
        assert_eq!(job.attempt, 1);
        assert_eq!(job.error, None);
        assert_eq!(
            job.metadata["attempt_history"][0]["error"]["code"],
            "TOOL_CRASH"
        );
        assert_eq!(
            job.metadata["input_fingerprint_sha256"],
            proof.fingerprint_sha256
        );
    }

    #[test]
    fn failed_y_cancelled_no_se_reanudan_mediante_transiciones_ordinarias() {
        assert!(!TransformationState::Failed.can_transition_to(TransformationState::Queued));
        assert!(
            !TransformationState::FailedRecoverable.can_transition_to(TransformationState::Queued)
        );
        assert!(TransformationState::Completed.is_terminal());
        assert!(TransformationState::Cancelled.is_terminal());
        assert!(!TransformationState::Completed.can_transition_to(TransformationState::Queued));
        assert!(!TransformationState::Cancelled.can_transition_to(TransformationState::Running));
    }

    #[test]
    fn dto_rechaza_campos_extra_y_presupuestos_no_canonicos() {
        let mut serialized = serde_json::to_value(request()).unwrap();
        serialized["unexpected"] = Value::Bool(true);
        assert!(serde_json::from_value::<TransformationRequest>(serialized).is_err());

        let mut serialized = serde_json::to_value(request()).unwrap();
        serialized["hard_budget_bytes"] = Value::String("01".into());
        assert!(serde_json::from_value::<TransformationRequest>(serialized).is_err());
        assert!(!valid_timestamp("2026-99-99T99:99:99.000Z"));
        assert!(valid_timestamp(NOW));
    }
}
