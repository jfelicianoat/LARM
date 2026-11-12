//! Controlador de procesos de RuntimeInstance (P033).
//!
//! Las paradas graceful pasan por la sesión cualificada que creó la instancia.
//! El force cierra exclusivamente ese worker y su Job Object; un PID sin sesión
//! retenida nunca se convierte en autoridad para terminar procesos del sistema.

use async_trait::async_trait;
use larm_domain::{
    error::{ErrorCode, LarmError},
    instances::{HealthResult, InstanceRef},
    runtime_execution::{RuntimeDiscoverySession, RuntimeOperationAccess},
};
use larm_runtime::ports::PortReservation;
use larm_runtime::supervisor::{OwnedProcessController, ProcessIdentity, ProcessProbe};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Duration};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProcessKey {
    pid: u32,
    creation_time: String,
    executable_sha256: String,
    generation: u64,
}

impl From<&ProcessIdentity> for ProcessKey {
    fn from(identity: &ProcessIdentity) -> Self {
        Self {
            pid: identity.pid,
            creation_time: identity.creation_time.clone(),
            executable_sha256: identity.executable_sha256.clone(),
            generation: identity.generation,
        }
    }
}

struct ManagedSession {
    identity: ProcessIdentity,
    instance: InstanceRef,
    access: RuntimeOperationAccess,
    timeout: Duration,
    _port_reservation: Option<PortReservation>,
    session: Option<Box<dyn RuntimeDiscoverySession>>,
}

/// Conserva una sesión física por instancia viva para que stop opere sobre el
/// worker propietario, no sobre un worker nuevo sin estado de proceso.
#[derive(Clone, Default)]
pub struct RuntimeSessionRegistry {
    sessions: Arc<Mutex<HashMap<ProcessKey, Arc<AsyncMutex<ManagedSession>>>>>,
}

impl RuntimeSessionRegistry {
    // La ruta real de arranque que registra instancias es el bloqueo restante de P033.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn register(
        &self,
        identity: ProcessIdentity,
        instance: InstanceRef,
        access: RuntimeOperationAccess,
        timeout: Duration,
        session: Box<dyn RuntimeDiscoverySession>,
    ) -> Result<(), LarmError> {
        self.register_with_port(identity, instance, access, timeout, None, session)
            .await
    }

    /// Retiene el puerto comprobado por el coordinador durante todo el ciclo
    /// de vida de la sesión que lo recibió en su `LaunchPlan`.
    pub async fn register_with_port(
        &self,
        identity: ProcessIdentity,
        instance: InstanceRef,
        access: RuntimeOperationAccess,
        timeout: Duration,
        port_reservation: Option<PortReservation>,
        session: Box<dyn RuntimeDiscoverySession>,
    ) -> Result<(), LarmError> {
        if let Err(error) = identity.validate().and_then(|_| instance.validate()) {
            let _ = session.close().await;
            return Err(error);
        }
        if identity.generation != instance.generation
            || timeout.is_zero()
            || timeout > Duration::from_secs(600)
        {
            let _ = session.close().await;
            return Err(invalid("runtime_process_session_binding_invalid"));
        }
        let key = ProcessKey::from(&identity);
        let mut pending_session = Some(session);
        let duplicate = {
            let mut sessions = self.sessions.lock();
            match sessions.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => true,
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let Some(session) = pending_session.take() else {
                        return Err(invalid("runtime_process_session_missing"));
                    };
                    entry.insert(Arc::new(AsyncMutex::new(ManagedSession {
                        identity,
                        instance,
                        access,
                        timeout,
                        _port_reservation: port_reservation,
                        session: Some(session),
                    })));
                    false
                }
            }
        };
        if duplicate {
            let error = LarmError::new(
                ErrorCode::Conflict,
                "La identidad ya tiene una sesión de runtime retenida",
                "runtime_process_session_duplicate",
            );
            let Some(session) = pending_session else {
                return Err(error);
            };
            return match session.close().await {
                Ok(()) => Err(error),
                Err(close_error) => Err(error.with_technical(format!(
                    "cierre de sesión duplicada: {}",
                    close_error.details.reason
                ))),
            };
        }
        Ok(())
    }

    pub async fn health(
        &self,
        expected: &ProcessIdentity,
        timeout: Duration,
    ) -> Result<HealthResult, LarmError> {
        let entry = self
            .get(expected)
            .ok_or_else(|| unavailable("runtime_process_session_missing"))?;
        let entry = entry.lock().await;
        if entry.identity != *expected {
            return Err(identity_mismatch());
        }
        let session = entry
            .session
            .as_ref()
            .ok_or_else(|| unavailable("runtime_process_session_closed"))?;
        session
            .health(&entry.instance, &entry.access, Some(timeout))
            .await
    }

    fn get(&self, expected: &ProcessIdentity) -> Option<Arc<AsyncMutex<ManagedSession>>> {
        self.sessions
            .lock()
            .get(&ProcessKey::from(expected))
            .cloned()
    }

    async fn take_and_close(&self, expected: &ProcessIdentity) -> Result<(), LarmError> {
        let entry = self.sessions.lock().remove(&ProcessKey::from(expected));
        let Some(entry) = entry else {
            return Ok(());
        };
        let mut entry = entry.lock().await;
        if entry.identity != *expected {
            return Err(identity_mismatch());
        }
        if let Some(session) = entry.session.take() {
            session.close().await?;
        }
        Ok(())
    }

    /// Cierra workers retenidos al apagar el daemon. El Job Object termina los
    /// procesos descendientes que aún pertenezcan a cada worker.
    pub async fn close_all(&self) -> Result<(), LarmError> {
        let sessions = std::mem::take(&mut *self.sessions.lock());
        let mut first_error = None;
        for (_, entry) in sessions {
            let mut entry = entry.lock().await;
            if let Some(session) = entry.session.take() {
                if let Err(error) = session.close().await {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Inspección nativa con PID, hora de creación observada y hash del ejecutable.
/// El generation se copia del vínculo durable: el SO no publica esa dimensión.
pub(crate) fn inspect_process(expected: &ProcessIdentity) -> ProcessProbe {
    let pid = Pid::from_u32(expected.pid);
    let ids = [pid];
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let Some(process) = system.process(pid) else {
        return ProcessProbe::Exited;
    };
    let Some(executable) = process.exe() else {
        return ProcessProbe::Unavailable;
    };
    let Ok(executable) = std::fs::canonicalize(executable) else {
        return ProcessProbe::Unavailable;
    };
    let Ok(bytes) = std::fs::read(&executable) else {
        return ProcessProbe::Unavailable;
    };
    let Ok(start_seconds) = i64::try_from(process.start_time()) else {
        return ProcessProbe::Unavailable;
    };
    let Some(started) = chrono::DateTime::<chrono::Utc>::from_timestamp(start_seconds, 0) else {
        return ProcessProbe::Unavailable;
    };
    let observed = ProcessIdentity {
        pid: expected.pid,
        creation_time: larm_domain::bridge::timestamp(started),
        executable_sha256: hex::encode(Sha256::digest(bytes)),
        generation: expected.generation,
    };
    if observed.validate().is_err() {
        ProcessProbe::Unavailable
    } else {
        ProcessProbe::Running(observed)
    }
}

/// Confirma que el handle del worker identifica al PID observado por Windows
/// y que el ejecutable observado coincide con el LaunchPlan cualificado.
pub(crate) async fn verify_started_process(
    handle: &larm_domain::instances::InstanceHandle,
    executable: &str,
) -> Result<ProcessIdentity, LarmError> {
    handle.validate()?;
    let (Some(pid), Some(creation_time)) = (handle.pid, handle.process_started_at.clone()) else {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "El worker no devolvió identidad de proceso completa",
            "runtime_process_identity_missing",
        ));
    };
    let executable = std::path::PathBuf::from(executable);
    let executable_sha256 = tokio::task::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(&executable).map_err(|_| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "No se pudo resolver el ejecutable del LaunchPlan",
                "runtime_process_executable_unavailable",
            )
        })?;
        let bytes = std::fs::read(canonical).map_err(|_| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "No se pudo verificar el ejecutable del LaunchPlan",
                "runtime_process_executable_unavailable",
            )
        })?;
        Ok::<_, LarmError>(hex::encode(Sha256::digest(bytes)))
    })
    .await
    .map_err(|_| unavailable("runtime_process_identity_task_failed"))??;
    let expected = ProcessIdentity {
        pid,
        creation_time,
        executable_sha256,
        generation: handle.generation,
    };
    expected.validate()?;
    let probe_identity = expected.clone();
    let observed = tokio::task::spawn_blocking(move || inspect_process(&probe_identity))
        .await
        .map_err(|_| unavailable("runtime_process_identity_task_failed"))?;
    match observed {
        ProcessProbe::Running(identity) if identity == expected => Ok(expected),
        ProcessProbe::Running(_) => Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "El PID no coincide con el LaunchPlan y handle observados",
            "runtime_process_identity_mismatch",
        )),
        ProcessProbe::Exited => Err(LarmError::new(
            ErrorCode::RuntimeCrash,
            "El proceso terminó antes de confirmar su identidad",
            "runtime_process_exited_during_identity_capture",
        )),
        ProcessProbe::Unavailable => Err(unavailable("runtime_process_identity_unavailable")),
    }
}

#[derive(Clone)]
pub struct NativeRuntimeProcessController {
    sessions: RuntimeSessionRegistry,
}

impl NativeRuntimeProcessController {
    #[must_use]
    pub fn new(sessions: RuntimeSessionRegistry) -> Self {
        Self { sessions }
    }
}

#[async_trait]
impl OwnedProcessController for NativeRuntimeProcessController {
    async fn probe(&self, expected: &ProcessIdentity) -> Result<ProcessProbe, LarmError> {
        let expected = expected.clone();
        Ok(
            tokio::task::spawn_blocking(move || inspect_process(&expected))
                .await
                .unwrap_or(ProcessProbe::Unavailable),
        )
    }

    async fn request_graceful_stop(&self, expected: &ProcessIdentity) -> Result<(), LarmError> {
        let entry = self
            .sessions
            .get(expected)
            .ok_or_else(|| unavailable("runtime_process_session_missing"))?;
        let entry = entry.lock().await;
        if entry.identity != *expected {
            return Err(identity_mismatch());
        }
        let session = entry
            .session
            .as_ref()
            .ok_or_else(|| unavailable("runtime_process_session_closed"))?;
        let result = session
            .stop(&entry.instance, &entry.access, Some(entry.timeout))
            .await?;
        if result.stopped {
            Ok(())
        } else {
            Err(unavailable("runtime_process_graceful_stop_unconfirmed"))
        }
    }

    async fn wait_for_exit(
        &self,
        expected: &ProcessIdentity,
        timeout: Duration,
    ) -> Result<bool, LarmError> {
        let started = tokio::time::Instant::now();
        loop {
            match self.probe(expected).await? {
                ProcessProbe::Exited => return Ok(true),
                ProcessProbe::Running(observed) if observed != *expected => return Ok(false),
                ProcessProbe::Running(_) => {}
                ProcessProbe::Unavailable => return Ok(false),
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(false);
            }
            tokio::time::sleep((timeout - elapsed).min(Duration::from_millis(100))).await;
        }
    }

    async fn force_stop_tree(&self, expected: &ProcessIdentity) -> Result<(), LarmError> {
        if self.sessions.get(expected).is_none() {
            return Err(unavailable("runtime_process_session_missing"));
        }
        // Cerrar la sesión cierra su worker y el Job Object propietario. No se
        // invoca taskkill con un PID aislado.
        self.sessions.take_and_close(expected).await
    }

    async fn release_session(&self, expected: &ProcessIdentity) -> Result<(), LarmError> {
        self.sessions.take_and_close(expected).await
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Sesión de proceso de runtime no válida",
        reason,
    )
}

fn unavailable(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo controlar la sesión propietaria del runtime",
        reason,
    )
    .retryable(true)
}

fn identity_mismatch() -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "La sesión no coincide con la identidad del proceso",
        "runtime_process_session_identity_mismatch",
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use larm_domain::{
        error::ApiError,
        ports::AdapterQualificationGuard,
        runtime_execution::{RuntimeSessionBinding, RuntimeSessionGuard},
        runtime_install::{RuntimeVersionPage, RuntimeVersionSource, VersionQuery},
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone)]
    struct SessionFlags {
        stopped: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
    }

    struct FixtureSession {
        binding: RuntimeSessionBinding,
        flags: SessionFlags,
    }

    #[async_trait]
    impl AdapterQualificationGuard for FixtureSession {
        async fn recheck(&self) -> Result<(), ApiError> {
            Ok(())
        }
    }

    impl RuntimeSessionGuard for FixtureSession {
        fn binding(&self) -> &RuntimeSessionBinding {
            &self.binding
        }
    }

    #[async_trait]
    impl RuntimeVersionSource for FixtureSession {
        async fn list_versions(
            &self,
            _query: &VersionQuery,
        ) -> Result<RuntimeVersionPage, LarmError> {
            Err(unavailable("fixture_versions_not_supported"))
        }
    }

    #[async_trait]
    impl RuntimeDiscoverySession for FixtureSession {
        async fn stop(
            &self,
            request: &InstanceRef,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<larm_domain::instances::StopResult, LarmError> {
            request.validate()?;
            self.flags.stopped.store(true, Ordering::SeqCst);
            Ok(larm_domain::instances::StopResult { stopped: true })
        }

        async fn close(&self) -> Result<(), LarmError> {
            self.flags.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 4567,
            creation_time: "2026-10-24T00:00:00.000Z".into(),
            executable_sha256: "a".repeat(64),
            generation: 3,
        }
    }

    fn fixture_session(flags: SessionFlags) -> FixtureSession {
        FixtureSession {
            binding: RuntimeSessionBinding {
                runtime_id: "0192aaaa-0000-7000-8000-000000000001".into(),
                installation_sha256: "b".repeat(64),
                run_sha256: "c".repeat(64),
                actor_id: "operator".into(),
            },
            flags,
        }
    }

    #[tokio::test]
    async fn controller_usa_stop_cualificado_y_cierra_solo_la_sesion_vinculada() {
        let registry = RuntimeSessionRegistry::default();
        let flags = SessionFlags {
            stopped: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        };
        let instance = InstanceRef {
            instance_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            generation: 3,
        };
        registry
            .register(
                test_identity(),
                instance,
                RuntimeOperationAccess::default(),
                Duration::from_secs(5),
                Box::new(fixture_session(flags.clone())),
            )
            .await
            .unwrap();
        let controller = NativeRuntimeProcessController::new(registry.clone());
        controller
            .request_graceful_stop(&test_identity())
            .await
            .unwrap();
        assert!(flags.stopped.load(Ordering::SeqCst));
        controller.release_session(&test_identity()).await.unwrap();
        assert!(flags.closed.load(Ordering::SeqCst));
        assert!(registry.sessions.lock().is_empty());
    }

    #[tokio::test]
    async fn controlador_observa_pid_hora_de_creacion_y_hash_del_proceso_real() {
        let mut system = System::new();
        let pid = std::process::id();
        let ids = [Pid::from_u32(pid)];
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let process = system.process(ids[0]).unwrap();
        let expected = ProcessIdentity {
            pid,
            creation_time: larm_domain::bridge::timestamp(
                chrono::DateTime::<chrono::Utc>::from_timestamp(
                    i64::try_from(process.start_time()).unwrap(),
                    0,
                )
                .unwrap(),
            ),
            executable_sha256: hex::encode(Sha256::digest(
                std::fs::read(process.exe().unwrap()).unwrap(),
            )),
            generation: 7,
        };
        assert_eq!(
            NativeRuntimeProcessController::new(RuntimeSessionRegistry::default())
                .probe(&expected)
                .await
                .unwrap(),
            ProcessProbe::Running(expected)
        );
    }
}
