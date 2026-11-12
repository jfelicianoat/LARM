//! Modelo contractual de RuntimeInstance y su máquina de estados (P033).
//!
//! Este módulo valida únicamente identidad y transiciones. La autorización de
//! stop, las leases, el proceso y la persistencia pertenecen al supervisor y
//! al repositorio; una transición válida por sí sola no concede autoridad.

use crate::{
    bridge::{is_uuid_v7, JobPhase, JobReceipt},
    error::{ErrorCode, LarmError},
    estimate::RuntimeMemoryEstimate,
    launch::{EnvironmentConfig, LaunchParameters},
    runtime::RuntimeOwnership,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const RUNTIME_LAUNCH_SNAPSHOT_KEY: &str = "larm_runtime_launch_snapshot_v1";
pub const RUNTIME_HEALTH_SNAPSHOT_KEY: &str = "health_snapshot";

/// Estado mínimo que conserva los umbrales del monitor entre reinicios.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSnapshot {
    pub consecutive_failures: u8,
    pub return_state: Option<InstanceState>,
}

impl HealthSnapshot {
    pub fn validate(&self) -> Result<(), LarmError> {
        let valid_return_state = self
            .return_state
            .is_none_or(|state| matches!(state, InstanceState::Ready | InstanceState::Busy));
        let valid_counter = matches!(
            (self.consecutive_failures, self.return_state),
            (0..=1, None) | (5, None) | (2..=4, Some(_))
        );
        if valid_return_state && valid_counter {
            Ok(())
        } else {
            Err(invalid_request("runtime_health_snapshot_invalid"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstanceState {
    Created,
    Starting,
    Loading,
    Ready,
    Busy,
    Degraded,
    Failed,
    Stopping,
    Stopped,
    Crashed,
}

impl InstanceState {
    /// Transiciones de `01_DOMAIN_STATES.md`; no infiere éxito desde un PID.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use InstanceState::*;
        matches!(
            (self, next),
            (Created, Starting)
                | (Starting, Loading | Failed | Crashed)
                | (Loading, Ready | Failed | Crashed)
                | (Ready, Busy | Degraded | Stopping | Crashed)
                | (Busy, Ready | Degraded | Stopping | Crashed)
                | (Degraded, Ready | Busy | Failed | Stopping | Crashed)
                | (Failed, Stopping)
                | (Crashed, Stopping)
                | (Stopping, Stopped)
        )
    }

    /// Una muerte inesperada de un proceso propio invalida cualquier estado
    /// operativo. Durante stop, la salida del proceso completa el drenaje.
    pub fn on_process_exit(self, shutdown_requested: bool) -> Result<Self, LarmError> {
        if shutdown_requested && self == Self::Stopping {
            return Ok(Self::Stopped);
        }
        if !shutdown_requested && self.can_transition_to(Self::Crashed) {
            return Ok(Self::Crashed);
        }
        Err(invalid_transition(
            self,
            if shutdown_requested {
                Self::Stopped
            } else {
                Self::Crashed
            },
        ))
    }

    /// Un timeout de arranque/carga falla la instancia; no se presenta como
    /// crash porque no hay evidencia de salida inesperada.
    pub fn on_start_timeout(self) -> Result<Self, LarmError> {
        if matches!(self, Self::Starting | Self::Loading) {
            return Ok(Self::Failed);
        }
        Err(invalid_transition(self, Self::Failed))
    }
}

/// Forma exacta de `domain.schema.json#/$defs/runtime_instance`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstance {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub runtime_installation_id: String,
    pub variant_id: String,
    pub backend_id: String,
    pub launch_profile_id: String,
    pub state: InstanceState,
    #[serde(deserialize_with = "Option::deserialize")]
    pub pid: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub process_started_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub executable_hash: Option<String>,
    pub generation: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub execution_fingerprint: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_used_at: Option<String>,
    pub ownership: RuntimeOwnership,
}

/// Solicitud del método `runtime.launch_plan` (P033, schema compartido).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub installation_id: String,
    pub variant_id: String,
    pub launch_profile: LaunchParameters,
    pub constraints: crate::bridge::Constraints,
    #[serde(deserialize_with = "Option::deserialize")]
    pub resource_requirements: Option<crate::bridge::ResourceRequirements>,
}

impl LaunchRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.installation_id) || !is_uuid_v7(&self.variant_id) {
            return Err(invalid_request("runtime_launch_request_invalid"));
        }
        self.launch_profile
            .validate()
            .map_err(|_| invalid_request("runtime_launch_profile_invalid"))?;
        for id in [
            self.constraints.runtime_version_id.as_deref(),
            self.constraints.backend_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !is_uuid_v7(id) {
                return Err(invalid_request("runtime_launch_constraints_invalid"));
            }
        }
        if self
            .constraints
            .execution_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| !sha256(fingerprint))
            || self
                .constraints
                .max_memory_bytes
                .as_deref()
                .is_some_and(|value| crate::units::DecimalU64::parse(value).is_err())
            || self
                .resource_requirements
                .as_ref()
                .is_some_and(|requirements| {
                    requirements.capability.is_empty()
                        || requirements.batch == 0
                        || requirements
                            .hard_memory_budget_bytes
                            .as_deref()
                            .is_some_and(|value| crate::units::DecimalU64::parse(value).is_err())
                })
        {
            return Err(invalid_request("runtime_launch_constraints_invalid"));
        }
        Ok(())
    }
}

/// Plan completo devuelto por `runtime.launch_plan`; el hash cubre todos sus
/// campos salvo `plan_hash`, conforme al contrato de ejecución.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlan {
    pub plan_id: String,
    pub plan_hash: String,
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env_allowlist: Vec<String>,
    pub port: u32,
    pub memory_estimate: RuntimeMemoryEstimate,
    pub artifact_ids: Vec<String>,
    pub expires_at: String,
    pub effective_parameters: LaunchParameters,
    pub environment: EnvironmentConfig,
    #[serde(deserialize_with = "Option::deserialize")]
    pub effective_memory_limit_bytes: Option<String>,
}

impl LaunchPlan {
    pub fn validate(&self, now: chrono::DateTime<chrono::Utc>) -> Result<(), LarmError> {
        self.validate_integrity()?;
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map_err(|_| invalid_request("runtime_launch_plan_expiry_invalid"))?
            .with_timezone(&chrono::Utc);
        if expires <= now {
            return Err(invalid_request("runtime_launch_plan_expiry_invalid"));
        }
        Ok(())
    }

    /// Valida la forma y el hash de un plan persistido sin exigir que su
    /// vencimiento siga en el futuro. El snapshot de una instancia es evidencia
    /// histórica después de que el plan de un solo uso haya caducado.
    pub fn validate_integrity(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.plan_id)
            || !sha256(&self.plan_hash)
            || self.executable.is_empty()
            || self.cwd.is_empty()
            || self.port > u32::from(u16::MAX)
            || self.memory_estimate.validate().is_err()
        {
            return Err(invalid_request("runtime_launch_plan_invalid"));
        }
        self.effective_parameters
            .validate()
            .map_err(|_| invalid_request("runtime_launch_plan_invalid"))?;
        self.environment
            .validate()
            .map_err(|_| invalid_request("runtime_launch_plan_environment_invalid"))?;
        if self.env_allowlist != self.effective_parameters.environment.allowlist
            || self.environment != self.effective_parameters.environment
        {
            return Err(invalid_request("runtime_launch_plan_environment_invalid"));
        }
        if self
            .effective_memory_limit_bytes
            .as_deref()
            .is_some_and(|value| crate::units::DecimalU64::parse(value).is_err())
        {
            return Err(invalid_request("runtime_launch_plan_memory_invalid"));
        }
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map_err(|_| invalid_request("runtime_launch_plan_expiry_invalid"))?
            .with_timezone(&chrono::Utc);
        if crate::bridge::timestamp(expires) != self.expires_at {
            return Err(invalid_request("runtime_launch_plan_expiry_invalid"));
        }
        let mut value = serde_json::to_value(self)
            .map_err(|_| invalid_request("runtime_launch_plan_invalid"))?;
        value
            .as_object_mut()
            .ok_or_else(|| invalid_request("runtime_launch_plan_invalid"))?
            .remove("plan_hash");
        if crate::canonical::sha256_hex(&value)
            .map_err(|_| invalid_request("runtime_launch_plan_invalid"))?
            != self.plan_hash
        {
            return Err(invalid_request("runtime_launch_plan_hash_mismatch"));
        }
        Ok(())
    }
}

/// Snapshot íntegro del plan efectivo retenido por una instancia. Contiene
/// referencias de credenciales, nunca sus valores resueltos por el worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLaunchSnapshot {
    pub launch_plan: LaunchPlan,
    pub snapshot_sha256: String,
}

impl RuntimeLaunchSnapshot {
    pub fn from_plan(
        launch_plan: LaunchPlan,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Self, LarmError> {
        launch_plan.validate(now)?;
        let snapshot_sha256 = launch_snapshot_hash(&launch_plan)?;
        Ok(Self {
            launch_plan,
            snapshot_sha256,
        })
    }

    pub fn validate(&self) -> Result<(), LarmError> {
        self.launch_plan.validate_integrity()?;
        if !sha256(&self.snapshot_sha256)
            || launch_snapshot_hash(&self.launch_plan)? != self.snapshot_sha256
        {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El snapshot de lanzamiento no conserva su hash",
                "runtime_launch_snapshot_hash_mismatch",
            ));
        }
        Ok(())
    }
}

fn launch_snapshot_hash(plan: &LaunchPlan) -> Result<String, LarmError> {
    let value = serde_json::to_value(plan).map_err(|_| {
        LarmError::new(
            ErrorCode::IntegrityFailed,
            "No se pudo serializar el snapshot de lanzamiento",
            "runtime_launch_snapshot_hash_failed",
        )
    })?;
    crate::canonical::sha256_hex(&value).map_err(|_| {
        LarmError::new(
            ErrorCode::IntegrityFailed,
            "No se pudo calcular el hash del snapshot de lanzamiento",
            "runtime_launch_snapshot_hash_failed",
        )
    })
}

/// Referencia estable de una instancia dentro de la sesión de adaptador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRef {
    pub instance_id: String,
    pub generation: u64,
}

impl InstanceRef {
    pub fn validate(&self) -> Result<(), LarmError> {
        if is_uuid_v7(&self.instance_id) && self.generation <= i64::MAX as u64 {
            Ok(())
        } else {
            Err(invalid_request("runtime_instance_ref_invalid"))
        }
    }
}

/// Identidad de proceso entregada por `runtime.start` o `runtime.load`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceHandle {
    pub instance_id: String,
    pub generation: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub pid: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub process_started_at: Option<String>,
}

impl InstanceHandle {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.instance_id)
            || self.generation > i64::MAX as u64
            || self.pid.is_some() != self.process_started_at.is_some()
            || self
                .process_started_at
                .as_deref()
                .is_some_and(|value| !valid_timestamp(value))
        {
            return Err(invalid_request("runtime_instance_handle_invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeHealthState {
    Ready,
    Degraded,
    Failed,
    Loading,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResult {
    pub state: RuntimeHealthState,
    pub backend_effective: Option<String>,
    pub reason: String,
    pub evidence_refs: Vec<String>,
}

impl HealthResult {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.reason.is_empty() || self.backend_effective.as_deref().is_some_and(str::is_empty) {
            return Err(invalid_request("runtime_health_reply_invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopResult {
    pub stopped: bool,
}

/// Solicitud exacta de `POST /api/v1/instances/{id}/stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {
    pub reason: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
}

impl StopRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.reason.is_empty()
            || self
                .approval_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
        {
            return Err(invalid_request("runtime_instance_stop_request_invalid"));
        }
        Ok(())
    }
}

/// Fila durable del job de stop en `operation_journal`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstanceStopJob {
    pub job_id: String,
    pub actor_id: String,
    pub correlation_id: String,
    pub instance_id: String,
    pub request: StopRequest,
    pub state: JobPhase,
    #[serde(deserialize_with = "Option::deserialize")]
    pub result: Option<StopResult>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub error: Option<crate::bridge::ApiError>,
    pub evidence_refs: Vec<String>,
}

impl RuntimeInstanceStopJob {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.job_id)
            || !is_uuid_v7(&self.actor_id)
            || !is_uuid_v7(&self.correlation_id)
            || !is_uuid_v7(&self.instance_id)
        {
            return Err(invalid_request("runtime_instance_stop_job_invalid"));
        }
        self.request.validate()?;
        Ok(())
    }

    #[must_use]
    pub fn receipt(&self) -> JobReceipt {
        JobReceipt {
            job_id: self.job_id.clone(),
            status_url: format!("/api/v1/jobs/{}", self.job_id),
            state: self.state,
        }
    }

    #[must_use]
    pub fn job_state(&self) -> crate::bridge::JobState {
        crate::bridge::JobState {
            job_id: self.job_id.clone(),
            state: self.state,
            resource_type: "runtime_instance".into(),
            resource_id: Some(self.instance_id.clone()),
            progress: Map::new(),
            result: self
                .result
                .as_ref()
                .and_then(|result| serde_json::to_value(result).ok()),
            error: self.error.clone(),
            evidence_refs: self.evidence_refs.clone(),
        }
    }
}

fn invalid_request(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Solicitud o respuesta de runtime no válida",
        reason,
    )
}

impl RuntimeInstance {
    pub fn validate_record(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.id)
            || !is_uuid_v7(&self.runtime_installation_id)
            || !is_uuid_v7(&self.variant_id)
            || !is_uuid_v7(&self.backend_id)
            || !is_uuid_v7(&self.launch_profile_id)
            || self.row_version < 1
            || self.generation > i64::MAX as u64
            || !valid_timestamp(&self.created_at)
            || !valid_timestamp(&self.updated_at)
            || self
                .process_started_at
                .as_deref()
                .is_some_and(|value| !valid_timestamp(value))
            || self
                .last_used_at
                .as_deref()
                .is_some_and(|value| !valid_timestamp(value))
            || self
                .executable_hash
                .as_deref()
                .is_some_and(|value| !sha256(value))
            || self
                .execution_fingerprint
                .as_deref()
                .is_some_and(|value| !sha256(value))
        {
            return Err(invalid_record(&self.id));
        }
        self.launch_snapshot()?;
        self.health_snapshot()?;
        Ok(())
    }

    /// Devuelve el snapshot efectivo versionado, si la instancia lo conserva.
    pub fn launch_snapshot(&self) -> Result<Option<RuntimeLaunchSnapshot>, LarmError> {
        let Some(value) = self.metadata.get(RUNTIME_LAUNCH_SNAPSHOT_KEY) else {
            return Ok(None);
        };
        let snapshot: RuntimeLaunchSnapshot =
            serde_json::from_value(value.clone()).map_err(|_| {
                LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El snapshot persistido de lanzamiento no es válido",
                    "runtime_launch_snapshot_invalid",
                )
                .with_resource(&self.id)
            })?;
        snapshot
            .validate()
            .map_err(|error| error.with_resource(self.id.clone()))?;
        Ok(Some(snapshot))
    }

    /// Guarda el snapshot validado sin sustituir metadata de salud u otras
    /// claves que el supervisor ya haya persistido.
    pub fn store_launch_snapshot(
        &mut self,
        snapshot: &RuntimeLaunchSnapshot,
    ) -> Result<(), LarmError> {
        snapshot.validate()?;
        self.metadata.insert(
            RUNTIME_LAUNCH_SNAPSHOT_KEY.into(),
            serde_json::to_value(snapshot).map_err(|_| {
                LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El snapshot de lanzamiento no se pudo serializar",
                    "runtime_launch_snapshot_invalid",
                )
            })?,
        );
        Ok(())
    }

    /// Recupera el contador persistido que usa el monitor al reanudarse.
    pub fn health_snapshot(&self) -> Result<Option<HealthSnapshot>, LarmError> {
        let Some(value) = self.metadata.get(RUNTIME_HEALTH_SNAPSHOT_KEY) else {
            return Ok(None);
        };
        let snapshot: HealthSnapshot = serde_json::from_value(value.clone()).map_err(|_| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "El snapshot persistido de salud no es válido",
                "runtime_health_snapshot_invalid",
            )
            .with_resource(&self.id)
        })?;
        snapshot
            .validate()
            .map_err(|error| error.with_resource(self.id.clone()))?;
        Ok(Some(snapshot))
    }

    /// Guarda únicamente el estado versionado del monitor de salud.
    pub fn store_health_snapshot(&mut self, snapshot: &HealthSnapshot) -> Result<(), LarmError> {
        snapshot.validate()?;
        self.metadata.insert(
            RUNTIME_HEALTH_SNAPSHOT_KEY.into(),
            serde_json::to_value(snapshot).map_err(|_| {
                LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El snapshot de salud no se pudo serializar",
                    "runtime_health_snapshot_invalid",
                )
            })?,
        );
        Ok(())
    }

    pub fn transition_to(&self, next: InstanceState) -> Result<Self, LarmError> {
        self.validate_record()?;
        if !self.state.can_transition_to(next) {
            return Err(invalid_transition(self.state, next).with_resource(self.id.clone()));
        }
        let row_version = self
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid_record(&self.id))?;
        let mut updated = self.clone();
        updated.state = next;
        updated.updated_at = crate::bridge::timestamp(chrono::Utc::now());
        updated.row_version = row_version;
        Ok(updated)
    }
}

fn valid_timestamp(value: &str) -> bool {
    value.len() == 24
        && value.ends_with('Z')
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value.as_bytes().get(19) == Some(&b'.')
        && value
            .bytes()
            .enumerate()
            .all(|(i, byte)| [4, 7, 10, 13, 16, 19, 23].contains(&i) || byte.is_ascii_digit())
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn invalid_record(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "RuntimeInstance no válido",
        "runtime_instance_record_invalid",
    )
    .with_resource(id.to_owned())
}

fn invalid_transition(from: InstanceState, to: InstanceState) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "Transición de RuntimeInstance no permitida",
        "runtime_instance_transition_invalid",
    )
    .with_original(
        "larm-domain",
        "state_transition",
        &format!("{from:?}->{to:?}"),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    const ID: &str = "0192aaaa-0000-7000-8000-000000000001";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const STAMP: &str = "2026-10-21T22:00:00.000Z";

    fn record() -> RuntimeInstance {
        serde_json::from_value(json!({
            "id": ID,
            "created_at": STAMP,
            "updated_at": STAMP,
            "row_version": 1,
            "metadata": {},
            "runtime_installation_id": "0192aaaa-0000-7000-8000-000000000002",
            "variant_id": "0192aaaa-0000-7000-8000-000000000003",
            "backend_id": "0192aaaa-0000-7000-8000-000000000004",
            "launch_profile_id": "0192aaaa-0000-7000-8000-000000000005",
            "state": "CREATED",
            "pid": null,
            "process_started_at": null,
            "executable_hash": null,
            "generation": 0,
            "execution_fingerprint": null,
            "last_used_at": null,
            "ownership": "LARM_MANAGED"
        }))
        .unwrap()
    }

    #[test]
    fn transiciones_contractuales_y_terminales() {
        let allowed = [
            (InstanceState::Created, InstanceState::Starting),
            (InstanceState::Starting, InstanceState::Loading),
            (InstanceState::Loading, InstanceState::Ready),
            (InstanceState::Ready, InstanceState::Busy),
            (InstanceState::Busy, InstanceState::Ready),
            (InstanceState::Ready, InstanceState::Degraded),
            (InstanceState::Degraded, InstanceState::Busy),
            (InstanceState::Failed, InstanceState::Stopping),
            (InstanceState::Crashed, InstanceState::Stopping),
            (InstanceState::Stopping, InstanceState::Stopped),
        ];
        for (from, to) in allowed {
            assert!(from.can_transition_to(to), "{from:?}->{to:?}");
        }
        for (from, to) in [
            (InstanceState::Created, InstanceState::Ready),
            (InstanceState::Created, InstanceState::Crashed),
            (InstanceState::Ready, InstanceState::Stopped),
            (InstanceState::Stopped, InstanceState::Starting),
        ] {
            assert!(!from.can_transition_to(to), "{from:?}->{to:?}");
        }
    }

    #[test]
    fn proceso_timeout_crash_y_shutdown_se_distinguen() {
        assert_eq!(
            InstanceState::Starting.on_start_timeout().unwrap(),
            InstanceState::Failed
        );
        assert_eq!(
            InstanceState::Ready.on_process_exit(false).unwrap(),
            InstanceState::Crashed
        );
        assert_eq!(
            InstanceState::Stopping.on_process_exit(true).unwrap(),
            InstanceState::Stopped
        );
        assert!(InstanceState::Ready.on_start_timeout().is_err());
    }

    #[test]
    fn record_roundtrips_exact_schema_and_rejects_unknown_fields() {
        let value = record();
        value.validate_record().unwrap();
        let serialized = serde_json::to_value(&value).unwrap();
        assert_eq!(serialized["state"], "CREATED");
        assert!(serde_json::from_value::<RuntimeInstance>({
            let mut invalid = serialized.clone();
            invalid["surprise"] = json!(true);
            invalid
        })
        .is_err());
        let mut missing_nullable = serialized;
        missing_nullable.as_object_mut().unwrap().remove("pid");
        assert!(serde_json::from_value::<RuntimeInstance>(missing_nullable).is_err());
    }

    #[test]
    fn transicion_incrementa_revision_y_rechaza_salto() {
        let starting = record().transition_to(InstanceState::Starting).unwrap();
        assert_eq!(starting.state, InstanceState::Starting);
        assert_eq!(starting.row_version, 2);
        assert_ne!(starting.updated_at, "");
        assert_eq!(
            record()
                .transition_to(InstanceState::Ready)
                .unwrap_err()
                .code,
            ErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn transicion_rechaza_un_registro_invalido() {
        let mut invalid = record();
        invalid.row_version = 0;
        assert_eq!(
            invalid
                .transition_to(InstanceState::Starting)
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );
    }

    #[test]
    fn record_rechaza_hashes_mayusculas_e_ids_invalidos() {
        let mut invalid = record();
        invalid.executable_hash = Some(HASH.to_ascii_uppercase());
        assert_eq!(
            invalid.validate_record().unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        let mut invalid = record();
        invalid.id = "not-a-uuid".into();
        assert!(invalid.validate_record().is_err());
    }

    fn launch_request() -> LaunchRequest {
        LaunchRequest {
            installation_id: "0192aaaa-0000-7000-8000-000000000010".into(),
            variant_id: "0192aaaa-0000-7000-8000-000000000011".into(),
            launch_profile: LaunchParameters::builtin(
                "0192aaaa-0000-7000-8000-000000000004",
                4,
                None,
                128,
            ),
            constraints: crate::bridge::Constraints {
                runtime_version_id: None,
                backend_id: None,
                execution_fingerprint: None,
                allow_backend_fallback: false,
                max_memory_bytes: None,
            },
            resource_requirements: None,
        }
    }

    fn launch_plan(now: chrono::DateTime<chrono::Utc>) -> LaunchPlan {
        let mut plan = LaunchPlan {
            plan_id: "0192aaaa-0000-7000-8000-000000000012".into(),
            plan_hash: "a".repeat(64),
            executable: "runtime.exe".into(),
            args: vec!["--port".into(), "24000".into()],
            cwd: ".".into(),
            env_allowlist: vec![],
            port: 24000,
            memory_estimate: RuntimeMemoryEstimate {
                weights_bytes: None,
                kv_bytes: None,
                activation_bytes: None,
                workspace_bytes: None,
                overhead_bytes: None,
                total_bytes: None,
                per_pool: vec![],
                confidence: crate::estimate::Confidence::Unknown,
                evidence_refs: vec![],
                multimodal_bytes: None,
                temporary_bytes: None,
                safety_margin_bytes: None,
            },
            artifact_ids: vec![],
            expires_at: crate::bridge::timestamp(now + chrono::Duration::minutes(5)),
            effective_parameters: launch_request().launch_profile,
            environment: EnvironmentConfig::default(),
            effective_memory_limit_bytes: None,
        };
        let mut value = serde_json::to_value(&plan).unwrap();
        value.as_object_mut().unwrap().remove("plan_hash");
        plan.plan_hash = crate::canonical::sha256_hex(&value).unwrap();
        plan
    }

    fn refresh_plan_hash(plan: &mut LaunchPlan) {
        let mut value = serde_json::to_value(&*plan).unwrap();
        value.as_object_mut().unwrap().remove("plan_hash");
        plan.plan_hash = crate::canonical::sha256_hex(&value).unwrap();
    }

    #[test]
    fn launch_request_valida_ids_perfil_y_restricciones_antes_del_io() {
        launch_request().validate().unwrap();

        let mut missing_nullable = serde_json::to_value(launch_request()).unwrap();
        missing_nullable
            .as_object_mut()
            .unwrap()
            .remove("resource_requirements");
        assert!(serde_json::from_value::<LaunchRequest>(missing_nullable).is_err());

        let mut missing_constraint = serde_json::to_value(launch_request()).unwrap();
        missing_constraint["constraints"]
            .as_object_mut()
            .unwrap()
            .remove("runtime_version_id");
        assert!(serde_json::from_value::<LaunchRequest>(missing_constraint).is_err());

        let mut invalid = launch_request();
        invalid.installation_id = "runtime-path".into();
        assert_eq!(
            invalid.validate().unwrap_err().details.reason,
            "runtime_launch_request_invalid"
        );

        let mut invalid = launch_request();
        invalid.constraints.max_memory_bytes = Some("invalid".into());
        assert_eq!(
            invalid.validate().unwrap_err().details.reason,
            "runtime_launch_constraints_invalid"
        );
        let mut zero_budget = launch_request();
        zero_budget.constraints.max_memory_bytes = Some("0".into());
        zero_budget.validate().unwrap();

        let mut invalid = launch_request();
        invalid.resource_requirements = Some(crate::bridge::ResourceRequirements {
            capability: String::new(),
            context_tokens: None,
            max_output_tokens: None,
            batch: 1,
            width: None,
            height: None,
            frames: None,
            sample_rate: None,
            audio_duration_ms: None,
            hard_memory_budget_bytes: None,
        });
        assert_eq!(
            invalid.validate().unwrap_err().details.reason,
            "runtime_launch_constraints_invalid"
        );
    }

    #[test]
    fn launch_plan_exige_hash_entorno_y_expiracion_vigentes() {
        let now = chrono::Utc::now();
        let mut plan = launch_plan(now);
        plan.validate(now).unwrap();

        plan.args.push("--unsafe".into());
        assert_eq!(
            plan.validate(now).unwrap_err().details.reason,
            "runtime_launch_plan_hash_mismatch"
        );

        let expired = launch_plan(now - chrono::Duration::minutes(10));
        assert_eq!(
            expired.validate(now).unwrap_err().details.reason,
            "runtime_launch_plan_expiry_invalid"
        );
        expired.validate_integrity().unwrap();

        let mut historical = RuntimeLaunchSnapshot::from_plan(launch_plan(now), now).unwrap();
        historical.launch_plan.expires_at =
            crate::bridge::timestamp(now - chrono::Duration::minutes(10));
        refresh_plan_hash(&mut historical.launch_plan);
        historical.snapshot_sha256 = launch_snapshot_hash(&historical.launch_plan).unwrap();
        historical.validate().unwrap();
    }

    #[test]
    fn snapshot_de_plan_efectivo_persiste_argumentos_entorno_limites_y_hash() {
        let now = chrono::Utc::now();
        let mut plan = launch_plan(now);
        plan.args.extend(["--threads".into(), "4".into()]);
        plan.effective_parameters.runtime_arguments = vec!["--threads".into(), "4".into()];
        let environment = crate::launch::EnvironmentConfig {
            allowlist: vec!["LARM_MODE".into(), "SERVICE_TOKEN".into()],
            variables: vec![
                crate::launch::EnvironmentBinding::Value {
                    name: "LARM_MODE".into(),
                    value: "production".into(),
                },
                crate::launch::EnvironmentBinding::Credential {
                    name: "SERVICE_TOKEN".into(),
                    credential_reference: "larm/runtime/service".into(),
                },
            ],
        };
        plan.environment = environment.clone();
        plan.effective_parameters.environment = environment.clone();
        plan.env_allowlist = environment.allowlist.clone();
        plan.effective_memory_limit_bytes = Some("4096".into());
        refresh_plan_hash(&mut plan);

        let snapshot = RuntimeLaunchSnapshot::from_plan(plan, now).unwrap();
        snapshot.validate().unwrap();
        let mut instance = record();
        instance
            .store_health_snapshot(&HealthSnapshot {
                consecutive_failures: 1,
                return_state: None,
            })
            .unwrap();
        instance.store_launch_snapshot(&snapshot).unwrap();
        instance.validate_record().unwrap();

        let restored = instance.launch_snapshot().unwrap().unwrap();
        assert_eq!(restored, snapshot);
        assert_eq!(
            restored.launch_plan.effective_memory_limit_bytes.as_deref(),
            Some("4096")
        );
        assert_eq!(
            restored.launch_plan.environment.variables[1],
            crate::launch::EnvironmentBinding::Credential {
                name: "SERVICE_TOKEN".into(),
                credential_reference: "larm/runtime/service".into(),
            }
        );
        assert_eq!(
            instance.metadata[RUNTIME_HEALTH_SNAPSHOT_KEY]["consecutive_failures"],
            1
        );
        let stored = serde_json::to_string(&instance.metadata).unwrap();
        assert!(!stored.contains("resolved-secret-value"));
    }

    #[test]
    fn snapshot_persistido_detecta_cambios_en_plan_o_en_hash_exterior() {
        let now = chrono::Utc::now();
        let snapshot = RuntimeLaunchSnapshot::from_plan(launch_plan(now), now).unwrap();
        let mut instance = record();
        instance.store_launch_snapshot(&snapshot).unwrap();
        instance.metadata[RUNTIME_LAUNCH_SNAPSHOT_KEY]["launch_plan"]["args"] =
            serde_json::json!(["runtime.exe", "--tampered"]);
        assert_eq!(
            instance.launch_snapshot().unwrap_err().details.reason,
            "runtime_launch_plan_hash_mismatch"
        );

        instance.store_launch_snapshot(&snapshot).unwrap();
        instance.metadata[RUNTIME_LAUNCH_SNAPSHOT_KEY]["snapshot_sha256"] =
            serde_json::json!("b".repeat(64));
        assert_eq!(
            instance.launch_snapshot().unwrap_err().details.reason,
            "runtime_launch_snapshot_hash_mismatch"
        );
    }

    #[test]
    fn snapshot_de_salud_tipado_se_valida_al_leer_la_instancia() {
        let mut instance = record();
        instance
            .store_health_snapshot(&HealthSnapshot {
                consecutive_failures: 2,
                return_state: Some(InstanceState::Busy),
            })
            .unwrap();
        instance.validate_record().unwrap();
        assert_eq!(
            instance.health_snapshot().unwrap().unwrap().return_state,
            Some(InstanceState::Busy)
        );

        instance.metadata[RUNTIME_HEALTH_SNAPSHOT_KEY]["consecutive_failures"] =
            serde_json::json!(5);
        assert_eq!(
            instance.validate_record().unwrap_err().details.reason,
            "runtime_health_snapshot_invalid"
        );
    }

    #[test]
    fn respuestas_de_runtime_validan_binding_de_proceso_y_salud() {
        let reference = InstanceRef {
            instance_id: "0192aaaa-0000-7000-8000-000000000013".into(),
            generation: 2,
        };
        reference.validate().unwrap();

        let invalid_handle = InstanceHandle {
            instance_id: reference.instance_id,
            generation: 2,
            pid: Some(41),
            process_started_at: None,
        };
        assert_eq!(
            invalid_handle.validate().unwrap_err().details.reason,
            "runtime_instance_handle_invalid"
        );
        assert!(serde_json::from_value::<InstanceRef>(serde_json::json!({
            "instance_id": "0192aaaa-0000-7000-8000-000000000013",
            "generation": 2,
            "unexpected": true
        }))
        .is_err());
        assert!(HealthResult {
            state: RuntimeHealthState::Ready,
            backend_effective: None,
            reason: String::new(),
            evidence_refs: vec![],
        }
        .validate()
        .is_err());
    }
}
