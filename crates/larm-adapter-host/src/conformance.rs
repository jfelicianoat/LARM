//! Ejecutor nativo P026. Sólo suites de código del host, nunca veredictos JSON
//! del cliente. La composición debe registrar los oráculos completos de cada
//! familia; este motor no transforma un smoke test en conformidad.
use crate::package::{check_managed_path, verify_published, PackageControl, MAX_BYTES};
use crate::policy::{GuardedWorker, LaunchAuthorization, Policy, TrustedOnlyGrant};
use crate::worker::{HostConfig, TreeKill, WorkerSpec, WorkerState};
use async_trait::async_trait;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{hash, BindingHashes, Execution, Observation, Proof, Run};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterConformanceRunner, MutationContext};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
mod qualification;
mod raw;
pub use raw::ProbeExit;
pub mod benchmark;
pub mod catalog;
pub mod gguf;
mod gguf_golden;
pub mod onnx;
mod onnx_golden;
pub mod profiles;
pub mod protocol;
pub mod runtime;
pub mod service;
pub mod source;
pub mod source_http;
pub mod transformation;
pub mod wire;

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo ejecutar la conformidad del adaptador",
        reason,
    )
}
fn invalid(reason: &str) -> LarmError {
    error(ErrorCode::ValidationFailed, reason)
}
fn io(_: std::io::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "conformance_file_unavailable",
    )
}
fn path_text(path: &Path) -> Result<&str, LarmError> {
    path.to_str()
        .ok_or_else(|| invalid("conformance_path_invalid"))
}
fn canonical(path: &Path) -> Result<PathBuf, LarmError> {
    check_managed_path(path)?;
    std::fs::canonicalize(path).map_err(io)
}
fn file_digest(path: &Path) -> Result<String, LarmError> {
    let started = std::time::Instant::now();
    check_managed_path(path)?;
    let mut file = std::fs::File::open(path).map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() {
        return Err(invalid("conformance_file_invalid"));
    }
    // Reject impossible inputs before streaming, then retain the real-byte
    // limit below in case a file changes after its metadata was observed.
    if metadata.len() > MAX_BYTES {
        return Err(invalid("conformance_file_too_large"));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let size = file.read(&mut buffer).map_err(io)?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > MAX_BYTES {
            return Err(invalid("conformance_file_too_large"));
        }
        digest.update(&buffer[..size]);
    }
    tracing::debug!(target:"larm_adapter_host::conformance::measurement",phase="file_digest",
        bytes=total,elapsed_ms=started.elapsed().as_millis() as u64,"qualification byte digest");
    Ok(hex::encode(digest.finalize()))
}

/// Plan interno preparado por el composition root. No implementa Deserialize.
#[derive(Clone)]
pub struct NativeTarget {
    pub spec: WorkerSpec,
    /// Resolución de raíces del perfil local, sin ampliar permisos del manifiesto.
    pub roots: Option<larm_adapter_sdk::permissions::RootMap>,
    pub custom: bool,
    pub grant: Option<TrustedOnlyGrant>,
    /// Documento completo de descriptores, relativo al paquete (si anuncia caps).
    pub capability_profile: Option<PathBuf>,
    /// Datos/fixtures/config inmutables usados por los oráculos, no sus salidas.
    pub immutable_inputs: Vec<PathBuf>,
    pub suite: Arc<dyn NativeSuite>,
    pub timeout: Duration,
}

#[async_trait]
pub trait NativeTargets: Send + Sync {
    async fn target(
        &self,
        installation: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<NativeTarget, LarmError>;
}

/// Implementaciones compiladas y auditadas con el host. descriptor identifica
/// familia/capacidades/oráculos/versiones; el binario real del host se mide también.
/// Cada llamada debe declarar Access exacto y observar conducta/IO independiente.
/// Su descriptor y immutable_inputs deben cubrir toda configuración efectiva;
/// sus propios recursos/IO deben terminar antes de devolver. No se admiten suites
/// ni resultados suministrados por el cliente o por el paquete del plugin.
#[async_trait]
pub trait NativeSuite: Send + Sync {
    fn descriptor(&self) -> Value;
    /// Evita abrir estado por un handshake automático antes de probar el inicial.
    fn protocol_probes_first(&self) -> bool {
        false
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError>;
}

/// IO propio del oráculo (por ejemplo, su conexión al ledger de la fixture).
/// Su cierre forma parte de la barrera final, también tras timeout o panic.
#[async_trait]
pub trait NativeIo: Send + Sync {
    async fn close_io(&self) -> Result<(), LarmError>;
}

pub struct NativeConformanceRunner {
    policy: Arc<Policy>,
    targets: Arc<dyn NativeTargets>,
    config: HostConfig,
    activity: Arc<Activity>,
}

struct Activity {
    state: Mutex<(bool, BTreeSet<String>)>,
    changed: tokio::sync::watch::Sender<usize>,
}
struct ActiveRun {
    activity: Arc<Activity>,
    id: String,
}
impl Activity {
    fn acquire(self: &Arc<Self>, id: &str) -> Result<ActiveRun, LarmError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?;
        if !state.0 {
            return Err(error(
                ErrorCode::ResourceUnavailable,
                "conformance_runner_closed",
            ));
        }
        if !state.1.insert(id.into()) {
            return Err(error(ErrorCode::Conflict, "conformance_already_running"));
        }
        self.changed.send_replace(state.1.len());
        Ok(ActiveRun {
            activity: self.clone(),
            id: id.into(),
        })
    }
}
impl Drop for ActiveRun {
    fn drop(&mut self) {
        if let Ok(mut state) = self.activity.state.lock() {
            state.1.remove(&self.id);
            self.activity.changed.send_replace(state.1.len());
        }
    }
}
impl NativeConformanceRunner {
    fn target_policy(&self, target: &NativeTarget) -> Arc<Policy> {
        Arc::new(Policy {
            trust: self.policy.trust.clone(),
            roots: target
                .roots
                .clone()
                .unwrap_or_else(|| self.policy.roots.clone()),
            elevation: self.policy.elevation.clone(),
        })
    }
    pub fn new(policy: Arc<Policy>, targets: Arc<dyn NativeTargets>, config: HostConfig) -> Self {
        Self {
            policy,
            targets,
            config,
            activity: Arc::new(Activity {
                state: Mutex::new((true, BTreeSet::new())),
                changed: tokio::sync::watch::channel(0).0,
            }),
        }
    }
    /// Cierra admisión y espera también ejecuciones cuyo observador se canceló.
    /// Un proceso/canal sin terminar conserva su ownership y no acredita idle.
    pub async fn shutdown(&self) -> Result<(), LarmError> {
        let mut changed = self.activity.changed.subscribe();
        self.activity
            .state
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?
            .0 = false;
        while *changed.borrow_and_update() != 0 {
            changed
                .changed()
                .await
                .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    schema_version: String,
    entries: Vec<larm_adapter_sdk::capabilities::Descriptor>,
}
fn profile_hash(target: &NativeTarget) -> Result<String, LarmError> {
    let Some(relative) = &target.capability_profile else {
        if !target.spec.manifest.capabilities.is_empty() {
            return Err(invalid("conformance_capability_profile_missing"));
        }
        return hash(&json!({"schema_version":"1.0","entries":[]}));
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("conformance_capability_profile_path_invalid"));
    }
    let path = canonical(&target.spec.package_dir.join(relative))?;
    if !path.starts_with(&target.spec.package_dir) {
        return Err(invalid("conformance_capability_profile_path_invalid"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(io)?
        .take((64 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > 64 << 20 {
        return Err(invalid("conformance_capability_profile_invalid"));
    }
    let profile: Profile = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("conformance_capability_profile_invalid"))?;
    let ids: BTreeSet<_> = profile.entries.iter().map(|e| e.id.as_str()).collect();
    if profile.schema_version != "1.0"
        || profile.entries.len() > 128
        || ids.len() != profile.entries.len()
        || target
            .spec
            .manifest
            .capabilities
            .iter()
            .any(|id| !ids.contains(id.as_str()))
    {
        return Err(invalid("conformance_capability_profile_invalid"));
    }
    // Binding de bytes; la activación real de mapping/estimador pertenece al SDK
    // del worker y a los oráculos de la suite, no a callbacks inventados del host.
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn measured_paths(paths: &[PathBuf]) -> Result<Vec<Value>, LarmError> {
    if paths.len() > 128 {
        return Err(invalid("conformance_inputs_invalid"));
    }
    let mut found = BTreeMap::new();
    for path in paths {
        let path = canonical(path)?;
        let name = path_text(&path)?.to_owned();
        let key = if cfg!(windows) {
            name.to_uppercase()
        } else {
            name.clone()
        };
        if found
            .insert(key, json!({"path":name,"sha256":file_digest(&path)?}))
            .is_some()
        {
            return Err(invalid("conformance_inputs_duplicate"));
        }
    }
    Ok(found.into_values().collect())
}
fn root_context(paths: &[PathBuf]) -> Result<Vec<Value>, LarmError> {
    paths
        .iter()
        .map(|path| {
            let resolved = std::fs::canonicalize(path).map_err(io)?;
            Ok(json!({"declared":path_text(path)?,"resolved":path_text(&resolved)?}))
        })
        .collect()
}
fn measure(
    target: &NativeTarget,
    policy: &Policy,
    config: HostConfig,
) -> Result<(BindingHashes, LaunchAuthorization), LarmError> {
    measure_with_paths(
        target,
        policy,
        config,
        &mut crate::package_paths::PinnedPaths::default(),
    )
}

fn measure_with_paths(
    target: &NativeTarget,
    policy: &Policy,
    config: HostConfig,
    paths: &mut crate::package_paths::PinnedPaths,
) -> Result<(BindingHashes, LaunchAuthorization), LarmError> {
    let started = std::time::Instant::now();
    crate::package::verify_published_with_paths(
        &target.spec.package_dir,
        &target.spec.manifest,
        &PackageControl::default(),
        paths,
    )?;
    measurement_phase("package_verify", started);
    let started = std::time::Instant::now();
    let authorization = policy.authorize(
        &target.spec.package_dir,
        &target.spec.manifest,
        target.custom,
        target.grant.as_ref(),
        chrono::Utc::now(),
    )?;
    measurement_phase("policy_authorize", started);
    let started = std::time::Instant::now();
    let descriptor = target.suite.descriptor();
    let caps = descriptor
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("conformance_suite_mismatch"))?;
    let suite_caps = caps
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or_else(|| invalid("conformance_suite_mismatch"))?;
    let expected_caps = target
        .spec
        .manifest
        .capabilities
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if descriptor["family"].as_str() != Some(target.spec.manifest.family.as_str())
        || caps.len() != suite_caps.len()
        || suite_caps != expected_caps
        || serde_json::to_vec(&descriptor)
            .map_err(|_| invalid("conformance_suite_invalid"))?
            .len()
            > 1 << 20
    {
        return Err(invalid("conformance_suite_mismatch"));
    }
    let engine = canonical(&std::env::current_exe().map_err(io)?)?;
    let grants = &authorization.grants;
    let context = json!({
        "package":path_text(&target.spec.package_dir)?,"work":path_text(&target.spec.work_dir)?,
        "environment":target.spec.environment,"custom":target.custom,"grant":target.grant,
        "execution_digest":authorization.execution_digest,"assessment":authorization.assessment,
        "read_roots":root_context(&grants.read_roots)?,"write_roots":root_context(&grants.write_roots)?,
        "network_origins":grants.network_origins,"process_allowlist":grants.process_allowlist,"secret_refs":grants.secret_refs,
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "limits":{"handshake_ns":config.handshake_timeout.as_nanos().to_string(),"rpc_ns":config.rpc_timeout.as_nanos().to_string(),
            "heartbeat_ns":config.heartbeat.as_nanos().to_string(),"misses":config.heartbeat_misses,
            "cancel_ns":config.cancel_grace.as_nanos().to_string(),"stderr":config.stderr_limit_bytes.to_string(),
            "queue":config.queue,"concurrent":config.max_concurrent,"malformed":config.malformed_limit,
            "suite_ns":target.timeout.as_nanos().to_string()}
    });
    measurement_phase("definition_context", started);
    let started = std::time::Instant::now();
    let suite_sha256 = hash(
        &json!({"engine":{"path":path_text(&engine)?,"sha256":file_digest(&engine)?},
            "descriptor":descriptor,"protocol_probes_first":target.suite.protocol_probes_first(),
            "inputs":measured_paths(&target.immutable_inputs)?}),
    )?;
    measurement_phase("engine_and_inputs", started);
    let started = std::time::Instant::now();
    let capability_profile_sha256 = profile_hash(target)?;
    measurement_phase("capability_profile", started);
    let started = std::time::Instant::now();
    let execution_context_sha256 = hash(&context)?;
    measurement_phase("context_digest", started);
    Ok((
        BindingHashes {
            suite_sha256,
            capability_profile_sha256,
            execution_context_sha256,
        },
        authorization,
    ))
}

fn measurement_phase(name: &str, started: std::time::Instant) {
    tracing::debug!(target:"larm_adapter_host::conformance::measurement",phase=name,
        elapsed_ms=started.elapsed().as_millis() as u64,"qualification measurement phase");
}

struct Prepared {
    target: NativeTarget,
    policy: Arc<Policy>,
    config: HostConfig,
    run: Run,
    _active: Option<ActiveRun>,
}
impl Prepared {
    async fn recheck(self: &Arc<Self>) -> Result<LaunchAuthorization, LarmError> {
        self.recheck_with_paths(crate::package_paths::PinnedPaths::default())
            .await
    }

    async fn recheck_with_paths(
        self: &Arc<Self>,
        mut paths: crate::package_paths::PinnedPaths,
    ) -> Result<LaunchAuthorization, LarmError> {
        let prepared = self.clone();
        tokio::task::spawn_blocking(move || {
            let (bindings, authorization) = measure_with_paths(
                &prepared.target,
                &prepared.policy,
                prepared.config,
                &mut paths,
            )?;
            if bindings != prepared.run.bindings {
                return Err(error(
                    ErrorCode::StaleRevision,
                    "conformance_context_changed",
                ));
            }
            Ok(authorization)
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "conformance_measurement_failed"))?
    }
}

/// Propietario de todos los workers de la suite. No expone handles clonables
/// que permitan devolver observaciones mientras queden llamadas en segundo plano.
pub struct NativeSession {
    prepared: Arc<Prepared>,
    worker: Option<GuardedWorker>,
    probe: Option<raw::RawProbe>,
    generation: u64,
    oracle_io: Vec<Arc<dyn NativeIo>>,
}
impl NativeSession {
    pub fn register_io(&mut self, resource: Arc<dyn NativeIo>) {
        self.oracle_io.push(resource);
    }
    fn start_oracle_cleanup(&mut self) -> Vec<tokio::task::JoinHandle<Result<(), LarmError>>> {
        std::mem::take(&mut self.oracle_io)
            .into_iter()
            .map(|resource| {
                let owner = self.prepared.clone();
                tokio::spawn(async move {
                    let result = resource.close_io().await;
                    drop(owner);
                    result
                })
            })
            .collect()
    }
    async fn finish_oracle_io(&mut self) -> Result<(), LarmError> {
        let cleanup = self.start_oracle_cleanup();
        let wait = async {
            let mut failure = None;
            for task in cleanup {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        failure.get_or_insert(error);
                    }
                    Err(_) => {
                        failure.get_or_insert(error(
                            ErrorCode::Internal,
                            "conformance_oracle_cleanup_failed",
                        ));
                    }
                }
            }
            failure.map_or(Ok(()), Err)
        };
        // Al vencer se descartan observadores, no los propietarios de IO.
        tokio::time::timeout(self.prepared.config.cancel_grace, wait)
            .await
            .map_err(|_| error(ErrorCode::Timeout, "worker_io_cleanup_timeout"))?
    }
    pub(crate) fn protocol_pristine(&self) -> bool {
        self.generation == 0 && self.worker.is_none() && self.probe.is_none()
    }
    pub fn manifest(&self) -> &larm_adapter_sdk::manifest::AdapterManifest {
        &self.prepared.target.spec.manifest
    }
    pub fn work_dir(&self) -> &Path {
        &self.prepared.target.spec.work_dir
    }
    pub fn package_dir(&self) -> &Path {
        &self.prepared.target.spec.package_dir
    }
    pub async fn probe_start(&mut self) -> Result<(), LarmError> {
        self.finish().await?;
        let authorization = self.prepared.recheck().await?;
        self.generation += 1;
        self.probe = Some(raw::RawProbe::start(
            &self.prepared.target.spec,
            &authorization,
            self.prepared.config,
            self.prepared.clone(),
        )?);
        Ok(())
    }
    pub async fn probe_send(
        &self,
        bytes: &[u8],
        access: &larm_adapter_sdk::permissions::Access,
    ) -> Result<(), LarmError> {
        self.probe
            .as_ref()
            .ok_or_else(|| invalid("conformance_probe_missing"))?
            .send(bytes, access)
            .await
    }
    pub async fn probe_next(&mut self, timeout: Duration) -> Result<Vec<u8>, LarmError> {
        self.probe
            .as_mut()
            .ok_or_else(|| invalid("conformance_probe_missing"))?
            .next(timeout)
            .await
    }
    pub async fn probe_close(&mut self) -> Result<ProbeExit, LarmError> {
        let result = self
            .probe
            .as_mut()
            .ok_or_else(|| invalid("conformance_probe_missing"))?
            .finish()
            .await;
        if !result
            .as_ref()
            .is_err_and(|e| e.details.reason == "worker_io_cleanup_timeout")
        {
            self.probe = None;
        }
        result
    }
    pub async fn restart(&mut self) -> Result<(), LarmError> {
        self.finish().await?;
        let authorization = self.prepared.recheck().await?;
        self.generation += 1;
        self.worker = Some(
            GuardedWorker::start_owned(
                self.prepared.target.spec.clone(),
                &authorization,
                self.prepared.config,
                self.generation,
                Arc::new(TreeKill),
                Some(self.prepared.clone()),
            )
            .await?,
        );
        Ok(())
    }
    fn active(&self) -> Result<&GuardedWorker, LarmError> {
        self.worker.as_ref().ok_or_else(|| {
            error(
                ErrorCode::InvalidStateTransition,
                "conformance_worker_missing",
            )
        })
    }
    pub async fn call(
        &self,
        request: larm_adapter_sdk::protocol::Request,
        access: &larm_adapter_sdk::permissions::Access,
        timeout: Option<Duration>,
        resource: Option<&str>,
    ) -> Result<Value, LarmError> {
        self.active()?
            .call_request(request, access, timeout, resource)
            .await
    }
    pub async fn cancel(&self, id: &str) -> Result<bool, LarmError> {
        self.active()?.cancel(id).await
    }
    pub fn progress(
        &self,
    ) -> Result<tokio::sync::broadcast::Receiver<larm_adapter_sdk::protocol::Progress>, LarmError>
    {
        Ok(self.active()?.subscribe_progress())
    }
    pub fn state(&self) -> Result<WorkerState, LarmError> {
        Ok(self.active()?.state())
    }
    pub fn pid(&self) -> Result<u32, LarmError> {
        if let Some(probe) = &self.probe {
            return Ok(probe.pid());
        }
        Ok(self.active()?.pid())
    }
    pub fn crash(&self) -> Result<(), LarmError> {
        if let Some(probe) = &self.probe {
            probe.terminate();
            return Ok(());
        }
        if let Some(worker) = &self.worker {
            worker.terminate();
            return Ok(());
        }
        Err(invalid("conformance_process_missing"))
    }
    async fn finish(&mut self) -> Result<(), LarmError> {
        if let Some(worker) = &self.worker {
            worker.stop_and_wait().await?;
        }
        self.worker = None;
        if self.probe.is_some() {
            let closed = self.probe_close().await?;
            if closed.extra_frames != 0 {
                return Err(error(ErrorCode::ProtocolError, "probe_unconsumed_frames"));
            }
            if !closed.success {
                return Err(error(ErrorCode::ProtocolError, "probe_eof_not_cooperative"));
            }
        }
        Ok(())
    }
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        // También cuando el futuro del oráculo paniquea: cada tarea retiene
        // Prepared/admisión hasta terminar el cierre de su recurso real.
        drop(self.start_oracle_cleanup());
    }
}

async fn execute_owned(prepared: Arc<Prepared>) -> Result<Execution, LarmError> {
    let mut session = NativeSession {
        prepared: prepared.clone(),
        worker: None,
        probe: None,
        generation: 0,
        oracle_io: vec![],
    };
    let initialize = if prepared.target.suite.protocol_probes_first() {
        Ok(())
    } else {
        session.restart().await
    };
    let mut observed = match initialize {
        Ok(()) => match tokio::time::timeout(
            prepared.target.timeout,
            prepared.target.suite.execute(&prepared.run, &mut session),
        )
        .await
        {
            Ok(observed) => observed,
            Err(_) => Err(error(ErrorCode::Timeout, "conformance_suite_timeout")),
        },
        Err(error) if error.details.reason == "worker_io_cleanup_timeout" => return Err(error),
        Err(error) => Err(error),
    };
    // El timeout cancela sólo el oráculo; nunca salta la barrera del proceso/IO.
    let worker_closed = session.finish().await;
    let oracle_closed = session.finish_oracle_io().await;
    // Un cierre auxiliar fallido nunca produce Execution confirmable.
    oracle_closed?;
    if let Err(close) = worker_closed {
        if close.details.reason == "worker_io_cleanup_timeout" {
            return Err(close);
        }
        if observed.is_ok() {
            observed = Err(close);
        }
    }
    let proof = match prepared.recheck().await {
        Ok(_) => observed.and_then(|observations| Proof::evaluate(&prepared.run, observations)),
        Err(error) => Err(error),
    };
    Ok(Execution {
        run: prepared.run.clone(),
        proof,
    })
}

#[async_trait]
impl AdapterConformanceRunner for NativeConformanceRunner {
    async fn wait_conformance(&self, id: &str) -> Result<(), LarmError> {
        let mut changed = self.activity.changed.subscribe();
        loop {
            let active = self
                .activity
                .state
                .lock()
                .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?
                .1
                .contains(id);
            if !active {
                return Ok(());
            }
            changed
                .changed()
                .await
                .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?;
        }
    }

    async fn execute_conformance(
        &self,
        installation: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<Execution, LarmError> {
        if ctx.actor_id.is_empty()
            || ctx.actor_id.len() > 256
            || ctx.actor_id.chars().any(char::is_control)
            || !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id)
        {
            return Err(invalid("conformance_context_invalid"));
        }
        if installation.trust == AdapterTrust::Revoked {
            return Err(error(ErrorCode::Forbidden, "adapter_revoked"));
        }
        if !larm_domain::bridge::is_uuid_v7(&installation.id)
            || installation.row_version < 1
            || installation.row_version == i64::MAX
        {
            return Err(invalid("conformance_installation_invalid"));
        }
        if !matches!(
            installation.state,
            AdapterState::Installed | AdapterState::Validated
        ) {
            return Err(error(
                ErrorCode::InvalidStateTransition,
                "adapter_not_installed",
            ));
        }
        let active = self.activity.acquire(&installation.id)?;
        let mut target = self.targets.target(installation, ctx).await?;
        if serde_json::to_value(&target.spec.manifest)
            .map_err(|_| invalid("conformance_manifest_invalid"))?
            != installation.manifest
        {
            return Err(invalid("conformance_manifest_mismatch"));
        }
        larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())?;
        if target
            .grant
            .as_ref()
            .is_some_and(|grant| grant.actor_id != ctx.actor_id)
        {
            return Err(error(ErrorCode::Forbidden, "conformance_actor_mismatch"));
        }
        // Los conversores de producto reales (cuantización K en Python
        // stdlib: la ruta de emisión única cuantiza todo el fixture en el
        // preflight del plan y de nuevo en la conversión) legítimamente
        // superan los 10 minutos por suite: el timeout sigue acotado y es un
        // detector de cuelgue, no un presupuesto de rendimiento.
        if target.timeout.is_zero() || target.timeout > Duration::from_secs(2400) {
            return Err(invalid("conformance_timeout_invalid"));
        }
        let policy = self.target_policy(&target);
        let config = self.config;
        let installation = installation.clone();
        let actor_id = ctx.actor_id.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            target.spec.package_dir = canonical(&target.spec.package_dir)?;
            // Denegaciones P022 no crean ni siquiera el directorio de trabajo.
            verify_published(
                &target.spec.package_dir,
                &target.spec.manifest,
                &PackageControl::default(),
            )?;
            policy.authorize(
                &target.spec.package_dir,
                &target.spec.manifest,
                target.custom,
                target.grant.as_ref(),
                chrono::Utc::now(),
            )?;
            check_managed_path(&target.spec.work_dir)?;
            std::fs::create_dir_all(target.spec.work_dir.join("tmp")).map_err(io)?;
            target.spec.work_dir = canonical(&target.spec.work_dir)?;
            // Congela el entorno efectivo: Worker aplica estos valores después
            // de su mínimo y no puede heredar cambios posteriores del proceso.
            let mut environment = crate::worker::minimal_env(&target.spec.work_dir);
            environment.extend(target.spec.environment.clone());
            target.spec.environment = environment;
            let (bindings, _) = measure(&target, &policy, config)?;
            let run = Run::new(&installation, bindings, &actor_id)?;
            Ok(Arc::new(Prepared {
                target,
                policy,
                config,
                run,
                _active: Some(active),
            }))
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "conformance_measurement_failed"))??;
        // Cancelar al observador no abandona el IO. Esta tarea conserva todo el
        // ownership y drena antes de terminar; Core cancelado no confirma nada.
        tokio::spawn(execute_owned(prepared))
            .await
            .map_err(|_| error(ErrorCode::Internal, "conformance_execution_failed"))?
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod digest_io_tests {
    use super::*;
    #[test]
    fn full_digest_spans_batches_and_detects_a_late_byte_change() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned.bin");
        let mut data: Vec<u8> = (0..(2 * 1024 * 1024 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&path, &data).unwrap();
        let before = file_digest(&path).unwrap();
        assert_eq!(before, hex::encode(Sha256::digest(&data)));
        let last = data.len() - 1;
        data[last] ^= 0xff;
        std::fs::write(&path, &data).unwrap();
        assert_eq!(
            file_digest(&path).unwrap(),
            hex::encode(Sha256::digest(&data))
        );
        assert_ne!(file_digest(&path).unwrap(), before);
    }
    #[test]
    fn oversize_metadata_rejects_extended_input_without_streaming_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("oversize.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(
            file_digest(&path).unwrap_err().details.reason,
            "conformance_file_too_large"
        );
    }
}
