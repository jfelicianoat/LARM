//! Dedicated Windows process owner for recovery across a host crash.
//!
//! The descriptor is routing/public verification metadata, never closure
//! authority. Every observation challenges a live guardian which retains the
//! outer Job Object and Worker's actual stdout/stderr/exit supervisors. This
//! component does not grant qualification. Registered attempts combine its
//! live native witness with exact server-owned job/journal identity.
use crate::worker::{ProcessJob, TreeKill, Worker, WorkerSpec};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use larm_domain::{
    error::{ErrorCode, LarmError},
    transformation_execution::{
        TransformationAttemptFence, TransformationClaimProposal, TransformationNativeOwner,
    },
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer},
};

mod activity;
mod control;
mod service;
pub use service::{serve, serve_with};
mod attempt;
mod launch;
mod transformation;
pub use attempt::NativeTransformationAttempt;
pub use launch::GuardianProcess;
pub use transformation::{GuardianTransformationClient, TransformationRpc};
#[cfg(test)]
mod tests;

const MAX_MESSAGE: usize = 512 * 1024;
const MAX_BOOTSTRAP: usize = 512 * 1024;
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);
const SIGNING_CONTEXT: &[u8] = b"larm-native-worker-guardian-v1\0";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardianDescriptor {
    pub schema: u32,
    pub session_id: String,
    pub fence: TransformationAttemptFence,
    pub public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<crate::transformation::SessionIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_owner: Option<TransformationNativeOwner>,
}

impl GuardianDescriptor {
    fn validate(&self) -> Result<VerifyingKey, LarmError> {
        self.fence.validate()?;
        if !larm_domain::bridge::is_uuid_v7(&self.session_id)
            || !matches!(
                (self.schema, self.session_identity.is_some()),
                (1, false) | (2, true) | (3, true)
            )
        {
            return Err(error("guardian_descriptor_invalid"));
        }
        if let Some(identity) = &self.session_identity {
            identity.validate()?;
        }
        match (&self.native_owner, self.schema) {
            (None, 1 | 2) => {}
            (Some(owner), 3) => {
                owner.validate()?;
                let identity = self
                    .session_identity
                    .as_ref()
                    .ok_or_else(|| error("guardian_descriptor_invalid"))?;
                if owner.session_id != self.session_id
                    || owner.public_key != self.public_key
                    || owner.fence != self.fence
                    || owner.adapter_id != identity.adapter_id
                    || owner.binding.adapter_installation_id != identity.installation_id
                    || owner.binding.conformance_run_id != identity.run_id
                    || owner.binding.conformance_run_sha256 != identity.run_sha256
                    || owner.binding.profile_sha256 != identity.profile_sha256
                {
                    return Err(error("guardian_descriptor_invalid"));
                }
            }
            _ => return Err(error("guardian_descriptor_invalid")),
        }
        let key: [u8; 32] = hex::decode(&self.public_key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| error("guardian_descriptor_invalid"))?;
        VerifyingKey::from_bytes(&key).map_err(|_| error("guardian_descriptor_invalid"))
    }

    #[must_use]
    pub fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\LARM-guardian-{}", self.session_id)
    }

    pub fn from_native_owner(owner: TransformationNativeOwner) -> Result<Self, LarmError> {
        owner.validate()?;
        let descriptor = Self {
            schema: 3,
            session_id: owner.session_id.clone(),
            public_key: owner.public_key.clone(),
            fence: owner.fence.clone(),
            session_identity: Some(crate::transformation::SessionIdentity {
                adapter_id: owner.adapter_id.clone(),
                installation_id: owner.binding.adapter_installation_id.clone(),
                run_id: owner.binding.conformance_run_id.clone(),
                run_sha256: owner.binding.conformance_run_sha256.clone(),
                profile_sha256: owner.binding.profile_sha256.clone(),
            }),
            native_owner: Some(owner),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
}

// Private launch DTO: carries environment and a secret signing seed only over
// an inherited anonymous stdin pipe. Intentionally no Debug, disk writer or CLI
// arguments for the seed. It is not a public API or an admission DTO.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardianBootstrap {
    descriptor: GuardianDescriptor,
    signing_seed: [u8; 32],
    launch: GuardianLaunch,
}

/// Private, server-origin launch document; never an API request or authority
/// to admit a Core job. Qualified launch resolves all code/permissions from DB.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum GuardianLaunch {
    Basic {
        package_dir: PathBuf,
        manifest: Box<larm_adapter_sdk::manifest::AdapterManifest>,
        environment: Vec<(String, String)>,
        work_dir: PathBuf,
    },
    Transformation {
        database: PathBuf,
        actor_id: String,
        correlation_id: String,
    },
}

impl GuardianLaunch {
    pub async fn start_basic(self, generation: u64) -> Result<Arc<dyn GuardianWorker>, LarmError> {
        let Self::Basic {
            package_dir,
            manifest,
            environment,
            work_dir,
        } = self
        else {
            return Err(error("guardian_qualified_factory_required"));
        };
        let worker = Worker::start(
            WorkerSpec {
                package_dir,
                manifest: *manifest,
                environment,
                work_dir,
            },
            Default::default(),
            generation,
            Arc::new(TreeKill),
        )
        .await?;
        Ok(worker)
    }
}

#[async_trait::async_trait]
pub trait GuardianWorker: Send + Sync {
    fn pid(&self) -> u32;
    fn io_drained(&self) -> bool;
    async fn close(&self) -> Result<(), LarmError>;
    async fn ready(&self) -> Result<(), LarmError> {
        Err(error("guardian_qualification_unavailable"))
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        Err(error("guardian_qualification_unavailable"))
    }
    async fn invoke(&self, _request: &TransformationRpc) -> Result<serde_json::Value, LarmError> {
        Err(error("guardian_rpc_unavailable"))
    }
    /// Retira las notificaciones `core.progress` acumuladas del worker
    /// (observación pura del progreso de sus solicitudes en vuelo).
    fn drain_progress(&self) -> Vec<larm_adapter_sdk::protocol::Progress> {
        Vec::new()
    }
}

#[async_trait::async_trait]
impl GuardianWorker for Worker {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn io_drained(&self) -> bool {
        self.io_drained()
    }
    async fn close(&self) -> Result<(), LarmError> {
        self.stop_and_wait().await
    }
}

impl GuardianBootstrap {
    /// The caller must already own launch authorization. This low-level P021
    /// primitive does not qualify an adapter, admit a job or release resources.
    pub fn new(spec: WorkerSpec, fence: TransformationAttemptFence) -> Result<Self, LarmError> {
        Self::from_launch(
            fence,
            None,
            GuardianLaunch::Basic {
                package_dir: spec.package_dir,
                manifest: Box::new(spec.manifest),
                environment: spec.environment,
                work_dir: spec.work_dir,
            },
        )
    }

    pub fn transformation(
        database: PathBuf,
        identity: crate::transformation::SessionIdentity,
        context: larm_domain::ports::MutationContext,
        fence: TransformationAttemptFence,
    ) -> Result<Self, LarmError> {
        identity.validate()?;
        if !database.is_absolute()
            || context.actor_id.is_empty()
            || !larm_domain::bridge::is_uuid_v7(&context.correlation_id)
        {
            return Err(error("guardian_bootstrap_invalid"));
        }
        Self::from_launch(
            fence,
            Some(identity),
            GuardianLaunch::Transformation {
                database,
                actor_id: context.actor_id,
                correlation_id: context.correlation_id,
            },
        )
    }

    /// Prepare one queued server proposal. Transformation RPC remains gated
    /// on committed registration, while P022 preparation/handshake is allowed.
    pub fn registered_transformation(
        database: PathBuf,
        identity: crate::transformation::SessionIdentity,
        proposal: &TransformationClaimProposal,
    ) -> Result<Self, LarmError> {
        proposal
            .claim
            .plan
            .verify_seal()
            .map_err(|_| error("guardian_execution_binding_invalid"))?;
        let binding = proposal
            .claim
            .plan
            .execution_binding
            .clone()
            .ok_or_else(|| error("guardian_execution_binding_invalid"))?;
        let mut bootstrap = Self::transformation(
            database,
            identity,
            proposal.claim.context.clone(),
            proposal.claim.fence.clone(),
        )?;
        let owner = TransformationNativeOwner {
            schema_version: "1.0".into(),
            session_id: bootstrap.descriptor.session_id.clone(),
            public_key: bootstrap.descriptor.public_key.clone(),
            queued_row_version: proposal.queued_row_version,
            fence: proposal.claim.fence.clone(),
            adapter_id: proposal.claim.plan.request.adapter_id.clone(),
            binding,
            boot_session: crate::system_boot_session(),
        };
        let descriptor = GuardianDescriptor::from_native_owner(owner)?;
        if descriptor.session_identity != bootstrap.descriptor.session_identity {
            return Err(error("guardian_execution_binding_invalid"));
        }
        bootstrap.descriptor = descriptor;
        Ok(bootstrap)
    }

    fn from_launch(
        fence: TransformationAttemptFence,
        identity: Option<crate::transformation::SessionIdentity>,
        launch: GuardianLaunch,
    ) -> Result<Self, LarmError> {
        fence.validate()?;
        let mut signing_seed = [0; 32];
        // UUID v4 uses the OS-backed random source. Two independent values
        // supply 244 random bits; no key is derived from a PID or timestamp.
        signing_seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        signing_seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let key = SigningKey::from_bytes(&signing_seed);
        Ok(Self {
            descriptor: GuardianDescriptor {
                schema: if identity.is_some() { 2 } else { 1 },
                session_id: uuid::Uuid::now_v7().to_string(),
                fence,
                public_key: hex::encode(key.verifying_key().as_bytes()),
                session_identity: identity,
                native_owner: None,
            },
            signing_seed,
            launch,
        })
    }

    #[must_use]
    pub fn descriptor(&self) -> &GuardianDescriptor {
        &self.descriptor
    }

    /// A single bounded line, then the owner keeps this same stdin open. EOF
    /// after the line means owner loss and requests physical cleanup.
    pub async fn write_to(&self, writer: &mut (impl AsyncWrite + Unpin)) -> Result<(), LarmError> {
        let bytes = serde_json::to_vec(self).map_err(|_| error("guardian_bootstrap_invalid"))?;
        if bytes.len() > MAX_BOOTSTRAP {
            return Err(error("guardian_bootstrap_too_large"));
        }
        writer.write_all(&bytes).await.map_err(io)?;
        writer.write_all(b"\n").await.map_err(io)?;
        writer.flush().await.map_err(io)
    }

    pub async fn read_from<R: AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> Result<Self, LarmError> {
        let line = crate::framing::read_line(reader, MAX_BOOTSTRAP, false)
            .await
            .map_err(io)?;
        let Some(crate::framing::Line::Frame(bytes, observed)) = line else {
            return Err(error("guardian_bootstrap_invalid"));
        };
        if observed != bytes.len() as u64 + 1 {
            return Err(error("guardian_bootstrap_incomplete"));
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| error("guardian_bootstrap_invalid"))?;
        let key = value.descriptor.validate()?;
        match &value.launch {
            GuardianLaunch::Basic { manifest, .. }
                if value.descriptor.session_identity.is_none() =>
            {
                larm_adapter_sdk::manifest::AdapterManifest::parse(
                    &serde_json::to_string(manifest)
                        .map_err(|_| error("guardian_bootstrap_invalid"))?,
                )?;
            }
            GuardianLaunch::Transformation {
                database,
                actor_id,
                correlation_id,
            } if value.descriptor.session_identity.is_some()
                && database.is_absolute()
                && !actor_id.is_empty()
                && actor_id.len() <= 256
                && !actor_id.chars().any(char::is_control)
                && larm_domain::bridge::is_uuid_v7(correlation_id) => {}
            _ => return Err(error("guardian_bootstrap_invalid")),
        }
        if SigningKey::from_bytes(&value.signing_seed).verifying_key() != key {
            return Err(error("guardian_bootstrap_key_mismatch"));
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GuardianState {
    Running,
    Closing,
    Closed,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Action {
    Observe,
    Stop,
    Release,
    Ready,
    Recheck,
    Rpc(Box<TransformationRpc>),
    /// Retira las notificaciones `core.progress` acumuladas del worker. Es
    /// observación pura: no ejecuta IO ni concede autoridad alguna.
    Progress,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    descriptor: GuardianDescriptor,
    nonce: String,
    action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_signature: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    challenge: Challenge,
    state: GuardianState,
    guardian_pid: u32,
    worker_pid: u32,
    active_rpcs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Result<serde_json::Value, LarmError>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedObservation {
    observation: Observation,
    signature: String,
}

/// Live native observer. Cannot be deserialized from a journal or HTTP result.
/// A missing helper, pipe, invalid signature or timeout always fails closed.
pub struct NativeGuardian {
    descriptor: GuardianDescriptor,
    key: VerifyingKey,
}

impl NativeGuardian {
    pub async fn attach(descriptor: GuardianDescriptor) -> Result<Self, LarmError> {
        let key = descriptor.validate()?;
        let owner = Self { descriptor, key };
        owner.observe().await?;
        Ok(owner)
    }

    pub async fn observe(&self) -> Result<GuardianState, LarmError> {
        self.exchange(Action::Observe).await
    }

    /// Diagnostic count measured by the live helper; never closure authority.
    pub async fn active_rpcs(&self) -> Result<u32, LarmError> {
        Ok(self
            .exchange_signed(Action::Observe, None)
            .await?
            .active_rpcs)
    }

    async fn check(&self, action: Action) -> Result<(), LarmError> {
        let requires_ready = action == Action::Ready;
        let observed = self.exchange_signed(action, None).await?;
        let value = observed
            .result
            .ok_or_else(|| error("guardian_check_reply_missing"))??;
        if (requires_ready && observed.state != GuardianState::Running)
            || value != serde_json::Value::Bool(true)
        {
            return Err(error("guardian_not_ready"));
        }
        Ok(())
    }

    /// Requests cleanup; a successful request is not a closed-tree proof.
    pub async fn stop(&self) -> Result<GuardianState, LarmError> {
        self.exchange(Action::Stop).await
    }

    /// Fresh challenge to the retained physical owner, never a cached state.
    pub async fn require_closed(&self) -> Result<(), LarmError> {
        if self.observe().await? != GuardianState::Closed {
            return Err(error("guardian_tree_not_closed"));
        }
        Ok(())
    }

    /// Explicitly retires the witness only after fresh native closure. Journal
    /// callers must finish their guarded operation before retiring its owner.
    pub async fn release(self) -> Result<(), LarmError> {
        if self.exchange(Action::Release).await? != GuardianState::Closed {
            return Err(error("guardian_tree_not_closed"));
        }
        Ok(())
    }

    async fn exchange(&self, action: Action) -> Result<GuardianState, LarmError> {
        Ok(self.exchange_signed(action, None).await?.state)
    }

    async fn exchange_signed(
        &self,
        action: Action,
        signer: Option<&SigningKey>,
    ) -> Result<Observation, LarmError> {
        let timeout = match &action {
            Action::Rpc(request) => request.exchange_timeout()?,
            Action::Ready | Action::Recheck => Duration::from_secs(60),
            _ => EXCHANGE_TIMEOUT,
        };
        let mut challenge = Challenge {
            descriptor: self.descriptor.clone(),
            nonce: uuid::Uuid::new_v4().to_string(),
            action,
            control_signature: None,
        };
        if let Some(signer) = signer {
            challenge.control_signature = Some(hex::encode(
                signer.sign(&control_bytes(&challenge)?).to_bytes(),
            ));
        }
        tokio::time::timeout(timeout, async {
            let mut pipe = connect_pipe(&self.descriptor.pipe_name()).await?;
            write_message(&mut pipe, &challenge).await?;
            let response: SignedObservation = read_message(&mut pipe).await?;
            verify_observation(&self.key, &challenge, response)
        })
        .await
        .map_err(|_| error("guardian_observation_timeout"))?
    }
}

/// Concurrent clients can arrive before the server replaces its connected
/// listener. Retry only PIPE_BUSY, before sending any bytes, under the caller's
/// existing deadline. Never replay a request after an ambiguous write/read.
async fn connect_pipe(name: &str) -> Result<NamedPipeClient, LarmError> {
    loop {
        match ClientOptions::new().open(name) {
            Ok(pipe) => return Ok(pipe),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(e) => return Err(io(e)),
        }
    }
}

fn control_bytes(challenge: &Challenge) -> Result<Vec<u8>, LarmError> {
    let mut bytes = b"larm-native-guardian-control-v1\0".to_vec();
    bytes.extend(
        serde_json::to_vec(&(&challenge.descriptor, &challenge.nonce, &challenge.action))
            .map_err(|_| error("guardian_message_invalid"))?,
    );
    Ok(bytes)
}

fn verify_control(key: &VerifyingKey, challenge: &Challenge) -> Result<(), LarmError> {
    let signature = challenge
        .control_signature
        .as_ref()
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or_else(|| error("guardian_control_required"))?;
    key.verify_strict(&control_bytes(challenge)?, &signature)
        .map_err(|_| error("guardian_control_invalid"))
}

fn signed_bytes(observation: &Observation) -> Result<Vec<u8>, LarmError> {
    let mut bytes = SIGNING_CONTEXT.to_vec();
    bytes.extend(serde_json::to_vec(observation).map_err(|_| error("guardian_message_invalid"))?);
    Ok(bytes)
}

#[cfg(test)]
fn verify(
    key: &VerifyingKey,
    expected: &Challenge,
    response: SignedObservation,
) -> Result<GuardianState, LarmError> {
    Ok(verify_observation(key, expected, response)?.state)
}

fn verify_observation(
    key: &VerifyingKey,
    expected: &Challenge,
    response: SignedObservation,
) -> Result<Observation, LarmError> {
    if response.observation.challenge != *expected {
        return Err(error("guardian_observation_binding_mismatch"));
    }
    let signature = hex::decode(response.signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or_else(|| error("guardian_observation_signature_invalid"))?;
    key.verify_strict(&signed_bytes(&response.observation)?, &signature)
        .map_err(|_| error("guardian_observation_signature_invalid"))?;
    Ok(response.observation)
}

async fn read_message<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<T, LarmError> {
    let length = reader.read_u32_le().await.map_err(io)? as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err(error("guardian_message_too_large"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.map_err(io)?;
    serde_json::from_slice(&bytes).map_err(|_| error("guardian_message_invalid"))
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> Result<(), LarmError> {
    let bytes = serde_json::to_vec(value).map_err(|_| error("guardian_message_invalid"))?;
    if bytes.len() > MAX_MESSAGE {
        return Err(error("guardian_message_too_large"));
    }
    writer.write_u32_le(bytes.len() as u32).await.map_err(io)?;
    writer.write_all(&bytes).await.map_err(io)?;
    writer.flush().await.map_err(io)
}

fn error(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo comprobar el supervisor del proceso",
        reason,
    )
}

fn io(error: std::io::Error) -> LarmError {
    self::error("guardian_io_unavailable").with_technical(error.to_string())
}
