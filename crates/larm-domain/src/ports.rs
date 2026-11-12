//! Puertos del puente Broker–LARM (spec §62–§64, contratos 04, 06 y 07).
//!
//! Viven en el dominio porque los implementa `larm-runtime`, que según
//! `crate-graph.json` puede depender de `larm-domain` pero no de `larm-core`.
//! `larm-core::bridge` los reexporta para la API; la composición está en el daemon.

use crate::bridge::{ApiError, BrokerCatalog, JobState, LeaseRequest, Page, Renew, RuntimeLease};
use async_trait::async_trait;
use serde_json::Value;

pub use crate::auth::{ApiScope, Principal};

/// Autenticación Bearer de principales de la API administrativa.
#[async_trait]
pub trait PrincipalAuthenticator: Send + Sync {
    /// Principal del token, `None` si no corresponde a ninguno.
    ///
    /// # Errors
    /// `AUTH_BACKEND_UNAVAILABLE` si el almacén de credenciales no responde:
    /// se falla cerrado, sin confundirlo con un token incorrecto.
    async fn authenticate(&self, bearer: &str) -> Result<Option<Principal>, ApiError>;
}

/// Almacén de secretos (Windows Credential Manager en producción). El
/// contrato 04 guarda cada token de endpoint como
/// `service=larm/lease/{lease_id}, username=owner_id`.
pub trait SecretStore: Send + Sync {
    /// Guarda un secreto; error = texto técnico sin el secreto.
    fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String>;
    /// Lee un secreto; `None` si no existe.
    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String>;
    /// Borra un secreto; borrar uno inexistente no es error.
    fn delete(&self, service: &str, username: &str) -> Result<(), String>;
}

/// Quién y en qué petición hace una mutación: va al evento del outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    pub actor_id: String,
    pub correlation_id: String,
}

/// Configuración persistida con su revisión CAS (P003).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsSnapshot {
    pub payload: Value,
    pub revision: i64,
}

/// Almacén de Settings; cada escritura y su evento de auditoría son atómicos.
#[async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_or_initialize(&self, initial: Value) -> Result<SettingsSnapshot, ApiError>;
    async fn update(
        &self,
        expected_revision: i64,
        payload: Value,
        ctx: &MutationContext,
    ) -> Result<SettingsSnapshot, ApiError>;
}

/// Lectura y estado visto de notificaciones persistidas (P113). Las
/// transiciones leído/no leído usan CAS y escriben su evento en el outbox.
#[async_trait]
pub trait NotificationRepository: Send + Sync {
    async fn notification(
        &self,
        id: &str,
    ) -> Result<Option<crate::watchers::Notification>, ApiError>;
    async fn page_notifications(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::watchers::Notification>, ApiError>;
    async fn set_notification_read(
        &self,
        id: &str,
        if_match: i64,
        read: bool,
        ctx: &MutationContext,
    ) -> Result<crate::watchers::Notification, ApiError>;
}

/// P093: API del caso de uso de transformación, disponible desde el dominio
/// para que API y persistencia respeten el grafo de crates publicado.
#[async_trait]
pub trait TransformationService: Send + Sync {
    async fn create_plan(
        &self,
        request: &crate::transformations::TransformationRequest,
        ctx: &MutationContext,
    ) -> Result<crate::transformations::TransformationPlanResponse, ApiError>;

    async fn execute_plan(
        &self,
        request: &crate::transformations::ExecuteTransformationPlan,
        ctx: &MutationContext,
        idempotency: &crate::transformations::TransformationActivationIdempotency,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;

    async fn restart_plan(
        &self,
        restart: &crate::transformations::TransformationRestart,
        request: &crate::transformations::ExecuteTransformationPlan,
        ctx: &MutationContext,
        idempotency: &crate::transformations::TransformationActivationIdempotency,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;

    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<crate::transformations::TransformationJob>, ApiError>;

    /// Solicita la cancelación de un job propio y no terminal (contrato 01:
    /// cualquiera no terminal → CANCELLING; sólo el propietario la ve). En
    /// cola cierra CANCELLED con liberación de recursos; en ejecución deja
    /// CANCELLING duradero y el consumidor cierra el intento con el testigo.
    /// Un trabajo ajeno o desconocido queda oculto (NOT_FOUND).
    async fn cancel_job(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// Persistencia de `credential_reference` (sin secretos). Cada mutación
/// escribe su evento en el outbox en la misma transacción y devuelve su `event_id`.
#[async_trait]
pub trait CredentialRepository: Send + Sync {
    /// Inserta; `CONFLICT` si ya existe otra con el mismo servicio y usuario.
    async fn insert(
        &self,
        credential: &crate::credential::CredentialReference,
        ctx: &MutationContext,
    ) -> Result<String, ApiError>;
    async fn get(
        &self,
        id: &str,
    ) -> Result<Option<crate::credential::CredentialReference>, ApiError>;
    /// Página ordenada por (`created_at`, `id`) estrictamente después de `after`.
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::credential::CredentialReference>, ApiError>;
    /// Todas las de un prefijo de servicio (p. ej. `larm/principal/`).
    async fn by_service_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<crate::credential::CredentialReference>, ApiError>;
    /// Borra con `If-Match`: `NOT_FOUND` o `STALE_REVISION`.
    async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<String, ApiError>;
    /// Nueva revisión tras rotar el secreto (incrementa `row_version`).
    async fn touch(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::credential::CredentialReference, ApiError>;
}

/// Persistencia de aprobaciones (`operation_journal`, `kind=approval`).
#[async_trait]
pub trait ApprovalRepository: Send + Sync {
    async fn insert(
        &self,
        approval: &crate::approval::ApprovalRecord,
        ctx: &MutationContext,
    ) -> Result<String, ApiError>;
    async fn get(&self, id: &str) -> Result<Option<crate::approval::ApprovalRecord>, ApiError>;
    /// Marca consumida si aún no lo estaba; `false` si otra operación se adelantó.
    async fn consume(&self, id: &str, at: &str) -> Result<bool, ApiError>;
}

/// Sólo el composition root registra recetas compiladas; consumo y outbox atómicos.
#[async_trait]
pub trait ConformanceProfileRepository: Send + Sync {
    async fn conformance_profile(
        &self,
        installation: &str,
    ) -> Result<Option<crate::conformance_profiles::ConformanceProfile>, ApiError>;
    async fn store_conformance_profile(
        &self,
        profile: &crate::conformance_profiles::ConformanceProfile,
        expected_profile: Option<&str>,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
}

/// Respuesta guardada para repetir una mutación idempotente.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredResponse {
    pub body_hash: String,
    pub status: u16,
    pub response: Value,
}

/// `Idempotency-Key` de las mutaciones POST (contrato 07).
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    async fn lookup(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
    ) -> Result<Option<StoredResponse>, ApiError>;
    async fn save(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
        stored: &StoredResponse,
        expires_at: &str,
    ) -> Result<(), ApiError>;
}

/// Resultado de `POST /leases`: ACTIVE inmediata (201) o job (202). Un
/// replay idempotente devuelve exactamente lo que se respondió la primera vez.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateLeaseOutcome {
    pub status: u16,
    pub body: Value,
}

/// Resultado de persistir una solicitud de lease con Idempotency-Key.
#[derive(Debug, Clone, PartialEq)]
pub enum LeaseCreatePersistence {
    Created(crate::bridge::RuntimeLease),
    Existing(crate::bridge::RuntimeLease),
    Replayed(CreateLeaseOutcome),
}

/// Datos autorizados para una transición durable de lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseStateUpdate {
    pub from: crate::bridge::LeaseState,
    pub to: crate::bridge::LeaseState,
    pub instance_id: Option<String>,
    pub endpoint_id: Option<String>,
    pub expires_at: Option<String>,
}

/// Datos CAS y de replay para la renovación durable de una lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRenewal {
    pub owner_id: String,
    pub if_match: u64,
    pub generation: u64,
    pub idempotency_key: String,
    pub body_hash: String,
    pub expires_at: String,
}

/// Tipo de job durable para la admisión de leases Broker–LARM.
pub const BROKER_LEASE_ADMISSION_JOB_KIND: &str = "broker.lease.admission";

/// Persistencia transaccional del ciclo de vida de leases P100.
#[async_trait]
pub trait RuntimeLeaseRepository: Send + Sync {
    /// Inserta REQUESTED, el replay idempotente y el evento outbox atómicamente.
    async fn create_requested(
        &self,
        lease: &crate::bridge::RuntimeLease,
        body_hash: &str,
        idempotency_key: &str,
        initial_response: &CreateLeaseOutcome,
        ctx: &MutationContext,
    ) -> Result<LeaseCreatePersistence, ApiError>;

    async fn get_for_owner(
        &self,
        lease_id: &str,
        owner_id: &str,
    ) -> Result<Option<crate::bridge::RuntimeLease>, ApiError>;

    /// Orden estable `created_at,id`; el cursor ya está validado por la API.
    async fn list_for_owner(
        &self,
        owner_id: &str,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::bridge::RuntimeLease>, ApiError>;

    /// Compara propietario, generación, row_version y estado de origen.
    async fn transition(
        &self,
        lease_id: &str,
        owner_id: &str,
        if_match: u64,
        generation: u64,
        update: &LeaseStateUpdate,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::RuntimeLease, ApiError>;

    /// Renueva una lease ACTIVE con CAS e idempotencia durable. `expires_at`
    /// se calcula desde el momento de la petición y la respuesta del primer
    /// intento queda guardada durante la misma transacción.
    async fn renew_active(
        &self,
        lease_id: &str,
        renewal: &LeaseRenewal,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::RuntimeLease, ApiError>;

    /// Aplica vencimientos ACTIVE → EXPIRING → EXPIRED y sus eventos outbox.
    /// `grace_seconds` fija la gracia entre ambas transiciones.
    async fn expire_due(
        &self,
        grace_seconds: u64,
        ctx: &MutationContext,
    ) -> Result<Vec<crate::bridge::RuntimeLease>, ApiError>;

    /// Tras reiniciar, expira leases de generaciones antiguas sin revivir ACTIVE.
    /// Devuelve las filas para que el servicio revoque sus credenciales locales.
    async fn expire_unreconciled(
        &self,
        current_generation: u64,
        ctx: &MutationContext,
    ) -> Result<Vec<crate::bridge::RuntimeLease>, ApiError>;
}

/// Respuesta del gateway de inferencia de una lease.
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Token de endpoint entregado por el canal local de credenciales.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointCredential {
    pub token: String,
    pub expires_at: String,
}

/// Casos de uso que el Broker consume de LARM.
#[async_trait]
pub trait BrokerBridge: Send + Sync {
    /// `GET /api/v1/broker/catalog`: solo variantes BROKER_ELIGIBLE.
    async fn catalog(&self) -> Result<BrokerCatalog, ApiError>;

    /// `GET /api/v1/launch-profiles` (lista `launch_profile` del dominio).
    async fn launch_profiles(&self) -> Result<Page<Value>, ApiError>;

    /// `POST /api/v1/leases` con su `Idempotency-Key`.
    async fn create_lease(
        &self,
        principal: &Principal,
        request: LeaseRequest,
        idempotency_key: &str,
    ) -> Result<CreateLeaseOutcome, ApiError>;

    async fn get_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
    ) -> Result<RuntimeLease, ApiError>;

    async fn list_leases(&self, principal: &Principal) -> Result<Page<RuntimeLease>, ApiError>;

    /// `POST /api/v1/leases/{id}/renew` con If-Match e Idempotency-Key.
    async fn renew_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
        if_match: u64,
        idempotency_key: &str,
        renew: Renew,
    ) -> Result<RuntimeLease, ApiError>;

    /// `DELETE /api/v1/leases/{id}` con If-Match; idempotente.
    async fn release_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
        if_match: u64,
    ) -> Result<(), ApiError>;

    async fn get_job(&self, principal: &Principal, job_id: &str) -> Result<JobState, ApiError>;

    /// Gateway autenticado del endpoint de una lease (`/gateway/{lease}/v1/...`).
    async fn gateway(
        &self,
        lease_id: &str,
        bearer: Option<&str>,
        path: &str,
        body: Value,
    ) -> Result<GatewayResponse, ApiError>;

    /// Canal local de credenciales: token de endpoint de una lease propia.
    async fn endpoint_credential(
        &self,
        principal: &Principal,
        lease_id: &str,
        generation: u64,
        credential_reference_id: &str,
    ) -> Result<EndpointCredential, ApiError>;
}

/// Fuente de jobs para `GET /api/v1/jobs/{id}` (P007). Cada dominio que produce
/// jobs (puente Broker, descargas, transformaciones...) registra la suya; la
/// fuente decide qué jobs puede ver `principal` (contrato 11/H06: un principal
/// sin administración solo ve los suyos).
#[async_trait]
pub trait JobSource: Send + Sync {
    /// `Ok(None)` si el job no es de esta fuente o el principal no puede verlo.
    async fn job(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<Option<crate::bridge::JobState>, ApiError>;
}

/// Sondeo del hardware local (P010/P011). Los componentes que no se pueden
/// observar quedan `null` con motivo; sólo falla si no hay snapshot posible.
#[async_trait]
pub trait HardwareProber: Send + Sync {
    async fn probe(&self) -> Result<crate::hardware::HardwareProbe, ApiError>;
}

/// Persistencia de `hardware_profile`: una fila por configuración (huella).
#[async_trait]
pub trait HardwareRepository: Send + Sync {
    /// Perfil de la configuración más reciente, sin recorrer el historial.
    async fn latest(&self) -> Result<Option<crate::hardware::HardwareProfile>, ApiError>;
    /// Registra un sondeo. Si la huella coincide con la del perfil más
    /// reciente, actualiza ese perfil (snapshot, `probed_at`, `row_version`+1);
    /// si no, crea uno nuevo. Devuelve el perfil resultante.
    async fn record(
        &self,
        probe: &crate::hardware::HardwareProbe,
    ) -> Result<crate::hardware::HardwareProfile, ApiError>;
    /// Página ordenada por (`created_at`, `id`) estrictamente después de `after`.
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::hardware::HardwareProfile>, ApiError>;
    async fn get(&self, id: &str) -> Result<Option<crate::hardware::HardwareProfile>, ApiError>;
}

/// Un job de refresco de hardware y quién lo pidió.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalJob {
    pub state: crate::bridge::JobState,
    pub actor_id: String,
}

/// Jobs de refresco de hardware (P011) en `operation_journal`.
#[async_trait]
pub trait JournalJobRepository: Send + Sync {
    /// Encola un job salvo que ya haya uno QUEUED o RUNNING, en cuyo caso
    /// devuelve ese (`false`): dos refrescos no sondean a la vez.
    async fn enqueue(
        &self,
        ctx: &MutationContext,
        resource_id: Option<&str>,
    ) -> Result<(JournalJob, bool), ApiError>;
    async fn running(&self, id: &str) -> Result<(), ApiError>;
    /// COMPLETED con el recurso resultante y su resultado.
    async fn complete(
        &self,
        id: &str,
        resource_id: &str,
        result: serde_json::Value,
        evidence_refs: &[String],
    ) -> Result<(), ApiError>;
    async fn fail(&self, id: &str, error: &ApiError) -> Result<(), ApiError>;
    async fn get(&self, id: &str) -> Result<Option<JournalJob>, ApiError>;
    /// Tras un reinicio, los jobs QUEUED o RUNNING no van a terminar: pasan a
    /// FAILED (`job_interrupted`). Devuelve cuántos.
    async fn recover(&self) -> Result<u64, ApiError>;
}

/// Sondeo de almacenamiento (P012): volúmenes locales, unidades de red y las
/// raíces UNC pedidas.
#[async_trait]
pub trait StorageProber: Send + Sync {
    async fn probe(
        &self,
        unc_roots: &[String],
    ) -> Result<crate::storage::StorageObservation, ApiError>;
}

/// Refresca la observación de almacenamiento y persiste el último estado de
/// las ubicaciones antes de un preflight que depende del espacio disponible.
#[async_trait]
pub trait StorageObservationRefresher: Send + Sync {
    async fn refresh(
        &self,
        ctx: &MutationContext,
    ) -> Result<crate::storage::StorageObservation, ApiError>;
}

/// Cambio de estado de una `storage_location` tras aplicar una observación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageStateChange {
    pub location_id: String,
    pub from: String,
    pub to: String,
    pub row_version: i64,
}

/// Raíz de una ubicación registrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationRoot {
    pub id: String,
    pub canonical_path: String,
    pub unc_path: Option<String>,
}

/// Lo que acompaña a una observación al aplicarla.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObservationContext {
    /// Por id de ubicación: ¿existe su carpeta raíz? Ausente = no se pudo
    /// comprobar (no se inventa MISSING).
    pub root_present: std::collections::BTreeMap<String, bool>,
    /// Umbrales de H09; `None` = sin nivel de disco.
    pub thresholds: Option<crate::locations::DiskThresholds>,
    /// Revisión de Settings que define los umbrales observados. Permite auditar
    /// qué configuración produjo una transición H09.
    pub settings_revision: Option<i64>,
    /// Ubicaciones conocidas al empezar el sondeo (sus UNC se sondearon).
    /// Una creada después no se juzga con esta observación: se la marcaría
    /// OFFLINE sin haber mirado su recurso. `None` = todas.
    pub known: Option<std::collections::BTreeSet<String>>,
}

/// Último estado observado de cada `storage_location` (P012/P014).
#[async_trait]
pub trait StorageStateRepository: Send + Sync {
    /// Raíces registradas, para comprobarlas y sondear las UNC.
    async fn roots(&self) -> Result<Vec<LocationRoot>, ApiError>;
    /// Aplica la observación: actualiza `free_bytes` y `last_seen_at` de las
    /// ubicaciones visibles y, si cambia el estado o la identidad observable,
    /// sube `row_version` y escribe `storage.state_changed` en la misma
    /// transacción. Las no vistas pasan a OFFLINE conservando sus datos.
    async fn apply(
        &self,
        observation: &crate::storage::StorageObservation,
        context: &ObservationContext,
        ctx: &MutationContext,
    ) -> Result<Vec<StorageStateChange>, ApiError>;
}

/// Comprueba si existen las carpetas raíz (FS; lo implementa `larm-storage`).
#[async_trait]
pub trait RootInspector: Send + Sync {
    /// `None` si no se pudo comprobar (p. ej., ruta de red que no responde).
    async fn present(&self, canonical_path: &str) -> Option<bool>;
}

/// Filtros de `GET /storage-locations` (contrato 07: `q` y `state`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocationFilter {
    /// Texto en el nombre o la ruta (sin distinguir mayúsculas); vacío = sin filtro.
    pub q: Option<String>,
    pub state: Option<crate::locations::LocationState>,
}

/// Persistencia de `storage_location` (P014). Cada mutación escribe su
/// evento `storage.state_changed` en el outbox en la misma transacción.
#[async_trait]
pub trait StorageLocationRepository: Send + Sync {
    async fn insert(
        &self,
        location: &crate::locations::StorageLocation,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn get(&self, id: &str) -> Result<Option<crate::locations::StorageLocation>, ApiError>;
    /// Página por (`created_at`, `id`) estrictamente después de `after`.
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &LocationFilter,
    ) -> Result<Vec<crate::locations::StorageLocation>, ApiError>;
    async fn all(&self) -> Result<Vec<crate::locations::StorageLocation>, ApiError>;
    /// Sustituye los campos editables y los calculados con `If-Match`:
    /// `NOT_FOUND` o `STALE_REVISION`.
    async fn update(
        &self,
        location: &crate::locations::StorageLocation,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<crate::locations::StorageLocation, ApiError>;
    /// Borra con `If-Match`; `CONFLICT` (`location_has_artifacts`) si alguna
    /// `artifact_location` la usa.
    async fn delete(&self, id: &str, if_match: i64, ctx: &MutationContext) -> Result<(), ApiError>;
    /// Artefactos que viven en la ubicación.
    async fn artifact_count(&self, id: &str) -> Result<u64, ApiError>;
}

/// Casos de uso de ubicaciones (P014), implementados en `larm-storage`: la
/// API no toca el disco ni la base directamente.
#[async_trait]
pub trait LocationService: Send + Sync {
    async fn create(
        &self,
        request: &crate::locations::StorageLocationCreate,
        ctx: &MutationContext,
    ) -> Result<crate::locations::StorageLocation, ApiError>;
    async fn update(
        &self,
        id: &str,
        if_match: i64,
        request: &crate::locations::StorageLocationCreate,
        ctx: &MutationContext,
    ) -> Result<crate::locations::StorageLocation, ApiError>;
    async fn delete(&self, id: &str, if_match: i64, ctx: &MutationContext) -> Result<(), ApiError>;
    async fn get(&self, id: &str) -> Result<crate::locations::StorageLocation, ApiError>;
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &LocationFilter,
    ) -> Result<Vec<crate::locations::StorageLocation>, ApiError>;
}

/// Lo que una admisión necesita en un volumen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDemand {
    pub volume_key: String,
    /// `required_free` del estimate (pico nuevo + margen).
    pub required_bytes: u64,
    /// Libre del SO observado en la instantánea.
    pub os_free: u64,
    /// Mínimo de política del volumen (mayor reserva de sus raíces o global).
    pub policy_minimum: u64,
}

/// Reservas de espacio por volumen (P015, tabla `volume_reservation`).
#[async_trait]
pub trait VolumeReservationRepository: Send + Sync {
    /// Restante de las reservas vigentes de otros trabajos en el volumen.
    async fn others(&self, volume_key: &str, exclude_job: Option<&str>) -> Result<u64, ApiError>;
    /// Admite el trabajo en una sola transacción de escritura: la instantánea
    /// de espacio debe tener menos de 5 s; en cada volumen, libre − mínimo −
    /// reservas de otros ≥ requerido (aritmética comprobada). Si todo cabe,
    /// reserva cada volumen; si no, nada (`STORAGE_INSUFFICIENT`).
    async fn admit(
        &self,
        job_id: &str,
        demands: &[VolumeDemand],
        snapshot_at: chrono::DateTime<chrono::Utc>,
        expires_at: Option<String>,
    ) -> Result<(), ApiError>;
    /// Bytes ya creados en disco: `remaining = total − created_live` (el SO ya
    /// los refleja; no se descuentan dos veces).
    async fn consume(
        &self,
        job_id: &str,
        volume_key: &str,
        created_live: u64,
    ) -> Result<(), ApiError>;
    /// Libera las reservas del trabajo (idempotente).
    async fn release(&self, job_id: &str) -> Result<u64, ApiError>;
}

/// Preflight de una descarga. La interfaz HTTP valida e identifica la
/// petición; la implementación combina catálogo, tamaños observados y espacio
/// real disponible sin crear jobs ni reservas.
#[async_trait]
pub trait DownloadPlanPreviewService: Send + Sync {
    async fn preview(
        &self,
        request: &crate::download::DownloadPlanRequest,
        ctx: &MutationContext,
    ) -> Result<crate::download::DownloadPlanResponse, ApiError>;
}

/// Catálogo remoto validado contra las relaciones y la cualificación actuales.
/// No afirma verificación de bytes ni autorización de cuenta/licencia. La
/// persistencia del plan/job vuelve a cotejar el binding dentro de su escritura.
#[async_trait]
pub trait AcquisitionManifestCatalog: Send + Sync {
    async fn latest_current(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<Option<crate::acquisition::ModelAcquisitionManifest>, ApiError>;

    async fn bound_current(
        &self,
        variant_id: &str,
        binding: &crate::acquisition::AcquisitionPlanBinding,
        ctx: &MutationContext,
    ) -> Result<crate::acquisition::ModelAcquisitionManifest, ApiError>;
}

/// Guarda un plan inmutable cuando el operador ha elegido un destino elegible.
#[async_trait]
pub trait DownloadPlanRepository: Send + Sync {
    /// Recupera el snapshot inmutable para comprobar que el catálogo y los
    /// artefactos actuales siguen correspondiendo con el plan confirmado.
    async fn get_plan(
        &self,
        plan_id: &str,
    ) -> Result<Option<crate::download::DownloadPlanRecord>, ApiError>;
    async fn store_plan(
        &self,
        record: &crate::download::DownloadPlanRecord,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
}

/// Contexto HTTP que debe quedar idempotente junto con la aceptación del plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadActivationIdempotency {
    pub key: String,
    pub body_hash: String,
}

/// Aceptación durable de un plan ya confirmado: job, reserva, journal, outbox
/// e idempotencia se escriben en una sola transacción.
#[async_trait]
pub trait DownloadPlanActivationRepository: Send + Sync {
    async fn activate(
        &self,
        request: &crate::download::ExecutePlan,
        ctx: &MutationContext,
        idempotency: &DownloadActivationIdempotency,
        snapshot_at: chrono::DateTime<chrono::Utc>,
        demands: &[VolumeDemand],
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// Gate obligatorio de P022/P043 para confirmar una descarga: verifica
/// autorización/licencia de la fuente y que exista un backend de ejecución.
#[async_trait]
pub trait DownloadPlanExecutionGate: Send + Sync {
    async fn authorize(
        &self,
        record: &crate::download::DownloadPlanRecord,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
}

/// Caso de uso que vuelve a validar el catálogo y el espacio antes de consumir
/// la confirmación explícita del operador.
#[async_trait]
pub trait DownloadPlanActivationService: Send + Sync {
    async fn activate(
        &self,
        request: &crate::download::ExecutePlan,
        ctx: &MutationContext,
        idempotency: &DownloadActivationIdempotency,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// Consulta de la cola durable de descargas, limitada al principal autenticado.
#[async_trait]
pub trait DownloadQueueService: Send + Sync {
    async fn control(
        &self,
        job_id: &str,
        command: crate::download::DownloadCommand,
        request: &crate::download::DownloadStop,
        ctx: &MutationContext,
        idempotency: &crate::download::DownloadControlIdempotency,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;

    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<crate::download::DownloadJob>, ApiError>;

    /// Cambia la prioridad con control de versión optimista. La implementación
    /// debe limitar el cambio a trabajos propiedad del actor autenticado.
    async fn update_priority(
        &self,
        job_id: &str,
        expected_row_version: i64,
        priority: crate::download::DownloadPriority,
        ctx: &MutationContext,
    ) -> Result<crate::download::DownloadJob, ApiError>;
}

/// Lock vigente de un recurso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldLock {
    pub resource_id: String,
    pub owner_id: String,
    pub generation: i64,
    pub mode: crate::guard::LockMode,
}

/// Resultado de una reconciliación de locks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reconciliation {
    /// Titulares liberados porque el journal o la lease confirman que terminaron.
    pub released: Vec<String>,
    /// Titulares que siguen vivos según el journal o la lease.
    pub kept: Vec<String>,
    /// Titulares sin journal ni lease: no se liberan solos (sin timeout arbitrario).
    pub unverified: Vec<String>,
}

/// Resource Guard (P019) sobre `resource_lock`.
#[async_trait]
pub trait ResourceGuard: Send + Sync {
    /// Adquiere todos los locks (orden global) o ninguno. Si `journal` existe,
    /// lo escribe en la misma transacción. Conflicto con una lease viva:
    /// `ACTIVE_LEASE`; con otro trabajo: `CONFLICT` (`resource_locked`).
    async fn acquire(
        &self,
        owner_id: &str,
        generation: i64,
        requests: &[(crate::guard::Resource, crate::guard::LockMode)],
        journal: Option<&crate::guard::JournalStep>,
    ) -> Result<(), ApiError>;
    /// Libera solo los locks de ese titular y generación; con `journal`,
    /// escribe su fase en la misma transacción. Devuelve cuántos.
    async fn release(
        &self,
        owner_id: &str,
        generation: i64,
        journal: Option<&crate::guard::JournalStep>,
    ) -> Result<u64, ApiError>;
    /// Fencing: ¿sigue ese titular/generación teniendo el recurso en ese modo?
    async fn holds(
        &self,
        owner_id: &str,
        generation: i64,
        resource: &crate::guard::Resource,
        mode: crate::guard::LockMode,
    ) -> Result<bool, ApiError>;
    async fn holders(&self, resource: &crate::guard::Resource) -> Result<Vec<HeldLock>, ApiError>;
    /// Libera los locks cuyos titulares están terminados según el journal o la lease.
    async fn reconcile(&self) -> Result<Reconciliation, ApiError>;
    /// Motivos por los que un artefacto no se puede borrar: variantes que lo
    /// referencian (CAS compartido) y locks vigentes. Vacío = borrable.
    async fn delete_blockers(&self, artifact_id: &str) -> Result<Vec<String>, ApiError>;
}

/// Una reserva solicitada en un pool de memoria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolReservationRequest {
    pub pool_key: String,
    pub bytes: u64,
    pub budget: u64,
}

/// Reservas de memoria por pool (P019; las consume la admisión de P100).
#[async_trait]
pub trait PoolReservations: Send + Sync {
    /// Reserva `bytes` en el pool si la suma con lo ya reservado no supera
    /// `budget` (aritmética comprobada, atómico). Repetir sustituye.
    async fn reserve(
        &self,
        owner_id: &str,
        pool_key: &str,
        bytes: u64,
        budget: u64,
    ) -> Result<(), ApiError>;
    /// Sustituye todas las reservas del titular en una sola operación. Todos
    /// los pools se validan antes de escribir; un fallo no deja reservas a
    /// medias ni elimina las que ya tenía el titular.
    async fn reserve_many(
        &self,
        owner_id: &str,
        requests: &[PoolReservationRequest],
    ) -> Result<(), ApiError>;
    async fn release(&self, owner_id: &str) -> Result<u64, ApiError>;
    async fn reserved(&self, pool_key: &str) -> Result<u64, ApiError>;
}

/// Ubicación de un artefacto de una variante, con lo que el plan de
/// movimiento necesita saber (P016).
#[derive(Debug, Clone, PartialEq)]
pub struct VariantArtifact {
    pub location: crate::artifacts::ArtifactLocation,
    /// Variantes que usan el artefacto: la suya más las de
    /// `artifact_reference` (más de una = blob compartido).
    pub referenced_by: u64,
}

/// Cambio de registro de una `artifact_location` (P016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationSwitch {
    pub artifact_location_id: String,
    /// CAS: la fila debe seguir en esta versión.
    pub expected_row_version: i64,
    pub storage_location_id: String,
    pub relative_path: String,
    pub sha256: Option<String>,
    pub file_size: Option<u64>,
    pub verified_at: Option<String>,
}

/// Copia física de un fichero duplicado.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DuplicateCopy {
    pub storage_location_id: String,
    pub volume_key: String,
    pub relative_path: String,
    pub artifact_location_ids: Vec<String>,
}

/// Grupo de duplicados por hash (P017). `reclaimable_bytes` solo cuenta
/// copias de más **en el mismo volumen** (entre volúmenes no se asume
/// deduplicación física); `duplicate_bytes`, todas las copias de más.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DuplicateGroup {
    pub sha256: String,
    pub size_bytes: u64,
    pub copies: Vec<DuplicateCopy>,
    pub duplicate_bytes: u64,
    pub reclaimable_bytes: u64,
}

/// Fencing de una escritura: solo vale si `owner_id`/`generation` sigue
/// teniendo `resource` en exclusiva.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub owner_id: String,
    pub generation: i64,
    pub resource: crate::guard::Resource,
}

/// Repositorio de `artifact_location` (P016).
#[async_trait]
pub trait ArtifactLocationRepository: Send + Sync {
    /// Página ordenada por `(created_at, id)` tras `after`.
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::artifacts::ArtifactLocation>, ApiError>;
    async fn get(&self, id: &str) -> Result<Option<crate::artifacts::ArtifactLocation>, ApiError>;
    /// Ubicaciones de los artefactos de la variante; `None` si la variante no existe.
    async fn for_variant(&self, variant_id: &str)
        -> Result<Option<Vec<VariantArtifact>>, ApiError>;
    /// Alta de ubicaciones (importación P017) en una transacción, con un
    /// evento por fila nueva. Idempotente: una fila con la misma
    /// `(artifact_id, storage_location_id, relative_path)` no se duplica.
    /// Devuelve cuántas se crearon.
    async fn insert_locations(
        &self,
        rows: &[crate::artifacts::ArtifactLocation],
        ctx: &MutationContext,
    ) -> Result<usize, ApiError>;
    /// Filas en esa ruta de esa ubicación (un blob CAS puede tener varias).
    async fn at_path(
        &self,
        storage_location_id: &str,
        relative_path: &str,
    ) -> Result<Vec<crate::artifacts::ArtifactLocation>, ApiError>;
    /// Pasa a `SHARED` las filas `LARM_MANAGED` de esa ruta (blob con más de
    /// una referencia), con CAS y evento. Devuelve cuántas cambió.
    async fn mark_shared(
        &self,
        storage_location_id: &str,
        relative_path: &str,
        ctx: &MutationContext,
    ) -> Result<usize, ApiError>;
    /// Ficheros con el mismo sha256 en más de un sitio físico, con el
    /// ahorro posible (sin borrar nada).
    async fn duplicates(&self) -> Result<Vec<DuplicateGroup>, ApiError>;
    /// Filas de una ubicación (P018), por ruta.
    async fn by_location(
        &self,
        storage_location_id: &str,
    ) -> Result<Vec<crate::artifacts::ArtifactLocation>, ApiError>;
    /// Cambia el estado de una fila (reconciliación P018) con CAS y evento.
    /// `verified_at`, si llega, se guarda; si no, se conserva.
    async fn set_state(
        &self,
        id: &str,
        expected_row_version: i64,
        state: crate::artifacts::ArtifactState,
        verified_at: Option<&str>,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Variantes que usan algún artefacto con filas en la ubicación.
    async fn variants_at(&self, storage_location_id: &str) -> Result<Vec<String>, ApiError>;
    /// Cambia el registro de todas las filas o de ninguna, en una transacción
    /// con el bloqueo de escritura: comprueba el fencing, aplica el CAS de
    /// cada fila (`STALE_REVISION` si alguna cambió), escribe `journal` y un
    /// evento `artifact.moved` por fila. Devuelve las versiones nuevas.
    async fn switch(
        &self,
        switches: &[LocationSwitch],
        fence: &Fence,
        journal: &crate::guard::JournalStep,
        ctx: &MutationContext,
    ) -> Result<Vec<i64>, ApiError>;
}

/// Entrada de `operation_journal` de un movimiento (P016).
#[derive(Debug, Clone, PartialEq)]
pub struct MoveRecord {
    pub id: String,
    pub phase: String,
    pub plan_hash: String,
    pub resource_ids: Vec<String>,
    pub payload: Value,
    pub updated_at: String,
}

/// Jobs con journal todavía vivos (no terminados), de cualquier tipo. Sus
/// provisionales no se limpian (P018).
#[async_trait]
pub trait LiveJobs: Send + Sync {
    async fn live_jobs(&self) -> Result<std::collections::BTreeSet<String>, ApiError>;
}

/// Journal durable de los movimientos (P016).
#[async_trait]
pub trait MoveJournal: Send + Sync {
    /// Escribe el paso; con `from_phases`, solo desde esas fases
    /// (`CONFLICT` `journal_phase_conflict` si no).
    async fn write(&self, step: &crate::guard::JournalStep) -> Result<(), ApiError>;
    async fn get(&self, id: &str) -> Result<Option<MoveRecord>, ApiError>;
    /// Plan `PLANNED` más reciente con ese hash.
    async fn find_planned(&self, plan_hash: &str) -> Result<Option<MoveRecord>, ApiError>;
    /// Movimientos empezados y sin terminar (ni `PLANNED` ni terminales).
    async fn unfinished(&self) -> Result<Vec<MoveRecord>, ApiError>;
}

/// Trabajo de fondo que la API lanza con `tokio::spawn`.
pub type BackgroundTask = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Movimiento de modelos visto desde la API (P016).
#[async_trait]
pub trait ModelMover: Send + Sync {
    /// Plan sin efectos (`dry_run`), guardado como `PLANNED`.
    async fn plan(
        &self,
        variant_id: &str,
        target_storage_location_id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::artifacts::Plan, ApiError>;
    /// Plan `PLANNED` vigente con ese hash, si existe.
    async fn planned(&self, plan_hash: &str) -> Result<Option<crate::artifacts::Plan>, ApiError>;
    /// Ejecuta el plan: locks, lease, reserva; devuelve el plan y el job.
    async fn execute(
        &self,
        variant_id: &str,
        request: &crate::artifacts::MoveRequest,
        ctx: &MutationContext,
    ) -> Result<(crate::artifacts::Plan, BackgroundTask), ApiError>;
}

/// Filtros de `GET /models` (contrato 07: `q`, `capability`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelFilter {
    pub q: Option<String>,
    pub capability: Option<String>,
}

/// Filtros de `GET /model-distributions` (`q`, `source_id`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistributionFilter {
    pub q: Option<String>,
    pub source_id: Option<String>,
}

/// Filtros de `GET /model-revisions` (`state`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionFilter {
    pub state: Option<crate::model::RevisionState>,
}

/// Campos de evidencia por los que se buscan candidatos de identidad (P041).
pub const EVIDENCE_FIELDS: [&str; 6] = [
    "architecture",
    "author",
    "base_model",
    "config_hash",
    "tokenizer_hash",
    "upstream_identity",
];

/// Registro de modelos, distribuciones, revisiones y procedencia (P040).
/// Cada alta valida el dominio, respeta la identidad del contrato 01 y
/// escribe su evento en la misma transacción.
#[async_trait]
pub trait ModelRepository: Send + Sync {
    async fn list_models(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &ModelFilter,
    ) -> Result<Vec<crate::model::CanonicalModel>, ApiError>;
    async fn get_model(&self, id: &str) -> Result<Option<crate::model::CanonicalModel>, ApiError>;
    /// Modelo con esa identidad upstream explícita, si existe.
    async fn model_by_upstream(
        &self,
        upstream_identity: &str,
    ) -> Result<Option<crate::model::CanonicalModel>, ApiError>;
    /// Modelos con el mismo nombre sin distinguir mayúsculas (candidatos a
    /// `possible_duplicates`, nunca a fusión).
    async fn models_named(&self, name: &str)
        -> Result<Vec<crate::model::CanonicalModel>, ApiError>;
    /// Modelos cuya evidencia de identidad guardada
    /// (`metadata.identity_evidence.<field>`) vale `value`, sin distinguir
    /// mayúsculas. `field` es uno de [`EVIDENCE_FIELDS`]; otro es un error.
    async fn models_by_evidence(
        &self,
        field: &str,
        value: &str,
    ) -> Result<Vec<crate::model::CanonicalModel>, ApiError>;
    async fn insert_model(
        &self,
        model: &crate::model::CanonicalModel,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    /// Añade `duplicate` a `possible_duplicates` de `id` (idempotente).
    async fn add_possible_duplicate(
        &self,
        id: &str,
        duplicate: &str,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn list_distributions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &DistributionFilter,
    ) -> Result<Vec<crate::model::ModelDistribution>, ApiError>;
    async fn get_distribution(
        &self,
        id: &str,
    ) -> Result<Option<crate::model::ModelDistribution>, ApiError>;
    async fn distribution_by_identity(
        &self,
        source_id: &str,
        upstream_namespace: &str,
        upstream_model_id: &str,
    ) -> Result<Option<crate::model::ModelDistribution>, ApiError>;
    async fn insert_distribution(
        &self,
        distribution: &crate::model::ModelDistribution,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn list_revisions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RevisionFilter,
    ) -> Result<Vec<crate::model::ModelRevision>, ApiError>;
    async fn get_revision(&self, id: &str)
        -> Result<Option<crate::model::ModelRevision>, ApiError>;
    async fn revision_by_key(
        &self,
        distribution_id: &str,
        revision_key: &str,
    ) -> Result<Option<crate::model::ModelRevision>, ApiError>;
    /// Alta; `CONFLICT` `current_revision_exists` si la distribución ya tiene
    /// una revisión CURRENT y esta también lo es.
    async fn insert_revision(
        &self,
        revision: &crate::model::ModelRevision,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    /// Alta de una revisión CURRENT que sustituye a la CURRENT anterior de
    /// su distribución (pasa a SUPERSEDED), en una transacción. Para fuentes
    /// cuyo contenido cambia sin otra identidad (carpetas locales).
    async fn replace_current_revision(
        &self,
        revision: &crate::model::ModelRevision,
        ctx: &MutationContext,
    ) -> Result<Option<String>, ApiError>;
    async fn insert_provenance(
        &self,
        provenance: &crate::model::ArtifactProvenance,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn provenance_for(
        &self,
        artifact_id: &str,
    ) -> Result<Vec<crate::model::ArtifactProvenance>, ApiError>;
}

/// Filtros de `GET /variants` y `/models/{id}/variants` (`state` = ciclo de
/// vida, `capability`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariantFilter {
    pub state: Option<String>,
    pub capability: Option<String>,
    /// Solo las variantes de este modelo canónico.
    pub model_id: Option<String>,
}

/// Variantes, artefactos y bundles (P041).
#[async_trait]
pub trait VariantRepository: Send + Sync {
    async fn list_variants(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &VariantFilter,
    ) -> Result<Vec<crate::model::ModelVariant>, ApiError>;
    async fn get_variant(&self, id: &str) -> Result<Option<crate::model::ModelVariant>, ApiError>;
    /// Detalle contractual de una variante, incluyendo relaciones asociadas.
    async fn variant_detail(&self, id: &str) -> Result<Option<Value>, ApiError>;
    /// Registra la selección de una variante (DISCOVERED → SELECTED) con CAS y evento.
    /// Repetir la selección de una variante ya SELECTED es idempotente.
    async fn select_variant(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::model::ModelVariant, ApiError>;
    /// Variante con esa identidad (contrato 01).
    async fn find_variant(
        &self,
        revision_id: &str,
        format: Option<&str>,
        quantization: Option<&str>,
        architecture: Option<&str>,
        bundle_hash: Option<&str>,
    ) -> Result<Option<crate::model::ModelVariant>, ApiError>;
    /// Alta atómica de la variante, sus artefactos y su bundle, con eventos.
    async fn insert_variant(
        &self,
        variant: &crate::model::ModelVariant,
        artifacts: &[crate::model::ModelArtifact],
        bundle: Option<&crate::model::ModelBundle>,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn list_artifacts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        q: Option<&str>,
    ) -> Result<Vec<crate::model::ModelArtifact>, ApiError>;
    async fn get_artifact(&self, id: &str)
        -> Result<Option<crate::model::ModelArtifact>, ApiError>;
    async fn artifacts_of(
        &self,
        variant_id: &str,
    ) -> Result<Vec<crate::model::ModelArtifact>, ApiError>;
    async fn list_bundles(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        state: Option<crate::model::BundleState>,
    ) -> Result<Vec<crate::model::ModelBundle>, ApiError>;
    async fn get_bundle(&self, id: &str) -> Result<Option<crate::model::ModelBundle>, ApiError>;
    async fn bundle_of(
        &self,
        variant_id: &str,
    ) -> Result<Option<crate::model::ModelBundle>, ApiError>;
    /// Physical descriptor projection for verification. Local digests/sizes
    /// must not overwrite source declarations returned by `artifacts_of`.
    /// Remote implementations resolve immutable completed acquisition evidence.
    async fn bundle_artifacts(
        &self,
        bundle: &crate::model::ModelBundle,
    ) -> Result<Vec<crate::model::ModelArtifact>, ApiError> {
        Ok(self
            .artifacts_of(&bundle.variant_id)
            .await?
            .into_iter()
            .filter(|a| {
                bundle.required_artifact_ids.contains(&a.id)
                    || bundle.optional_artifact_ids.contains(&a.id)
            })
            .collect())
    }
    /// Cambia `availability` de la variante con CAS y evento, fusionando
    /// `metadata_patch` (claves con `null` se borran). Nunca toca la etapa
    /// del ciclo de vida.
    async fn set_availability(
        &self,
        id: &str,
        expected_row_version: i64,
        availability: &str,
        metadata_patch: Value,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Promueve una variante en la máquina de estados P120; `DEFAULT` es
    /// exclusivo por (revision, format, quantization) y rebaja la previa.
    async fn set_promotion(
        &self,
        id: &str,
        expected_row_version: i64,
        promotion: &str,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Registra corrupción confirmada: lifecycle y availability pasan a
    /// `CORRUPT` en la misma escritura CAS y con evento.
    async fn mark_variant_corrupt(
        &self,
        id: &str,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Tras verificar de nuevo todos los artefactos requeridos, vuelve de
    /// `CORRUPT` a `VERIFIED` y deja availability en `UNKNOWN` hasta recalcular
    /// los gates técnicos.
    async fn finish_variant_reverification(
        &self,
        id: &str,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Borra la variante y sus artefactos, bundles, referencias, procedencia
    /// y ubicaciones en una transacción (P018), con CAS y evento. Si algo
    /// más la referencia (instancias, compatibilidad…), `CONFLICT`
    /// `variant_in_use` y no se borra nada.
    async fn delete_variant(
        &self,
        id: &str,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    /// Cambia el estado del bundle con CAS por `row_version` y evento.
    async fn set_bundle_state(
        &self,
        id: &str,
        expected_row_version: i64,
        state: crate::model::BundleState,
        evidence: Value,
        ctx: &MutationContext,
    ) -> Result<i64, ApiError>;
    /// Modelos canónicos con alguna variante que tenga un artefacto con
    /// alguno de estos sha256 (candidatos para la resolución de identidad).
    async fn models_with_artifact_hashes(
        &self,
        hashes: &[String],
    ) -> Result<Vec<crate::model::CanonicalModel>, ApiError>;
}

/// Enumera y valida las fuentes actuales que pueden entregar todos los
/// artefactos seleccionados con el mismo hash y tamaño exacto.
#[async_trait]
pub trait DownloadMirrorCatalog: Send + Sync {
    async fn eligible_candidates(
        &self,
        model_id: &str,
        selected_artifacts: &[crate::download::DownloadArtifactEstimate],
    ) -> Result<Vec<crate::download::DownloadSourceCandidate>, ApiError>;
}

/// Lectura de registros de compatibilidad (P090). Esta operación no calcula
/// gates ni crea evidencia; devuelve únicamente evaluaciones persistidas.
#[async_trait]
pub trait CompatibilityRepository: Send + Sync {
    async fn list_all_compatibility(
        &self,
    ) -> Result<Vec<crate::compatibility::CompatibilityRecord>, ApiError>;
}

/// Mantenimiento del almacenamiento visto desde la API (P018).
#[async_trait]
pub trait StorageMaintenance: Send + Sync {
    /// `StorageAnalysis` de la ubicación.
    async fn analysis(&self, storage_location_id: &str) -> Result<Value, ApiError>;
    async fn cleanup_plan(
        &self,
        storage_location_id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::artifacts::Plan, ApiError>;
    /// Plan de limpieza vigente (para validar la aprobación antes de usarlo).
    async fn planned_cleanup(
        &self,
        storage_location_id: &str,
        plan_id: &str,
        plan_hash: &str,
    ) -> Result<crate::artifacts::Plan, ApiError>;
    async fn cleanup(
        &self,
        storage_location_id: &str,
        plan_id: &str,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(crate::bridge::JobReceipt, BackgroundTask), ApiError>;
    async fn delete_plan(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::artifacts::Plan, ApiError>;
    async fn planned_delete(
        &self,
        variant_id: &str,
        plan_hash: &str,
    ) -> Result<crate::artifacts::Plan, ApiError>;
    async fn delete(
        &self,
        variant_id: &str,
        if_match: i64,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Value, ApiError>;
}

/// Persistencia de `adapter_installation`. Cada mutación escribe su evento
/// (`adapter.updated`) en la misma transacción.
#[async_trait]
pub trait AdapterInstallationRepository: Send + Sync {
    async fn insert(
        &self,
        installation: &crate::adapters::AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn get(&self, id: &str)
        -> Result<Option<crate::adapters::AdapterInstallation>, ApiError>;
    /// La instalación de esa versión, si existe.
    async fn find(
        &self,
        adapter_id: &str,
        version: &str,
    ) -> Result<Option<crate::adapters::AdapterInstallation>, ApiError>;
}

/// P026: confirmación de evidencia producida por el ejecutor interno, después
/// de cerrar/esperar todo su IO. No acepta recibos externos ni ejecuta workers.
/// CAS, journal y outbox son atómicos; un retry idéntico devuelve el recibo
/// histórico y nunca vuelve a marcar la instalación como válida.
#[async_trait]
pub trait AdapterConformanceRepository: Send + Sync {
    /// Conformidad y estado terminal del sondeo público se confirman juntos.
    async fn commit_probe_conformance(
        &self,
        job_id: &str,
        run: &crate::conformance::Run,
        outcome: &crate::conformance::Outcome,
        ctx: &MutationContext,
    ) -> Result<crate::conformance::Receipt, ApiError>;
    async fn commit_conformance(
        &self,
        run: &crate::conformance::Run,
        outcome: &crate::conformance::Outcome,
        ctx: &MutationContext,
    ) -> Result<crate::conformance::Receipt, ApiError>;
    async fn conformance_receipt(
        &self,
        run_id: &str,
    ) -> Result<Option<crate::conformance::Receipt>, ApiError>;
}

/// Ejecutar conformidad es un puerto de infraestructura. Su implementación
/// debe medir bytes/permisos/contexto, aplicar P022 y los oráculos completos,
/// y esperar todo IO antes de devolver. Nunca importar un recibo externo PASS.
#[async_trait]
pub trait AdapterConformanceRunner: Send + Sync {
    /// Espera IO retenido aun cuando execute devolvió un error de cleanup.
    async fn wait_conformance(&self, installation_id: &str) -> Result<(), ApiError>;
    async fn execute_conformance(
        &self,
        installation: &crate::adapters::AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<crate::conformance::Execution, ApiError>;
}

#[async_trait]
pub trait AdapterProbeRepository: Send + Sync {
    async fn enqueue_probe(
        &self,
        target: &str,
        request: &crate::hardware::Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::adapter_probes::ProbeJob, ApiError>;
    async fn claim_probe(&self) -> Result<Option<crate::adapter_probes::ProbeJob>, ApiError>;
    async fn probe_job(
        &self,
        id: &str,
    ) -> Result<Option<crate::adapter_probes::ProbeJob>, ApiError>;
    /// Sólo después de confirmar el cierre del IO. Nunca concede VALIDATED.
    async fn fail_probe(
        &self,
        id: &str,
        error: &ApiError,
        authority_pending: bool,
    ) -> Result<(), ApiError>;
}
#[async_trait]
pub trait AdapterProbeExecutor: Send + Sync {
    async fn execute_probe(
        &self,
        job: &crate::adapter_probes::ProbeJob,
    ) -> Result<crate::conformance::Receipt, crate::adapter_probes::ProbeFailure>;
    async fn wait_probe_io(&self, installation_id: &str) -> Result<(), ApiError>;
}
#[async_trait]
pub trait AdapterProber: JobSource {
    async fn request_probe(
        self: std::sync::Arc<Self>,
        target: &str,
        request: &crate::hardware::Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// P026: autorización física interna para consumir una instalación ya validada.
/// El repositorio proporciona el Run de su journal verificado, nunca del cliente.
/// No inicia plugins ni convierte una medición de bytes en conformidad funcional.
#[async_trait]
pub trait AdapterQualification: Send + Sync {
    async fn acquire(
        &self,
        installation: &crate::adapters::AdapterInstallation,
        run: &crate::conformance::Run,
        ctx: &MutationContext,
    ) -> Result<Box<dyn AdapterQualificationGuard>, ApiError>;
}

/// Conserva admisión y recursos físicos hasta que acaba la operación consumidora.
/// Se vuelve a comprobar inmediatamente antes de COMMIT/uso, también en no-op.
#[async_trait]
pub trait AdapterQualificationGuard: Send + Sync {
    async fn recheck(&self) -> Result<(), ApiError>;
}

/// Lectura paginada del registro, separada del puerto de alta de P023.
#[async_trait]
pub trait AdapterCatalogRepository: AdapterInstallationRepository {
    async fn page(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &crate::adapters::AdapterFilter,
    ) -> Result<Vec<crate::adapters::AdapterInstallation>, ApiError>;
}

/// Selección con CAS: lectura, decisión, journal y outbox en una transacción.
/// `expected_row_version = 0` sólo sirve para la primera promoción. No hay
/// operación de este puerto que conceda VALIDATED: lo aporta P026.
#[async_trait]
pub trait AdapterSelectionRepository: Send + Sync {
    async fn selection(
        &self,
        adapter_id: &str,
    ) -> Result<Option<crate::adapters::AdapterSelection>, ApiError>;
    async fn change_selection(
        &self,
        adapter_id: &str,
        expected_row_version: i64,
        change: &crate::adapters::AdapterSelectionChange,
        ctx: &MutationContext,
    ) -> Result<crate::adapters::AdapterSelection, ApiError>;
}

/// P024: journal durable de instalación. La finalización confirma candidato,
/// job, evento/outbox y liberación de reservas/locks en una transacción.
#[async_trait]
pub trait AdapterUpdateRepository: Send + Sync {
    /// Clave por actor/operación; mismo cuerpo y destino devuelve el receipt
    /// original (QUEUED), aunque el job ya haya terminado. bool = trabajo nuevo.
    async fn enqueue_update(
        &self,
        target: &str,
        request: &crate::adapter_updates::AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<(crate::bridge::JobReceipt, bool), ApiError>;
    async fn update_job(
        &self,
        id: &str,
    ) -> Result<Option<crate::adapter_updates::AdapterUpdate>, ApiError>;
    async fn pending_updates(&self)
        -> Result<Vec<crate::adapter_updates::AdapterUpdate>, ApiError>;
    /// Siguiente QUEUED por UUIDv7, sin cargar todos los planes en memoria.
    async fn queued_update(
        &self,
    ) -> Result<Option<crate::adapter_updates::AdapterUpdate>, ApiError> {
        Ok(self
            .pending_updates()
            .await?
            .into_iter()
            .find(|r| r.step == crate::adapter_updates::UpdateStep::Queued))
    }
    async fn advance_update(
        &self,
        id: &str,
        from: crate::adapter_updates::UpdateStep,
        to: crate::adapter_updates::UpdateStep,
    ) -> Result<(), ApiError>;
    /// Reserva, locks y consumo de aprobación se confirman junto al plan.
    async fn prepare_update(
        &self,
        id: &str,
        plan: &crate::adapter_updates::UpdatePlan,
        demand: &VolumeDemand,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ApiError>;
    /// Sólo después de verificar los bytes publicados. No promueve ni valida.
    async fn complete_update(&self, id: &str) -> Result<(), ApiError>;
    /// Sólo después de confirmar que ya no existe IO en curso. Nunca usarla
    /// como cancelación de un hilo todavía activo o por un timeout de observación.
    async fn fail_update(&self, id: &str, error: &ApiError) -> Result<(), ApiError>;
}

/// P024: medición de la raíz gestionada reutilizando P012/P014/P015 en la
/// composición. UNKNOWN, offline o read-only se rechazan; no inventa volumen.
#[async_trait]
pub trait AdapterPackageSpace: Send + Sync {
    async fn measure_package_space(
        &self,
        root: &str,
        required_bytes: u64,
    ) -> Result<(VolumeDemand, chrono::DateTime<chrono::Utc>), ApiError>;
}

/// P024: API del caso de uso. La aceptación sobrevive al cierre del request;
/// cada trabajo se ejecuta bajo supervisión y JobSource filtra por propietario.
#[async_trait]
pub trait AdapterUpdater: JobSource {
    async fn request_update(
        self: std::sync::Arc<Self>,
        target: &str,
        request: &crate::adapter_updates::AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// P028: journal durable de importación inicial de un paquete firmado.
/// La publicación física siempre se confirma con la fila INSTALLED y el
/// evento outbox en la misma transacción; no promueve la instalación.
#[async_trait]
pub trait AdapterImportRepository: Send + Sync {
    async fn enqueue_import(
        &self,
        request: &crate::adapter_updates::AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<(crate::bridge::JobReceipt, bool), ApiError>;
    async fn import_job(
        &self,
        id: &str,
    ) -> Result<Option<crate::adapter_updates::AdapterImportJournal>, ApiError>;
    async fn pending_imports(
        &self,
    ) -> Result<Vec<crate::adapter_updates::AdapterImportJournal>, ApiError>;
    async fn queued_import(
        &self,
    ) -> Result<Option<crate::adapter_updates::AdapterImportJournal>, ApiError> {
        Ok(self
            .pending_imports()
            .await?
            .into_iter()
            .find(|r| r.step == crate::adapter_updates::UpdateStep::Queued))
    }
    async fn advance_import(
        &self,
        id: &str,
        from: crate::adapter_updates::UpdateStep,
        to: crate::adapter_updates::UpdateStep,
    ) -> Result<(), ApiError>;
    async fn prepare_import(
        &self,
        id: &str,
        plan: &crate::adapter_updates::AdapterImportPlan,
        demand: &VolumeDemand,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ApiError>;
    async fn complete_import(&self, id: &str) -> Result<(), ApiError>;
    async fn fail_import(&self, id: &str, error: &ApiError) -> Result<(), ApiError>;
}

/// P028: cola en segundo plano y acceso sanitizado a los jobs de importación.
#[async_trait]
pub trait AdapterImporter: JobSource {
    async fn request_import(
        self: std::sync::Arc<Self>,
        request: &crate::adapter_updates::AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}

/// P028: exporta una instalación verificada a una ruta elegida por el operador.
#[async_trait]
pub trait AdapterPackageExporter: Send + Sync {
    async fn export_package(
        &self,
        request: &crate::adapter_packages::AdapterExport,
        ctx: &MutationContext,
    ) -> Result<crate::adapter_packages::AdapterExportReceipt, ApiError>;
}

/// P043: catálogo. La creación incluye replay/outbox atómicos. El registro
/// local de binding deriva capabilities del manifest, sin dar VALIDATED.
#[async_trait]
pub trait SourceCatalogRepository: Send + Sync {
    /// Replay original sin depender de que la fuente/binding aún exista.
    /// La creación vuelve a comprobarlo bajo el lock de escritura.
    async fn source_replay(
        &self,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Option<crate::sources::ModelSource>, ApiError>;
    async fn register_source_adapter(
        &self,
        installation: &str,
        source_type: &str,
        metadata: &serde_json::Map<String, Value>,
        ctx: &MutationContext,
    ) -> Result<crate::sources::ModelSourceAdapter, ApiError>;
    async fn source_adapter(
        &self,
        id: &str,
    ) -> Result<Option<crate::sources::ModelSourceAdapter>, ApiError>;
    async fn create_source(
        &self,
        request: &crate::sources::ModelSourceCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<crate::sources::ModelSource, ApiError>;
    async fn source(&self, id: &str) -> Result<Option<crate::sources::ModelSource>, ApiError>;
    async fn page_sources(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &crate::sources::SourceFilter,
    ) -> Result<Vec<crate::sources::ModelSource>, ApiError>;
    async fn update_source(
        &self,
        id: &str,
        if_match: i64,
        request: &crate::sources::ModelSourceCreate,
        ctx: &MutationContext,
    ) -> Result<crate::sources::ModelSource, ApiError>;
    async fn delete_source(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
}

/// Persistencia y ciclo de ejecución de `source_watch` (P050). Las
/// observaciones, notificaciones y eventos de un sync se confirman con el
/// avance del cursor en una transacción.
#[async_trait]
pub trait SourceWatchRepository: Send + Sync {
    async fn source_watch(
        &self,
        id: &str,
    ) -> Result<Option<crate::watchers::SourceWatch>, ApiError>;
    async fn page_source_watches(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::watchers::SourceWatch>, ApiError>;
    async fn create_source_watch(
        &self,
        request: &crate::watchers::SourceWatchCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<crate::watchers::SourceWatch, ApiError>;
    async fn update_source_watch(
        &self,
        id: &str,
        if_match: i64,
        request: &crate::watchers::SourceWatchCreate,
        ctx: &MutationContext,
    ) -> Result<crate::watchers::SourceWatch, ApiError>;
    async fn delete_source_watch(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn due_source_watches(
        &self,
        now: &str,
        limit: u32,
    ) -> Result<Vec<crate::watchers::SourceWatch>, ApiError>;
    async fn recover_interrupted_source_syncs(
        &self,
        recovered_at: &str,
        ctx: &MutationContext,
    ) -> Result<u64, ApiError>;
    async fn start_source_watch_sync(
        &self,
        id: &str,
        if_match: i64,
        started_at: &str,
        ctx: &MutationContext,
    ) -> Result<(), ApiError>;
    async fn record_source_watch_success(
        &self,
        id: &str,
        if_match: i64,
        commit: &crate::watchers::SourceWatchSyncSuccess,
        ctx: &MutationContext,
    ) -> Result<crate::watchers::SourceWatch, ApiError>;
    async fn record_source_watch_failure(
        &self,
        id: &str,
        if_match: i64,
        commit: &crate::watchers::SourceWatchSyncFailure,
        ctx: &MutationContext,
    ) -> Result<crate::watchers::SourceWatch, ApiError>;
}

/// P045: cuentas y consentimientos de licencia pertenecientes a una fuente.
/// Los secretos se resuelven aparte mediante `credential_reference_id`.
#[async_trait]
pub trait SourceAccountRepository: Send + Sync {
    async fn create_source_account(
        &self,
        account: &crate::source_accounts::ModelSourceAccount,
        ctx: &MutationContext,
    ) -> Result<crate::source_accounts::ModelSourceAccount, ApiError>;
    async fn source_account(
        &self,
        id: &str,
    ) -> Result<Option<crate::source_accounts::ModelSourceAccount>, ApiError>;
    async fn page_source_accounts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<crate::source_accounts::ModelSourceAccount>, ApiError>;
    /// Persiste una referencia Basic y la vincula a la cuenta en una sola
    /// transacción; SQLite nunca recibe el contenido secreto.
    async fn set_basic_credential(
        &self,
        account_id: &str,
        credential: &crate::credential::CredentialReference,
        ctx: &MutationContext,
    ) -> Result<crate::credential::CredentialReference, ApiError>;
    async fn set_source_license(
        &self,
        source_id: &str,
        request: &crate::source_accounts::CheckSourceLicense,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<crate::source_accounts::SourceLicenseReceipt, ApiError>;
    async fn delete_source_account(&self, id: &str, ctx: &MutationContext) -> Result<(), ApiError>;
}

/// Caso de uso para guardar Basic en el almacén protegido y asociar solo su
/// referencia al registro de cuenta.
#[async_trait]
pub trait SourceBasicCredentialWriter: Send + Sync {
    async fn write_basic_credential(
        &self,
        source_id: &str,
        request: &crate::source_credentials::BasicCredentialWrite,
        ctx: &MutationContext,
    ) -> Result<crate::credential::CredentialReference, ApiError>;
}

/// P045: journal durable y aislamiento por propietario para source.auth_probe.
#[async_trait]
pub trait SourceProbeRepository: Send + Sync {
    async fn enqueue_source_probe(
        &self,
        target: &str,
        request: &crate::hardware::Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::source_probes::SourceProbeJob, ApiError>;
    async fn claim_source_probe(
        &self,
    ) -> Result<Option<crate::source_probes::SourceProbeJob>, ApiError>;
    async fn source_probe_job(
        &self,
        id: &str,
    ) -> Result<Option<crate::source_probes::SourceProbeJob>, ApiError>;
    async fn finish_source_probe(
        &self,
        id: &str,
        result: &crate::source_probes::SourceAuthProbeResult,
    ) -> Result<(), ApiError>;
    async fn fail_source_probe(
        &self,
        id: &str,
        error: &ApiError,
        authority_pending: bool,
    ) -> Result<(), ApiError>;
}

#[async_trait]
pub trait SourceAuthProbeExecutor: Send + Sync {
    async fn execute_source_probe(
        &self,
        job: &crate::source_probes::SourceProbeJob,
    ) -> Result<crate::source_probes::SourceAuthProbeResult, crate::source_probes::SourceProbeFailure>;
    async fn wait_source_probe_io(&self, source_id: &str) -> Result<(), ApiError>;
}

#[async_trait]
pub trait SourceProber: JobSource {
    async fn request_source_probe(
        self: std::sync::Arc<Self>,
        target: &str,
        request: &crate::hardware::Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<crate::bridge::JobReceipt, ApiError>;
}
