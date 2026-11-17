//! Puertos P030. El catálogo no otorga conformidad ni acceso a ejecución.
use crate::error::LarmError;
use crate::instances::RuntimeInstance;
use crate::ports::MutationContext;
use crate::protocol::{EndpointQualification, InferenceEndpoint};
use crate::runtime::*;
use crate::{
    bridge::JobReceipt,
    instances::{RuntimeInstanceStopJob, StopRequest, StopResult},
};
use async_trait::async_trait;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeFilter {
    pub q: Option<String>,
    pub state: Option<String>,
    pub capability: Option<String>,
    pub runtime_id: Option<String>,
    pub storage_location_id: Option<String>,
}

#[async_trait]
pub trait RuntimeCatalogService: Send + Sync {
    async fn create(
        &self,
        request: &RuntimeCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError>;
    async fn change_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeChannelPolicy,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError>;
}

#[async_trait]
pub trait RuntimeRepository: Send + Sync {
    async fn register_runtime_adapter(
        &self,
        installation: &str,
        family: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeAdapter, LarmError>;
    async fn runtime_adapter(&self, id: &str) -> Result<Option<RuntimeAdapter>, LarmError>;
    async fn create_runtime(
        &self,
        request: &RuntimeCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError>;
    async fn runtime(&self, id: &str) -> Result<Option<Runtime>, LarmError>;
    async fn runtimes(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<Runtime>, LarmError>;
    async fn change_channel_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeChannelPolicy,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError>;
    async fn change_retention_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeRetentionPolicy,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError>;
    async fn rollback_version(
        &self,
        version_id: &str,
        request: &RuntimeRollbackRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeMutationReceipt, LarmError>;
    async fn pin_version(
        &self,
        version_id: &str,
        request: &RuntimePinRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeMutationReceipt, LarmError>;
    async fn pins(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        runtime_version_id: Option<&str>,
    ) -> Result<Vec<RuntimeVersionPin>, LarmError>;
    async fn unpin(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn pin_for(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
    ) -> Result<Option<RuntimeVersionPin>, LarmError>;
    async fn record_version(
        &self,
        version: &RuntimeVersion,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn version(&self, id: &str) -> Result<Option<RuntimeVersion>, LarmError>;
    /// Resuelve la identidad upstream exacta usada por InstallRequest.
    /// Una misma versión textual ambigua no se escoge arbitrariamente.
    async fn version_by_runtime_and_string(
        &self,
        runtime_id: &str,
        version_string: &str,
    ) -> Result<Option<RuntimeVersion>, LarmError>;
    async fn versions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeVersion>, LarmError>;
    async fn record_installation(
        &self,
        installation: &RuntimeInstallation,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn installation(&self, id: &str) -> Result<Option<RuntimeInstallation>, LarmError>;
    async fn installations(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeInstallation>, LarmError>;
    async fn record_backend(
        &self,
        backend: &RuntimeBackend,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn backend(&self, id: &str) -> Result<Option<RuntimeBackend>, LarmError>;
    async fn backends(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeBackend>, LarmError>;
    async fn record_external_host(
        &self,
        host: &ExternalHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn external_host(&self, id: &str) -> Result<Option<ExternalHost>, LarmError>;
    async fn external_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExternalHost>, LarmError>;
    /// P081: registro read-only de hosts de ejecución observados. El registro
    /// no habilita ejecución ni acredita compatibilidad.
    async fn record_execution_host(
        &self,
        host: &ExecutionHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn execution_host(&self, id: &str) -> Result<Option<ExecutionHost>, LarmError>;
    async fn execution_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExecutionHost>, LarmError>;
}

/// Lecturas persistidas de instancias de runtime (P033).
#[async_trait]
pub trait RuntimeInstanceRepository: Send + Sync {
    /// Inserta una instancia recién creada y su evento en la misma transacción.
    async fn create_instance(
        &self,
        instance: &RuntimeInstance,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;

    /// Registra de forma atómica una instancia externa que acaba de superar
    /// un handshake y su endpoint ya cualificado. No inicia ni controla el
    /// proceso externo; la evidencia de protocolo procede del probe P036.
    async fn register_external_qualified_instance(
        &self,
        instance: &RuntimeInstance,
        endpoint: &InferenceEndpoint,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let _ = (instance, endpoint, ctx);
        Err(LarmError::new(
            crate::error::ErrorCode::ResourceUnavailable,
            "El repositorio no registra instancias externas cualificadas",
            "runtime_external_registration_unsupported",
        ))
    }

    async fn instance(&self, id: &str) -> Result<Option<RuntimeInstance>, LarmError>;

    /// Página estrictamente posterior a (`created_at`, `id`), opcionalmente
    /// limitada al runtime al que pertenece la instalación.
    async fn instances(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        runtime_id: Option<&str>,
    ) -> Result<Vec<RuntimeInstance>, LarmError>;

    async fn runtime_exists(&self, id: &str) -> Result<bool, LarmError>;

    /// Persiste una transición válida con CAS de `row_version` y escribe el
    /// evento/outbox de estado dentro de la misma transacción.
    async fn transition_instance(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError>;

    /// Marca una instancia para stop sólo si sigue en la revisión indicada y
    /// no conserva leases no terminales. La comprobación de leases y el CAS
    /// deben confirmarse en la misma transacción que el evento del outbox.
    async fn begin_stop_instance(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError>;
}

/// Persistencia CAS de observaciones obtenidas desde una sesión de runtime
/// cualificada. No cambia la identidad ni el binding del proceso.
#[async_trait]
pub trait RuntimeInstanceHealthRepository: Send + Sync {
    async fn record_health_observation(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError>;
}

/// Durable registry of protocol-qualified endpoints associated with instances (P036).
#[async_trait]
pub trait InferenceEndpointRepository: Send + Sync {
    /// Stores only a completed probe result and its outbox event atomically.
    async fn register_endpoint(
        &self,
        endpoint: &InferenceEndpoint,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;

    async fn endpoint(&self, id: &str) -> Result<Option<InferenceEndpoint>, LarmError>;

    /// Returns every protocol endpoint for one instance in stable order.
    async fn endpoints_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<InferenceEndpoint>, LarmError>;

    /// Retains a dead endpoint as FAILED, using row-version CAS and an outbox event.
    async fn fail_endpoint(
        &self,
        id: &str,
        expected_row_version: i64,
        evidence_refs: &[String],
        ctx: &MutationContext,
    ) -> Result<InferenceEndpoint, LarmError>;

    /// Read-only view consumed by a Broker after a lease is associated with the instance.
    async fn qualified_endpoints_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<InferenceEndpoint>, LarmError> {
        Ok(self
            .endpoints_for_instance(instance_id)
            .await?
            .into_iter()
            .filter(|endpoint| endpoint.qualification == EndpointQualification::Qualified)
            .collect())
    }
}

/// Journal e idempotencia de `POST /instances/{id}/stop` (P033).
#[async_trait]
pub trait RuntimeInstanceStopRepository: crate::ports::JobSource + Send + Sync {
    /// Encola el job, transiciona la instancia a STOPPING, comprueba leases y
    /// guarda la respuesta idempotente en una sola transacción.
    async fn submit_stop(
        &self,
        instance_id: &str,
        request: &StopRequest,
        body_hash: &str,
        idempotency_key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError>;

    /// Reclama en orden un job QUEUED. Un solo consumidor por fila.
    async fn claim_stop_job(&self) -> Result<Option<RuntimeInstanceStopJob>, LarmError>;

    /// Confirma STOPPED, el resultado y el job terminal de forma atómica.
    async fn complete_stop_job(&self, job_id: &str, result: StopResult) -> Result<(), LarmError>;

    /// Marca el job fallido sin fingir que el proceso se detuvo.
    async fn fail_stop_job(&self, job_id: &str, error: &LarmError) -> Result<(), LarmError>;

    /// Tras el reinicio, reencola RUNNING para que el supervisor revalide la
    /// identidad persistida antes de repetir la parada.
    async fn recover_stop_jobs(&self) -> Result<u64, LarmError>;
}
