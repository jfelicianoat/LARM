//! Typed internal RPC over an authenticated guardian channel. It preserves
//! TransformationWorker's validation/P022 checks and grants no Core admission.
use super::*;
use crate::transformation::{
    SessionIdentity, TransformationDescription, TransformationOutputManifest, TransformationWorker,
};
use larm_adapter_sdk::{permissions::Access, source::ArtifactManifest};
use larm_domain::{
    bridge::JobReceipt,
    transformation_execution::TransformationClaim,
    transformations::{
        ExecuteTransformationPlan, TransformationExecutionBinding, TransformationPlanResponse,
        TransformationRequest, TransformationState,
    },
};
use serde_json::Value;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredAccess {
    reads: Vec<PathBuf>,
    writes: Vec<PathBuf>,
    urls: Vec<String>,
    processes: Vec<String>,
    secrets: Vec<String>,
}
impl From<&Access> for DeclaredAccess {
    fn from(a: &Access) -> Self {
        Self {
            reads: a.reads.clone(),
            writes: a.writes.clone(),
            urls: a.urls.clone(),
            processes: a.processes.clone(),
            secrets: a.secrets.clone(),
        }
    }
}
impl DeclaredAccess {
    fn resolve(&self) -> Access {
        Access {
            reads: self.reads.clone(),
            writes: self.writes.clone(),
            urls: self.urls.clone(),
            processes: self.processes.clone(),
            secrets: self.secrets.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", deny_unknown_fields)]
enum Operation {
    Describe,
    Plan(TransformationRequest),
    Execute(ExecuteTransformationPlan),
    Verify {
        job_id: String,
    },
    /// `core.cancel` del worker para el RPC en vuelo cuya respuesta ya no se
    /// quiere. Un id ajeno o sin solicitud pendiente es un no-op (`false`).
    Cancel {
        request_id: String,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationRpc {
    id: String,
    operation: Operation,
    access: DeclaredAccess,
    timeout_ms: Option<u64>,
}
impl TransformationRpc {
    pub fn is_execute(&self) -> bool {
        matches!(self.operation, Operation::Execute(_))
    }
    /// `true` sólo para `Operation::Cancel`.
    pub fn is_cancel(&self) -> bool {
        matches!(self.operation, Operation::Cancel { .. })
    }
    pub fn check_registered_scope(
        &self,
        binding: &TransformationExecutionBinding,
        worker_job: Option<&str>,
    ) -> Result<(), LarmError> {
        match &self.operation {
            Operation::Execute(request)
                if request.plan_id == binding.worker_plan_id
                    && request.plan_hash == binding.worker_plan_hash =>
            {
                Ok(())
            }
            Operation::Verify { job_id } if worker_job == Some(job_id.as_str()) => Ok(()),
            // La cancelación no concede IO: sólo pide abortar un RPC propio en
            // vuelo. El worker ignora ids sin solicitud pendiente.
            Operation::Cancel { request_id } if larm_domain::bridge::is_uuid_v7(request_id) => {
                Ok(())
            }
            _ => Err(error("guardian_registered_rpc_scope_invalid")),
        }
    }
    pub(super) fn exchange_timeout(&self) -> Result<Duration, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(&self.id)
            || self.timeout_ms.is_some_and(|ms| ms == 0 || ms > 600_000)
        {
            return Err(error("guardian_rpc_request_invalid"));
        }
        Ok(Duration::from_millis(self.timeout_ms.unwrap_or(30_000))
            .saturating_add(Duration::from_secs(30)))
    }

    /// Invokes the existing typed worker, including its qualification checks
    /// before/after the RPC and declared-access check before sending anything.
    pub async fn invoke(&self, worker: &TransformationWorker) -> Result<Value, LarmError> {
        self.exchange_timeout()?;
        let access = self.access.resolve();
        let timeout = self.timeout_ms.map(Duration::from_millis);
        let value = match &self.operation {
            Operation::Describe => {
                serde_json::to_value(worker.describe(&self.id, &access, timeout).await?)
            }
            Operation::Plan(request) => {
                serde_json::to_value(worker.plan(&self.id, request, &access, timeout).await?)
            }
            Operation::Execute(request) => {
                serde_json::to_value(worker.execute(&self.id, request, &access, timeout).await?)
            }
            Operation::Verify { job_id } => {
                serde_json::to_value(worker.verify(&self.id, job_id, &access, timeout).await?)
            }
            Operation::Cancel { request_id } => {
                serde_json::to_value(worker.cancel(request_id).await?)
            }
        };
        value.map_err(|_| error("guardian_rpc_reply_invalid"))
    }
}

/// Execution client created from the server's private bootstrap, never from a
/// persisted public descriptor. Lost private control authority cannot be
/// reconstructed to resume execution; recovery may only observe/close.
pub struct GuardianTransformationClient {
    owner: NativeGuardian,
    signer: SigningKey,
    identity: SessionIdentity,
}

/// Validate labels before sending Verify. The helper independently validates
/// the committed owner/current qualification and the job returned by its one
/// Execute. Public descriptors and caller-supplied JSON alone grant no proof.
fn output_owner(
    descriptor: &GuardianDescriptor,
    claim: &TransformationClaim,
) -> Result<TransformationNativeOwner, LarmError> {
    descriptor.validate()?;
    claim.fence.validate()?;
    claim
        .plan
        .verify_seal()
        .map_err(|_| error("guardian_output_claim_invalid"))?;
    claim
        .job
        .validate()
        .map_err(|_| error("guardian_output_claim_invalid"))?;
    let owner = descriptor
        .native_owner
        .as_ref()
        .ok_or_else(|| error("guardian_output_registered_owner_required"))?;
    if owner.fence != claim.fence
        || claim.plan.execution_binding.as_ref() != Some(&owner.binding)
        || claim.job.id != claim.fence.job_id
        || claim.job.plan_hash != claim.fence.plan_hash
        || claim.plan.response.plan_hash != claim.fence.plan_hash
        || claim.job.attempt != claim.fence.attempt
        || claim.job.adapter_id != owner.binding.adapter_installation_id
        || claim.plan.request.adapter_id != owner.adapter_id
        || !matches!(
            claim.job.state,
            TransformationState::Running | TransformationState::Verifying
        )
        || claim.job.metadata.get("native_owner") != Some(&serde_json::json!(owner))
        || claim.job.metadata.get("attempt_execution") != Some(&serde_json::json!(claim.fence))
        || claim.job.metadata.get("actor_id").and_then(Value::as_str)
            != Some(claim.context.actor_id.as_str())
        || claim
            .job
            .metadata
            .get("correlation_id")
            .and_then(Value::as_str)
            != Some(claim.context.correlation_id.as_str())
    {
        return Err(error("guardian_output_attempt_mismatch"));
    }
    Ok(owner.clone())
}

impl GuardianBootstrap {
    pub async fn transformation_client(&self) -> Result<GuardianTransformationClient, LarmError> {
        let identity = self
            .descriptor
            .session_identity
            .clone()
            .ok_or_else(|| error("guardian_transformation_identity_missing"))?;
        let owner = NativeGuardian::attach(self.descriptor.clone()).await?;
        Ok(GuardianTransformationClient {
            owner,
            signer: SigningKey::from_bytes(&self.signing_seed),
            identity,
        })
    }
}

impl GuardianTransformationClient {
    pub fn owner(&self) -> &NativeGuardian {
        &self.owner
    }
    pub fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    async fn call_progress(&self) -> Result<Vec<larm_adapter_sdk::protocol::Progress>, LarmError> {
        let response = self
            .owner
            .exchange_signed(Action::Progress, Some(&self.signer))
            .await?;
        let value = response
            .result
            .ok_or_else(|| error("guardian_rpc_reply_missing"))??;
        serde_json::from_value(value).map_err(|_| error("guardian_rpc_reply_invalid"))
    }

    async fn call<T: DeserializeOwned>(
        &self,
        id: &str,
        operation: Operation,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<T, LarmError> {
        let timeout_ms = timeout
            .map(|d| {
                u64::try_from(d.as_millis()).map_err(|_| error("guardian_rpc_request_invalid"))
            })
            .transpose()?;
        let request = TransformationRpc {
            id: id.into(),
            operation,
            access: access.into(),
            timeout_ms,
        };
        request.exchange_timeout()?;
        let response = self
            .owner
            .exchange_signed(Action::Rpc(Box::new(request)), Some(&self.signer))
            .await?;
        let value = response
            .result
            .ok_or_else(|| error("guardian_rpc_reply_missing"))??;
        serde_json::from_value(value).map_err(|_| error("guardian_rpc_reply_invalid"))
    }

    pub async fn describe(
        &self,
        id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationDescription, LarmError> {
        self.call(id, Operation::Describe, access, timeout).await
    }
    pub async fn plan(
        &self,
        id: &str,
        request: &TransformationRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationPlanResponse, LarmError> {
        self.call(id, Operation::Plan(request.clone()), access, timeout)
            .await
    }
    pub async fn execute(
        &self,
        id: &str,
        request: &ExecuteTransformationPlan,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<JobReceipt, LarmError> {
        self.call(id, Operation::Execute(request.clone()), access, timeout)
            .await
    }
    pub async fn verify(
        &self,
        id: &str,
        job_id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<ArtifactManifest, LarmError> {
        self.call(
            id,
            Operation::Verify {
                job_id: job_id.into(),
            },
            access,
            timeout,
        )
        .await
    }

    /// Retira del helper las notificaciones `core.progress` acumuladas del
    /// worker. Observación pura: lista posiblemente vacía, sin autoridad.
    pub async fn progress(&self) -> Result<Vec<larm_adapter_sdk::protocol::Progress>, LarmError> {
        self.call_progress().await
    }

    /// Pide al helper `core.cancel` del RPC en vuelo `request_id` (execute o
    /// verify de este intento). Devuelve si el worker tenía esa solicitud
    /// pendiente; un id ajeno es un no-op honesto (`false`).
    pub async fn cancel(
        &self,
        id: &str,
        request_id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<bool, LarmError> {
        self.call(
            id,
            Operation::Cancel {
                request_id: request_id.into(),
            },
            access,
            timeout,
        )
        .await
    }

    /// An opaque observed reply for this registered attempt, minted only after
    /// a fresh signed Verify and live qualification/registration checks. It
    /// grants neither physical integrity nor closure/publication authority.
    pub async fn verify_claim_output(
        &self,
        claim: &TransformationClaim,
        id: &str,
        worker_job_id: &str,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<TransformationOutputManifest, LarmError> {
        let owner = output_owner(&self.owner.descriptor, claim)?;
        self.owner.check(Action::Recheck).await?;
        let manifest = self.verify(id, worker_job_id, access, timeout).await?;
        self.owner.check(Action::Recheck).await?;
        TransformationOutputManifest::observed(
            owner.fence,
            owner.binding,
            worker_job_id.into(),
            manifest,
        )
    }
    pub fn execution_binding(
        &self,
        plan: &TransformationPlanResponse,
    ) -> Result<TransformationExecutionBinding, LarmError> {
        let result = TransformationExecutionBinding {
            adapter_installation_id: self.identity.installation_id.clone(),
            conformance_run_id: self.identity.run_id.clone(),
            conformance_run_sha256: self.identity.run_sha256.clone(),
            profile_sha256: self.identity.profile_sha256.clone(),
            worker_plan_id: plan.plan_id.clone(),
            worker_plan_hash: plan.plan_hash.clone(),
        };
        result
            .validate()
            .map_err(|_| error("guardian_execution_binding_invalid"))?;
        Ok(result)
    }
    pub async fn release(self) -> Result<(), LarmError> {
        self.owner.release().await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn intento_registrado_solo_ejecuta_plan_fijado_y_verifica_su_propio_job() {
        let binding = TransformationExecutionBinding {
            adapter_installation_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_sha256: "a".repeat(64),
            profile_sha256: "b".repeat(64),
            worker_plan_id: uuid::Uuid::now_v7().to_string(),
            worker_plan_hash: "c".repeat(64),
        };
        let mut rpc = TransformationRpc {
            id: uuid::Uuid::now_v7().to_string(),
            operation: Operation::Describe,
            access: (&Access::default()).into(),
            timeout_ms: None,
        };
        assert!(rpc.check_registered_scope(&binding, None).is_err());
        let execute = ExecuteTransformationPlan {
            plan_id: binding.worker_plan_id.clone(),
            plan_hash: binding.worker_plan_hash.clone(),
            approval_id: None,
        };
        rpc.operation = Operation::Execute(execute.clone());
        assert!(rpc.is_execute());
        rpc.check_registered_scope(&binding, None).unwrap();
        let mut foreign = execute.clone();
        foreign.plan_id = uuid::Uuid::now_v7().to_string();
        rpc.operation = Operation::Execute(foreign);
        assert!(rpc.check_registered_scope(&binding, None).is_err());
        let mut foreign = execute;
        foreign.plan_hash = "d".repeat(64);
        rpc.operation = Operation::Execute(foreign);
        assert!(rpc.check_registered_scope(&binding, None).is_err());
        let job = uuid::Uuid::now_v7().to_string();
        rpc.operation = Operation::Verify {
            job_id: job.clone(),
        };
        assert!(!rpc.is_execute());
        assert!(rpc.check_registered_scope(&binding, None).is_err());
        assert!(rpc
            .check_registered_scope(&binding, Some("another-job"))
            .is_err());
        rpc.check_registered_scope(&binding, Some(&job)).unwrap();
    }
    #[test]
    fn cancelacion_registrada_exige_id_propio_y_codec_redondo() {
        let binding = TransformationExecutionBinding {
            adapter_installation_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_sha256: "a".repeat(64),
            profile_sha256: "b".repeat(64),
            worker_plan_id: uuid::Uuid::now_v7().to_string(),
            worker_plan_hash: "c".repeat(64),
        };
        let mut rpc = TransformationRpc {
            id: uuid::Uuid::now_v7().to_string(),
            operation: Operation::Cancel {
                request_id: uuid::Uuid::now_v7().to_string(),
            },
            access: (&Access::default()).into(),
            timeout_ms: None,
        };
        assert!(!rpc.is_execute());
        rpc.check_registered_scope(&binding, None).unwrap();
        rpc.operation = Operation::Cancel {
            request_id: "no-es-uuid".into(),
        };
        assert!(rpc.check_registered_scope(&binding, None).is_err());
        // Codec: la variante nueva viaja y vuelve sin campos desconocidos.
        let wire = serde_json::to_string(&rpc).unwrap();
        let round: TransformationRpc = serde_json::from_str(&wire).unwrap();
        assert_eq!(serde_json::to_string(&round).unwrap(), wire);
        assert!(round.is_cancel());
        assert!(!round.is_execute());
    }
    #[test]
    fn rpc_cerrado_plazo_acotado_y_id_obligatorio() {
        let request = TransformationRpc {
            id: uuid::Uuid::now_v7().to_string(),
            operation: Operation::Describe,
            access: (&Access::default()).into(),
            timeout_ms: None,
        };
        request.exchange_timeout().unwrap();
        for bad in [0, 600_001, u64::MAX] {
            let mut wrong = request.clone();
            wrong.timeout_ms = Some(bad);
            assert!(wrong.exchange_timeout().is_err());
        }
        let mut wrong = request.clone();
        wrong.id = "not-an-id".into();
        assert!(wrong.exchange_timeout().is_err());
        let mut json = serde_json::to_value(&request).unwrap();
        json["approved"] = serde_json::json!(true);
        assert!(serde_json::from_value::<TransformationRpc>(json).is_err());
    }
}
