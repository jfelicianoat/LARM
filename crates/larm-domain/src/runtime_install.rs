//! P031: contratos de descubrimiento e instalación. Ninguno concede ejecución.
use crate::artifacts::Plan;
use crate::bridge::{is_uuid_v7, timestamp, JobPhase, JobReceipt};
use crate::error::{ErrorCode, LarmError};
use crate::ports::MutationContext;
use crate::runtime::{InstallationStrategy, RuntimeChannel, RuntimeInstallation, RuntimeVersion};
use crate::runtime_execution::RuntimeDiscoverySession;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const INSTALL_SPEC: &str = "runtime_install_v1";
pub const INSTALL_OPERATION: &str = "runtime.install";
pub const INSTALL_JOB_KIND: &str = "runtime.install";
pub const INSTALL_IDEMPOTENCY_OPERATION: &str = "P031_post__runtime_installations";
pub const INSTALL_WORKSPACE_MARKER: &str = ".larm-install-workspace.json";

/// Puerto interno de publicación de una instalación gestionada. No es un DTO
/// del contrato del adaptador ni una autoridad aceptada desde la API.
#[async_trait]
pub trait ManagedRuntimeInstallStore: Send + Sync {
    async fn publish(
        &self,
        job_id: &str,
        plan_hash: &str,
        adapter_plan_hash: &str,
        storage_location_id: &str,
        relative_destination: &str,
        workspace: Option<&crate::runtime_execution::RuntimeInstallWorkspace>,
    ) -> Result<(), LarmError>;

    async fn discard_workspace(
        &self,
        job_id: &str,
        adapter_plan_hash: &str,
        workspace: &crate::runtime_execution::RuntimeInstallWorkspace,
    ) -> Result<(), LarmError>;
}

/// Marker exacto que liga el directorio temporal al job y al plan retenido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstallWorkspaceMarker {
    pub job_id: String,
    pub adapter_plan_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub runtime_id: String,
    pub upstream_version: String,
    pub strategy: InstallationStrategy,
    pub storage_location_id: String,
    pub profile_lock_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub external_host_id: Option<String>,
    /// P081: host de ejecución referenciado para estrategias WSL/CONTAINER.
    #[serde(deserialize_with = "Option::deserialize")]
    pub execution_host_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutePlan {
    pub plan_id: String,
    pub plan_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
}

/// Consulta idempotente del estado durable de una instalación ya admitida.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallStatusRequest {
    pub plan_id: String,
    pub plan_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionQuery {
    pub runtime_id: String,
    pub channel: RuntimeChannel,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeVersionPage {
    pub items: Vec<RuntimeVersion>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub next_cursor: Option<String>,
    pub snapshot_at: String,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Solicitud de runtime no válida",
        reason,
    )
}
fn protocol(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ProtocolError,
        "El adaptador devolvió un plan de instalación no válido",
        reason,
    )
}
fn id(value: &str) -> Result<(), LarmError> {
    if is_uuid_v7(value) {
        Ok(())
    } else {
        Err(invalid("runtime_request_id_invalid"))
    }
}
fn hash(value: &str) -> Result<(), LarmError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid("runtime_request_hash_invalid"))
    }
}
fn cursor(value: &Option<String>) -> Result<(), LarmError> {
    if value.as_ref().is_some_and(String::is_empty) {
        Err(invalid("runtime_cursor_invalid"))
    } else {
        Ok(())
    }
}

impl InstallRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.runtime_id)?;
        id(&self.storage_location_id)?;
        hash(&self.profile_lock_hash)?;
        if let Some(host) = &self.external_host_id {
            id(host)?;
        }
        if let Some(host) = &self.execution_host_id {
            id(host)?;
        }
        // P081: coherencia estructural sin base de datos — las estrategias
        // virtualizadas exigen host referenciado y las nativas lo prohíben.
        let virtualized = matches!(
            self.strategy,
            InstallationStrategy::Wsl | InstallationStrategy::Container
        );
        if virtualized && self.execution_host_id.is_none() {
            return Err(invalid("execution_host_reference_missing"));
        }
        if !virtualized && self.execution_host_id.is_some() {
            return Err(invalid("execution_host_reference_unexpected"));
        }
        if self.upstream_version.is_empty() {
            return Err(invalid("upstream_version_invalid"));
        }
        Ok(())
    }
    /// Comprobar antes del callback/descarga. No sustituye P022/P026/P099.
    pub fn require_advertised(&self, declared: &[InstallationStrategy]) -> Result<(), LarmError> {
        self.validate()?;
        if !declared.contains(&self.strategy) {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El adaptador no anuncia esta estrategia",
                "installation_strategy_not_advertised",
            ));
        }
        Ok(())
    }
}
impl ExecutePlan {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.plan_id)?;
        hash(&self.plan_hash)?;
        if let Some(approval) = &self.approval_id {
            id(approval)?;
        }
        Ok(())
    }
}
impl InstallStatusRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.plan_id)?;
        hash(&self.plan_hash)
    }
}
impl VersionQuery {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.runtime_id)?;
        cursor(&self.cursor)
    }
}
impl RuntimeVersionPage {
    pub fn validate_for(&self, query: &VersionQuery) -> Result<(), LarmError> {
        query.validate()?;
        cursor(&self.next_cursor)?;
        let at = chrono::DateTime::parse_from_rfc3339(&self.snapshot_at)
            .map_err(|_| invalid("runtime_snapshot_invalid"))?;
        if timestamp(at.with_timezone(&chrono::Utc)) != self.snapshot_at {
            return Err(invalid("runtime_snapshot_invalid"));
        }
        for version in &self.items {
            version.validate()?;
            version.observed_channel()?;
            if version.runtime_id != query.runtime_id || version.channel != query.channel {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "El adaptador respondió para otro runtime o canal",
                    "runtime_version_binding_mismatch",
                ));
            }
        }
        Ok(())
    }
}

/// Hash del plan conforme al contrato: el campo `plan_hash` no se incluye en
/// su propio digest.
pub fn plan_hash(plan: &Plan) -> Result<String, LarmError> {
    let mut value = serde_json::to_value(plan).map_err(|_| invalid("runtime_install_serialize"))?;
    value
        .as_object_mut()
        .ok_or_else(|| invalid("runtime_install_serialize"))?
        .remove("plan_hash");
    crate::canonical::sha256_hex(&value).map_err(|_| invalid("runtime_install_canonical"))
}

/// Verifica la respuesta sin autoridad del adaptador antes de que el host la
/// combine con IDs persistidos, perfil, aprobación y estimación reservable.
/// No cualifica el plan ni permite ejecutarlo.
pub fn validate_plan_reply(
    request: &InstallRequest,
    plan: &Plan,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LarmError> {
    request.validate()?;
    let mut resources = std::collections::BTreeSet::new();
    if !is_uuid_v7(&plan.plan_id)
        || plan.operation != INSTALL_OPERATION
        || !plan.resource_ids.contains(&request.runtime_id)
        || !plan.resource_ids.contains(&request.storage_location_id)
        || plan
            .resource_ids
            .iter()
            .any(|resource| resource.is_empty() || !resources.insert(resource))
        || plan.steps.iter().any(|step| {
            step.phase.is_empty() || step.adapter_id.as_deref().is_some_and(str::is_empty)
        })
    {
        return Err(protocol("runtime_install_plan_reply_invalid"));
    }
    let expires = chrono::DateTime::parse_from_rfc3339(&plan.expires_at)
        .map_err(|_| protocol("runtime_install_plan_expiry_invalid"))?
        .with_timezone(&chrono::Utc);
    if timestamp(expires) != plan.expires_at {
        return Err(protocol("runtime_install_plan_expiry_invalid"));
    }
    if expires <= now {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "El plan de instalación del adaptador ya ha caducado",
            "runtime_install_plan_expired",
        ));
    }
    if plan_hash(plan)? != plan.plan_hash {
        return Err(protocol("runtime_install_plan_hash_mismatch"));
    }
    Ok(())
}

/// El request queda ligado al plan por el binding interno del adapter.
pub fn request_from_plan(plan: &Plan) -> Result<InstallRequest, LarmError> {
    let mut bindings = plan
        .steps
        .iter()
        .filter_map(|step| step.request.get(INSTALL_SPEC));
    let binding = bindings
        .next()
        .ok_or_else(|| invalid("runtime_install_spec_missing"))?;
    if bindings.next().is_some() {
        return Err(invalid("runtime_install_spec_ambiguous"));
    }
    let request = binding
        .get("request")
        .cloned()
        .ok_or_else(|| invalid("runtime_install_request_missing"))?;
    let request: InstallRequest =
        serde_json::from_value(request).map_err(|_| invalid("runtime_install_request_invalid"))?;
    request.validate()?;
    Ok(request)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeInstallJob {
    pub job_id: String,
    pub state: JobPhase,
    pub plan_hash: String,
    pub resource_ids: Vec<String>,
    pub plan: Plan,
    pub installation: RuntimeInstallation,
    pub actor_id: String,
    pub correlation_id: String,
    pub request_hash: String,
    pub updated_at: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<crate::error::ApiError>,
}

/// Entradas para la admisión atómica de un plan de instalación.
pub struct RuntimeInstallAdmission<'a> {
    pub installation: &'a RuntimeInstallation,
    pub plan: &'a Plan,
    pub principal_id: &'a str,
    pub idempotency_key: &'a str,
    pub request_hash: &'a str,
    pub idempotency_expires_at: &'a str,
    pub ctx: &'a MutationContext,
    /// La sesión cualificada permanece abierta hasta que SQLite vuelva a
    /// comprobarla dentro de la admisión atómica.
    pub session: &'a dyn RuntimeDiscoverySession,
}

#[async_trait]
pub trait RuntimeInstallJobRepository: Send + Sync {
    /// Busca replay de idempotencia antes de abrir el worker o generar otro plan.
    async fn replay(
        &self,
        principal_id: &str,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<Option<JobReceipt>, LarmError>;

    /// Persiste plan, registro INSTALL_PLANNED, locks de volumen, reservas,
    /// outbox e idempotencia como una única mutación SQLite.
    async fn enqueue_plan(
        &self,
        admission: RuntimeInstallAdmission<'_>,
    ) -> Result<(JobReceipt, bool), LarmError>;

    /// Transición cercada QUEUED → RUNNING e INSTALL_PLANNED → INSTALLING.
    /// No ejecuta pasos del adaptador.
    async fn start_job(&self, id: &str) -> Result<RuntimeInstallJob, LarmError>;

    /// Cierra el job como COMPLETED e INSTALLING → INSTALLED. El consumidor
    /// sólo puede llamar esta operación tras verificar que el worker terminó
    /// correctamente; un recibo QUEUED/RUNNING no acredita la instalación.
    async fn complete_job(
        &self,
        id: &str,
        worker_receipt: &JobReceipt,
    ) -> Result<RuntimeInstallJob, LarmError>;

    /// Lee IDs de un lote acotado y ordenado de trabajos QUEUED para el
    /// consumidor durable. Cada job se vuelve a leer antes de ejecutarlo.
    async fn queued_job_ids(&self, limit: u32) -> Result<Vec<String>, LarmError>;

    /// Lee IDs de trabajos RUNNING para reconciliar el estado tras reiniciar
    /// el daemon. Consultar IDs nunca modifica el journal ni libera guards.
    async fn running_job_ids(&self, limit: u32) -> Result<Vec<String>, LarmError>;

    /// Cierra un job fallido junto con la instalación, journal, evento y la
    /// liberación de sus locks y reservas. Sólo acepta QUEUED o RUNNING.
    async fn fail_job(&self, id: &str, error: &LarmError) -> Result<RuntimeInstallJob, LarmError>;

    /// Cancela un job sólo mientras sigue QUEUED, antes de cualquier IO del worker.
    /// La transición, el evento y la liberación de guards son atómicos.
    async fn cancel_queued_job(
        &self,
        id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstallJob, LarmError>;

    /// Cierra RUNNING como CANCELLED sólo después de que el worker confirme
    /// cancelación, drene su IO y el host cierre la sesión limpiamente.
    async fn cancel_running_job(
        &self,
        id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstallJob, LarmError>;

    async fn get_job(&self, id: &str) -> Result<Option<RuntimeInstallJob>, LarmError>;
}

/// Sesión ya ligada a instalación/actor/permisos por el host. El consumidor
/// concreto debe conservar cualificación, timeout/cancel y binding de P021/P026.
/// Este puerto no recibe ni acepta Proof o autoridad aportada por el cliente.
#[async_trait]
pub trait RuntimeVersionSource: Send + Sync {
    async fn list_versions(&self, query: &VersionQuery) -> Result<RuntimeVersionPage, LarmError>;
}

/// Caso de uso expuesto por `POST /runtime-installations`.
#[async_trait]
pub trait RuntimeInstallSubmitter: Send + Sync {
    async fn submit(
        &self,
        request: &InstallRequest,
        principal_id: &str,
        idempotency_key: &str,
        request_hash: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError>;

    /// Cancela un job en cola o solicita cancelación cooperativa a su worker activo.
    async fn cancel(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError>;
}
