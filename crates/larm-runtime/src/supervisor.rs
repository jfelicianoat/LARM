//! Primitivas de supervisión de procesos e instancias (P033).
//!
//! Este módulo sólo autoriza operaciones cuando la identidad observada
//! coincide con la identidad persistida de un proceso LARM. La integración
//! nativa con el host proporciona el controlador de procesos y conserva los
//! handles de Windows; este núcleo no convierte un PID aislado en autoridad.

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use larm_domain::{
    error::{ErrorCode, LarmError},
    instances::RuntimeInstance,
    runtime::RuntimeOwnership,
};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, time::Duration};

pub const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(15);
pub const DEFAULT_CRASH_WINDOW: ChronoDuration = ChronoDuration::seconds(600);
pub const DEFAULT_CRASH_RESTART_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(15),
];

/// Identidad observada durante la ejecución o la reconciliación tras reinicio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time: String,
    pub executable_sha256: String,
    pub generation: u64,
}

impl ProcessIdentity {
    pub fn validate(&self) -> Result<(), LarmError> {
        let valid_hash = self.executable_sha256.len() == 64
            && self
                .executable_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        let valid_time = DateTime::parse_from_rfc3339(&self.creation_time).is_ok_and(|time| {
            time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) == self.creation_time
        });
        if self.pid == 0 || self.generation > i64::MAX as u64 || !valid_hash || !valid_time {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Identidad de proceso no válida",
                "runtime_process_identity_invalid",
            ));
        }
        Ok(())
    }

    fn persisted(instance: &RuntimeInstance) -> Option<Self> {
        Some(Self {
            pid: instance.pid?,
            creation_time: instance.process_started_at.clone()?,
            executable_sha256: instance.executable_hash.clone()?,
            generation: instance.generation,
        })
    }
}

/// Resultado de inspección del PID guardado. `Unavailable` significa que el
/// proveedor no pudo comprobarlo; no equivale a que el proceso haya terminado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessProbe {
    Running(ProcessIdentity),
    Exited,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationDecision {
    Resume(ProcessIdentity),
    MarkCrashed,
    LeaveExternal,
    RetryProbe,
    QuarantineAmbiguous,
}

/// Reconciliación conservadora: un PID ajeno o ambiguo nunca se termina.
#[must_use]
pub fn reconcile_process(
    instance: &RuntimeInstance,
    probe: ProcessProbe,
) -> ReconciliationDecision {
    if instance.ownership != RuntimeOwnership::LarmManaged {
        return ReconciliationDecision::LeaveExternal;
    }
    let Some(expected) = ProcessIdentity::persisted(instance) else {
        return ReconciliationDecision::QuarantineAmbiguous;
    };
    match probe {
        ProcessProbe::Unavailable => ReconciliationDecision::RetryProbe,
        ProcessProbe::Exited => ReconciliationDecision::MarkCrashed,
        ProcessProbe::Running(observed) if observed == expected => {
            ReconciliationDecision::Resume(expected)
        }
        // El PID puede haber sido reutilizado. El registro viejo queda
        // interrumpido, pero no se actúa sobre el proceso observado.
        ProcessProbe::Running(_) => ReconciliationDecision::MarkCrashed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    Graceful,
    Forced,
}

/// El host debe vincular estas operaciones a un handle propio y validar de
/// nuevo la identidad justo antes de terminar el árbol de procesos.
#[async_trait]
pub trait OwnedProcessController: Send + Sync {
    async fn probe(&self, expected: &ProcessIdentity) -> Result<ProcessProbe, LarmError>;
    async fn request_graceful_stop(&self, expected: &ProcessIdentity) -> Result<(), LarmError>;
    async fn wait_for_exit(
        &self,
        expected: &ProcessIdentity,
        timeout: Duration,
    ) -> Result<bool, LarmError>;
    async fn force_stop_tree(&self, expected: &ProcessIdentity) -> Result<(), LarmError>;
    /// Libera el controlador/sesión cuando la salida ya fue confirmada. Los
    /// hosts que no retienen una sesión por instancia pueden dejar el no-op.
    async fn release_session(&self, _expected: &ProcessIdentity) -> Result<(), LarmError> {
        Ok(())
    }
}

/// Pide parada normal, espera la gracia contractual y fuerza el árbol sólo
/// después de comprobar que el PID aún corresponde al proceso propio.
pub async fn stop_owned_process(
    instance: &RuntimeInstance,
    controller: &dyn OwnedProcessController,
    grace: Duration,
) -> Result<StopOutcome, LarmError> {
    if instance.ownership != RuntimeOwnership::LarmManaged {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "La instancia externa no se puede terminar",
            "runtime_external_process_not_killable",
        ));
    }
    let expected = ProcessIdentity::persisted(instance).ok_or_else(|| {
        LarmError::new(
            ErrorCode::IntegrityFailed,
            "Falta identidad persistida del proceso propio",
            "runtime_process_identity_missing",
        )
    })?;
    expected.validate()?;
    match controller.probe(&expected).await? {
        ProcessProbe::Exited => {
            controller.release_session(&expected).await?;
            return Ok(StopOutcome::Graceful);
        }
        ProcessProbe::Running(observed) if observed == expected => {}
        ProcessProbe::Running(_) => {
            return Err(ambiguous_identity());
        }
        ProcessProbe::Unavailable => return Err(probe_unavailable()),
    }

    // Un adapter puede haber rechazado o perdido el RPC graceful mientras el
    // proceso sigue siendo propio. Esperar y volver a comprobar identidad da
    // paso al force controlado si el proceso no sale; nunca autoriza por sí
    // solo matar el PID.
    let _graceful_request = controller.request_graceful_stop(&expected).await;
    if controller.wait_for_exit(&expected, grace).await? {
        controller.release_session(&expected).await?;
        return Ok(StopOutcome::Graceful);
    }

    // Revalidate after the wait: the original PID may now refer to an
    // unrelated process, so timeout alone never grants force-kill authority.
    match controller.probe(&expected).await? {
        ProcessProbe::Exited => {
            controller.release_session(&expected).await?;
            return Ok(StopOutcome::Graceful);
        }
        ProcessProbe::Running(observed) if observed == expected => {}
        ProcessProbe::Running(_) => return Err(ambiguous_identity()),
        ProcessProbe::Unavailable => return Err(probe_unavailable()),
    }
    controller.force_stop_tree(&expected).await?;
    if controller
        .wait_for_exit(&expected, Duration::from_secs(5))
        .await?
    {
        controller.release_session(&expected).await?;
        Ok(StopOutcome::Forced)
    } else {
        Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "El proceso propio no terminó después de forzar la parada",
            "runtime_process_force_stop_incomplete",
        )
        .retryable(true))
    }
}

fn ambiguous_identity() -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "El PID ya no coincide con la identidad persistida",
        "runtime_process_identity_ambiguous",
    )
}

fn probe_unavailable() -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo verificar la identidad del proceso",
        "runtime_process_probe_unavailable",
    )
    .retryable(true)
}

/// Secuencia de backoff del contrato 04: [1, 5, 15] dentro de 10 minutos.
#[derive(Debug, Clone)]
pub struct CrashRestartPolicy {
    pub delays: Vec<Duration>,
    pub window: ChronoDuration,
}

impl Default for CrashRestartPolicy {
    fn default() -> Self {
        Self {
            delays: DEFAULT_CRASH_RESTART_DELAYS.to_vec(),
            window: DEFAULT_CRASH_WINDOW,
        }
    }
}

impl CrashRestartPolicy {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.delays.is_empty() || self.window <= ChronoDuration::zero() {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Política de reinicio no válida",
                "runtime_crash_restart_policy_invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashDecision {
    RestartAfter(Duration),
    Fail,
}

#[derive(Debug, Clone)]
pub struct CrashRestartTracker {
    policy: CrashRestartPolicy,
    crashes: VecDeque<DateTime<Utc>>,
}

impl CrashRestartTracker {
    pub fn new(policy: CrashRestartPolicy) -> Result<Self, LarmError> {
        policy.validate()?;
        Ok(Self {
            policy,
            crashes: VecDeque::new(),
        })
    }

    pub fn unexpected_exit(&mut self, now: DateTime<Utc>) -> CrashDecision {
        let cutoff = now - self.policy.window;
        while self.crashes.front().is_some_and(|at| *at <= cutoff) {
            self.crashes.pop_front();
        }
        let Some(delay) = self.policy.delays.get(self.crashes.len()).copied() else {
            return CrashDecision::Fail;
        };
        self.crashes.push_back(now);
        CrashDecision::RestartAfter(delay)
    }

    #[must_use]
    pub fn recent_crashes(&self) -> usize {
        self.crashes.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use larm_domain::instances::InstanceState;
    use serde_json::Map;
    use std::{
        io::{BufRead, Write},
        process::{Child, ChildStdin, Command, Stdio},
        sync::Mutex,
        time::Instant,
    };

    const STAMP: &str = "2026-10-22T10:00:00.000Z";

    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 1234,
            creation_time: STAMP.into(),
            executable_sha256: "a".repeat(64),
            generation: 4,
        }
    }

    fn instance() -> RuntimeInstance {
        RuntimeInstance {
            id: "0192aaaa-0000-7000-8000-000000000001".into(),
            created_at: STAMP.into(),
            updated_at: STAMP.into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            variant_id: "0192aaaa-0000-7000-8000-000000000003".into(),
            backend_id: "0192aaaa-0000-7000-8000-000000000004".into(),
            launch_profile_id: "0192aaaa-0000-7000-8000-000000000005".into(),
            state: InstanceState::Ready,
            pid: Some(1234),
            process_started_at: Some(STAMP.into()),
            executable_hash: Some("a".repeat(64)),
            generation: 4,
            execution_fingerprint: None,
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        }
    }

    #[test]
    fn reconciliacion_reanuda_solo_identidad_completa_y_exacta() {
        let instance = instance();
        assert_eq!(
            reconcile_process(&instance, ProcessProbe::Running(identity())),
            ReconciliationDecision::Resume(identity())
        );
        assert_eq!(
            reconcile_process(&instance, ProcessProbe::Exited),
            ReconciliationDecision::MarkCrashed
        );
        assert_eq!(
            reconcile_process(&instance, ProcessProbe::Unavailable),
            ReconciliationDecision::RetryProbe
        );
        let mut external = instance.clone();
        external.ownership = RuntimeOwnership::External;
        assert_eq!(
            reconcile_process(&external, ProcessProbe::Running(identity())),
            ReconciliationDecision::LeaveExternal
        );
    }

    #[test]
    fn pid_reutilizado_no_se_adopta_ni_se_termina_y_falta_de_identidad_se_cuarentena() {
        let mut reused = identity();
        reused.executable_sha256 = "b".repeat(64);
        assert_eq!(
            reconcile_process(&instance(), ProcessProbe::Running(reused)),
            ReconciliationDecision::MarkCrashed
        );
        let mut incomplete = instance();
        incomplete.process_started_at = None;
        assert_eq!(
            reconcile_process(&incomplete, ProcessProbe::Running(identity())),
            ReconciliationDecision::QuarantineAmbiguous
        );
    }

    #[test]
    fn identidad_rechaza_pid_hash_o_fecha_no_validos() {
        let mut value = identity();
        value.pid = 0;
        assert!(value.validate().is_err());
        let mut value = identity();
        value.executable_sha256 = "A".repeat(64);
        assert!(value.validate().is_err());
        let mut value = identity();
        value.creation_time = "ayer".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn crashes_aplican_backoff_y_ventana_movil_de_diez_minutos() {
        let now = DateTime::parse_from_rfc3339(STAMP)
            .unwrap()
            .with_timezone(&Utc);
        let mut crashes = CrashRestartTracker::new(CrashRestartPolicy::default()).unwrap();
        assert_eq!(
            crashes.unexpected_exit(now),
            CrashDecision::RestartAfter(Duration::from_secs(1))
        );
        assert_eq!(
            crashes.unexpected_exit(now + ChronoDuration::seconds(20)),
            CrashDecision::RestartAfter(Duration::from_secs(5))
        );
        assert_eq!(
            crashes.unexpected_exit(now + ChronoDuration::seconds(40)),
            CrashDecision::RestartAfter(Duration::from_secs(15))
        );
        assert_eq!(
            crashes.unexpected_exit(now + ChronoDuration::seconds(60)),
            CrashDecision::Fail
        );
        assert_eq!(
            crashes.unexpected_exit(now + ChronoDuration::seconds(641)),
            CrashDecision::RestartAfter(Duration::from_secs(1))
        );
    }

    #[derive(Default)]
    struct FakeController {
        probes: Mutex<VecDeque<ProcessProbe>>,
        waits: Mutex<VecDeque<bool>>,
        actions: Mutex<Vec<&'static str>>,
        fail_graceful: bool,
    }

    #[async_trait]
    impl OwnedProcessController for FakeController {
        async fn probe(&self, _expected: &ProcessIdentity) -> Result<ProcessProbe, LarmError> {
            self.actions.lock().unwrap().push("probe");
            Ok(self.probes.lock().unwrap().pop_front().unwrap())
        }
        async fn request_graceful_stop(
            &self,
            _expected: &ProcessIdentity,
        ) -> Result<(), LarmError> {
            self.actions.lock().unwrap().push("graceful");
            if self.fail_graceful {
                Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El RPC graceful no respondió",
                    "runtime_process_graceful_stop_failed",
                ))
            } else {
                Ok(())
            }
        }
        async fn wait_for_exit(
            &self,
            _expected: &ProcessIdentity,
            _timeout: Duration,
        ) -> Result<bool, LarmError> {
            self.actions.lock().unwrap().push("wait");
            Ok(self.waits.lock().unwrap().pop_front().unwrap())
        }
        async fn force_stop_tree(&self, _expected: &ProcessIdentity) -> Result<(), LarmError> {
            self.actions.lock().unwrap().push("force");
            Ok(())
        }
    }

    #[tokio::test]
    async fn parada_graceful_evade_force_y_revalida_antes_de_terminar_arbol() {
        let graceful = FakeController {
            probes: Mutex::new(VecDeque::from([ProcessProbe::Running(identity())])),
            waits: Mutex::new(VecDeque::from([true])),
            actions: Mutex::new(Vec::new()),
            fail_graceful: false,
        };
        assert_eq!(
            stop_owned_process(&instance(), &graceful, DEFAULT_STOP_GRACE)
                .await
                .unwrap(),
            StopOutcome::Graceful
        );
        assert_eq!(
            *graceful.actions.lock().unwrap(),
            ["probe", "graceful", "wait"]
        );

        let forced = FakeController {
            probes: Mutex::new(VecDeque::from([
                ProcessProbe::Running(identity()),
                ProcessProbe::Running(identity()),
            ])),
            waits: Mutex::new(VecDeque::from([false, true])),
            actions: Mutex::new(Vec::new()),
            fail_graceful: false,
        };
        assert_eq!(
            stop_owned_process(&instance(), &forced, Duration::ZERO)
                .await
                .unwrap(),
            StopOutcome::Forced
        );
        assert_eq!(
            *forced.actions.lock().unwrap(),
            ["probe", "graceful", "wait", "probe", "force", "wait"]
        );
    }

    #[tokio::test]
    async fn rpc_graceful_fallido_espera_y_fuerza_solo_tras_revalidar_identidad() {
        let controller = FakeController {
            probes: Mutex::new(VecDeque::from([
                ProcessProbe::Running(identity()),
                ProcessProbe::Running(identity()),
            ])),
            waits: Mutex::new(VecDeque::from([false, true])),
            actions: Mutex::new(Vec::new()),
            fail_graceful: true,
        };
        assert_eq!(
            stop_owned_process(&instance(), &controller, Duration::ZERO)
                .await
                .unwrap(),
            StopOutcome::Forced
        );
        assert_eq!(
            *controller.actions.lock().unwrap(),
            ["probe", "graceful", "wait", "probe", "force", "wait"]
        );
    }

    #[tokio::test]
    async fn parada_se_neiga_a_externos_y_pid_ambiguo_no_se_mata() {
        let external = FakeController::default();
        let mut foreign = instance();
        foreign.ownership = RuntimeOwnership::External;
        assert_eq!(
            stop_owned_process(&foreign, &external, DEFAULT_STOP_GRACE)
                .await
                .unwrap_err()
                .details
                .reason,
            "runtime_external_process_not_killable"
        );
        assert!(external.actions.lock().unwrap().is_empty());

        let ambiguous = FakeController {
            probes: Mutex::new(VecDeque::from([
                ProcessProbe::Running(identity()),
                ProcessProbe::Running(ProcessIdentity {
                    executable_sha256: "b".repeat(64),
                    ..identity()
                }),
            ])),
            waits: Mutex::new(VecDeque::from([false])),
            actions: Mutex::new(Vec::new()),
            fail_graceful: false,
        };
        assert_eq!(
            stop_owned_process(&instance(), &ambiguous, Duration::ZERO)
                .await
                .unwrap_err()
                .details
                .reason,
            "runtime_process_identity_ambiguous"
        );
        assert_eq!(
            *ambiguous.actions.lock().unwrap(),
            ["probe", "graceful", "wait", "probe"]
        );
    }

    #[test]
    fn child_fixture() {
        if std::env::var_os("LARM_P033_CHILD_FIXTURE").is_none() {
            return;
        }
        if std::env::var_os("LARM_P033_IGNORE_GRACEFUL").is_some() {
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let input = std::io::stdin();
        for line in input.lock().lines() {
            if line.as_deref().is_ok_and(|value| value == "stop") {
                return;
            }
        }
    }

    struct FixtureChildController {
        child: Mutex<Child>,
        stdin: Mutex<ChildStdin>,
        identity: ProcessIdentity,
    }

    impl Drop for FixtureChildController {
        fn drop(&mut self) {
            if let Ok(child) = self.child.get_mut() {
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
        }
    }

    #[async_trait]
    impl OwnedProcessController for FixtureChildController {
        async fn probe(&self, expected: &ProcessIdentity) -> Result<ProcessProbe, LarmError> {
            if expected != &self.identity {
                return Ok(ProcessProbe::Running(self.identity.clone()));
            }
            let mut child = self.child.lock().unwrap();
            if child
                .try_wait()
                .map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo consultar el proceso fixture",
                        "runtime_process_probe_unavailable",
                    )
                    .with_technical(error.to_string())
                })?
                .is_some()
            {
                Ok(ProcessProbe::Exited)
            } else {
                Ok(ProcessProbe::Running(self.identity.clone()))
            }
        }

        async fn request_graceful_stop(
            &self,
            _expected: &ProcessIdentity,
        ) -> Result<(), LarmError> {
            let mut stdin = self.stdin.lock().unwrap();
            stdin
                .write_all(b"stop\n")
                .and_then(|()| stdin.flush())
                .map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo solicitar parada graceful al fixture",
                        "runtime_process_graceful_stop_failed",
                    )
                    .with_technical(error.to_string())
                })
        }

        async fn wait_for_exit(
            &self,
            _expected: &ProcessIdentity,
            timeout: Duration,
        ) -> Result<bool, LarmError> {
            let started = Instant::now();
            loop {
                if self
                    .child
                    .lock()
                    .unwrap()
                    .try_wait()
                    .map_err(|error| {
                        LarmError::new(
                            ErrorCode::ResourceUnavailable,
                            "No se pudo esperar el proceso fixture",
                            "runtime_process_wait_failed",
                        )
                        .with_technical(error.to_string())
                    })?
                    .is_some()
                {
                    return Ok(true);
                }
                if started.elapsed() >= timeout {
                    return Ok(false);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn force_stop_tree(&self, _expected: &ProcessIdentity) -> Result<(), LarmError> {
            self.child.lock().unwrap().kill().map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo finalizar el proceso fixture",
                    "runtime_process_force_stop_failed",
                )
                .with_technical(error.to_string())
            })
        }
    }

    fn spawn_fixture(ignore_graceful: bool) -> (RuntimeInstance, FixtureChildController) {
        use sha2::{Digest, Sha256};

        let executable = std::env::current_exe().unwrap();
        let hash = hex::encode(Sha256::digest(std::fs::read(&executable).unwrap()));
        let mut command = Command::new(executable);
        command
            .arg("--exact")
            .arg("supervisor::tests::child_fixture")
            .arg("--nocapture")
            .env("LARM_P033_CHILD_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if ignore_graceful {
            command.env("LARM_P033_IGNORE_GRACEFUL", "1");
        }
        let mut child = command.spawn().unwrap();
        let identity = ProcessIdentity {
            pid: child.id(),
            creation_time: larm_domain::bridge::timestamp(Utc::now()),
            executable_sha256: hash,
            generation: 4,
        };
        let stdin = child.stdin.take().unwrap();
        let mut instance = instance();
        instance.pid = Some(identity.pid);
        instance.process_started_at = Some(identity.creation_time.clone());
        instance.executable_hash = Some(identity.executable_sha256.clone());
        let controller = FixtureChildController {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            identity,
        };
        (instance, controller)
    }

    #[tokio::test]
    async fn proceso_fixture_windows_cubre_parada_graceful_y_forzada() {
        let (instance, graceful) = spawn_fixture(false);
        assert_eq!(
            stop_owned_process(&instance, &graceful, Duration::from_secs(2))
                .await
                .unwrap(),
            StopOutcome::Graceful
        );

        let (instance, forced) = spawn_fixture(true);
        assert_eq!(
            stop_owned_process(&instance, &forced, Duration::from_millis(50))
                .await
                .unwrap(),
            StopOutcome::Forced
        );
    }
}
