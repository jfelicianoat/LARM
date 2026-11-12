//! Worker de un adaptador fuera de proceso (P021; spec §12, §94; contrato
//! 03; `defaults.json` `adapters`).
//!
//! - Se lanza el ejecutable del manifiesto **resuelto dentro del paquete**
//!   con sus argumentos estructurados: nunca una cadena de shell. El entorno
//!   se vacía y solo lleva lo mínimo explícito más lo que pida el perfil.
//! - Handshake obligatorio en `handshake_timeout`; si falla, el worker queda
//!   `FAILED` y el proceso se mata.
//! - Llamadas: el método debe estar en el manifiesto; cola acotada (64),
//!   hasta 4 en paralelo y mutaciones serializadas por `resource_id`;
//!   timeout por operación: al vencer se envía `core.cancel`, se espera la
//!   gracia (5 s) y, solo como último recurso, se mata el proceso.
//! - stdout solo admite frames: una línea corrupta deja el worker
//!   `DEGRADED`; tres, `FAILED`. stderr se drena siempre como logs
//!   etiquetados, redactados y con límite total (10 MiB).
//! - Heartbeat cada 10 s; 3 ausencias → `ADAPTER_CRASHED` y `FAILED`.
//! - Un proceso que sale sin que se le pida → `FAILED`; las llamadas en
//!   curso reciben `ADAPTER_CRASHED`.
//! - El estado del worker es de proceso: nunca cambia el estado de la
//!   instalación del adaptador.
//! - En Windows, el worker vive en un Job Object kill-on-close. Es contención
//!   del ciclo de vida, no aislamiento del SO (AppContainer es P130).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::{
    check_handshake, decode, encode, handshake_request, Frame, HandshakeResult, Progress, Request,
    Response, RpcError, MAX_FRAME_BYTES,
};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Semaphore};

/// Estado del proceso worker (independiente del de la instalación).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkerState {
    Starting,
    Ready,
    Degraded,
    Failed,
    Stopped,
}

/// Límites (`defaults.json` `adapters` y contrato 03).
#[derive(Debug, Clone, Copy)]
pub struct HostConfig {
    pub handshake_timeout: Duration,
    pub rpc_timeout: Duration,
    pub heartbeat: Duration,
    pub heartbeat_misses: u32,
    pub cancel_grace: Duration,
    pub stderr_limit_bytes: u64,
    pub queue: usize,
    pub max_concurrent: usize,
    pub malformed_limit: u32,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            rpc_timeout: Duration::from_secs(30),
            heartbeat: Duration::from_secs(10),
            heartbeat_misses: 3,
            cancel_grace: Duration::from_secs(5),
            stderr_limit_bytes: 10 << 20,
            queue: 64,
            max_concurrent: 4,
            malformed_limit: 3,
        }
    }
}

/// Cómo se termina un proceso como último recurso.
pub trait Containment: Send + Sync {
    /// Mata el proceso y sus descendientes.
    fn kill_tree(&self, pid: u32);
}

/// Job Object de vida del worker. En Windows, el objeto se prepara antes del
/// spawn, se asigna al proceso y vive hasta que el handle nativo confirma su
/// salida. Al cerrarse, Windows termina también los descendientes del job.
pub(crate) struct ProcessJob {
    #[cfg(windows)]
    job: win32job::Job,
}

impl ProcessJob {
    /// Mantiene el job abierto hasta confirmar que no quedan miembros. Cerrar
    /// kill-on-close inicia la terminación, pero no acredita que haya acabado.
    async fn drain(self) {
        #[cfg(windows)]
        {
            let mut reported = false;
            loop {
                match self.terminate_members_except(None).await {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(error) if !reported => {
                        tracing::error!(%error, "No se pudo confirmar el cierre del árbol del worker");
                        reported = true;
                    }
                    Err(_) => {}
                }
                // Un fallo conserva el job y su admisión. El observador tiene
                // un plazo acotado y no podrá acreditar un cierre incompleto.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }

    #[cfg(windows)]
    pub(crate) async fn terminate_members_except(
        &self,
        excluded: Option<u32>,
    ) -> Result<bool, LarmError> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let members = self
            .job
            .query_process_id_list()
            .map_err(|error| job_error("worker_job_object_query_failed", error))?;
        if members.iter().all(|pid| Some(*pid as u32) == excluded) {
            return Ok(true);
        }
        let ids = members
            .into_iter()
            .filter_map(|pid| u32::try_from(pid).ok())
            .filter(|pid| Some(*pid) != excluded)
            .map(Pid::from_u32)
            .collect::<Vec<_>>();
        let mut processes = System::new();
        processes.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing(),
        );
        // sysinfo conserva los handles de estos procesos. Revalidar su
        // pertenencia después de abrirlos evita actuar sobre un PID reciclado;
        // el handle retenido impide su reutilización durante la terminación.
        let confirmed = self
            .job
            .query_process_id_list()
            .map_err(|error| job_error("worker_job_object_query_failed", error))?;
        let targets: Vec<_> = processes
            .processes()
            .iter()
            .filter(|(pid, process)| {
                process.start_time() != 0 && confirmed.contains(&(pid.as_u32() as usize))
            })
            .map(|(pid, _)| pid.to_string())
            .collect();
        // taskkill supports repeated /PID arguments. One bounded batch avoids
        // paying process startup once per member, including console helpers.
        // No /T or image wildcard: every target is a revalidated job member.
        // Keep the identity handles until all batches finish, then query the
        // job again. A helper exit never stands in for confirmed departure.
        for batch in targets.chunks(32) {
            let system_root = std::env::var_os("SystemRoot")
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
            let mut command = Command::new(system_root.join("System32/taskkill.exe"));
            for pid in batch {
                command.args(["/PID", pid]);
            }
            command
                .arg("/F")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000)
                .kill_on_drop(true);
            if let Ok(mut termination) = command.spawn() {
                let _ = tokio::time::timeout(Duration::from_secs(1), termination.wait()).await;
            }
        }
        drop(processes);
        // La siguiente lectura se hace después de liberar todos los handles,
        // y también incluye descendientes creados durante esta iteración.
        Ok(false)
    }

    fn create() -> Result<Self, LarmError> {
        #[cfg(windows)]
        {
            let mut limits = win32job::ExtendedLimitInfo::new();
            limits.limit_kill_on_job_close();
            let job = win32job::Job::create_with_limit_info(&limits)
                .map_err(|error| job_error("worker_job_object_create_failed", error))?;
            Ok(Self { job })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    /// A dedicated guardian joins before it executes any adapter code. Every
    /// child consequently inherits this outer job at process creation, including
    /// children born before Worker assigns its additional inner job.
    #[cfg(windows)]
    pub(crate) fn enter_guardian() -> Result<Self, LarmError> {
        let owner = Self::create()?;
        owner
            .job
            .assign_current_process()
            .map_err(|error| job_error("guardian_job_object_assign_failed", error))?;
        Ok(owner)
    }

    #[cfg(windows)]
    pub(crate) fn guardian_is_alone(&self) -> Result<bool, LarmError> {
        let own_pid = std::process::id() as usize;
        let members = self
            .job
            .query_process_id_list()
            .map_err(|error| job_error("guardian_job_object_query_failed", error))?;
        // Empty does not prove ownership of this process.
        Ok(members.contains(&own_pid) && members.iter().all(|pid| *pid == own_pid))
    }

    #[cfg(windows)]
    pub(crate) fn contains_member(&self, pid: u32) -> Result<bool, LarmError> {
        self.job
            .query_process_id_list()
            .map(|members| members.contains(&(pid as usize)))
            .map_err(|error| job_error("guardian_job_object_query_failed", error))
    }

    /// Only the trusted guardian may disarm its kill-on-close after observing
    /// that it is the sole member. No adapter runs after this transition.
    #[cfg(windows)]
    pub(crate) fn finish_guardian(self) -> Result<(), LarmError> {
        if !self.guardian_is_alone()? {
            return Err(job_error("guardian_tree_not_closed", "remaining members"));
        }
        // This private outer job was created with kill-on-close only. Reset
        // its limits through the safe dependency API once no children remain.
        let limits = win32job::ExtendedLimitInfo::new();
        self.job
            .set_extended_limit_info(&limits)
            .map_err(|error| job_error("guardian_job_object_disarm_failed", error))
    }

    fn assign(&self, child: &Child) -> Result<(), LarmError> {
        #[cfg(windows)]
        {
            let handle = child.raw_handle().ok_or_else(|| {
                job_error(
                    "worker_job_object_process_handle_missing",
                    "Tokio no expuso el handle del worker",
                )
            })?;
            self.job
                .assign_process(handle as isize)
                .map_err(|error| job_error("worker_job_object_assign_failed", error))
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(())
        }
    }
}

#[cfg(windows)]
fn job_error(reason: &str, error: impl std::fmt::Display) -> LarmError {
    err(
        ErrorCode::ResourceUnavailable,
        "No se pudo asegurar el ciclo de vida del worker",
        reason,
    )
    .with_technical(error.to_string())
}

/// Fallback sin código inseguro: `taskkill /T /F` (árbol completo). El Job
/// Object es la garantía de cierre de descendientes en Windows; ninguno de los
/// dos mecanismos aísla disco o red.
pub struct TreeKill;

impl Containment for TreeKill {
    fn kill_tree(&self, pid: u32) {
        #[cfg(windows)]
        {
            let system = std::env::var_os("SystemRoot")
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
            let _ = std::process::Command::new(system.join("System32").join("taskkill.exe"))
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}

/// Qué arrancar.
#[derive(Clone)]
pub struct WorkerSpec {
    /// Carpeta del paquete del adaptador.
    pub package_dir: PathBuf,
    pub manifest: AdapterManifest,
    /// Variables extra ya resueltas por el perfil (sin secretos en logs).
    pub environment: Vec<(String, String)>,
    /// Carpeta de trabajo y temporal del worker.
    pub work_dir: PathBuf,
}

/// Línea de log de un worker (stderr), ya redactada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    pub adapter_id: String,
    pub generation: u64,
    pub line: String,
}

/// Líneas de stderr que se conservan para diagnóstico: las primeras (errores
/// de arranque) y las últimas.
pub const RECENT_HEAD: usize = 50;
pub const RECENT_TAIL: usize = 150;

struct PendingCall {
    sender: oneshot::Sender<Response>,
    method: String,
}
type Pending = Arc<Mutex<BTreeMap<String, PendingCall>>>;

struct Shared {
    state: watch::Sender<WorkerState>,
    pending: Pending,
    progress: broadcast::Sender<Progress>,
    logs: broadcast::Sender<LogLine>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    malformed: AtomicU32,
    stderr_bytes: AtomicU64,
    stderr_dropped: AtomicU64,
    reason: Mutex<Option<String>>,
    exited: std::sync::atomic::AtomicBool,
    recent: Mutex<(Vec<LogLine>, std::collections::VecDeque<LogLine>)>,
    // 1=stdout drenado, 2=stderr drenado, 4=proceso y job terminados.
    io_done: watch::Sender<u8>,
    // El task que posee Child también puede terminarlo por su handle nativo.
    terminate: mpsc::UnboundedSender<()>,
    // El propietario nativo conserva su admisión hasta cerrar todo el IO,
    // incluso si start falla o el observador cancela su futuro.
    _owner: Option<Arc<dyn Send + Sync>>,
}

fn supervise_io(
    shared: Arc<Shared>,
    bit: u8,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::spawn(async move {
        future.await;
        // Sólo una finalización normal acredita drenaje; un panic no lo hace.
        shared.io_done.send_modify(|done| *done |= bit);
    });
}

// El futuro de start puede cancelarse durante el handshake. Su propietario
// debe terminar el árbol aunque nunca llegue a construirse un Worker.
struct StartingProcess {
    shared: Arc<Shared>,
    containment: Arc<dyn Containment>,
    pid: u32,
    armed: bool,
}
impl Drop for StartingProcess {
    fn drop(&mut self) {
        if self.armed && !self.shared.exited.load(Ordering::SeqCst) {
            set_failed(&self.shared, "startup_aborted");
            request_child_termination(&self.shared);
            self.containment.kill_tree(self.pid);
        }
    }
}

async fn wait_for_io(shared: &Shared, timeout: Duration) -> Result<(), LarmError> {
    let mut done = shared.io_done.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if *done.borrow_and_update() == 7 {
            if shared.exited.load(Ordering::SeqCst) {
                return Ok(());
            }
            break;
        }
        if !matches!(
            tokio::time::timeout_at(deadline, done.changed()).await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    Err(err(
        ErrorCode::Timeout,
        "El IO del adaptador no terminó",
        "worker_io_cleanup_timeout",
    ))
}

/// Worker vivo.
pub struct Worker {
    pub manifest: AdapterManifest,
    pub generation: u64,
    pub pid: u32,
    config: HostConfig,
    shared: Arc<Shared>,
    containment: Arc<dyn Containment>,
    permits: Arc<Semaphore>,
    queued: Arc<Semaphore>,
    serial: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
    handshake: HandshakeResult,
}

fn err(code: ErrorCode, message: &str, reason: &str) -> LarmError {
    LarmError::new(code, message, reason)
}

/// Ejecutable del manifiesto resuelto dentro del paquete.
///
/// # Errors
/// `VALIDATION_FAILED` si no existe, no es un fichero o sale del paquete
/// (enlaces incluidos).
pub fn resolve_executable(
    package_dir: &Path,
    manifest: &AdapterManifest,
) -> Result<PathBuf, LarmError> {
    let bad = |m: &str| err(ErrorCode::ValidationFailed, m, "executable_path_invalid");
    let package = std::fs::canonicalize(package_dir)
        .map_err(|_| bad("El paquete del adaptador no existe"))?;
    let candidate = manifest
        .executable
        .relative_path
        .split(['/', '\\'])
        .fold(package.clone(), |p, c| p.join(c));
    let exe = std::fs::canonicalize(&candidate)
        .map_err(|_| bad("El ejecutable del adaptador no existe"))?;
    if !exe.starts_with(&package) || !exe.is_file() {
        return Err(bad("El ejecutable sale del paquete o no es un fichero"));
    }
    Ok(exe)
}

/// Entorno mínimo explícito de Windows para que un proceso arranque.
pub(crate) fn minimal_env(work_dir: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.to_string(), v));
        }
    }
    let tmp = work_dir.join("tmp").display().to_string();
    env.push(("TEMP".into(), tmp.clone()));
    env.push(("TMP".into(), tmp));
    env
}

/// Misma construcción para el worker y los probes internos P026: sin shell,
/// executable dentro del paquete, entorno explícito y sin consola.
pub(crate) fn process_command(spec: &WorkerSpec) -> Result<Command, LarmError> {
    let exe = resolve_executable(&spec.package_dir, &spec.manifest)?;
    std::fs::create_dir_all(spec.work_dir.join("tmp")).map_err(|e| {
        err(
            ErrorCode::ResourceUnavailable,
            "No se pudo crear la carpeta de trabajo",
            "io_failed",
        )
        .with_technical(e.to_string())
    })?;
    for (name, _) in &spec.environment {
        if !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(err(
                ErrorCode::ValidationFailed,
                "Nombre de variable no válido",
                "environment_invalid",
            ));
        }
    }
    let mut cmd = Command::new(&exe);
    cmd.args(&spec.manifest.executable.args)
        .current_dir(&spec.work_dir)
        .env_clear()
        .envs(minimal_env(&spec.work_dir))
        .envs(spec.environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    Ok(cmd)
}

fn redact(line: &str) -> String {
    larm_domain::redact::redact(line)
}

impl Worker {
    /// Arranca el worker y hace el handshake.
    ///
    /// # Errors
    /// Ejecutable inválido, fallo al lanzar, handshake fallido o vencido.
    pub async fn start(
        spec: WorkerSpec,
        config: HostConfig,
        generation: u64,
        containment: Arc<dyn Containment>,
    ) -> Result<Arc<Self>, LarmError> {
        Self::start_owned(spec, config, generation, containment, None).await
    }

    pub(crate) async fn start_owned(
        spec: WorkerSpec,
        config: HostConfig,
        generation: u64,
        containment: Arc<dyn Containment>,
        owner: Option<Arc<dyn Send + Sync>>,
    ) -> Result<Arc<Self>, LarmError> {
        let process_job = ProcessJob::create()?;
        let mut cmd = process_command(&spec)?;
        let mut child: Child = cmd.spawn().map_err(|e| {
            err(
                ErrorCode::AdapterCrashed,
                "No se pudo lanzar el adaptador",
                "spawn_failed",
            )
            .with_technical(e.to_string())
        })?;
        if let Err(error) = process_job.assign(&child) {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
        let pid = child.id().unwrap_or(0);
        let (state, _) = watch::channel(WorkerState::Starting);
        let (terminate, termination_requests) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            state,
            pending: Arc::default(),
            progress: broadcast::channel(256).0,
            logs: broadcast::channel(256).0,
            stdin: tokio::sync::Mutex::new(child.stdin.take()),
            malformed: AtomicU32::new(0),
            stderr_bytes: AtomicU64::new(0),
            stderr_dropped: AtomicU64::new(0),
            reason: Mutex::new(None),
            exited: std::sync::atomic::AtomicBool::new(false),
            recent: Mutex::default(),
            io_done: watch::channel(0).0,
            terminate,
            _owner: owner,
        });
        let mut starting = StartingProcess {
            shared: shared.clone(),
            containment: containment.clone(),
            pid,
            armed: true,
        };
        let adapter_id = spec.manifest.id.clone();
        if let Some(stdout) = child.stdout.take() {
            supervise_io(
                shared.clone(),
                1,
                read_stdout(stdout, shared.clone(), config, containment.clone(), pid),
            );
        } else {
            shared.io_done.send_modify(|done| *done |= 1);
        }
        if let Some(stderr) = child.stderr.take() {
            supervise_io(
                shared.clone(),
                2,
                read_stderr(
                    stderr,
                    shared.clone(),
                    config.stderr_limit_bytes,
                    adapter_id.clone(),
                    generation,
                ),
            );
        } else {
            shared.io_done.send_modify(|done| *done |= 2);
        }
        supervise_io(
            shared.clone(),
            4,
            watch_exit(child, shared.clone(), termination_requests, process_job),
        );

        let (req, nonce) = handshake_request(&adapter_id);
        let handshake = async {
            let response =
                match tokio::time::timeout(config.handshake_timeout, send(&shared, req)).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(err(
                            ErrorCode::Timeout,
                            "El adaptador no respondió al handshake",
                            "handshake_timeout",
                        ))
                    }
                };
            let result: HandshakeResult = match (response.result, response.error) {
                (Some(v), None) => match serde_json::from_value(v) {
                    Ok(h) => h,
                    Err(e) => {
                        return Err(err(
                            ErrorCode::ProtocolError,
                            "Respuesta de handshake inválida",
                            "handshake_invalid",
                        )
                        .with_technical(e.to_string()))
                    }
                },
                (_, Some(e)) => return Err(e.to_larm()),
                _ => {
                    return Err(err(
                        ErrorCode::ProtocolError,
                        "Handshake vacío",
                        "handshake_invalid",
                    ))
                }
            };
            check_handshake(&result, &nonce, &spec.manifest)?;
            Ok(result)
        }
        .await;
        let result = match handshake {
            Ok(result) => result,
            Err(error) => {
                set_failed(&shared, &error.details.reason);
                drop(shared.stdin.lock().await.take());
                if !shared.exited.load(Ordering::SeqCst) {
                    request_child_termination(&shared);
                    containment.kill_tree(pid);
                }
                wait_for_io(&shared, Duration::from_secs(5)).await?;
                starting.armed = false;
                return Err(error);
            }
        };
        if *shared.state.borrow() == WorkerState::Starting {
            shared.state.send_replace(WorkerState::Ready);
        }
        let worker = Arc::new(Self {
            manifest: spec.manifest,
            generation,
            pid,
            config,
            shared,
            containment,
            permits: Arc::new(Semaphore::new(config.max_concurrent)),
            queued: Arc::new(Semaphore::new(config.queue)),
            serial: Mutex::default(),
            handshake: result,
        });
        tokio::spawn(heartbeat(Arc::downgrade(&worker)));
        starting.armed = false;
        Ok(worker)
    }

    #[must_use]
    pub fn state(&self) -> WorkerState {
        *self.shared.state.borrow()
    }

    /// Motivo del último paso a DEGRADED/FAILED.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.shared.reason.lock().ok().and_then(|r| r.clone())
    }

    #[must_use]
    pub fn handshake(&self) -> &HandshakeResult {
        &self.handshake
    }

    /// Bytes de stderr descartados por superar el límite.
    #[must_use]
    pub fn stderr_dropped(&self) -> u64 {
        self.shared.stderr_dropped.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn subscribe_progress(&self) -> broadcast::Receiver<Progress> {
        self.shared.progress.subscribe()
    }

    /// Primeras y últimas líneas de stderr (redactadas), para diagnóstico.
    #[must_use]
    pub fn recent_logs(&self) -> Vec<LogLine> {
        self.shared
            .recent
            .lock()
            .map(|r| r.0.iter().chain(r.1.iter()).cloned().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn subscribe_logs(&self) -> broadcast::Receiver<LogLine> {
        self.shared.logs.subscribe()
    }

    /// Llama a un método del manifiesto. `resource_id` serializa las
    /// mutaciones sobre el mismo recurso.
    ///
    /// # Errors
    /// `UNSUPPORTED_CAPABILITY` (no anunciado), `RESOURCE_UNAVAILABLE`
    /// (worker no listo o cola llena), `TIMEOUT`, `ADAPTER_CRASHED` o el
    /// error de dominio del adaptador.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
        resource_id: Option<&str>,
    ) -> Result<Value, LarmError> {
        self.call_request(Request::new(method, params), timeout, resource_id)
            .await
    }

    /// Como [`Worker::call`], con la petición (y su id) construida por quien
    /// llama, para poder cancelarla con [`Worker::cancel`].
    ///
    /// # Errors
    /// Los de [`Worker::call`].
    pub async fn call_request(
        &self,
        request: Request,
        timeout: Option<Duration>,
        resource_id: Option<&str>,
    ) -> Result<Value, LarmError> {
        let method = request.method.clone();
        self.manifest.require_method(&method)?;
        if !matches!(self.state(), WorkerState::Ready | WorkerState::Degraded) {
            return Err(err(
                ErrorCode::ResourceUnavailable,
                "El adaptador no está listo",
                "worker_not_ready",
            )
            .retryable(true));
        }
        let _slot = self.queued.clone().try_acquire_owned().map_err(|_| {
            err(
                ErrorCode::ResourceUnavailable,
                "Cola del adaptador llena",
                "queue_full",
            )
            .retryable(true)
        })?;
        let lock = resource_id.map(|r| {
            self.serial
                .lock()
                .map(|mut m| m.entry(r.to_string()).or_default().clone())
                .unwrap_or_default()
        });
        let _serial = match &lock {
            Some(l) => Some(l.lock().await),
            None => None,
        };
        let _permit = self.permits.clone().acquire_owned().await.map_err(|_| {
            err(
                ErrorCode::ResourceUnavailable,
                "El adaptador se está deteniendo",
                "worker_stopping",
            )
        })?;
        let id = request.id.clone();
        let deadline = timeout.unwrap_or(self.config.rpc_timeout);
        let fut = send(&self.shared, request);
        tokio::pin!(fut);
        let response = match tokio::time::timeout(deadline, &mut fut).await {
            Ok(r) => r?,
            Err(_) => {
                // Primero se pide cancelar; matar es el último recurso.
                let _ = self.cancel(&id).await;
                match tokio::time::timeout(self.config.cancel_grace, &mut fut).await {
                    Ok(_) => {}
                    Err(_) => {
                        set_failed(&self.shared, "cancel_grace_expired");
                        self.terminate();
                    }
                }
                return Err(err(
                    ErrorCode::Timeout,
                    "El adaptador no respondió a tiempo",
                    "rpc_timeout",
                )
                .retryable(true));
            }
        };
        match (response.result, response.error) {
            (_, Some(e)) => Err(e.to_larm()),
            (Some(v), None) => Ok(v),
            (None, None) => Err(err(
                ErrorCode::ProtocolError,
                "Respuesta sin resultado",
                "empty_response",
            )),
        }
    }

    /// Pide cancelar una llamada en curso.
    ///
    /// # Errors
    /// Fallo al escribir o respuesta inválida.
    pub async fn cancel(&self, request_id: &str) -> Result<bool, LarmError> {
        let request = Request::new("core.cancel", serde_json::json!({"request_id": request_id}));
        let r = tokio::time::timeout(self.config.cancel_grace, send(&self.shared, request))
            .await
            .map_err(|_| {
                err(
                    ErrorCode::Timeout,
                    "Sin respuesta a la cancelación",
                    "cancel_timeout",
                )
            })??;
        Ok(r.result
            .and_then(|v| v.get("cancelled").and_then(Value::as_bool))
            .unwrap_or(false))
    }

    /// Señala al propietario de `Child` y solicita además la terminación del árbol.
    pub(crate) fn terminate(&self) {
        request_child_termination(&self.shared);
        self.containment.kill_tree(self.pid);
    }

    /// Parada compatible; los consumidores que deban confirmar metadata usan
    /// stop_and_wait para propagar un fallo de drenaje.
    pub async fn stop(&self) {
        if let Err(error) = self.stop_and_wait().await {
            tracing::error!(reason = %error.details.reason, "No terminó el IO del adaptador");
        }
    }

    /// Cierra stdin, espera la gracia, termina el árbol si hace falta y espera
    /// tanto el proceso como stdout/stderr. Un timeout nunca acredita cierre.
    pub async fn stop_and_wait(&self) -> Result<(), LarmError> {
        self.shared.state.send_replace(WorkerState::Stopped);
        // Un escritor bloqueado en una pipe llena no puede impedir para siempre
        // que el cierre alcance el último recurso.
        match tokio::time::timeout(self.config.cancel_grace, self.shared.stdin.lock()).await {
            Ok(mut stdin) => {
                drop(stdin.take());
            }
            Err(_) => {
                if !self.exited() {
                    self.terminate();
                }
            }
        }
        if wait_for_io(&self.shared, self.config.cancel_grace)
            .await
            .is_ok()
        {
            return Ok(());
        }
        if !self.exited() {
            self.terminate();
        }
        let result = wait_for_io(&self.shared, Duration::from_secs(5)).await;
        if result.is_err() {
            self.shared.state.send_replace(WorkerState::Failed);
            if let Ok(mut reason) = self.shared.reason.lock() {
                *reason = Some("worker_io_cleanup_timeout".into());
            }
        }
        result
    }

    #[must_use]
    pub fn io_drained(&self) -> bool {
        *self.shared.io_done.borrow() == 7 && self.exited()
    }

    /// ¿Ha terminado el proceso?
    #[must_use]
    pub fn exited(&self) -> bool {
        self.shared.exited.load(Ordering::SeqCst)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if !self.exited() {
            set_failed(&self.shared, "host_owner_dropped");
            self.terminate();
        }
    }
}

fn set_failed(shared: &Shared, reason: &str) {
    if let Ok(mut r) = shared.reason.lock() {
        *r = Some(reason.to_string());
    }
    let current = *shared.state.borrow();
    if current != WorkerState::Stopped {
        shared.state.send_replace(WorkerState::Failed);
    }
    // Ninguna llamada se queda esperando a un proceso que ya no responderá.
    if let Ok(mut pending) = shared.pending.lock() {
        pending.clear();
    }
}

async fn send(shared: &Arc<Shared>, request: Request) -> Result<Response, LarmError> {
    let (tx, rx) = oneshot::channel();
    let id = request.id.clone();
    let method = request.method.clone();
    let bytes = encode(&Frame::Request(request))?;
    {
        let mut pending = shared.pending.lock().map_err(|_| {
            err(
                ErrorCode::ResourceUnavailable,
                "Estado RPC no disponible",
                "rpc_state_unavailable",
            )
        })?;
        match pending.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(PendingCall { sender: tx, method });
            }
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(err(
                    ErrorCode::Conflict,
                    "El id RPC sigue en uso",
                    "request_id_in_use",
                ));
            }
        }
    }
    {
        let mut stdin = shared.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            return Err(err(
                ErrorCode::AdapterCrashed,
                "El adaptador no acepta peticiones",
                "stdin_closed",
            ));
        };
        if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
            return Err(err(
                ErrorCode::AdapterCrashed,
                "El adaptador cerró su entrada",
                "stdin_closed",
            ));
        }
    }
    rx.await.map_err(|_| {
        err(
            ErrorCode::AdapterCrashed,
            "El adaptador terminó sin responder",
            "adapter_crashed",
        )
    })
}

async fn read_stdout(
    stdout: tokio::process::ChildStdout,
    shared: Arc<Shared>,
    config: HostConfig,
    containment: Arc<dyn Containment>,
    pid: u32,
) {
    let contracts = match larm_adapter_sdk::contracts::Contracts::shared() {
        Ok(contracts) => contracts,
        Err(_) => {
            set_failed(&shared, "adapter_contracts_unavailable");
            request_child_termination(&shared);
            containment.kill_tree(pid);
            return;
        }
    };
    let mut reader = BufReader::new(stdout);
    loop {
        let line = match crate::framing::read_line(&mut reader, MAX_FRAME_BYTES, false).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => {
                set_failed(&shared, "stdout_read_failed");
                request_child_termination(&shared);
                containment.kill_tree(pid);
                return;
            }
        };
        let crate::framing::Line::Frame(line, _) = line else {
            set_failed(&shared, "stdout_frame_too_large");
            request_child_termination(&shared);
            containment.kill_tree(pid);
            return;
        };
        let frame = match decode(&line) {
            Ok(f) => f,
            Err(_) => {
                if malformed(&shared, &config, containment.as_ref(), pid) {
                    return;
                }
                continue;
            }
        };
        match frame {
            Frame::Response(r) => {
                let call =
                    r.id.as_ref()
                        .and_then(|id| shared.pending.lock().ok().and_then(|mut p| p.remove(id)));
                if let Some(call) = call {
                    if let Some(value) = &r.result {
                        // El handshake conserva su comprobación de nonce/major
                        // en start; los demás replies se validan aquí.
                        if call.method != "core.handshake" {
                            if let Err(error) = contracts.response(&call.method, value) {
                                let _ = call
                                    .sender
                                    .send(Response::err(r.id.as_deref(), RpcError::domain(error)));
                                if malformed(&shared, &config, containment.as_ref(), pid) {
                                    return;
                                }
                                continue;
                            }
                        }
                    }
                    let _ = call.sender.send(r);
                } else if malformed(&shared, &config, containment.as_ref(), pid) {
                    return;
                }
            }
            Frame::Notification(n) => {
                if let Ok(p) = serde_json::from_value::<Progress>(n.params) {
                    if shared
                        .pending
                        .lock()
                        .is_ok_and(|pending| pending.contains_key(&p.request_id))
                    {
                        let _ = shared.progress.send(p);
                    } else if malformed(&shared, &config, containment.as_ref(), pid) {
                        return;
                    }
                }
            }
            // Un adaptador no hace peticiones al host.
            Frame::Request(_) => {
                if malformed(&shared, &config, containment.as_ref(), pid) {
                    return;
                }
            }
        }
    }
}

/// Todos los frames inválidos usan el mismo presupuesto, también IDs ajenos,
/// respuestas duplicadas y peticiones en sentido inverso.
fn malformed(
    shared: &Arc<Shared>,
    config: &HostConfig,
    containment: &dyn Containment,
    pid: u32,
) -> bool {
    if shared.malformed.fetch_add(1, Ordering::SeqCst) + 1 >= config.malformed_limit {
        set_failed(shared, "stdout_corrupt");
        request_child_termination(shared);
        containment.kill_tree(pid);
        return true;
    }
    if *shared.state.borrow() == WorkerState::Ready {
        if let Ok(mut reason) = shared.reason.lock() {
            *reason = Some("stdout_malformed".into());
        }
        shared.state.send_replace(WorkerState::Degraded);
    }
    false
}

async fn read_stderr(
    stderr: tokio::process::ChildStderr,
    shared: Arc<Shared>,
    limit: u64,
    adapter_id: String,
    generation: u64,
) {
    let mut reader = BufReader::new(stderr);
    // Se drena siempre: un stderr lleno bloquearía al adaptador.
    while let Ok(Some(line)) = crate::framing::read_line(&mut reader, 16 << 10, true).await {
        let size = match &line {
            crate::framing::Line::Frame(_, size) | crate::framing::Line::Overflow(size) => *size,
        };
        let total = shared.stderr_bytes.fetch_add(size, Ordering::SeqCst) + size;
        if total > limit || matches!(line, crate::framing::Line::Overflow(_)) {
            shared.stderr_dropped.fetch_add(size, Ordering::SeqCst);
            continue;
        }
        let crate::framing::Line::Frame(line, _) = line else {
            continue;
        };
        let text = redact(&String::from_utf8_lossy(&line));
        tracing::info!(target: "larm::adapter", adapter = %adapter_id, generation, "{text}");
        let entry = LogLine {
            adapter_id: adapter_id.clone(),
            generation,
            line: text,
        };
        if let Ok(mut recent) = shared.recent.lock() {
            let (head, tail) = &mut *recent;
            if head.len() < RECENT_HEAD {
                head.push(entry.clone());
            } else {
                if tail.len() >= RECENT_TAIL {
                    tail.pop_front();
                }
                tail.push_back(entry.clone());
            }
        }
        let _ = shared.logs.send(entry);
    }
}

fn request_child_termination(shared: &Shared) {
    let _ = shared.terminate.send(());
}

async fn watch_exit(
    mut child: Child,
    shared: Arc<Shared>,
    mut termination_requests: mpsc::UnboundedReceiver<()>,
    process_job: ProcessJob,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        Some(()) = termination_requests.recv() => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    if let Err(error) = status {
        set_failed(&shared, "worker_wait_failed");
        tracing::error!(%error, "No se pudo esperar la salida del worker");
        // No se publica salida ni se libera la admisión sin confirmación.
        std::future::pending::<()>().await;
    }
    drop(child);
    shared.exited.store(true, Ordering::SeqCst);
    let state = *shared.state.borrow();
    if state == WorkerState::Stopped || state == WorkerState::Failed {
        if let Ok(mut p) = shared.pending.lock() {
            p.clear();
        }
    } else {
        set_failed(&shared, "process_exited");
    }

    // Permite que descendientes de corta vida cierren los canales heredados y
    // drena su salida. Después de un segundo se terminan y esperan todos los
    // miembros restantes antes de acreditar el cierre completo.
    let mut io_done = shared.io_done.subscribe();
    let drain = async {
        loop {
            if *io_done.borrow_and_update() & 0b11 == 0b11 || io_done.changed().await.is_err() {
                return;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
    process_job.drain().await;
}

async fn heartbeat(worker: std::sync::Weak<Worker>) {
    let mut misses = 0;
    loop {
        let Some(w) = worker.upgrade() else { return };
        let interval = w.config.heartbeat;
        if !matches!(w.state(), WorkerState::Ready | WorkerState::Degraded) {
            return;
        }
        let beat = tokio::time::timeout(
            interval,
            send(
                &w.shared,
                Request::new("core.heartbeat", serde_json::json!({})),
            ),
        )
        .await;
        match beat {
            Ok(Ok(r))
                if r.result
                    .as_ref()
                    .and_then(|v| v.get("alive"))
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                misses = 0
            }
            _ => misses += 1,
        }
        if misses >= w.config.heartbeat_misses {
            set_failed(&w.shared, "heartbeat_missed");
            w.terminate();
            return;
        }
        drop(w);
        tokio::time::sleep(interval).await;
    }
}
