//! Puente Broker–LARM sobre un runtime local adoptado. LARM no instala, lanza
//! ni mata este runtime: lo adopta tal cual está, lo describe con lo que puede
//! verificar por su API y solo publica como BROKER_ELIGIBLE lo que el operador
//! ha declarado y aceptado expresamente (modo EXTERNAL_LOCAL_TRUSTED del
//! contrato 04).
//!
//! Inventario de ejecución:
//! El hardware y los drivers proceden del último perfil persistido (P011/P013).
//! Sin perfil o sin versiones observadas se publican como UNAVAILABLE.
//!
//! Persistencia P100 actual:
//! - REQUESTED, la respuesta idempotente, ADMITTING y el job de admisión se
//!   escriben y consultan en SQLite;
//! - renovación, liberación y vencimiento se aplican al repositorio durable;
//! - ACTIVE y su token siguen ligados al proceso actual, y el arranque invalida
//!   generaciones anteriores hasta que haya reconciliación cualificada.
//!
//! Pendiente: enlazar la admisión con RuntimeInstance e InferenceEndpoint
//! cualificados para persistir ACTIVE y habilitar el ciclo durable completo.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use larm_domain::bridge::{
    is_uuid_v7, parse_byte_count, renew_after_seconds, timestamp, ApiError, BrokerCatalog,
    CatalogItem, Constraints, EvidenceComponent, EvidenceStatus, Fingerprint, JobPhase, JobReceipt,
    JobState, LeaseEndpoint, LeaseRequest, LeaseResponse, LeaseState, Page, ProtocolProfile, Renew,
    RuntimeLease, BROKER_CONTRACT_VERSION,
};
use larm_domain::ports::{
    ApiScope, BrokerBridge, CreateLeaseOutcome, EndpointCredential, GatewayResponse,
    HardwareRepository, JournalJobRepository, LeaseCreatePersistence, LeaseRenewal,
    LeaseStateUpdate, MutationContext, Principal, RuntimeLeaseRepository, SecretStore,
};
use larm_domain::protocol::InferenceEndpointCreate;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

const MIB: u64 = 1024 * 1024;
const MIN_TTL: u64 = 30;
const MAX_TTL: u64 = 600;
const EXPIRY_GRACE: Duration = Duration::from_secs(30);
const IDLE_KEEP_ALIVE: &str = "5m";
const KNOWN_CAPABILITIES: [&str; 3] = ["text.generate", "embedding.create", "vision.understand"];
const LAUNCH_PROFILE_NAMES: [&str; 5] =
    ["ECO", "BALANCED", "PERFORMANCE", "LONG_CONTEXT", "CUSTOM"];

/// Variante declarada por el operador sobre el runtime adoptado.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptedVariant {
    pub variant_id: String,
    /// Nombre del modelo en el runtime adoptado (p. ej. etiqueta de Ollama).
    pub model: String,
    pub capabilities: Vec<String>,
}

/// Configuración de la adopción (fichero JSON pasado al daemon).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptionConfig {
    pub host_id: String,
    pub runtime: String,
    pub base_url: String,
    /// El runtime adoptado no autentica: cualquier proceso del PC puede
    /// llamarlo directamente. El contrato exige aceptarlo por escrito antes de
    /// publicar nada al Broker.
    pub accept_external_local_trusted: bool,
    pub runtime_version_id: String,
    /// Presupuesto duro del operador en bytes (cadena decimal).
    pub memory_budget_bytes: String,
    /// Base pública del gateway: `{gateway_base_url}/{lease_id}/v1`.
    pub gateway_base_url: String,
    /// `launch_profile` del dominio que se publica tal cual.
    pub launch_profile: Value,
    pub variants: Vec<AdoptedVariant>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_load_timeout")]
    pub load_timeout_seconds: u64,
    #[serde(default = "default_inference_timeout")]
    pub inference_timeout_seconds: u64,
}

/// Inputs needed to bind one adopted variant to persisted P033/P036 records.
/// The implementing daemon resolves every foreign key and revalidates the
/// endpoint before committing either record.
#[derive(Debug, Clone)]
pub struct ExternalAdoptionQualification {
    pub instance_id: String,
    pub host_id: String,
    pub runtime_version_id: String,
    pub variant_id: String,
    pub launch_profile_id: String,
    pub launch_profile_name: String,
    pub launch_profile_parameters: Option<Value>,
    pub backend_type: String,
    pub generation: u64,
    pub execution_fingerprint: String,
    pub endpoint: InferenceEndpointCreate,
    pub actor_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedAdoptionReferences {
    pub instance_id: String,
    pub endpoint_id: String,
}

/// Persists references only after P036 has produced a qualified endpoint.
#[async_trait]
pub trait QualifiedAdoptionReferenceStore: Send + Sync {
    async fn qualify_and_register(
        &self,
        request: &ExternalAdoptionQualification,
    ) -> Result<QualifiedAdoptionReferences, ApiError>;
}

/// Resultado del selector de versión de runtime aplicado a una pareja
/// variante/perfil. `pinned` indica que la selección procede de un pin P032.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeVersion {
    pub runtime_version_id: String,
    pub version_string: String,
    pub pinned: bool,
}

/// Resuelve el pin P032 contra el catálogo durable. El resolver debe fallar
/// cerrado si el pin no está disponible en el host adoptado.
#[async_trait]
pub trait RuntimeVersionResolver: Send + Sync {
    async fn resolve(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
        configured_runtime_version_id: &str,
        host_id: &str,
    ) -> Result<ResolvedRuntimeVersion, ApiError>;
}

fn default_request_timeout() -> u64 {
    30
}
fn default_load_timeout() -> u64 {
    600
}
fn default_inference_timeout() -> u64 {
    1800
}

/// Generación única de proceso, compartida por la reconciliación durable y el
/// puente Broker para que una lease anterior nunca se confunda con una nueva.
#[must_use]
pub fn process_generation() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis())
        .unwrap_or(1)
        .max(1)
}

fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

impl AdoptionConfig {
    /// Valida la configuración antes de hacer IO.
    pub fn validate(&self) -> Result<u64, String> {
        if !is_loopback_http(&self.base_url) {
            return Err("base_url del runtime adoptado debe ser http loopback".into());
        }
        if !is_loopback_http(&self.gateway_base_url) {
            return Err("gateway_base_url debe ser http loopback".into());
        }
        for (field, value) in [
            ("host_id", &self.host_id),
            ("runtime_version_id", &self.runtime_version_id),
        ] {
            if !is_uuid_v7(value) {
                return Err(format!("{field} debe ser UUIDv7"));
            }
        }
        let profile_id = self
            .launch_profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let profile_name = self
            .launch_profile
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !is_uuid_v7(profile_id) || !LAUNCH_PROFILE_NAMES.contains(&profile_name) {
            return Err("launch_profile necesita id UUIDv7 y name del contrato".into());
        }
        let mut seen = std::collections::HashSet::new();
        for variant in &self.variants {
            if !is_uuid_v7(&variant.variant_id) || !seen.insert(variant.variant_id.clone()) {
                return Err(format!(
                    "variant_id inválido o repetido: {}",
                    variant.variant_id
                ));
            }
            if variant.model.trim().is_empty() || variant.capabilities.is_empty() {
                return Err(format!(
                    "la variante {} necesita model y capabilities",
                    variant.variant_id
                ));
            }
            if let Some(cap) = variant
                .capabilities
                .iter()
                .find(|c| !KNOWN_CAPABILITIES.contains(&c.as_str()))
            {
                return Err(format!("capacidad no soportada por la adopción: {cap}"));
            }
        }
        parse_byte_count(&self.memory_budget_bytes)
            .ok_or_else(|| "memory_budget_bytes no es un tamaño válido".to_string())
    }
}

/// Lo que el runtime adoptado dice de una variante.
#[derive(Debug, Clone)]
struct VariantProbe {
    size_bytes: u64,
    digest: Option<String>,
    template_hash: Option<String>,
    runtime_version: Option<String>,
    kv_bytes_per_token: Option<u64>,
}

#[derive(Debug)]
struct LeaseEntry {
    record: RuntimeLease,
    variant_id: String,
    model: String,
    credential_reference: String,
    token: String,
    reserved_bytes: u64,
    expires: Option<Instant>,
    grace_until: Option<Instant>,
    response: Option<LeaseResponse>,
}

#[derive(Debug)]
struct JobEntry {
    owner: String,
    state: JobState,
}

#[derive(Debug, Clone)]
struct IdempotentReply {
    body_hash: String,
    outcome: CreateLeaseOutcome,
}

#[derive(Default)]
struct State {
    leases: HashMap<String, LeaseEntry>,
    jobs: HashMap<String, JobEntry>,
    replies: HashMap<String, IdempotentReply>,
    renewals: HashMap<String, (String, RuntimeLease)>,
    instances: HashMap<String, String>,
    qualified_references: HashMap<(String, String), CachedQualifiedReferences>,
}

#[derive(Debug, Clone)]
struct CachedQualifiedReferences {
    instance_id: String,
    endpoint_id: String,
    generation: u64,
    execution_fingerprint: String,
}

struct Inner {
    config: AdoptionConfig,
    budget_bytes: u64,
    generation: u64,
    http: reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    state: Mutex<State>,
    reference_lock: tokio::sync::Mutex<()>,
}

/// Implementación de [`BrokerBridge`] sobre el runtime adoptado.
#[derive(Clone)]
pub struct ExternalAdoptionBridge {
    inner: Arc<Inner>,
    hardware: Option<Arc<dyn HardwareRepository>>,
    lease_repository: Option<Arc<dyn RuntimeLeaseRepository>>,
    job_repository: Option<Arc<dyn JournalJobRepository>>,
    reference_store: Option<Arc<dyn QualifiedAdoptionReferenceStore>>,
    runtime_version_resolver: Option<Arc<dyn RuntimeVersionResolver>>,
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn now_ts() -> String {
    timestamp(chrono::Utc::now())
}

fn ts_in(duration: Duration) -> String {
    let delta = chrono::Duration::from_std(duration).unwrap_or_else(|_| chrono::Duration::zero());
    timestamp(chrono::Utc::now() + delta)
}

fn duration_until(value: &str) -> Duration {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|expiry| {
            expiry
                .with_timezone(&chrono::Utc)
                .signed_duration_since(chrono::Utc::now())
                .to_std()
                .ok()
        })
        .unwrap_or_default()
}

/// Hash de huella: SHA-256 del JSON canónico `{nombre: [valor, estado]}`
/// ordenado; `evidence_refs` es trazabilidad y no altera la identidad.
#[must_use]
pub fn fingerprint_hash(components: &BTreeMap<String, EvidenceComponent>) -> String {
    let canonical: BTreeMap<&String, (Option<&String>, EvidenceStatus)> = components
        .iter()
        .map(|(name, c)| (name, (c.value.as_ref(), c.status)))
        .collect();
    sha256_hex(
        serde_json::to_string(&canonical)
            .unwrap_or_default()
            .as_bytes(),
    )
}

fn fingerprint(components: BTreeMap<String, EvidenceComponent>) -> Fingerprint {
    Fingerprint {
        hash: fingerprint_hash(&components),
        components,
    }
}

fn upstream(error: &reqwest::Error, what: &str) -> ApiError {
    let code = if error.is_timeout() {
        "TIMEOUT"
    } else {
        "UPSTREAM_UNAVAILABLE"
    };
    ApiError::new(
        code,
        format!("El runtime adoptado no respondió a {what}"),
        "external_runtime_unreachable",
    )
    .retryable(true)
    .with_original("ollama", code, &error.to_string())
}

fn forbidden(message: &str) -> ApiError {
    ApiError::new("FORBIDDEN", message, "principal_not_allowed")
}

fn not_found(kind: &str, id: &str) -> ApiError {
    ApiError::new("NOT_FOUND", format!("{kind} desconocido"), "not_found").with_resource(id)
}

fn require(principal: &Principal, scope: ApiScope) -> Result<(), ApiError> {
    if principal.has_api_scope(scope) {
        Ok(())
    } else {
        Err(forbidden(&format!(
            "El principal no tiene el scope {}",
            scope.as_str()
        )))
    }
}

/// Ollama can expose cloud aliases on the same local HTTP listener. Neither
/// their small manifest size nor an operator-declared capability proves local
/// execution. The versioned API uses optional strings for these two fields.
fn require_local_model_metadata(value: &Value, variant_id: &str) -> Result<(), ApiError> {
    if !value.is_object() {
        return Err(ApiError::new(
            "INCOMPATIBLE",
            "Los metadatos del modelo adoptado no son válidos",
            "external_model_metadata_invalid",
        )
        .with_resource(variant_id));
    }
    for field in ["remote_model", "remote_host"] {
        match value.get(field) {
            None => {}
            Some(Value::String(value)) if value.is_empty() => {}
            Some(Value::String(_)) => {
                return Err(ApiError::new(
                    "INCOMPATIBLE",
                    "El modelo del host adoptado delega su ejecución a otro host",
                    "external_model_is_remote",
                )
                .with_resource(variant_id));
            }
            Some(_) => {
                return Err(ApiError::new(
                    "INCOMPATIBLE",
                    "Los metadatos del modelo adoptado no son válidos",
                    "external_model_metadata_invalid",
                )
                .with_resource(variant_id));
            }
        }
    }
    Ok(())
}

impl ExternalAdoptionBridge {
    /// Construye el puente; valida la configuración antes de cualquier IO.
    pub fn new(config: AdoptionConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, String> {
        Self::new_in_generation(config, secrets, process_generation())
    }

    /// Construye el puente con la generación que el daemon ya usó para
    /// reconciliar las leases persistidas antes de aceptar tráfico.
    pub fn new_in_generation(
        config: AdoptionConfig,
        secrets: Arc<dyn SecretStore>,
        generation: u64,
    ) -> Result<Self, String> {
        if generation == 0 {
            return Err("la generación de runtime debe ser positiva".into());
        }
        let budget_bytes = config.validate()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("cliente HTTP: {e}"))?;
        // Generación de este arranque: un reinicio la cambia y ninguna lease
        // anterior puede renovarse con la nueva (contrato 04).
        Ok(Self {
            hardware: None,
            lease_repository: None,
            job_repository: None,
            reference_store: None,
            runtime_version_resolver: None,
            inner: Arc::new(Inner {
                config,
                budget_bytes,
                generation,
                http,
                secrets,
                state: Mutex::new(State::default()),
                reference_lock: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// Conecta el inventario persistido; se consulta de nuevo por cada huella.
    #[must_use]
    pub fn with_hardware(mut self, hardware: Arc<dyn HardwareRepository>) -> Self {
        self.hardware = Some(hardware);
        self
    }

    /// Conecta las lecturas durables de leases recuperadas por el daemon.
    /// Las leases vivas del puente prevalecen sobre la instantánea SQLite.
    #[must_use]
    pub fn with_lease_repository(mut self, repository: Arc<dyn RuntimeLeaseRepository>) -> Self {
        self.lease_repository = Some(repository);
        self
    }

    /// Persiste los jobs de admisión del puente y los recupera tras reiniciar.
    #[must_use]
    pub fn with_job_repository(mut self, repository: Arc<dyn JournalJobRepository>) -> Self {
        self.job_repository = Some(repository);
        self
    }

    /// Enables durable ACTIVE transitions after the daemon creates a
    /// handshake-qualified external instance and endpoint.
    #[must_use]
    pub fn with_reference_store(mut self, store: Arc<dyn QualifiedAdoptionReferenceStore>) -> Self {
        self.reference_store = Some(store);
        self
    }

    /// Conecta la selección durable de pins por variante y perfil (P032).
    #[must_use]
    pub fn with_runtime_version_resolver(
        mut self,
        resolver: Arc<dyn RuntimeVersionResolver>,
    ) -> Self {
        self.runtime_version_resolver = Some(resolver);
        self
    }

    async fn persisted_leases(&self, owner_id: &str) -> Result<Vec<RuntimeLease>, ApiError> {
        let Some(repository) = &self.lease_repository else {
            return Ok(Vec::new());
        };
        const PAGE_SIZE: u32 = 200;
        let mut after = None;
        let mut leases = Vec::new();
        loop {
            let page = repository
                .list_for_owner(owner_id, after.clone(), PAGE_SIZE)
                .await?;
            if page.is_empty() {
                break;
            }
            let last = page
                .last()
                .map(|lease| (lease.created_at.clone(), lease.id.clone()))
                .ok_or_else(|| {
                    ApiError::new(
                        "INTERNAL",
                        "El repositorio devolvió una página de leases inconsistente",
                        "lease_repository_pagination_invalid",
                    )
                })?;
            if after.as_ref().is_some_and(|cursor| cursor >= &last) {
                return Err(ApiError::new(
                    "INTERNAL",
                    "El repositorio no avanzó al paginar leases",
                    "lease_repository_pagination_invalid",
                ));
            }
            let complete = page.len() < PAGE_SIZE as usize;
            after = Some(last);
            leases.extend(page);
            if complete {
                break;
            }
        }
        Ok(leases)
    }

    async fn hardware_components(
        &self,
    ) -> Result<(EvidenceComponent, EvidenceComponent), ApiError> {
        let profile = match &self.hardware {
            Some(repo) => repo.latest().await?,
            None => None,
        };
        let Some(profile) = profile else {
            return Ok((
                EvidenceComponent::unavailable("hardware_profile_not_observed"),
                EvidenceComponent::unavailable("driver_versions_not_observed"),
            ));
        };
        let reference = format!(
            "hardware_profile:{}#row_version={}",
            profile.id, profile.row_version
        );
        let fingerprint = EvidenceComponent::verified(profile.hardware_fingerprint, &reference);
        let devices = profile.snapshot.canonical().devices;
        let drivers = if devices.is_empty()
            || devices.iter().any(|d| {
                d.driver_version
                    .as_ref()
                    .is_none_or(|v| v.trim().is_empty())
            }) {
            EvidenceComponent::unavailable("driver_versions_incomplete")
        } else {
            let values: Vec<_> = devices
                .iter()
                .map(|d| {
                    json!({
                        "kind": d.kind, "device_id": d.device_id, "luid": d.luid,
                        "name": d.name, "driver_version": d.driver_version,
                    })
                })
                .collect();
            EvidenceComponent::verified(Value::Array(values).to_string(), &reference)
        };
        Ok((fingerprint, drivers))
    }

    /// Caduca leases sin renovar: ACTIVE → EXPIRING (sin nuevas llamadas) →
    /// EXPIRED tras la gracia, revocando su token. Se llama cada segundo.
    pub fn sweep(&self) {
        self.sweep_at(Instant::now());
    }

    /// Barrido con un instante dado (los tests avanzan el reloj así).
    pub fn sweep_at(&self, now: Instant) {
        let mut released_variants = Vec::new();
        let mut revoked = Vec::new();
        {
            let mut state = self.inner.state.lock();
            for entry in state.leases.values_mut() {
                match entry.record.state {
                    LeaseState::Active if entry.expires.is_some_and(|at| now >= at) => {
                        entry.record.state = LeaseState::Expiring;
                        entry.grace_until = Some(now + EXPIRY_GRACE);
                        Self::touch(&mut entry.record);
                    }
                    LeaseState::Expiring if entry.grace_until.is_some_and(|at| now >= at) => {
                        entry.record.state = LeaseState::Expired;
                        Self::touch(&mut entry.record);
                        revoked.push((entry.record.id.clone(), entry.record.owner_id.clone()));
                        released_variants.push(entry.variant_id.clone());
                    }
                    _ => {}
                }
            }
        }
        for (lease_id, owner) in revoked {
            tracing::info!(lease_id = %lease_id, "lease expirada: token revocado");
            self.forget_secret(&lease_id, &owner);
        }
        for variant in released_variants {
            self.relax_if_idle(&variant);
        }
    }

    async fn sweep_durable(&self) {
        let Some(repository) = &self.lease_repository else {
            return;
        };
        let context = MutationContext {
            actor_id: "larm:lease-sweeper".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let changed = match repository
            .expire_due(EXPIRY_GRACE.as_secs(), &context)
            .await
        {
            Ok(changed) => changed,
            Err(error) => {
                tracing::error!(
                    reason = %error.details.reason,
                    "no se pudieron persistir los vencimientos de leases"
                );
                return;
            }
        };
        let mut revoked = Vec::new();
        let mut released_variants = Vec::new();
        {
            let mut state = self.inner.state.lock();
            for record in changed {
                let Some(entry) = state.leases.get_mut(&record.id) else {
                    if record.state == LeaseState::Expired {
                        revoked.push((record.id, record.owner_id));
                    }
                    continue;
                };
                entry.record = record.clone();
                match record.state {
                    LeaseState::Expiring => {
                        entry.expires = Some(Instant::now() - Duration::from_millis(1));
                        entry.grace_until = Some(Instant::now() + EXPIRY_GRACE);
                    }
                    LeaseState::Expired => {
                        entry.expires = None;
                        entry.grace_until = None;
                        revoked.push((record.id, record.owner_id));
                        released_variants.push(entry.variant_id.clone());
                    }
                    _ => {}
                }
            }
        }
        for (lease_id, owner) in revoked {
            self.forget_secret(&lease_id, &owner);
        }
        for variant in released_variants {
            self.relax_if_idle(&variant);
        }
    }

    /// Lanza el barrido de caducidad en segundo plano.
    pub fn spawn_sweeper(&self) -> tokio::task::JoinHandle<()> {
        let bridge = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                bridge.sweep_durable().await;
                bridge.sweep();
            }
        })
    }

    fn touch(record: &mut RuntimeLease) {
        record.row_version += 1;
        record.updated_at = now_ts();
    }

    fn variant(&self, variant_id: &str) -> Option<&AdoptedVariant> {
        self.inner
            .config
            .variants
            .iter()
            .find(|v| v.variant_id == variant_id)
    }

    fn base(&self, path: &str) -> String {
        format!(
            "{}{}",
            self.inner.config.base_url.trim_end_matches('/'),
            path
        )
    }

    async fn get_json(&self, path: &str) -> Result<Value, ApiError> {
        let response = self
            .inner
            .http
            .get(self.base(path))
            .timeout(Duration::from_secs(
                self.inner.config.request_timeout_seconds,
            ))
            .send()
            .await
            .map_err(|e| upstream(&e, path))?;
        response
            .json::<Value>()
            .await
            .map_err(|e| upstream(&e, path))
    }

    async fn post_json(
        &self,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let response = self
            .inner
            .http
            .post(self.base(path))
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| upstream(&e, path))?;
        let status = response.status();
        let payload = response
            .json::<Value>()
            .await
            .map_err(|e| upstream(&e, path))?;
        if status.is_success() {
            Ok(payload)
        } else {
            let message = payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("error del runtime");
            let code = if status.as_u16() == 404 {
                "MODEL_NOT_FOUND"
            } else {
                "RUNTIME_CRASH"
            };
            Err(ApiError::new(
                code,
                "El runtime adoptado rechazó la operación",
                "external_runtime_error",
            )
            .retryable(status.is_server_error())
            .with_original("ollama", status.as_str(), message))
        }
    }

    /// Sondea la variante en el runtime: presencia, digest, plantilla, versión
    /// y la geometría de la caché KV si el runtime la publica.
    async fn probe(&self, variant: &AdoptedVariant) -> Result<VariantProbe, ApiError> {
        let tags = self.get_json("/api/tags").await?;
        let listed = tags
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .find(|m| m.get("name").and_then(Value::as_str) == Some(variant.model.as_str()))
            })
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    "MODEL_OFFLINE",
                    "El runtime adoptado no tiene la variante",
                    "model_absent_in_runtime",
                )
                .retryable(true)
                .with_resource(variant.variant_id.clone())
            })?;
        require_local_model_metadata(&listed, &variant.variant_id)?;
        let size_bytes = listed.get("size").and_then(Value::as_u64).ok_or_else(|| {
            ApiError::new(
                "UNKNOWN_SIZE",
                "El runtime no informa el tamaño de la variante",
                "size_unknown",
            )
        })?;
        let show = self
            .post_json(
                "/api/show",
                &json!({"model": variant.model}),
                Duration::from_secs(30),
            )
            .await?;
        require_local_model_metadata(&show, &variant.variant_id)?;
        let template_hash = show
            .get("template")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(|t| sha256_hex(t.as_bytes()));
        let version = self.get_json("/api/version").await?;
        Ok(VariantProbe {
            size_bytes,
            digest: listed
                .get("digest")
                .and_then(Value::as_str)
                .map(str::to_string),
            template_hash,
            runtime_version: version
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
            kv_bytes_per_token: kv_bytes_per_token(show.get("model_info")),
        })
    }

    fn protocol_profile(
        &self,
        variant: &AdoptedVariant,
        runtime_version: Option<&str>,
    ) -> ProtocolProfile {
        let embedding = variant.capabilities.iter().all(|c| c == "embedding.create");
        let (request, response) = if embedding {
            (
                "openai.embeddings.request.v1",
                "openai.embeddings.response.v1",
            )
        } else {
            (
                "openai.chat_completions.request.v1",
                "openai.chat_completions.response.v1",
            )
        };
        let descriptor = format!(
            "openai|{request}|{response}|{}|{}",
            self.inner.config.runtime,
            runtime_version.unwrap_or("unknown")
        );
        ProtocolProfile {
            protocol: "openai".into(),
            profile_hash: sha256_hex(descriptor.as_bytes()),
            capabilities: variant.capabilities.clone(),
            streaming: false,
            tools: false,
            request_schema_id: request.into(),
            response_schema_id: response.into(),
        }
    }

    /// Hash del perfil: RFC 8785 + sha256 (contrato 04/05, P013). Si el
    /// perfil es un `LaunchParameters` válido se usa su `profile_hash`, que
    /// además valida las reglas H03–H04.
    fn launch_profile_hash(&self) -> String {
        let profile = &self.inner.config.launch_profile;
        serde_json::from_value::<larm_domain::launch::LaunchParameters>(profile.clone())
            .ok()
            .and_then(|p| p.profile_hash().ok())
            .or_else(|| larm_domain::canonical::sha256_hex(profile).ok())
            .unwrap_or_default()
    }

    async fn components(
        &self,
        variant: &AdoptedVariant,
        probe: &VariantProbe,
        backend: EvidenceComponent,
    ) -> Result<BTreeMap<String, EvidenceComponent>, ApiError> {
        let (hardware, drivers) = self.hardware_components().await?;
        let profile = self.protocol_profile(variant, probe.runtime_version.as_deref());
        let optional = |value: &Option<String>, evidence: &str, reason: &str| match value {
            Some(v) => EvidenceComponent::verified(v.clone(), evidence),
            None => EvidenceComponent::unavailable(reason),
        };
        Ok(BTreeMap::from([
            (
                "artifact_bundle_hash".into(),
                optional(
                    &probe.digest,
                    "ollama:/api/tags#digest",
                    "digest_not_reported",
                ),
            ),
            (
                "revision".into(),
                EvidenceComponent::unavailable("external_adoption:no_revision_registry"),
            ),
            (
                "runtime_id".into(),
                EvidenceComponent::verified(
                    self.inner.config.runtime.clone(),
                    "ollama:/api/version",
                ),
            ),
            (
                "runtime_version".into(),
                optional(
                    &probe.runtime_version,
                    "ollama:/api/version",
                    "version_not_reported",
                ),
            ),
            (
                "installation_hash".into(),
                EvidenceComponent::unavailable("external_adoption:not_installed_by_larm"),
            ),
            ("backend".into(), backend),
            ("driver_versions".into(), drivers),
            ("hardware_fingerprint".into(), hardware),
            (
                "launch_profile_hash".into(),
                EvidenceComponent::declared(
                    self.launch_profile_hash(),
                    "external_adoption:runtime_controls_launch",
                ),
            ),
            (
                "tokenizer_hash".into(),
                EvidenceComponent::unavailable("external_adoption:tokenizer_not_exposed"),
            ),
            (
                "chat_template_hash".into(),
                optional(
                    &probe.template_hash,
                    "ollama:/api/show#template",
                    "template_not_reported",
                ),
            ),
            (
                "protocol".into(),
                EvidenceComponent::declared("openai", "external_adoption:openai_compat_endpoint"),
            ),
            (
                "protocol_profile_hash".into(),
                EvidenceComponent::declared(profile.profile_hash, "larm:protocol_profile"),
            ),
            (
                "effective_generation".into(),
                EvidenceComponent::unavailable("per_request"),
            ),
        ]))
    }

    async fn resolved_runtime_version(
        &self,
        variant_id: &str,
        observed_version: Option<&str>,
    ) -> Result<ResolvedRuntimeVersion, ApiError> {
        let launch_profile_id = self
            .inner
            .config
            .launch_profile
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_uuid_v7(id))
            .ok_or_else(|| {
                ApiError::new(
                    "INTEGRITY_FAILED",
                    "La configuración de adopción no tiene un perfil persistido válido",
                    "launch_profile_binding_invalid",
                )
            })?;
        let resolved = if let Some(resolver) = &self.runtime_version_resolver {
            resolver
                .resolve(
                    variant_id,
                    launch_profile_id,
                    &self.inner.config.runtime_version_id,
                    &self.inner.config.host_id,
                )
                .await?
        } else {
            // Harnesses y callers legacy sin catálogo conectado siguen usando
            // la versión explícita configurada; producción conecta el resolver.
            ResolvedRuntimeVersion {
                runtime_version_id: self.inner.config.runtime_version_id.clone(),
                version_string: observed_version.unwrap_or_default().to_owned(),
                pinned: false,
            }
        };
        if !is_uuid_v7(&resolved.runtime_version_id)
            || (resolved.pinned && resolved.version_string.is_empty())
        {
            return Err(ApiError::new(
                "INTEGRITY_FAILED",
                "El resolver devolvió una identidad de versión inválida",
                "runtime_version_resolution_invalid",
            ));
        }
        if observed_version != Some(resolved.version_string.as_str()) {
            let reason = if resolved.pinned {
                "runtime_pin_version_mismatch"
            } else {
                "runtime_version_observation_mismatch"
            };
            return Err(ApiError::new(
                "INCOMPATIBLE",
                "La versión observada no coincide con la versión seleccionada para el perfil",
                reason,
            )
            .with_resource(variant_id));
        }
        Ok(resolved)
    }

    fn constraints(&self, runtime_version_id: &str) -> Constraints {
        Constraints {
            runtime_version_id: Some(runtime_version_id.to_owned()),
            backend_id: None,
            execution_fingerprint: None,
            allow_backend_fallback: false,
            max_memory_bytes: None,
        }
    }

    async fn catalog_item(&self, variant: &AdoptedVariant) -> Result<CatalogItem, ApiError> {
        let probe = self.probe(variant).await?;
        let runtime_version = self
            .resolved_runtime_version(&variant.variant_id, probe.runtime_version.as_deref())
            .await?;
        let backend = EvidenceComponent::unavailable("backend_known_only_after_load");
        Ok(CatalogItem {
            variant_id: variant.variant_id.clone(),
            capabilities: variant.capabilities.clone(),
            runtime_version_ids: vec![runtime_version.runtime_version_id.clone()],
            backend_ids: Vec::new(),
            protocol_profiles: vec![
                self.protocol_profile(variant, probe.runtime_version.as_deref())
            ],
            fingerprint: fingerprint(self.components(variant, &probe, backend).await?),
            constraints: self.constraints(&runtime_version.runtime_version_id),
        })
    }

    /// Bytes que reserva la variante para estos requisitos (contrato 04):
    /// pesos + KV (si el runtime publica su geometría) + overhead del runtime,
    /// más el margen de seguridad. Sin geometría KV no se inventa: el
    /// presupuesto duro del operador es lo que autoriza la admisión.
    fn estimate(&self, probe: &VariantProbe, request: &LeaseRequest) -> u64 {
        let weights = probe.size_bytes;
        let profile_context = self
            .inner
            .config
            .launch_profile
            .pointer("/parameters/context")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let requested = request.resource_requirements.as_ref().map_or(0, |r| {
            r.context_tokens
                .unwrap_or(0)
                .saturating_add(r.max_output_tokens.unwrap_or(0))
                .saturating_mul(r.batch.max(1))
        });
        let kv = probe.kv_bytes_per_token.map_or(0, |per_token| {
            per_token.saturating_mul(profile_context.max(requested))
        });
        let overhead = (256 * MIB).max(weights.div_ceil(10));
        let base = weights.saturating_add(kv).saturating_add(overhead);
        let safety = (512 * MIB).max((base.saturating_mul(500)).div_ceil(10_000));
        base.saturating_add(safety)
    }

    async fn loaded_models(&self) -> Result<Vec<Value>, ApiError> {
        let ps = self.get_json("/api/ps").await?;
        Ok(ps
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn validate_request(
        &self,
        principal: &Principal,
        request: &LeaseRequest,
    ) -> Result<AdoptedVariant, ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        if request.owner_id != principal.name {
            return Err(forbidden("owner_id debe ser el principal autenticado"));
        }
        if !(MIN_TTL..=MAX_TTL).contains(&request.ttl_seconds) {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "ttl_seconds fuera de 30..600",
                "ttl_out_of_range",
            ));
        }
        if request.request_id.is_empty() {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "request_id vacío",
                "request_id_missing",
            ));
        }
        let profile_id = self
            .inner
            .config
            .launch_profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if request.launch_profile_id != profile_id {
            return Err(not_found("launch_profile", &request.launch_profile_id));
        }
        let Some(variant_id) = request.variant_id.as_deref() else {
            return Err(ApiError::new(
                "UNSUPPORTED_CAPABILITY",
                "La adopción externa solo admite leases por variant_id exacto",
                "variant_required",
            ));
        };
        let variant = self.variant(variant_id).cloned().ok_or_else(|| {
            ApiError::new(
                "MODEL_NOT_FOUND",
                "Variante no registrada",
                "variant_unknown",
            )
            .with_resource(variant_id)
        })?;
        if let Some(capability) = request.capability.as_deref() {
            if !variant.capabilities.iter().any(|c| c == capability) {
                return Err(ApiError::new(
                    "UNSUPPORTED_CAPABILITY",
                    "La variante no anuncia esa capacidad",
                    "capability_not_announced",
                ));
            }
        }
        if let Some(requirements) = &request.resource_requirements {
            if Some(requirements.capability.as_str()) != request.capability.as_deref()
                && request.capability.is_some()
            {
                return Err(ApiError::new(
                    "VALIDATION_FAILED",
                    "resource_requirements.capability no coincide",
                    "capability_mismatch",
                ));
            }
            if let Some(budget) = &requirements.hard_memory_budget_bytes {
                parse_byte_count(budget).ok_or_else(|| {
                    ApiError::new(
                        "VALIDATION_FAILED",
                        "hard_memory_budget_bytes inválido",
                        "bytes_invalid",
                    )
                })?;
            }
        }
        let constraints = &request.constraints;
        let mismatched =
            constraints.backend_id.is_some() || constraints.execution_fingerprint.is_some();
        if mismatched {
            return Err(ApiError::new(
                "INCOMPATIBLE",
                "Las restricciones pedidas no se pueden garantizar en el runtime adoptado",
                "constraints_not_satisfiable",
            ));
        }
        if let Some(max) = &constraints.max_memory_bytes {
            parse_byte_count(max).ok_or_else(|| {
                ApiError::new(
                    "VALIDATION_FAILED",
                    "max_memory_bytes inválido",
                    "bytes_invalid",
                )
            })?;
        }
        Ok(variant)
    }

    fn forget_secret(&self, lease_id: &str, owner: &str) {
        if let Err(error) = self
            .inner
            .secrets
            .delete(&format!("larm/lease/{lease_id}"), owner)
        {
            tracing::warn!(lease_id = %lease_id, error = %error, "no se pudo borrar la credencial de la lease");
        }
    }

    /// Cuando una variante se queda sin leases, el runtime vuelve a su política
    /// de descarga por inactividad (300 s). LARM no descarga ni mata nada.
    fn relax_if_idle(&self, variant_id: &str) {
        let still_leased = self.inner.state.lock().leases.values().any(|entry| {
            entry.variant_id == variant_id
                && matches!(
                    entry.record.state,
                    LeaseState::Active | LeaseState::Admitting | LeaseState::Expiring
                )
        });
        if still_leased {
            return;
        }
        let Some(variant) = self.variant(variant_id).cloned() else {
            return;
        };
        let bridge = self.clone();
        tokio::spawn(async move {
            if let Err(error) = bridge.restore_idle_policy(&variant).await {
                tracing::warn!(code = %error.code, "no se pudo devolver la variante a su keep_alive normal");
            }
        });
    }

    async fn restore_idle_policy(&self, variant: &AdoptedVariant) -> Result<(), ApiError> {
        // An external owner may retag a model after a lease is released. Even
        // this empty-prompt request must not dispatch to a remote alias.
        self.probe(variant).await?;
        let body = json!({"model": variant.model, "prompt": "", "stream": false, "keep_alive": IDLE_KEEP_ALIVE});
        self.post_json("/api/generate", &body, Duration::from_secs(30))
            .await?;
        Ok(())
    }

    fn gateway_model(
        &self,
        lease_id: &str,
        bearer: Option<&str>,
    ) -> Result<(String, String), ApiError> {
        let state = self.inner.state.lock();
        let entry = state
            .leases
            .get(lease_id)
            .ok_or_else(|| not_found("lease", lease_id))?;
        if bearer != Some(entry.token.as_str()) {
            return Err(ApiError::new(
                "AUTH_REQUIRED",
                "Token de endpoint inválido para esta lease",
                "endpoint_token_invalid",
            ));
        }
        if entry.record.state != LeaseState::Active {
            return Err(ApiError::new(
                "INVALID_STATE_TRANSITION",
                "La lease no está ACTIVE",
                "lease_not_active",
            )
            .with_resource(lease_id));
        }
        Ok((entry.model.clone(), entry.variant_id.clone()))
    }

    /// Carga (o fija) la variante y pasa la lease a ACTIVE.
    async fn activate(&self, lease_id: &str) -> Result<LeaseResponse, ApiError> {
        let durable = self.lease_repository.is_some() && self.job_repository.is_some();
        if durable && self.reference_store.is_none() {
            return Err(ApiError::new(
                "RESOURCE_UNAVAILABLE",
                "La admisión durable necesita una instancia y un endpoint cualificados",
                "lease_activation_reference_unavailable",
            )
            .with_resource(lease_id));
        }
        let (variant_id, model, runtime_version_id, owner_id, generation, row_version, ttl) = {
            let state = self.inner.state.lock();
            let entry = state
                .leases
                .get(lease_id)
                .ok_or_else(|| not_found("lease", lease_id))?;
            if entry.record.state != LeaseState::Admitting {
                return Err(ApiError::new(
                    "INVALID_STATE_TRANSITION",
                    "La lease dejó de estar en admisión",
                    "lease_not_admitting",
                ));
            }
            (
                entry.variant_id.clone(),
                entry.model.clone(),
                entry
                    .record
                    .constraints
                    .runtime_version_id
                    .clone()
                    .unwrap_or_else(|| self.inner.config.runtime_version_id.clone()),
                entry.record.owner_id.clone(),
                entry.record.generation,
                entry.record.row_version,
                Duration::from_secs(
                    entry
                        .record
                        .metadata
                        .get("ttl_seconds")
                        .and_then(Value::as_u64)
                        .unwrap_or(120),
                ),
            )
        };
        let variant = self
            .variant(&variant_id)
            .cloned()
            .ok_or_else(|| not_found("variant", &variant_id))?;
        let preflight_probe = self.probe(&variant).await?;
        let current_runtime_version = self
            .resolved_runtime_version(&variant_id, preflight_probe.runtime_version.as_deref())
            .await?;
        if current_runtime_version.runtime_version_id != runtime_version_id {
            return Err(ApiError::new(
                "CONFLICT",
                "El pin cambió mientras la lease esperaba admisión; vuelve a solicitarla",
                "runtime_pin_changed_during_admission",
            )
            .with_resource(variant_id.clone()));
        }
        // keep_alive -1: mientras exista la lease el runtime no descarga (spec §64).
        let load = json!({"model": model, "prompt": "", "stream": false, "keep_alive": -1});
        self.post_json(
            "/api/generate",
            &load,
            Duration::from_secs(self.inner.config.load_timeout_seconds),
        )
        .await?;
        let probe = self.probe(&variant).await?;
        if probe.runtime_version.as_deref() != Some(current_runtime_version.version_string.as_str())
        {
            let reason = if current_runtime_version.pinned {
                "runtime_pin_version_mismatch"
            } else {
                "runtime_version_observation_mismatch"
            };
            return Err(ApiError::new(
                "INCOMPATIBLE",
                "La versión observada del runtime dejó de coincidir con la versión seleccionada",
                reason,
            )
            .with_resource(variant_id.clone()));
        }
        let backend = backend_from_ps(&self.loaded_models().await?, &model);
        let fp = fingerprint(self.components(&variant, &probe, backend.clone()).await?);
        let (instance_id, endpoint_id) = if durable {
            let _reference_guard = self.inner.reference_lock.lock().await;
            let reference_key = (variant_id.clone(), runtime_version_id.clone());
            let cached = self
                .inner
                .state
                .lock()
                .qualified_references
                .get(&reference_key)
                .cloned();
            if let Some(cached) = cached {
                if cached.generation != generation || cached.execution_fingerprint != fp.hash {
                    return Err(ApiError::new(
                        "INCOMPATIBLE",
                        "La instancia externa cambió durante la generación activa",
                        "lease_instance_binding_changed",
                    )
                    .with_resource(variant_id));
                }
                (cached.instance_id, cached.endpoint_id)
            } else {
                let backend_type = backend
                    .value
                    .as_deref()
                    .filter(|_| backend.status == EvidenceStatus::Verified)
                    .ok_or_else(|| {
                        ApiError::new(
                            "RESOURCE_UNAVAILABLE",
                            "El runtime no informó un backend verificable",
                            "lease_backend_unavailable",
                        )
                        .with_resource(variant_id.clone())
                    })?
                    .to_owned();
                let profile = self.protocol_profile(&variant, probe.runtime_version.as_deref());
                let instance_id = uuid::Uuid::now_v7().to_string();
                let endpoint = InferenceEndpointCreate {
                    instance_id: instance_id.clone(),
                    protocol: profile.protocol,
                    protocol_profile_hash: profile.profile_hash,
                    base_url: format!("{}/v1", self.inner.config.base_url.trim_end_matches('/')),
                    credential_reference_id: None,
                    capabilities: variant.capabilities.clone(),
                    qualification: larm_domain::protocol::EndpointQualification::Untested,
                    evidence_refs: Vec::new(),
                };
                let request = ExternalAdoptionQualification {
                    instance_id: instance_id.clone(),
                    host_id: self.inner.config.host_id.clone(),
                    runtime_version_id: runtime_version_id.clone(),
                    variant_id: variant.variant_id.clone(),
                    launch_profile_id: self
                        .inner
                        .config
                        .launch_profile
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    launch_profile_name: self
                        .inner
                        .config
                        .launch_profile
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    launch_profile_parameters: self
                        .inner
                        .config
                        .launch_profile
                        .get("parameters")
                        .cloned(),
                    backend_type,
                    generation,
                    execution_fingerprint: fp.hash.clone(),
                    endpoint,
                    actor_id: owner_id.clone(),
                };
                let references = self
                    .reference_store
                    .as_ref()
                    .ok_or_else(|| {
                        ApiError::new(
                            "RESOURCE_UNAVAILABLE",
                            "No está conectado el registro de referencias cualificadas",
                            "lease_activation_reference_unavailable",
                        )
                    })?
                    .qualify_and_register(&request)
                    .await?;
                if references.instance_id != instance_id
                    || !larm_domain::bridge::is_uuid_v7(&references.instance_id)
                    || !larm_domain::bridge::is_uuid_v7(&references.endpoint_id)
                {
                    return Err(ApiError::new(
                        "INTEGRITY_FAILED",
                        "El registro devolvió referencias de instancia o endpoint no válidas",
                        "lease_activation_reference_invalid",
                    )
                    .with_resource(variant_id));
                }
                self.inner.state.lock().qualified_references.insert(
                    reference_key,
                    CachedQualifiedReferences {
                        instance_id: references.instance_id.clone(),
                        endpoint_id: references.endpoint_id.clone(),
                        generation,
                        execution_fingerprint: fp.hash.clone(),
                    },
                );
                (references.instance_id, references.endpoint_id)
            }
        } else {
            let mut state = self.inner.state.lock();
            let instance_id = state
                .instances
                .entry(variant_id.clone())
                .or_insert_with(|| uuid::Uuid::now_v7().to_string())
                .clone();
            let endpoint_id = state
                .leases
                .get(lease_id)
                .and_then(|entry| entry.record.endpoint_id.clone())
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            (instance_id, endpoint_id)
        };
        let expires_at = ts_in(ttl);
        let durable_active = if durable {
            let repository = self.lease_repository.as_ref().ok_or_else(|| {
                ApiError::new(
                    "INTERNAL",
                    "Falta el repositorio durable de leases",
                    "lease_repository_missing",
                )
            })?;
            let context = MutationContext {
                actor_id: owner_id.clone(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            };
            Some(
                repository
                    .transition(
                        lease_id,
                        &owner_id,
                        row_version,
                        generation,
                        &LeaseStateUpdate {
                            from: LeaseState::Admitting,
                            to: LeaseState::Active,
                            instance_id: Some(instance_id.clone()),
                            endpoint_id: Some(endpoint_id.clone()),
                            expires_at: Some(expires_at.clone()),
                        },
                        &context,
                    )
                    .await?,
            )
        } else {
            None
        };
        let gateway = self
            .inner
            .config
            .gateway_base_url
            .trim_end_matches('/')
            .to_string();
        let mut state = self.inner.state.lock();
        let entry = state
            .leases
            .get_mut(lease_id)
            .ok_or_else(|| not_found("lease", lease_id))?;
        if entry.record.state != LeaseState::Admitting {
            return Err(ApiError::new(
                "INVALID_STATE_TRANSITION",
                "La lease dejó de estar en admisión",
                "lease_not_admitting",
            ));
        }
        if let Some(active) = durable_active {
            entry.record = active;
        } else {
            entry.record.state = LeaseState::Active;
            entry.record.instance_id = Some(instance_id.clone());
            entry.record.endpoint_id = Some(endpoint_id.clone());
            entry.record.expires_at = Some(expires_at);
            Self::touch(&mut entry.record);
        }
        entry.expires = Some(Instant::now() + ttl);
        let response = LeaseResponse {
            id: entry.record.id.clone(),
            owner_id: entry.record.owner_id.clone(),
            instance_id,
            generation: entry.record.generation,
            endpoint_id,
            endpoint: LeaseEndpoint {
                base_url: format!("{gateway}/{}/v1", entry.record.id),
                protocol: "openai".into(),
                credential_reference: Some(entry.credential_reference.clone()),
            },
            execution_fingerprint: fp,
            expires_at: entry.record.expires_at.clone().unwrap_or_default(),
            renew_after_seconds: renew_after_seconds(ttl.as_secs()),
        };
        entry.response = Some(response.clone());
        Ok(response)
    }

    fn fail_lease(&self, lease_id: &str) {
        let owner = {
            let mut state = self.inner.state.lock();
            let Some(entry) = state.leases.get_mut(lease_id) else {
                return;
            };
            // Una respuesta tardía del worker no debe reemplazar un estado
            // terminal establecido por otro flujo mientras corría el job.
            if !entry.record.state.is_terminal() {
                entry.record.state = LeaseState::Failed;
                Self::touch(&mut entry.record);
            }
            (entry.record.owner_id.clone(), entry.variant_id.clone())
        };
        self.forget_secret(lease_id, &owner.0);
        self.relax_if_idle(&owner.1);
    }

    async fn fail_persisted_lease(&self, lease_id: &str) {
        let Some(repository) = &self.lease_repository else {
            return;
        };
        let owner = {
            let state = self.inner.state.lock();
            state
                .leases
                .get(lease_id)
                .map(|entry| entry.record.owner_id.clone())
        };
        let Some(owner) = owner else {
            return;
        };
        let record = match repository.get_for_owner(lease_id, &owner).await {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(
                    lease_id = %lease_id,
                    reason = %error.details.reason,
                    "no se pudo leer la lease durable para marcarla FAILED"
                );
                return;
            }
        };
        if !matches!(record.state, LeaseState::Requested | LeaseState::Admitting) {
            return;
        }
        let context = MutationContext {
            actor_id: owner.clone(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let update = LeaseStateUpdate {
            from: record.state,
            to: LeaseState::Failed,
            instance_id: None,
            endpoint_id: None,
            expires_at: None,
        };
        if let Err(error) = repository
            .transition(
                lease_id,
                &owner,
                record.row_version,
                record.generation,
                &update,
                &context,
            )
            .await
        {
            tracing::error!(
                lease_id = %lease_id,
                reason = %error.details.reason,
                "no se pudo persistir el fallo de admisión de lease"
            );
        }
    }

    fn reserved_by_others(state: &State, variant_id: &str) -> (u64, bool) {
        let mut others = 0u64;
        let mut already = false;
        let mut counted = std::collections::HashSet::new();
        for entry in state.leases.values() {
            if !matches!(
                entry.record.state,
                LeaseState::Active | LeaseState::Admitting | LeaseState::Expiring
            ) {
                continue;
            }
            if entry.variant_id == variant_id {
                already = true;
            } else if counted.insert(entry.variant_id.clone()) {
                others = others.saturating_add(entry.reserved_bytes);
            }
        }
        (others, already)
    }
}

/// Bytes de caché KV por token de contexto, solo con la geometría explícita
/// que publica el runtime (sin deducir nada del nombre del modelo).
fn kv_bytes_per_token(model_info: Option<&Value>) -> Option<u64> {
    let info = model_info?.as_object()?;
    let arch = info.get("general.architecture")?.as_str()?;
    let field = |name: &str| info.get(&format!("{arch}.{name}")).and_then(Value::as_u64);
    let layers = field("block_count")?;
    let kv_heads = field("attention.head_count_kv")?;
    let key = field("attention.key_length")?;
    let value = field("attention.value_length")?;
    // f16 por elemento: 2 bytes (K y V con sus longitudes propias).
    layers
        .checked_mul(kv_heads)?
        .checked_mul(key.checked_add(value)?)?
        .checked_mul(2)
}

/// Backend efectivo observado tras cargar: toda la variante en memoria de
/// GPU, nada, o repartida. Es lo que el runtime informa, no una suposición.
fn backend_from_ps(models: &[Value], model: &str) -> EvidenceComponent {
    let Some(entry) = models
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some(model))
    else {
        return EvidenceComponent::unavailable("model_not_listed_in_ps");
    };
    match (
        entry.get("size").and_then(Value::as_u64),
        entry.get("size_vram").and_then(Value::as_u64),
    ) {
        (Some(size), Some(vram)) if size > 0 && vram >= size => {
            EvidenceComponent::verified("GPU", "ollama:/api/ps#size_vram")
        }
        (Some(size), Some(0)) if size > 0 => {
            EvidenceComponent::verified("CPU", "ollama:/api/ps#size_vram")
        }
        (Some(size), Some(_)) if size > 0 => {
            EvidenceComponent::verified("HYBRID", "ollama:/api/ps#size_vram")
        }
        _ => EvidenceComponent::unavailable("ps_without_sizes"),
    }
}

fn canonical_hash(value: &impl serde::Serialize) -> String {
    sha256_hex(serde_json::to_string(value).unwrap_or_default().as_bytes())
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

#[async_trait]
impl BrokerBridge for ExternalAdoptionBridge {
    async fn catalog(&self) -> Result<BrokerCatalog, ApiError> {
        let mut items = Vec::new();
        if self.inner.config.accept_external_local_trusted {
            for variant in &self.inner.config.variants {
                match self.catalog_item(variant).await {
                    Ok(item) => items.push(item),
                    Err(error) => {
                        tracing::info!(variant = %variant.variant_id, code = %error.code, "variante fuera del catálogo")
                    }
                }
            }
        }
        Ok(BrokerCatalog {
            contract_version: BROKER_CONTRACT_VERSION.into(),
            items,
            snapshot_at: now_ts(),
        })
    }

    async fn launch_profiles(&self) -> Result<Page<Value>, ApiError> {
        Ok(Page {
            items: vec![self.inner.config.launch_profile.clone()],
            next_cursor: None,
            snapshot_at: now_ts(),
        })
    }

    async fn create_lease(
        &self,
        principal: &Principal,
        request: LeaseRequest,
        idempotency_key: &str,
    ) -> Result<CreateLeaseOutcome, ApiError> {
        let scope_key = format!("{}:{idempotency_key}", principal.name);
        let body_hash = canonical_hash(&request);
        let durable = self.lease_repository.is_some() && self.job_repository.is_some();
        if let Some(previous) = self.inner.state.lock().replies.get(&scope_key) {
            if previous.body_hash == body_hash {
                return Ok(previous.outcome.clone());
            }
            return Err(ApiError::new(
                "IDEMPOTENCY_CONFLICT",
                "Misma Idempotency-Key con otro cuerpo",
                "idempotency_conflict",
            ));
        }
        let variant = self.validate_request(principal, &request)?;
        if !self.inner.config.accept_external_local_trusted {
            return Err(ApiError::new(
                "APPROVAL_REQUIRED",
                "El operador no aceptó el modo EXTERNAL_LOCAL_TRUSTED",
                "trust_not_accepted",
            ));
        }
        let probe = self.probe(&variant).await?;
        let runtime_version = self
            .resolved_runtime_version(&variant.variant_id, probe.runtime_version.as_deref())
            .await?;
        if request
            .constraints
            .runtime_version_id
            .as_ref()
            .is_some_and(|id| id != &runtime_version.runtime_version_id)
        {
            return Err(ApiError::new(
                "INCOMPATIBLE",
                "La versión solicitada no coincide con el pin de variante/perfil",
                "constraints_not_satisfiable",
            )
            .with_resource(variant.variant_id.clone()));
        }
        let mut resolved_constraints = request.constraints.clone();
        resolved_constraints.runtime_version_id = Some(runtime_version.runtime_version_id.clone());
        let needed = self.estimate(&probe, &request);
        let budget = request
            .constraints
            .max_memory_bytes
            .as_deref()
            .and_then(parse_byte_count)
            .map_or(self.inner.budget_bytes, |limit| {
                limit.min(self.inner.budget_bytes)
            });
        if needed > budget {
            return Err(ApiError::new(
                "RESOURCE_UNAVAILABLE",
                format!("La variante necesita {needed} bytes y el presupuesto total es {budget}"),
                "memory_budget_exceeded",
            )
            .with_evidence(format!("estimate:needed={needed};budget={budget}"))
            .with_resource(variant.variant_id.clone()));
        }
        let loaded = self.loaded_models().await?;
        let leased_models: std::collections::HashSet<String> = {
            let state = self.inner.state.lock();
            state
                .leases
                .values()
                .filter(|e| !e.record.state.is_terminal())
                .map(|e| e.model.clone())
                .collect()
        };
        // Lo cargado en el runtime sin lease de LARM es presión ajena: cuenta,
        // pero no se toca.
        let foreign: u64 = loaded
            .iter()
            .filter(|m| {
                let name = m.get("name").and_then(Value::as_str).unwrap_or_default();
                name != variant.model && !leased_models.contains(name)
            })
            .filter_map(|m| m.get("size").and_then(Value::as_u64))
            .sum();
        let already_loaded = loaded
            .iter()
            .any(|m| m.get("name").and_then(Value::as_str) == Some(variant.model.as_str()));
        let lease_id = uuid::Uuid::now_v7().to_string();
        let durable_job_id = durable.then(|| uuid::Uuid::now_v7().to_string());
        let credential_reference = uuid::Uuid::now_v7().to_string();
        let token = new_token();
        {
            let mut state = self.inner.state.lock();
            let (others, shares_instance) = Self::reserved_by_others(&state, &variant.variant_id);
            let extra = if shares_instance { 0 } else { needed };
            if others.saturating_add(foreign).saturating_add(extra) > budget {
                return Err(ApiError::new(
                    "RESOURCE_UNAVAILABLE",
                    "No hay memoria libre ahora para admitir la variante",
                    "memory_pressure",
                )
                .retryable(true)
                .with_evidence(format!(
                    "reserved={others};foreign={foreign};needed={needed};budget={budget}"
                ))
                .with_resource(variant.variant_id.clone()));
            }
            let now = now_ts();
            let mut metadata = serde_json::Map::new();
            metadata.insert("ttl_seconds".into(), json!(request.ttl_seconds));
            metadata.insert("variant_id".into(), json!(variant.variant_id));
            if let Some(job_id) = &durable_job_id {
                metadata.insert("job_id".into(), json!(job_id));
            }
            state.leases.insert(
                lease_id.clone(),
                LeaseEntry {
                    record: RuntimeLease {
                        id: lease_id.clone(),
                        created_at: now.clone(),
                        updated_at: now,
                        row_version: 1,
                        metadata,
                        owner_id: request.owner_id.clone(),
                        request_id: request.request_id.clone(),
                        instance_id: None,
                        generation: self.inner.generation,
                        state: LeaseState::Admitting,
                        expires_at: None,
                        endpoint_id: (!durable).then(|| uuid::Uuid::now_v7().to_string()),
                        constraints: resolved_constraints,
                    },
                    variant_id: variant.variant_id.clone(),
                    model: variant.model.clone(),
                    credential_reference,
                    token: token.clone(),
                    reserved_bytes: needed,
                    expires: None,
                    grace_until: None,
                    response: None,
                },
            );
        }
        let durable_outcome =
            if let (Some(repository), Some(job_id)) = (&self.lease_repository, &durable_job_id) {
                let mut requested = self
                    .inner
                    .state
                    .lock()
                    .leases
                    .get(&lease_id)
                    .map(|entry| entry.record.clone())
                    .ok_or_else(|| not_found("lease", &lease_id))?;
                requested.row_version = 1;
                requested.state = LeaseState::Requested;
                requested.instance_id = None;
                requested.endpoint_id = None;
                requested.expires_at = None;
                let queued = CreateLeaseOutcome {
                    status: 202,
                    body: serde_json::to_value(JobReceipt {
                        job_id: job_id.clone(),
                        status_url: format!("/api/v1/jobs/{job_id}"),
                        state: JobPhase::Queued,
                    })
                    .unwrap_or_default(),
                };
                let context = MutationContext {
                    actor_id: request.owner_id.clone(),
                    correlation_id: uuid::Uuid::now_v7().to_string(),
                };
                let persisted = match repository
                    .create_requested(&requested, &body_hash, idempotency_key, &queued, &context)
                    .await
                {
                    Ok(persisted) => persisted,
                    Err(error) => {
                        self.inner.state.lock().leases.remove(&lease_id);
                        return Err(error);
                    }
                };
                match persisted {
                    LeaseCreatePersistence::Replayed(outcome) => {
                        self.inner.state.lock().leases.remove(&lease_id);
                        return Ok(outcome);
                    }
                    LeaseCreatePersistence::Existing(existing) => {
                        self.inner.state.lock().leases.remove(&lease_id);
                        let Some(existing_job_id) = existing
                            .metadata
                            .get("job_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                        else {
                            return Err(ApiError::new(
                                "INTERNAL",
                                "La lease durable no tiene un job asociado",
                                "lease_job_missing",
                            )
                            .with_resource(existing.id));
                        };
                        return Ok(CreateLeaseOutcome {
                            status: 202,
                            body: serde_json::to_value(JobReceipt {
                                job_id: existing_job_id.clone(),
                                status_url: format!("/api/v1/jobs/{existing_job_id}"),
                                state: JobPhase::Queued,
                            })
                            .unwrap_or_default(),
                        });
                    }
                    LeaseCreatePersistence::Created(created) => {
                        let update = LeaseStateUpdate {
                            from: LeaseState::Requested,
                            to: LeaseState::Admitting,
                            instance_id: None,
                            endpoint_id: None,
                            expires_at: None,
                        };
                        let admitting = match repository
                            .transition(
                                &created.id,
                                &created.owner_id,
                                created.row_version,
                                created.generation,
                                &update,
                                &context,
                            )
                            .await
                        {
                            Ok(record) => record,
                            Err(error) => {
                                self.inner.state.lock().leases.remove(&lease_id);
                                if let Some(jobs) = &self.job_repository {
                                    let _ = jobs.fail(job_id, &error).await;
                                }
                                return Err(error);
                            }
                        };
                        if let Some(entry) = self.inner.state.lock().leases.get_mut(&lease_id) {
                            entry.record = admitting;
                        }
                        Some(queued)
                    }
                }
            } else {
                None
            };
        if let Err(error) =
            self.inner
                .secrets
                .store(&format!("larm/lease/{lease_id}"), &request.owner_id, &token)
        {
            self.fail_lease(&lease_id);
            if let Some(outcome) = durable_outcome.clone() {
                self.fail_persisted_lease(&lease_id).await;
                if let (Some(jobs), Some(job_id)) = (&self.job_repository, &durable_job_id) {
                    let failure = ApiError::new(
                        "AUTH_BACKEND_UNAVAILABLE",
                        "No se pudo guardar la credencial de la lease",
                        "secret_store_failed",
                    )
                    .retryable(true)
                    .with_original(
                        "credential_manager",
                        "STORE_FAILED",
                        &error,
                    );
                    let _ = jobs.fail(job_id, &failure).await;
                }
                self.inner.state.lock().replies.insert(
                    scope_key,
                    IdempotentReply {
                        body_hash,
                        outcome: outcome.clone(),
                    },
                );
                return Ok(outcome);
            }
            return Err(ApiError::new(
                "AUTH_BACKEND_UNAVAILABLE",
                "No se pudo guardar la credencial de la lease",
                "secret_store_failed",
            )
            .retryable(true)
            .with_original("credential_manager", "STORE_FAILED", &error));
        }
        let outcome = if already_loaded && !durable {
            match self.activate(&lease_id).await {
                Ok(response) => CreateLeaseOutcome {
                    status: 201,
                    body: serde_json::to_value(response).unwrap_or_default(),
                },
                Err(error) => {
                    self.fail_lease(&lease_id);
                    return Err(error);
                }
            }
        } else {
            let job_id = durable_job_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            if durable {
                let jobs = self.job_repository.as_ref().ok_or_else(|| {
                    ApiError::new(
                        "INTERNAL",
                        "Falta el repositorio de jobs",
                        "lease_job_repository_missing",
                    )
                })?;
                if let Err(error) = jobs.running(&job_id).await {
                    self.fail_lease(&lease_id);
                    self.fail_persisted_lease(&lease_id).await;
                    let _ = jobs.fail(&job_id, &error).await;
                    let outcome = durable_outcome
                        .clone()
                        .unwrap_or_else(|| CreateLeaseOutcome {
                            status: 202,
                            body: serde_json::to_value(JobReceipt {
                                job_id: job_id.clone(),
                                status_url: format!("/api/v1/jobs/{job_id}"),
                                state: JobPhase::Queued,
                            })
                            .unwrap_or_default(),
                        });
                    self.inner.state.lock().replies.insert(
                        scope_key,
                        IdempotentReply {
                            body_hash,
                            outcome: outcome.clone(),
                        },
                    );
                    return Ok(outcome);
                }
            }
            let receipt = JobReceipt {
                job_id: job_id.clone(),
                status_url: format!("/api/v1/jobs/{job_id}"),
                state: JobPhase::Running,
            };
            self.inner.state.lock().jobs.insert(
                job_id.clone(),
                JobEntry {
                    owner: principal.name.clone(),
                    state: JobState {
                        job_id: job_id.clone(),
                        state: JobPhase::Running,
                        resource_type: "runtime_lease".into(),
                        resource_id: Some(lease_id.clone()),
                        progress: serde_json::Map::new(),
                        result: None,
                        error: None,
                        evidence_refs: Vec::new(),
                    },
                },
            );
            let bridge = self.clone();
            let lease = lease_id.clone();
            tokio::spawn(async move {
                let result = bridge.activate(&lease).await;
                if result.is_err() {
                    bridge.fail_lease(&lease);
                    if durable {
                        bridge.fail_persisted_lease(&lease).await;
                    }
                }
                if durable {
                    if let Some(jobs) = &bridge.job_repository {
                        let persisted = match &result {
                            Ok(response) => {
                                jobs.complete(
                                    &job_id,
                                    &lease,
                                    serde_json::to_value(response).unwrap_or(Value::Null),
                                    &[],
                                )
                                .await
                            }
                            Err(error) => jobs.fail(&job_id, error).await,
                        };
                        if let Err(error) = persisted {
                            tracing::error!(
                                job_id = %job_id,
                                reason = %error.details.reason,
                                "no se pudo persistir el resultado del job de lease"
                            );
                        }
                    }
                }
                let mut state = bridge.inner.state.lock();
                if let Some(job) = state.jobs.get_mut(&job_id) {
                    match result {
                        Ok(response) => {
                            job.state.state = JobPhase::Completed;
                            job.state.result = serde_json::to_value(response).ok();
                        }
                        Err(error) => {
                            job.state.state = JobPhase::Failed;
                            job.state.error = Some(error);
                        }
                    }
                }
            });
            durable_outcome.unwrap_or_else(|| CreateLeaseOutcome {
                status: 202,
                body: serde_json::to_value(receipt).unwrap_or_default(),
            })
        };
        self.inner.state.lock().replies.insert(
            scope_key,
            IdempotentReply {
                body_hash,
                outcome: outcome.clone(),
            },
        );
        Ok(outcome)
    }

    async fn get_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
    ) -> Result<RuntimeLease, ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        let current = {
            let state = self.inner.state.lock();
            state.leases.get(lease_id).map(|entry| {
                (
                    entry.record.clone(),
                    entry.record.owner_id == principal.name,
                )
            })
        };
        if let Some((record, owned)) = current {
            if !owned {
                return Err(forbidden("La lease pertenece a otro propietario"));
            }
            return Ok(record);
        }
        match &self.lease_repository {
            Some(repository) => repository
                .get_for_owner(lease_id, &principal.name)
                .await?
                .ok_or_else(|| not_found("lease", lease_id)),
            None => Err(not_found("lease", lease_id)),
        }
    }

    async fn list_leases(&self, principal: &Principal) -> Result<Page<RuntimeLease>, ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        let mut by_id: HashMap<String, RuntimeLease> = self
            .persisted_leases(&principal.name)
            .await?
            .into_iter()
            .map(|lease| (lease.id.clone(), lease))
            .collect();
        let current = {
            let state = self.inner.state.lock();
            state
                .leases
                .values()
                .filter(|entry| entry.record.owner_id == principal.name)
                .map(|entry| entry.record.clone())
                .collect::<Vec<_>>()
        };
        for lease in current {
            by_id.insert(lease.id.clone(), lease);
        }
        let mut items: Vec<RuntimeLease> = by_id.into_values().collect();
        items.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        Ok(Page {
            items,
            next_cursor: None,
            snapshot_at: now_ts(),
        })
    }

    async fn renew_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
        if_match: u64,
        idempotency_key: &str,
        renew: Renew,
    ) -> Result<RuntimeLease, ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        if !(MIN_TTL..=MAX_TTL).contains(&renew.ttl_seconds) {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "ttl_seconds fuera de 30..600",
                "ttl_out_of_range",
            ));
        }
        let scope_key = format!("{}:{lease_id}:{idempotency_key}", principal.name);
        let body_hash = canonical_hash(&(lease_id, if_match, &renew));
        if let Some(repository) = &self.lease_repository {
            let cached_owner = self
                .inner
                .state
                .lock()
                .leases
                .get(lease_id)
                .map(|entry| entry.record.owner_id.clone());
            let Some(cached_owner) = cached_owner else {
                return Err(not_found("lease", lease_id));
            };
            if cached_owner != principal.name {
                return Err(forbidden("La lease pertenece a otro propietario"));
            }
            let ttl = Duration::from_secs(renew.ttl_seconds);
            let context = MutationContext {
                actor_id: principal.name.clone(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            };
            let renewal = LeaseRenewal {
                owner_id: principal.name.clone(),
                if_match,
                generation: renew.generation,
                idempotency_key: idempotency_key.into(),
                body_hash,
                expires_at: ts_in(ttl),
            };
            let renewed = repository
                .renew_active(lease_id, &renewal, &context)
                .await?;
            let current = repository
                .get_for_owner(lease_id, &principal.name)
                .await?
                .unwrap_or_else(|| renewed.clone());
            {
                let mut state = self.inner.state.lock();
                if let Some(entry) = state.leases.get_mut(lease_id) {
                    entry.record = current.clone();
                    entry.expires = current
                        .expires_at
                        .as_deref()
                        .filter(|_| current.state == LeaseState::Active)
                        .map(|expires_at| Instant::now() + duration_until(expires_at));
                    entry.grace_until = (current.state == LeaseState::Expiring)
                        .then(|| Instant::now() + EXPIRY_GRACE);
                }
            }
            return Ok(renewed);
        }
        let mut state = self.inner.state.lock();
        if let Some((hash, record)) = state.renewals.get(&scope_key) {
            if *hash == body_hash {
                return Ok(record.clone());
            }
            return Err(ApiError::new(
                "IDEMPOTENCY_CONFLICT",
                "Misma Idempotency-Key con otro cuerpo",
                "idempotency_conflict",
            ));
        }
        let entry = state
            .leases
            .get_mut(lease_id)
            .ok_or_else(|| not_found("lease", lease_id))?;
        if entry.record.owner_id != principal.name {
            return Err(forbidden("La lease pertenece a otro propietario"));
        }
        if entry.record.row_version != if_match {
            return Err(ApiError::new(
                "STALE_REVISION",
                "row_version desactualizada",
                "stale_revision",
            )
            .with_resource(lease_id));
        }
        if entry.record.state != LeaseState::Active || entry.record.generation != renew.generation {
            return Err(ApiError::new(
                "INVALID_STATE_TRANSITION",
                "La lease no está ACTIVE en esa generación",
                "lease_not_active",
            )
            .with_resource(lease_id));
        }
        // Repone desde ahora, no acumula (contrato 04).
        let ttl = Duration::from_secs(renew.ttl_seconds);
        entry.expires = Some(Instant::now() + ttl);
        entry.record.expires_at = Some(ts_in(ttl));
        Self::touch(&mut entry.record);
        let record = entry.record.clone();
        state
            .renewals
            .insert(scope_key, (body_hash, record.clone()));
        Ok(record)
    }

    async fn release_lease(
        &self,
        principal: &Principal,
        lease_id: &str,
        if_match: u64,
    ) -> Result<(), ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        if let Some(repository) = &self.lease_repository {
            let cached = {
                let state = self.inner.state.lock();
                state
                    .leases
                    .get(lease_id)
                    .map(|entry| (entry.record.owner_id.clone(), entry.variant_id.clone()))
            };
            if cached
                .as_ref()
                .is_some_and(|(owner, _)| owner != &principal.name)
            {
                return Err(forbidden("La lease pertenece a otro propietario"));
            }
            let record = repository
                .get_for_owner(lease_id, &principal.name)
                .await?
                .ok_or_else(|| not_found("lease", lease_id))?;
            if record.state.is_terminal() {
                if let Some((_, variant_id)) = cached {
                    self.forget_secret(lease_id, &principal.name);
                    self.relax_if_idle(&variant_id);
                }
                return Ok(());
            }
            if record.state != LeaseState::Active {
                return Err(ApiError::new(
                    "INVALID_STATE_TRANSITION",
                    "Sólo se puede liberar una lease ACTIVE",
                    "lease_not_active",
                )
                .with_resource(lease_id));
            }
            let context = MutationContext {
                actor_id: principal.name.clone(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            };
            let update = LeaseStateUpdate {
                from: LeaseState::Active,
                to: LeaseState::Released,
                instance_id: None,
                endpoint_id: None,
                expires_at: None,
            };
            let released = repository
                .transition(
                    lease_id,
                    &principal.name,
                    if_match,
                    record.generation,
                    &update,
                    &context,
                )
                .await?;
            {
                let mut state = self.inner.state.lock();
                if let Some(entry) = state.leases.get_mut(lease_id) {
                    entry.record = released;
                    entry.expires = None;
                    entry.grace_until = None;
                }
            }
            self.forget_secret(lease_id, &principal.name);
            if let Some((_, variant_id)) = cached {
                self.relax_if_idle(&variant_id);
            }
            return Ok(());
        }
        let (owner, variant_id) = {
            let mut state = self.inner.state.lock();
            let entry = state
                .leases
                .get_mut(lease_id)
                .ok_or_else(|| not_found("lease", lease_id))?;
            if entry.record.owner_id != principal.name {
                return Err(forbidden("La lease pertenece a otro propietario"));
            }
            if entry.record.state.is_terminal() {
                return Ok(());
            }
            if entry.record.row_version != if_match {
                return Err(ApiError::new(
                    "STALE_REVISION",
                    "row_version desactualizada",
                    "stale_revision",
                )
                .with_resource(lease_id));
            }
            if entry.record.state != LeaseState::Active {
                return Err(ApiError::new(
                    "INVALID_STATE_TRANSITION",
                    "Sólo se puede liberar una lease ACTIVE",
                    "lease_not_active",
                )
                .with_resource(lease_id));
            }
            entry.record.state = LeaseState::Released;
            entry.expires = None;
            Self::touch(&mut entry.record);
            (entry.record.owner_id.clone(), entry.variant_id.clone())
        };
        self.forget_secret(lease_id, &owner);
        self.relax_if_idle(&variant_id);
        Ok(())
    }

    async fn get_job(&self, principal: &Principal, job_id: &str) -> Result<JobState, ApiError> {
        require(principal, ApiScope::Read)?;
        if let Some(repository) = &self.job_repository {
            if let Some(job) = repository.get(job_id).await? {
                if job.actor_id != principal.name {
                    return Err(not_found("job", job_id));
                }
                return Ok(job.state);
            }
        }
        let state = self.inner.state.lock();
        let job = state
            .jobs
            .get(job_id)
            .ok_or_else(|| not_found("job", job_id))?;
        if job.owner != principal.name {
            return Err(not_found("job", job_id));
        }
        Ok(job.state.clone())
    }

    async fn gateway(
        &self,
        lease_id: &str,
        bearer: Option<&str>,
        path: &str,
        mut body: Value,
    ) -> Result<GatewayResponse, ApiError> {
        if !matches!(path, "chat/completions" | "embeddings") {
            return Err(ApiError::new(
                "NOT_FOUND",
                "Ruta de protocolo no anunciada",
                "protocol_route_unknown",
            ));
        }
        let (model, variant_id) = self.gateway_model(lease_id, bearer)?;
        let Some(object) = body.as_object_mut() else {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "El cuerpo debe ser un objeto JSON",
                "body_not_object",
            ));
        };
        if object.get("stream").and_then(Value::as_bool) == Some(true) {
            return Err(ApiError::new(
                "UNSUPPORTED_CAPABILITY",
                "El perfil de protocolo no anuncia streaming",
                "streaming_not_announced",
            ));
        }
        let variant = self
            .variant(&variant_id)
            .ok_or_else(|| not_found("variant", &variant_id))?;
        // Re-read remote metadata before dispatch: a tag can be replaced while
        // the lease remains ACTIVE. This does not make the external host an
        // atomic or isolated execution boundary.
        self.probe(variant).await?;
        // Metadata IO can outlive a concurrent release/expiry. Revalidate the
        // endpoint credential and lease state after that IO, before dispatch.
        self.gateway_model(lease_id, bearer)?;
        // La identidad la fija la lease: el modelo que pida el cliente no
        // puede cambiar qué variante se sirve.
        object.insert("model".into(), Value::String(model));
        let response = self
            .inner
            .http
            .post(self.base(&format!("/v1/{path}")))
            .timeout(Duration::from_secs(
                self.inner.config.inference_timeout_seconds,
            ))
            .json(&body)
            .send()
            .await
            .map_err(|e| upstream(&e, path))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response.bytes().await.map_err(|e| upstream(&e, path))?;
        Ok(GatewayResponse {
            status,
            content_type,
            body: bytes.to_vec(),
        })
    }

    async fn endpoint_credential(
        &self,
        principal: &Principal,
        lease_id: &str,
        generation: u64,
        credential_reference_id: &str,
    ) -> Result<EndpointCredential, ApiError> {
        require(principal, ApiScope::LeasesManage)?;
        let state = self.inner.state.lock();
        let entry = state
            .leases
            .get(lease_id)
            .ok_or_else(|| not_found("lease", lease_id))?;
        if entry.record.owner_id != principal.name {
            return Err(forbidden("La lease pertenece a otro propietario"));
        }
        if entry.record.generation != generation || entry.record.state != LeaseState::Active {
            return Err(ApiError::new(
                "INVALID_STATE_TRANSITION",
                "La lease no está ACTIVE en esa generación",
                "lease_not_active",
            ));
        }
        if entry.credential_reference != credential_reference_id {
            return Err(forbidden(
                "La referencia de credencial no es la de la lease",
            ));
        }
        Ok(EndpointCredential {
            token: entry.token.clone(),
            expires_at: entry.record.expires_at.clone().unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests;
