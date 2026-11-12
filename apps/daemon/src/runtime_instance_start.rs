//! Coordinación interna del ciclo LaunchPlan → RuntimeInstance (P033).
//!
//! El módulo no añade una ruta pública: los consumidores internos pasan una
//! sesión ya cualificada y un LaunchRequest admitido, y el coordinador conserva
//! únicamente recursos ligados a esa sesión física.

use crate::runtime_instance_process::{
    verify_started_process, NativeRuntimeProcessController, RuntimeSessionRegistry,
};
use larm_domain::{
    bridge::timestamp,
    error::{ErrorCode, LarmError},
    instances::{
        HealthResult, InstanceRef, InstanceState, LaunchPlan, LaunchRequest, RuntimeHealthState,
        RuntimeInstance, RuntimeLaunchSnapshot,
    },
    ports::MutationContext,
    runtime::RuntimeOwnership,
    runtime_execution::{RuntimeDiscoverySession, RuntimeOperationAccess},
    runtime_install::ExecutePlan,
    runtime_ports::RuntimeInstanceRepository,
};
use larm_runtime::{
    ports::{PortAllocator, PortReservation},
    supervisor::{stop_owned_process, ProcessIdentity, DEFAULT_STOP_GRACE},
};
use serde_json::{Map, Value};
use std::{sync::Arc, time::Duration};

const PLAN_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const START_RPC_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
const STOP_RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
#[allow(dead_code)] // El consumidor contractual de start está en P100.
pub struct RuntimeInstanceStartService {
    instances: Arc<dyn RuntimeInstanceRepository>,
    ports: PortAllocator,
    sessions: RuntimeSessionRegistry,
}

#[allow(dead_code)] // P033 prepara el coordinador; P100 lo invoca tras su admisión.
impl RuntimeInstanceStartService {
    #[must_use]
    pub fn new(
        instances: Arc<dyn RuntimeInstanceRepository>,
        ports: PortAllocator,
        sessions: RuntimeSessionRegistry,
    ) -> Self {
        Self {
            instances,
            ports,
            sessions,
        }
    }

    /// Inicia una instancia LARM administrada a partir de datos ya admitidos
    /// por un consumidor interno. `launch_profile_id` y `runtime_id` proceden
    /// del contexto cualificado, no de un request público.
    pub async fn start(
        &self,
        runtime_id: &str,
        launch_profile_id: &str,
        request: LaunchRequest,
        access: RuntimeOperationAccess,
        session: Box<dyn RuntimeDiscoverySession>,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError> {
        let mut session = Some(session);
        if !larm_domain::bridge::is_uuid_v7(runtime_id)
            || !larm_domain::bridge::is_uuid_v7(launch_profile_id)
        {
            return Err(close_after_error(
                &mut session,
                invalid("runtime_instance_start_identity_invalid"),
            )
            .await);
        }
        if session.as_ref().is_some_and(|value| {
            value.binding().runtime_id != runtime_id || value.binding().actor_id != ctx.actor_id
        }) {
            return Err(close_after_error(
                &mut session,
                LarmError::new(
                    ErrorCode::Forbidden,
                    "La sesión cualificada no corresponde al runtime y actor solicitados",
                    "runtime_instance_start_session_mismatch",
                ),
            )
            .await);
        }
        if let Err(error) = request.validate() {
            return Err(close_after_error(&mut session, error).await);
        }

        let planning_session = session
            .as_ref()
            .ok_or_else(|| invalid("runtime_instance_start_session_missing"))?;
        if let Err(error) = planning_session.recheck().await {
            return Err(close_after_error(&mut session, error).await);
        }
        let plan = match planning_session
            .launch_plan(&request, &access, Some(PLAN_RPC_TIMEOUT))
            .await
        {
            Ok(plan) => plan,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        if let Err(error) = plan.validate(chrono::Utc::now()) {
            return Err(close_after_error(&mut session, error).await);
        }
        if !request.constraints.allow_backend_fallback
            && (plan.effective_parameters.backend_id != request.launch_profile.backend_id
                || request
                    .constraints
                    .backend_id
                    .as_ref()
                    .is_some_and(|backend| backend != &plan.effective_parameters.backend_id))
        {
            return Err(close_after_error(
                &mut session,
                invalid("runtime_launch_plan_backend_mismatch"),
            )
            .await);
        }
        let snapshot = match RuntimeLaunchSnapshot::from_plan(plan.clone(), chrono::Utc::now()) {
            Ok(snapshot) => snapshot,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let port_reservation = match reserve_plan_port(&self.ports, plan.port) {
            Ok(reservation) => reservation,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };

        let execute = ExecutePlan {
            plan_id: plan.plan_id.clone(),
            plan_hash: plan.plan_hash.clone(),
            approval_id: None,
        };
        let handle = match session
            .as_ref()
            .ok_or_else(|| invalid("runtime_instance_start_session_missing"))?
            .start(&execute, &access, Some(START_RPC_TIMEOUT))
            .await
        {
            Ok(handle) => handle,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let identity = match verify_started_process(&handle, &plan.executable).await {
            Ok(identity) if identity.generation == 0 => identity,
            Ok(_) => {
                return Err(close_after_error(
                    &mut session,
                    invalid("runtime_instance_generation_invalid"),
                )
                .await)
            }
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };

        let created_at = timestamp(chrono::Utc::now());
        let effective_backend = plan.effective_parameters.backend_id.clone();
        let execution_fingerprint = match effective_profile_hash(&plan) {
            Ok(hash) => hash,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let mut created = RuntimeInstance {
            id: handle.instance_id.clone(),
            created_at: created_at.clone(),
            updated_at: created_at,
            row_version: 1,
            metadata: Map::<String, Value>::new(),
            runtime_installation_id: request.installation_id.clone(),
            variant_id: request.variant_id.clone(),
            backend_id: effective_backend,
            launch_profile_id: launch_profile_id.into(),
            state: InstanceState::Created,
            pid: None,
            process_started_at: None,
            executable_hash: Some(identity.executable_sha256.clone()),
            generation: handle.generation,
            execution_fingerprint: Some(execution_fingerprint),
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        };
        if let Err(error) = created.store_launch_snapshot(&snapshot) {
            return Err(close_after_error(&mut session, error).await);
        }
        if let Err(error) = self.instances.create_instance(&created, ctx).await {
            return Err(close_after_error(&mut session, error).await);
        }

        let mut starting = created.clone();
        starting.pid = Some(identity.pid);
        starting.process_started_at = Some(identity.creation_time.clone());
        let starting = match starting.transition_to(InstanceState::Starting) {
            Ok(instance) => instance,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let starting = match self
            .instances
            .transition_instance(&starting, created.row_version, ctx)
            .await
        {
            Ok(instance) => instance,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let loading = match starting.transition_to(InstanceState::Loading) {
            Ok(instance) => instance,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };
        let loading = match self
            .instances
            .transition_instance(&loading, starting.row_version, ctx)
            .await
        {
            Ok(instance) => instance,
            Err(error) => return Err(close_after_error(&mut session, error).await),
        };

        let instance_ref = InstanceRef {
            instance_id: loading.id.clone(),
            generation: loading.generation,
        };
        let session = session
            .take()
            .ok_or_else(|| invalid("runtime_instance_start_session_missing"))?;
        self.sessions
            .register_with_port(
                identity.clone(),
                instance_ref,
                access,
                STOP_RPC_TIMEOUT,
                port_reservation,
                session,
            )
            .await?;

        if let Err(error) = self.wait_until_ready(&identity, &loading).await {
            self.stop_failed_start(&loading, ctx).await?;
            return Err(error);
        }
        let ready = loading.transition_to(InstanceState::Ready)?;
        let ready = self
            .instances
            .transition_instance(&ready, loading.row_version, ctx)
            .await?;
        Ok(ready)
    }

    async fn wait_until_ready(
        &self,
        identity: &ProcessIdentity,
        instance: &RuntimeInstance,
    ) -> Result<HealthResult, LarmError> {
        let deadline = tokio::time::Instant::now() + LOAD_TIMEOUT;
        loop {
            let result = self.sessions.health(identity, HEALTH_RPC_TIMEOUT).await?;
            result.validate()?;
            match &result.state {
                RuntimeHealthState::Ready => return Ok(result),
                RuntimeHealthState::Loading => {}
                RuntimeHealthState::Degraded | RuntimeHealthState::Failed => {
                    return Err(LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "El runtime no alcanzó el estado READY durante el arranque",
                        "runtime_instance_start_health_failed",
                    )
                    .with_resource(instance.id.clone()))
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El runtime superó el límite de carga de 600 segundos",
                    "runtime_instance_load_timeout",
                )
                .retryable(true)
                .with_resource(instance.id.clone()));
            }
            tokio::time::sleep((deadline - now).min(HEALTH_INTERVAL)).await;
        }
    }

    async fn stop_failed_start(
        &self,
        instance: &RuntimeInstance,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let controller = NativeRuntimeProcessController::new(self.sessions.clone());
        stop_owned_process(instance, &controller, DEFAULT_STOP_GRACE).await?;
        let failed = instance.transition_to(InstanceState::Failed)?;
        self.instances
            .transition_instance(&failed, instance.row_version, ctx)
            .await?;
        Ok(())
    }
}

fn reserve_plan_port(
    allocator: &PortAllocator,
    port: u32,
) -> Result<Option<PortReservation>, LarmError> {
    if port == 0 {
        return Ok(None);
    }
    let port = u16::try_from(port).map_err(|_| invalid("runtime_launch_plan_port_invalid"))?;
    allocator.reserve_specific(port).map(Some)
}

fn effective_profile_hash(plan: &LaunchPlan) -> Result<String, LarmError> {
    let value = serde_json::json!({
        "backend_id": &plan.effective_parameters.backend_id,
        "parameters": &plan.effective_parameters,
    });
    larm_domain::canonical::sha256_hex(&value).map_err(|_| {
        LarmError::new(
            ErrorCode::IntegrityFailed,
            "No se pudo calcular la huella del perfil efectivo",
            "runtime_instance_execution_fingerprint_failed",
        )
    })
}

async fn close_after_error(
    session: &mut Option<Box<dyn RuntimeDiscoverySession>>,
    error: LarmError,
) -> LarmError {
    if let Some(session) = session.take() {
        if let Err(close_error) = session.close().await {
            return error.with_technical(format!(
                "cierre de sesión tras fallo: {}",
                close_error.details.reason
            ));
        }
    }
    error
}

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Solicitud interna de inicio de RuntimeInstance no válida",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use larm_domain::{
        error::{ApiError, ErrorCode},
        instances::{InstanceState, StopResult},
        ports::AdapterQualificationGuard,
        runtime_execution::{RuntimeSessionBinding, RuntimeSessionGuard},
        runtime_install::{RuntimeVersionPage, RuntimeVersionSource, VersionQuery},
        runtime_ports::RuntimeInstanceRepository,
    };
    use parking_lot::Mutex;
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
        sync::Arc,
    };
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    use tokio::sync::Mutex as AsyncMutex;

    const RUNTIME_ID: &str = "0192aaaa-0000-7000-8000-000000000001";
    const INSTALLATION_ID: &str = "0192aaaa-0000-7000-8000-000000000002";
    const VARIANT_ID: &str = "0192aaaa-0000-7000-8000-000000000003";
    const BACKEND_ID: &str = "0192aaaa-0000-7000-8000-000000000004";
    const PROFILE_ID: &str = "0192aaaa-0000-7000-8000-000000000005";

    #[derive(Default)]
    struct MemoryInstances(AsyncMutex<BTreeMap<String, RuntimeInstance>>);

    #[async_trait]
    impl RuntimeInstanceRepository for MemoryInstances {
        async fn create_instance(
            &self,
            instance: &RuntimeInstance,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            instance.validate_record()?;
            if instance.state != InstanceState::Created || instance.row_version != 1 {
                return Err(invalid("test_initial_instance_invalid"));
            }
            let mut rows = self.0.lock().await;
            if rows.contains_key(&instance.id) {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La instancia ya existe",
                    "runtime_instance_duplicate",
                ));
            }
            rows.insert(instance.id.clone(), instance.clone());
            Ok(())
        }

        async fn instance(&self, id: &str) -> Result<Option<RuntimeInstance>, LarmError> {
            Ok(self.0.lock().await.get(id).cloned())
        }

        async fn instances(
            &self,
            _after: Option<(String, String)>,
            _limit: u32,
            _runtime_id: Option<&str>,
        ) -> Result<Vec<RuntimeInstance>, LarmError> {
            Ok(self.0.lock().await.values().cloned().collect())
        }

        async fn runtime_exists(&self, _id: &str) -> Result<bool, LarmError> {
            Ok(true)
        }

        async fn transition_instance(
            &self,
            instance: &RuntimeInstance,
            expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            instance.validate_record()?;
            let mut rows = self.0.lock().await;
            let current = rows
                .get(&instance.id)
                .ok_or_else(|| invalid("test_instance_missing"))?;
            if current.row_version != expected_row_version
                || instance.row_version != expected_row_version + 1
                || !current.state.can_transition_to(instance.state)
            {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La transición CAS no coincide",
                    "runtime_instance_stale",
                ));
            }
            rows.insert(instance.id.clone(), instance.clone());
            Ok(instance.clone())
        }

        async fn begin_stop_instance(
            &self,
            instance: &RuntimeInstance,
            expected_row_version: i64,
            ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            self.transition_instance(instance, expected_row_version, ctx)
                .await
        }
    }

    #[derive(Default)]
    struct SessionFlags {
        started: bool,
        closed: bool,
    }

    struct FixtureSession {
        binding: RuntimeSessionBinding,
        plan: LaunchPlan,
        handle: larm_domain::instances::InstanceHandle,
        flags: Arc<Mutex<SessionFlags>>,
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
            Err(invalid("test_versions_not_supported"))
        }
    }

    #[async_trait]
    impl RuntimeDiscoverySession for FixtureSession {
        async fn launch_plan(
            &self,
            _request: &LaunchRequest,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<LaunchPlan, LarmError> {
            Ok(self.plan.clone())
        }

        async fn start(
            &self,
            request: &ExecutePlan,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<larm_domain::instances::InstanceHandle, LarmError> {
            if request.plan_id != self.plan.plan_id || request.plan_hash != self.plan.plan_hash {
                return Err(invalid("test_plan_binding_mismatch"));
            }
            self.flags.lock().started = true;
            Ok(self.handle.clone())
        }

        async fn health(
            &self,
            request: &InstanceRef,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<HealthResult, LarmError> {
            request.validate()?;
            Ok(HealthResult {
                state: RuntimeHealthState::Ready,
                backend_effective: Some(BACKEND_ID.into()),
                reason: "fixture_ready".into(),
                evidence_refs: vec!["fixture://runtime-health".into()],
            })
        }

        async fn stop(
            &self,
            request: &InstanceRef,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<StopResult, LarmError> {
            request.validate()?;
            Ok(StopResult { stopped: true })
        }

        async fn close(&self) -> Result<(), LarmError> {
            self.flags.lock().closed = true;
            Ok(())
        }
    }

    fn request() -> LaunchRequest {
        serde_json::from_value(serde_json::json!({
            "installation_id": INSTALLATION_ID,
            "variant_id": VARIANT_ID,
            "launch_profile": larm_domain::launch::LaunchParameters::builtin(
                BACKEND_ID.to_owned(), 4, None, 128
            ),
            "constraints": {
                "runtime_version_id": null,
                "backend_id": BACKEND_ID,
                "execution_fingerprint": null,
                "allow_backend_fallback": false,
                "max_memory_bytes": null
            },
            "resource_requirements": null
        }))
        .unwrap()
    }

    fn plan(port: u32) -> LaunchPlan {
        let parameters =
            larm_domain::launch::LaunchParameters::builtin(BACKEND_ID.to_owned(), 4, None, 128);
        let mut plan = LaunchPlan {
            plan_id: uuid::Uuid::now_v7().to_string(),
            plan_hash: "0".repeat(64),
            executable: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![],
            cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            env_allowlist: parameters.environment.allowlist.clone(),
            port,
            memory_estimate: larm_domain::estimate::RuntimeMemoryEstimate {
                weights_bytes: None,
                kv_bytes: None,
                activation_bytes: None,
                workspace_bytes: None,
                overhead_bytes: None,
                total_bytes: None,
                per_pool: vec![],
                confidence: larm_domain::estimate::Confidence::Unknown,
                evidence_refs: vec![],
                multimodal_bytes: None,
                temporary_bytes: None,
                safety_margin_bytes: None,
            },
            artifact_ids: vec![],
            expires_at: timestamp(chrono::Utc::now() + chrono::Duration::minutes(5)),
            effective_parameters: parameters.clone(),
            environment: parameters.environment.clone(),
            effective_memory_limit_bytes: None,
        };
        let mut unsigned = serde_json::to_value(&plan).unwrap();
        unsigned.as_object_mut().unwrap().remove("plan_hash");
        plan.plan_hash = larm_domain::canonical::sha256_hex(&unsigned).unwrap();
        plan
    }

    fn handle() -> larm_domain::instances::InstanceHandle {
        let pid = std::process::id();
        let ids = [Pid::from_u32(pid)];
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let process = system.process(ids[0]).unwrap();
        let started = chrono::DateTime::<chrono::Utc>::from_timestamp(
            i64::try_from(process.start_time()).unwrap(),
            0,
        )
        .unwrap();
        larm_domain::instances::InstanceHandle {
            instance_id: uuid::Uuid::now_v7().to_string(),
            generation: 0,
            pid: Some(pid),
            process_started_at: Some(timestamp(started)),
        }
    }

    fn session(port: u32) -> (FixtureSession, Arc<Mutex<SessionFlags>>) {
        let flags = Arc::new(Mutex::new(SessionFlags::default()));
        (
            FixtureSession {
                binding: RuntimeSessionBinding {
                    runtime_id: RUNTIME_ID.into(),
                    installation_sha256: "a".repeat(64),
                    run_sha256: "b".repeat(64),
                    actor_id: "operator".into(),
                },
                plan: plan(port),
                handle: handle(),
                flags: flags.clone(),
            },
            flags,
        )
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn free_port() -> u16 {
        for port in 25000..=29999 {
            if let Ok(listener) =
                TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            {
                drop(listener);
                return port;
            }
        }
        panic!("fixture encontró todos los puertos ocupados");
    }

    #[tokio::test]
    async fn launch_plan_crea_instancia_durable_y_retiene_reserva_hasta_cerrar_sesion() {
        let port = free_port();
        let instances = Arc::new(MemoryInstances::default());
        let sessions = RuntimeSessionRegistry::default();
        let allocator = PortAllocator::new(25000, 29999, IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        let service = RuntimeInstanceStartService::new(
            instances.clone(),
            allocator.clone(),
            sessions.clone(),
        );
        let (session, flags) = session(u32::from(port));
        let started = service
            .start(
                RUNTIME_ID,
                PROFILE_ID,
                request(),
                RuntimeOperationAccess::default(),
                Box::new(session),
                &context(),
            )
            .await
            .unwrap();

        assert_eq!(started.state, InstanceState::Ready);
        assert_eq!(started.row_version, 4);
        assert_eq!(started.pid, Some(std::process::id()));
        assert_eq!(started.backend_id, BACKEND_ID);
        assert!(started.launch_snapshot().unwrap().is_some());
        assert_eq!(
            instances.instance(&started.id).await.unwrap(),
            Some(started)
        );
        assert!(allocator.reserve_specific(port).is_err());

        sessions.close_all().await.unwrap();
        assert!(flags.lock().closed);
        assert_eq!(allocator.reserve_specific(port).unwrap().port(), port);
    }

    #[tokio::test]
    async fn puerto_ocupado_rechaza_el_arranque_antes_de_runtime_start() {
        let port = free_port();
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap();
        let instances = Arc::new(MemoryInstances::default());
        let sessions = RuntimeSessionRegistry::default();
        let allocator = PortAllocator::new(port, port, IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        let service = RuntimeInstanceStartService::new(instances.clone(), allocator, sessions);
        let (session, flags) = session(u32::from(port));
        let error = service
            .start(
                RUNTIME_ID,
                PROFILE_ID,
                request(),
                RuntimeOperationAccess::default(),
                Box::new(session),
                &context(),
            )
            .await
            .unwrap_err();

        assert_eq!(error.details.reason, "runtime_port_unavailable");
        assert!(!flags.lock().started);
        assert!(flags.lock().closed);
        assert!(instances.0.lock().await.is_empty());
        drop(listener);
    }
}
