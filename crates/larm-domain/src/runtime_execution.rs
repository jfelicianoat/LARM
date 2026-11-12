//! P031: contexto interno leído del registro/journal, nunca un DTO de autoridad.
use crate::adapters::AdapterInstallation;
use crate::artifacts::Plan;
use crate::conformance::{hash, Run};
use crate::error::{ErrorCode, LarmError};
use crate::ports::{AdapterQualificationGuard, MutationContext};
use crate::runtime::{Runtime, RuntimeAdapter, RuntimeVersion};
use crate::runtime_install::{ExecutePlan, InstallRequest, RuntimeVersionSource};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeExecutionContext {
    pub operation_id: String,
    pub runtime: Runtime,
    pub adapter: RuntimeAdapter,
    pub installation: AdapterInstallation,
    pub run: Run,
    pub actor_id: String,
}

/// Área temporal de una instalación administrada. El host la deriva del
/// directorio de trabajo ya cualificado y del `plan_id`; nunca viene de la API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInstallWorkspace {
    pub root: PathBuf,
    pub path: PathBuf,
}

/// Recursos concretos que una operación interna puede tocar. No implementa
/// `Deserialize`: el coordinador debe derivarlo del plan y del estado durable,
/// nunca aceptarlo como autoridad desde una petición pública.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOperationAccess {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
    pub urls: Vec<String>,
    pub processes: Vec<String>,
    pub secrets: Vec<String>,
}

/// Identidad de la sesión física. No implementa Deserialize: la API no presenta
/// hashes, guards o Proof en lugar de que el servidor los obtenga y verifique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSessionBinding {
    pub runtime_id: String,
    pub installation_sha256: String,
    pub run_sha256: String,
    pub actor_id: String,
}
impl RuntimeSessionBinding {
    pub fn new(
        runtime_id: &str,
        installation: &AdapterInstallation,
        run: &Run,
        actor: &str,
    ) -> Result<Self, LarmError> {
        Ok(Self {
            runtime_id: runtime_id.into(),
            installation_sha256: crate::adapter_probes::snapshot(installation)?,
            run_sha256: run.digest()?,
            actor_id: actor.into(),
        })
    }
}
impl RuntimeExecutionContext {
    pub fn binding(&self) -> Result<RuntimeSessionBinding, LarmError> {
        RuntimeSessionBinding::new(
            &self.runtime.id,
            &self.installation,
            &self.run,
            &self.actor_id,
        )
    }
    pub fn digest(&self) -> Result<String, LarmError> {
        let mut runtime = serde_json::to_value(&self.runtime).map_err(|_| invalid())?;
        let mut adapter = serde_json::to_value(&self.adapter).map_err(|_| invalid())?;
        runtime["row_version"] = serde_json::json!(self.runtime.row_version.to_string());
        adapter["row_version"] = serde_json::json!(self.adapter.row_version.to_string());
        hash(
            &serde_json::json!({"operation_id":self.operation_id,"runtime":runtime,"adapter":adapter,"installation_sha256":crate::adapter_probes::snapshot(&self.installation)?,"run_sha256":self.run.digest()?,"actor_id":self.actor_id}),
        )
    }
}
fn invalid() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "Contexto de runtime no válido",
        "runtime_execution_context_invalid",
    )
}

#[async_trait]
pub trait RuntimeSessionGuard: AdapterQualificationGuard {
    fn binding(&self) -> &RuntimeSessionBinding;
}

/// Sesión propietaria del proceso y su IO. Cerrar no exige que el paquete siga
/// válido; la cualificación permanece retenida hasta terminar el consumidor.
#[async_trait]
pub trait RuntimeDiscoverySession: RuntimeSessionGuard + RuntimeVersionSource {
    /// Solicita un plan de lanzamiento sin iniciar procesos. El alcance procede
    /// de la solicitud validada y se vuelve a contrastar con P022 en el host.
    async fn launch_plan(
        &self,
        _request: &crate::instances::LaunchRequest,
        _access: &RuntimeOperationAccess,
        _timeout: Option<Duration>,
    ) -> Result<crate::instances::LaunchPlan, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite planificación de lanzamiento",
            "runtime_launch_plan_unavailable",
        ))
    }
    /// Consume un plan ya admitido por el coordinador y devuelve la identidad
    /// de proceso que el worker observó.
    async fn start(
        &self,
        _request: &ExecutePlan,
        _access: &RuntimeOperationAccess,
        _timeout: Option<Duration>,
    ) -> Result<crate::instances::InstanceHandle, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite inicio de runtimes",
            "runtime_start_unavailable",
        ))
    }
    async fn load(
        &self,
        _request: &crate::instances::LaunchRequest,
        _access: &RuntimeOperationAccess,
        _timeout: Option<Duration>,
    ) -> Result<crate::instances::InstanceHandle, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite carga de runtimes",
            "runtime_load_unavailable",
        ))
    }
    async fn health(
        &self,
        _request: &crate::instances::InstanceRef,
        _access: &RuntimeOperationAccess,
        _timeout: Option<Duration>,
    ) -> Result<crate::instances::HealthResult, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite consultar salud de runtimes",
            "runtime_health_unavailable",
        ))
    }
    async fn stop(
        &self,
        _request: &crate::instances::InstanceRef,
        _access: &RuntimeOperationAccess,
        _timeout: Option<Duration>,
    ) -> Result<crate::instances::StopResult, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite parada de runtimes",
            "runtime_stop_unavailable",
        ))
    }
    /// Genera un plan contractual de instalación con el mismo worker cualificado.
    /// La implementación por defecto mantiene compatibles los hosts sólo lectura.
    async fn plan_install(&self, _request: &InstallRequest) -> Result<Plan, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite planificación de instalación",
            "runtime_install_plan_unavailable",
        ))
    }
    /// Ejecuta de forma supervisada un plan ya admitido en el journal.
    /// La respuesta COMPLETED sólo es válida tras terminar el trabajo de IO.
    async fn install(
        &self,
        _request: &ExecutePlan,
    ) -> Result<crate::bridge::JobReceipt, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite ejecución de instalaciones",
            "runtime_install_execution_unavailable",
        ))
    }
    /// Solicita al worker cancelar la llamada `runtime.install` de este plan.
    /// `true` sólo significa que el worker aceptó la solicitud; el consumidor
    /// debe esperar respuesta, cierre y drenaje antes de liberar sus guards.
    async fn cancel_install(&self, _plan_id: &str) -> Result<bool, LarmError> {
        Ok(false)
    }
    /// Consulta el journal durable del adaptador tras reiniciar LARM. El método
    /// por defecto conserva compatibilidad y deja el job sin reconciliar.
    async fn install_status(
        &self,
        _request: &crate::runtime_install::InstallStatusRequest,
    ) -> Result<crate::bridge::JobReceipt, LarmError> {
        Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La sesión no admite consultar jobs de instalación",
            "runtime_install_status_unavailable",
        ))
    }
    /// Resuelve (y, en ejecución, crea) el área propia de este plan. `create`
    /// es falso durante reconciliación para no fabricar resultados perdidos.
    async fn install_workspace(
        &self,
        _plan_id: &str,
        _plan_hash: &str,
        _create: bool,
    ) -> Result<Option<RuntimeInstallWorkspace>, LarmError> {
        Ok(None)
    }
    async fn close(&self) -> Result<(), LarmError>;
}

#[async_trait]
pub trait RuntimeDiscoveryHost: Send + Sync {
    /// El host resuelve permisos/perfil y conserva la cualificación física. No
    /// acepta Proof, configuración ejecutable o Access procedentes de la API.
    async fn open(
        &self,
        context: &RuntimeExecutionContext,
        ctx: &MutationContext,
    ) -> Result<Box<dyn RuntimeDiscoverySession>, LarmError>;
}

#[async_trait]
pub trait RuntimeExecutionRepository: Send + Sync {
    /// Carga Run desde el journal verificado, rechaza sondeos activos/pendientes.
    /// Aún no ejecuta ni reemplaza la cualificación física del host.
    async fn prepare_execution(
        &self,
        runtime: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeExecutionContext, LarmError>;
    /// Publica el batch completo bajo CAS de contexto y guard físico vigente;
    /// una página parcial, promoción del upstream o error no concede autoridad.
    async fn commit_discovery(
        &self,
        context: &RuntimeExecutionContext,
        observations: &[RuntimeVersion],
        session: &dyn RuntimeSessionGuard,
        ctx: &MutationContext,
    ) -> Result<Vec<RuntimeVersion>, LarmError>;
}
