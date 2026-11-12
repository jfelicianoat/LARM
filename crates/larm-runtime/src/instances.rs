//! Orquestación pura del estado de salud de instancias (P033).
//!
//! La persistencia de cada cambio corresponde a `RuntimeInstanceRepository`;
//! este módulo calcula la transición siguiente sin confundir un health check
//! con evidencia de salida del proceso.

use larm_domain::{
    error::{ErrorCode, LarmError},
    instances::{HealthResult, InstanceRef, InstanceState, RuntimeHealthState, RuntimeInstance},
    ports::MutationContext,
    runtime_execution::{RuntimeDiscoverySession, RuntimeOperationAccess},
    runtime_ports::{RuntimeInstanceHealthRepository, RuntimeInstanceRepository},
};
use std::{sync::Arc, time::Duration};

pub use larm_domain::instances::HealthSnapshot;

/// Contadores que el consumidor puede guardar en metadata para conservar la
/// política de salud al reiniciar el daemon.
/// Umbrales contractuales: DEGRADED tras dos fallos y FAILED tras cinco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    pub degraded_after: u8,
    pub failed_after: u8,
}

/// Resultado de una consulta del worker y de la actualización durable asociada.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthObservation {
    pub instance: RuntimeInstance,
    pub result: HealthResult,
}

/// Aplica respuestas de una sesión física cualificada al estado durable.
pub struct RuntimeInstanceHealthService {
    instances: Arc<dyn RuntimeInstanceRepository>,
    health: Arc<dyn RuntimeInstanceHealthRepository>,
}

impl RuntimeInstanceHealthService {
    #[must_use]
    pub fn new(
        instances: Arc<dyn RuntimeInstanceRepository>,
        health: Arc<dyn RuntimeInstanceHealthRepository>,
    ) -> Self {
        Self { instances, health }
    }

    pub async fn observe(
        &self,
        session: &dyn RuntimeDiscoverySession,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
        instance_id: &str,
        ctx: &MutationContext,
    ) -> Result<HealthObservation, LarmError> {
        if timeout.is_some_and(|value| value.is_zero() || value > Duration::from_secs(600)) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Tiempo de espera de salud no válido",
                "runtime_health_timeout_invalid",
            ));
        }
        let current = self.instances.instance(instance_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Instancia de runtime desconocida",
                "runtime_instance_not_found",
            )
            .with_resource(instance_id.to_owned())
        })?;
        if !matches!(
            current.state,
            InstanceState::Ready | InstanceState::Busy | InstanceState::Degraded
        ) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instancia no admite health checks operativos",
                "runtime_health_state_invalid",
            )
            .with_resource(current.id));
        }
        let request = InstanceRef {
            instance_id: current.id.clone(),
            generation: current.generation,
        };
        request.validate()?;
        let result = session.health(&request, access, timeout).await?;
        result.validate()?;

        let policy = HealthPolicy::default();
        let mut monitor = match current.health_snapshot()? {
            Some(snapshot) => InstanceHealthMonitor::from_snapshot(policy, snapshot)?,
            None => InstanceHealthMonitor::new(policy)?,
        };
        let next_state =
            monitor.observe(current.state, result.state == RuntimeHealthState::Ready)?;
        let next_version = current.row_version.checked_add(1).ok_or_else(|| {
            LarmError::new(
                ErrorCode::ValidationFailed,
                "Revisión de instancia no válida",
                "runtime_instance_revision_invalid",
            )
        })?;
        let mut proposed = current.clone();
        proposed.row_version = next_version;
        proposed.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        if let Some(state) = next_state {
            proposed.state = state;
        }
        proposed.store_health_snapshot(monitor.snapshot())?;
        let persisted = self
            .health
            .record_health_observation(&proposed, current.row_version, ctx)
            .await?;
        Ok(HealthObservation {
            instance: persisted,
            result,
        })
    }
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            degraded_after: 2,
            failed_after: 5,
        }
    }
}

impl HealthPolicy {
    pub fn validate(self) -> Result<Self, LarmError> {
        if self.degraded_after == 0 || self.failed_after <= self.degraded_after {
            return Err(LarmError::new(
                "VALIDATION_FAILED",
                "Política de salud no válida",
                "runtime_health_policy_invalid",
            ));
        }
        Ok(self)
    }
}

/// Calcula como máximo una transición persistible por resultado de health.
/// El llamador conserva el snapshot y escribe estado/evento con CAS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceHealthMonitor {
    policy: HealthPolicy,
    snapshot: HealthSnapshot,
}

impl InstanceHealthMonitor {
    pub fn new(policy: HealthPolicy) -> Result<Self, LarmError> {
        Ok(Self {
            policy: policy.validate()?,
            snapshot: HealthSnapshot::default(),
        })
    }

    pub fn from_snapshot(
        policy: HealthPolicy,
        snapshot: HealthSnapshot,
    ) -> Result<Self, LarmError> {
        let policy = policy.validate()?;
        if snapshot.consecutive_failures >= policy.failed_after
            || snapshot
                .return_state
                .is_some_and(|state| !matches!(state, InstanceState::Ready | InstanceState::Busy))
        {
            return Err(LarmError::new(
                "VALIDATION_FAILED",
                "Snapshot de salud no válido",
                "runtime_health_snapshot_invalid",
            ));
        }
        Ok(Self { policy, snapshot })
    }

    #[must_use]
    pub fn snapshot(&self) -> &HealthSnapshot {
        &self.snapshot
    }

    /// Health success recovers DEGRADED to its previous READY/BUSY state.
    /// Five consecutive failures move DEGRADED to FAILED.
    pub fn observe(
        &mut self,
        current: InstanceState,
        healthy: bool,
    ) -> Result<Option<InstanceState>, LarmError> {
        if !matches!(
            current,
            InstanceState::Ready | InstanceState::Busy | InstanceState::Degraded
        ) {
            return Err(LarmError::new(
                "INVALID_STATE_TRANSITION",
                "La instancia no admite health checks operativos",
                "runtime_health_state_invalid",
            ));
        }

        if healthy {
            self.snapshot.consecutive_failures = 0;
            if current == InstanceState::Degraded {
                let restore = self
                    .snapshot
                    .return_state
                    .take()
                    .unwrap_or(InstanceState::Ready);
                return Ok(Some(restore));
            }
            self.snapshot.return_state = None;
            return Ok(None);
        }

        self.snapshot.consecutive_failures = self.snapshot.consecutive_failures.saturating_add(1);
        if current == InstanceState::Degraded {
            if self.snapshot.consecutive_failures >= self.policy.failed_after {
                self.snapshot.return_state = None;
                return Ok(Some(InstanceState::Failed));
            }
            return Ok(None);
        }

        if self.snapshot.consecutive_failures >= self.policy.degraded_after {
            self.snapshot.return_state = Some(current);
            return Ok(Some(InstanceState::Degraded));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn degrada_dos_fallos_recupera_estado_previo_y_falla_al_quinto() {
        let mut health = InstanceHealthMonitor::new(HealthPolicy::default()).unwrap();
        assert_eq!(health.observe(InstanceState::Ready, false).unwrap(), None);
        assert_eq!(
            health.observe(InstanceState::Ready, false).unwrap(),
            Some(InstanceState::Degraded)
        );
        assert_eq!(health.snapshot().return_state, Some(InstanceState::Ready));
        assert_eq!(
            health.observe(InstanceState::Degraded, false).unwrap(),
            None
        );
        assert_eq!(
            health.observe(InstanceState::Degraded, false).unwrap(),
            None
        );
        assert_eq!(
            health.observe(InstanceState::Degraded, false).unwrap(),
            Some(InstanceState::Failed)
        );
    }

    #[test]
    fn recovery_de_busy_no_se_convierte_en_ready() {
        let mut health = InstanceHealthMonitor::new(HealthPolicy::default()).unwrap();
        health.observe(InstanceState::Busy, false).unwrap();
        assert_eq!(
            health.observe(InstanceState::Busy, false).unwrap(),
            Some(InstanceState::Degraded)
        );
        assert_eq!(
            health.observe(InstanceState::Degraded, true).unwrap(),
            Some(InstanceState::Busy)
        );
        assert_eq!(health.snapshot().consecutive_failures, 0);
    }

    #[test]
    fn snapshot_serializa_y_rechaza_estado_o_umbral_imposible() {
        let snapshot = HealthSnapshot {
            consecutive_failures: 3,
            return_state: Some(InstanceState::Busy),
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        let restored: HealthSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(restored, snapshot);
        assert!(InstanceHealthMonitor::from_snapshot(
            HealthPolicy::default(),
            HealthSnapshot {
                consecutive_failures: 5,
                return_state: None,
            }
        )
        .is_err());
        assert!(HealthPolicy {
            degraded_after: 2,
            failed_after: 2,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn health_no_cambia_estados_de_inicio_ni_terminales() {
        let mut health = InstanceHealthMonitor::new(HealthPolicy::default()).unwrap();
        assert_eq!(
            health
                .observe(InstanceState::Starting, false)
                .unwrap_err()
                .details
                .reason,
            "runtime_health_state_invalid"
        );
        assert_eq!(health.snapshot().consecutive_failures, 0);
    }
}

#[cfg(test)]
mod service_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use larm_domain::{
        error::ErrorCode,
        ports::AdapterQualificationGuard,
        runtime::RuntimeOwnership,
        runtime_execution::{RuntimeSessionBinding, RuntimeSessionGuard},
        runtime_install::{RuntimeVersionPage, RuntimeVersionSource, VersionQuery},
    };
    use serde_json::Map;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryInstance(Mutex<Option<RuntimeInstance>>);

    fn unsupported() -> LarmError {
        LarmError::new(
            ErrorCode::UnsupportedCapability,
            "Operación no disponible en el fixture",
            "fixture_operation_unavailable",
        )
    }

    #[async_trait]
    impl RuntimeInstanceRepository for MemoryInstance {
        async fn create_instance(
            &self,
            instance: &RuntimeInstance,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            *self.0.lock().unwrap() = Some(instance.clone());
            Ok(())
        }

        async fn instance(&self, id: &str) -> Result<Option<RuntimeInstance>, LarmError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .filter(|instance| instance.id == id)
                .cloned())
        }

        async fn instances(
            &self,
            _after: Option<(String, String)>,
            _limit: u32,
            _runtime_id: Option<&str>,
        ) -> Result<Vec<RuntimeInstance>, LarmError> {
            Ok(self.0.lock().unwrap().clone().into_iter().collect())
        }

        async fn runtime_exists(&self, _id: &str) -> Result<bool, LarmError> {
            Ok(true)
        }

        async fn transition_instance(
            &self,
            _instance: &RuntimeInstance,
            _expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            Err(unsupported())
        }

        async fn begin_stop_instance(
            &self,
            _instance: &RuntimeInstance,
            _expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            Err(unsupported())
        }
    }

    #[async_trait]
    impl RuntimeInstanceHealthRepository for MemoryInstance {
        async fn record_health_observation(
            &self,
            instance: &RuntimeInstance,
            expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            let mut row = self.0.lock().unwrap();
            let current = row.as_ref().ok_or_else(unsupported)?;
            if current.row_version != expected_row_version {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "Revisión obsoleta",
                    "runtime_instance_stale",
                ));
            }
            *row = Some(instance.clone());
            Ok(instance.clone())
        }
    }

    struct QualifiedHealthSession {
        binding: RuntimeSessionBinding,
        observed: Mutex<Option<InstanceRef>>,
    }

    #[async_trait]
    impl AdapterQualificationGuard for QualifiedHealthSession {
        async fn recheck(&self) -> Result<(), LarmError> {
            Ok(())
        }
    }

    #[async_trait]
    impl RuntimeVersionSource for QualifiedHealthSession {
        async fn list_versions(
            &self,
            _query: &VersionQuery,
        ) -> Result<RuntimeVersionPage, LarmError> {
            Err(unsupported())
        }
    }

    #[async_trait]
    impl RuntimeSessionGuard for QualifiedHealthSession {
        fn binding(&self) -> &RuntimeSessionBinding {
            &self.binding
        }
    }

    #[async_trait]
    impl RuntimeDiscoverySession for QualifiedHealthSession {
        async fn health(
            &self,
            request: &InstanceRef,
            _access: &RuntimeOperationAccess,
            _timeout: Option<Duration>,
        ) -> Result<HealthResult, LarmError> {
            *self.observed.lock().unwrap() = Some(request.clone());
            Ok(HealthResult {
                state: RuntimeHealthState::Ready,
                backend_effective: None,
                reason: "fixture healthy".into(),
                evidence_refs: vec!["fixture:health".into()],
            })
        }

        async fn close(&self) -> Result<(), LarmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn health_service_usa_la_sesion_calificada_y_persiste_el_snapshot() {
        let instance = RuntimeInstance {
            id: "0192aaaa-0000-7000-8000-000000000001".into(),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            variant_id: "0192aaaa-0000-7000-8000-000000000003".into(),
            backend_id: "0192aaaa-0000-7000-8000-000000000004".into(),
            launch_profile_id: "0192aaaa-0000-7000-8000-000000000005".into(),
            state: InstanceState::Ready,
            pid: None,
            process_started_at: None,
            executable_hash: None,
            generation: 3,
            execution_fingerprint: None,
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        };
        let repository = Arc::new(MemoryInstance(Mutex::new(Some(instance.clone()))));
        let service = RuntimeInstanceHealthService::new(repository.clone(), repository.clone());
        let session = QualifiedHealthSession {
            binding: RuntimeSessionBinding {
                runtime_id: "0192aaaa-0000-7000-8000-000000000010".into(),
                installation_sha256: "a".repeat(64),
                run_sha256: "b".repeat(64),
                actor_id: "fixture".into(),
            },
            observed: Mutex::new(None),
        };
        let outcome = service
            .observe(
                &session,
                &RuntimeOperationAccess::default(),
                Some(Duration::from_secs(10)),
                &instance.id,
                &MutationContext {
                    actor_id: "fixture".into(),
                    correlation_id: "0192aaaa-0000-7000-8000-000000000011".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.instance.row_version, 2);
        assert_eq!(outcome.instance.state, InstanceState::Ready);
        assert_eq!(
            outcome.instance.health_snapshot().unwrap().unwrap(),
            HealthSnapshot::default()
        );
        assert_eq!(
            *session.observed.lock().unwrap(),
            Some(InstanceRef {
                instance_id: instance.id,
                generation: 3,
            })
        );
    }
}
