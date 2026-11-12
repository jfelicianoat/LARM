//! Configuración tipada de LARM con defaults seguros y precedencia
//! `defaults < config file < env < CLI` (P003).
//!
//! `Settings` tiene exactamente la forma del schema `Settings` de
//! `larm.schema.json` (sin campos adicionales) y sus defaults son los de
//! `docs/contracts/defaults.json`. Bytes como string decimal (contrato 00),
//! enums cerrados donde el schema los fija y validación de conformidad v3:
//! - H08: `storage.download_bandwidth_limit_bytes_per_second` es `null`
//!   (sin límite) o un entero positivo; cero se rechaza.
//! - H09: `storage.low_disk_thresholds` admite `PERCENT` (basis points) o
//!   `BYTES` (valores absolutos), siempre `critical < warning`.
//!
//! Nivel y formato de log no son configuración persistida: los fija
//! [`LoggingOptions`] desde CLI o entorno (`LARM_LOG_LEVEL`, `LARM_LOG_FORMAT`).

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::units::DecimalU64;
use larm_domain::{bridge::is_uuid_v7, runtime::RuntimeRetentionPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Raíz de configuración de LARM (schema `Settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Settings {
    pub schema_version: String,
    pub api: ApiSettings,
    pub storage: StorageSettings,
    pub leases: LeaseSettings,
    pub process: ProcessSettings,
    pub adapters: AdapterSettings,
    pub events: EventSettings,
    pub watchers: WatcherSettings,
    pub benchmarks: BenchmarkSettings,
    pub security: SecuritySettings,
    pub diagnostics: DiagnosticsSettings,
    pub scheduler: SchedulerSettings,
    pub service: ServiceSettings,
    pub runtime_profiles: RuntimeProfileSettings,
    pub runtime_versions: RuntimeVersionsSettings,
    pub paths: PathSettings,
    pub privacy: PrivacySettings,
}

/// Secciones de primer nivel; solo `LARM_<SECCIÓN>__<CLAVE>` es override de config.
pub const SECTIONS: &[&str] = &[
    "api",
    "storage",
    "leases",
    "process",
    "adapters",
    "events",
    "watchers",
    "benchmarks",
    "security",
    "diagnostics",
    "scheduler",
    "service",
    "runtime_profiles",
    "runtime_versions",
    "paths",
    "privacy",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApiSettings {
    pub bind: String,
    pub port: u16,
    pub remote_enabled: bool,
    pub request_body_bytes: u64,
    pub page_default: u32,
    pub page_max: u32,
    pub request_timeout_seconds: u64,
    pub connect_timeout_seconds: u64,
    pub job_status_poll_seconds: u64,
    pub allowed_methods: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AllocationPolicy {
    AskAlways,
    PreferredLocation,
    MostFreeSpace,
    FastestStorage,
    ByModelType,
    CustomRules,
    BestFit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct StorageSettings {
    pub minimum_margin_bytes: DecimalU64,
    pub margin_basis_points: u64,
    pub minimum_free_bytes: DecimalU64,
    pub unknown_size_policy: String,
    pub ownership_default: String,
    pub cleanup_default: String,
    pub allocation_policy: AllocationPolicy,
    pub download_global_concurrency: u32,
    pub download_per_source_concurrency: u32,
    pub artifact_download_streams: u32,
    pub chunk_bytes: u64,
    pub retry_seconds: Vec<u64>,
    pub retries: u32,
    pub preferred_location_id: Option<String>,
    pub model_type_locations: ModelTypeLocations,
    pub auto_download_policy: AutoDownloadPolicy,
    /// H08: `null` sin límite; string decimal positivo para limitar. Cero inválido.
    pub download_bandwidth_limit_bytes_per_second: Option<DecimalU64>,
    /// H09: umbrales de disco por volumen, PERCENT (bps) o BYTES.
    pub low_disk_thresholds: LowDiskThresholds,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelTypeLocations {
    #[serde(rename = "LLM")]
    pub llm: Option<String>,
    #[serde(rename = "DIFFUSION")]
    pub diffusion: Option<String>,
    #[serde(rename = "AUDIO")]
    pub audio: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AutoDownloadPolicy {
    pub enabled: bool,
    pub allowed_source_ids: Vec<String>,
    pub allowed_capabilities: Vec<String>,
    pub minimum_trust_score: u32,
    pub max_daily_bytes: DecimalU64,
    pub target_storage_location_id: Option<String>,
    pub accepted_license_hashes: Vec<String>,
}

/// H09: dos modos excluyentes (schema `LowDiskThresholds`); el orden se valida en [`Settings::validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum LowDiskThresholds {
    Percent {
        warning_basis_points: u16,
        critical_basis_points: u16,
    },
    Bytes {
        warning_bytes: DecimalU64,
        critical_bytes: DecimalU64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct LeaseSettings {
    pub ttl_seconds: u64,
    pub min_ttl_seconds: u64,
    pub max_ttl_seconds: u64,
    pub renew_after_seconds: u64,
    pub expiry_grace_seconds: u64,
    pub idle_unload_seconds: u64,
    pub admission_wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProcessSettings {
    pub start_timeout_seconds: u64,
    pub load_timeout_seconds: u64,
    pub health_timeout_seconds: u64,
    pub health_interval_seconds: u64,
    pub health_failures_degraded: u32,
    pub health_failures_failed: u32,
    pub stop_grace_seconds: u64,
    pub port_min: u16,
    pub port_max: u16,
    pub crash_restart_seconds: Vec<u64>,
    pub crash_window_seconds: u64,
    pub crash_max_restarts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AdapterSettings {
    pub protocol: String,
    pub frame_max_bytes: u64,
    pub handshake_timeout_seconds: u64,
    pub rpc_timeout_seconds: u64,
    pub heartbeat_seconds: u64,
    pub cancel_grace_seconds: u64,
    pub stderr_limit_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EventSettings {
    pub retention_days: u32,
    pub replay_limit: u32,
    pub heartbeat_seconds: u64,
    pub subscriber_queue: u32,
    pub notification_dedupe_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WatcherSettings {
    pub interval_seconds: u64,
    pub jitter_max_seconds: u64,
    pub max_backoff_seconds: u64,
    pub automatic_download: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct BenchmarkSettings {
    pub warmup: u32,
    pub repetitions: u32,
    pub seed: u64,
    pub max_job_seconds: u64,
    pub throughput_regression_basis_points: u64,
    pub latency_regression_basis_points: u64,
    pub memory_regression_basis_points: u64,
    pub technical_fit_weights: FitWeights,
    pub recommendation_weights: RecommendationWeights,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FitWeights {
    pub memory: u32,
    pub backend: u32,
    pub maturity: u32,
    pub speed: u32,
    pub storage: u32,
    pub context: u32,
    pub stability: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecommendationWeights {
    pub technical: u32,
    pub evaluation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AutoUnloadPolicy {
    Never,
    IdleTimeout,
    MemoryPressure,
    Lru,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SchedulerSettings {
    pub system_reserve_bytes: DecimalU64,
    pub system_reserve_basis_points: u64,
    pub max_starting: u32,
    pub priorities: SchedulerPriorities,
    pub auto_unload_policy: AutoUnloadPolicy,
    pub warm_variant_ids: Vec<String>,
    pub benchmark_priority: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub struct SchedulerPriorities {
    pub pinned: u32,
    pub high: u32,
    pub normal: u32,
    pub background: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServiceMode {
    UserAgent,
    WindowsService,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ServiceSettings {
    pub mode: ServiceMode,
    pub auto_start: bool,
    pub service_install: bool,
    pub account_sid: Option<String>,
    pub credentials_pipe_allowed_sids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LaunchProfileKind {
    Eco,
    Balanced,
    Performance,
    LongContext,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuntimeProfileSettings {
    pub default_launch_profile: LaunchProfileKind,
    pub allow_backend_fallback: bool,
    pub pinned_variant_ids: Vec<String>,
    pub model_mcp_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuntimeVersionsSettings {
    pub retention: RuntimeRetentionPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PathSettings {
    pub data_root: String,
    pub database_relative_path: String,
    pub models_relative_path: String,
    pub runtimes_relative_path: String,
    pub logs_relative_path: String,
    pub service_data_root: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PrivacySettings {
    pub external_telemetry: bool,
    pub include_personal_paths: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SecuritySettings {
    pub allowed_origins: Vec<String>,
    pub token_backend: String,
    pub default_trust: String,
    pub model_mcp_enabled: bool,
    pub remote_tls_minimum: String,
    pub approval_ttl_seconds: u64,
    pub remote_cert_path: Option<String>,
    pub remote_key_credential_id: Option<String>,
    pub registry_url: Option<String>,
    pub publisher_keys: std::collections::BTreeMap<String, String>,
    pub registry_allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DiagnosticsSettings {
    /// Días de log conservados (rotación diaria); 0 conserva solo el del día.
    pub log_retention_days: u32,
    pub bundle_max_bytes: u64,
    pub include_user_content: bool,
}

/// Error de configuración: `VALIDATION_FAILED` con el detalle técnico solo en log.
fn config_error(detail: impl Into<String>) -> LarmError {
    let detail = detail.into();
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración inválida",
        "config_invalid",
    )
    .with_technical(detail)
}

impl Settings {
    /// Defaults del contrato `docs/contracts/defaults.json`, tal cual.
    #[must_use]
    pub fn defaults() -> Self {
        serde_json::from_str(include_str!("../../../../docs/contracts/defaults.json"))
            .unwrap_or_else(|e| unreachable!("defaults.json del contrato no tipa Settings: {e}"))
    }

    /// Aplica una capa de override JSON (objeto parcial) sobre un Value.
    fn merge_over(base: &Value, over: &Value) -> Value {
        match (base, over) {
            // Unión etiquetada (p. ej. low_disk_thresholds): cambiar `mode`
            // sustituye el objeto entero; mezclar dejaría campos del otro modo.
            (Value::Object(b), Value::Object(o))
                if o.get("mode").is_some() && o.get("mode") != b.get("mode") =>
            {
                over.clone()
            }
            (Value::Object(b), Value::Object(o)) => {
                let mut out = b.clone();
                for (k, v) in o {
                    let merged = match out.get(k) {
                        Some(prev) => Self::merge_over(prev, v),
                        None => v.clone(),
                    };
                    out.insert(k.clone(), merged);
                }
                Value::Object(out)
            }
            (_, o) => o.clone(),
        }
    }

    /// Overrides `LARM_<SECCIÓN>__<CLAVE>` desde las variables reales del proceso.
    #[must_use]
    pub fn overrides_from_env() -> Value {
        let pairs: Vec<(String, String)> = std::env::vars().collect();
        Self::overrides_from_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }

    /// Construye overrides JSON desde pares `LARM_API__PORT=9000`.
    ///
    /// Solo cuentan las claves con una sección de [`SECTIONS`] seguida de `__`;
    /// otras `LARM_*` (p. ej. `LARM_LOG_LEVEL` o variables de prueba) no son
    /// configuración. Un valor solo de dígitos se conserva como string si el
    /// default de esa ruta es string o null (bytes del contrato 00); en otro
    /// caso se interpreta como JSON escalar y, si no lo es, como string.
    #[must_use]
    pub fn overrides_from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
        let defaults = serde_json::to_value(Self::defaults()).unwrap_or(Value::Null);
        let mut root = serde_json::Map::new();
        for (key, raw) in pairs {
            let Some(rest) = key.strip_prefix("LARM_") else {
                continue;
            };
            let path: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
            if path.len() < 2 || !SECTIONS.contains(&path[0].as_str()) {
                continue;
            }
            let default_at = path.iter().try_fold(&defaults, |v, k| v.get(k));
            let keep_string = raw.bytes().all(|b| b.is_ascii_digit())
                && !raw.is_empty()
                && matches!(default_at, Some(Value::String(_) | Value::Null));
            let parsed = if keep_string {
                Value::String(raw.to_string())
            } else {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
            };
            set_json_path(&mut root, &path, parsed);
        }
        Value::Object(root)
    }

    /// Compone la configuración final: defaults < file < env < cli, y la valida.
    pub fn compose(
        file: Option<&Value>,
        env: Option<&Value>,
        cli: Option<&Value>,
    ) -> Result<Self, LarmError> {
        Self::compose_with_base(None, file, env, cli)
    }

    /// Compone sobre Settings persistido sin alterar la precedencia publicada:
    /// defaults < persistido < config file < env < CLI.
    pub fn compose_with_base(
        base: Option<&Value>,
        file: Option<&Value>,
        env: Option<&Value>,
        cli: Option<&Value>,
    ) -> Result<Self, LarmError> {
        let mut value = serde_json::to_value(Settings::defaults())
            .map_err(|e| config_error(format!("serializando defaults: {e}")))?;
        for layer in [base, file, env, cli].into_iter().flatten() {
            value = Self::merge_over(&value, layer);
        }
        let settings: Settings = serde_json::from_value(value)
            .map_err(|e| config_error(format!("config no válida tras aplicar overrides: {e}")))?;
        settings.validate()?;
        Ok(settings)
    }

    /// Restricciones del schema que el tipo no expresa y conformidad v3 (H08/H09).
    pub fn validate(&self) -> Result<(), LarmError> {
        self.runtime_versions.retention.validate()?;
        // H08: límite de velocidad null o positivo (cero rechazado).
        if self
            .storage
            .download_bandwidth_limit_bytes_per_second
            .is_some_and(|l| l.get() == 0)
        {
            return Err(config_error(
                "storage.download_bandwidth_limit_bytes_per_second = 0 no permitido; use null para sin límite",
            ));
        }

        // H09: rangos del schema y critical < warning estricto en el modo activo.
        match &self.storage.low_disk_thresholds {
            LowDiskThresholds::Percent {
                warning_basis_points: w,
                critical_basis_points: c,
            } => {
                if !(1..=10_000).contains(w) || *c > 9_999 {
                    return Err(config_error(
                        "low_disk_thresholds PERCENT: warning 1..=10000 y critical 0..=9999 basis points",
                    ));
                }
                if c >= w {
                    return Err(config_error(
                        "low_disk_thresholds: critical debe ser < warning (PERCENT)",
                    ));
                }
            }
            LowDiskThresholds::Bytes {
                warning_bytes: w,
                critical_bytes: c,
            } => {
                if c >= w {
                    return Err(config_error(
                        "low_disk_thresholds: critical debe ser < warning (BYTES)",
                    ));
                }
            }
        }

        if self.api.port == 0 {
            return Err(config_error("api.port debe estar en 1..=65535"));
        }
        if self.leases.min_ttl_seconds > self.leases.max_ttl_seconds {
            return Err(config_error("leases.min_ttl_seconds > max_ttl_seconds"));
        }
        if self.process.port_min > self.process.port_max {
            return Err(config_error("process.port_min > port_max"));
        }
        if !(1..=10_000).contains(&self.events.replay_limit) {
            return Err(config_error(
                "events.replay_limit debe estar en 1..=10000 para SSE",
            ));
        }

        // Identificadores UUIDv7 del schema.
        let ids = [
            (
                "storage.preferred_location_id",
                &self.storage.preferred_location_id,
            ),
            (
                "storage.model_type_locations.LLM",
                &self.storage.model_type_locations.llm,
            ),
            (
                "storage.model_type_locations.DIFFUSION",
                &self.storage.model_type_locations.diffusion,
            ),
            (
                "storage.model_type_locations.AUDIO",
                &self.storage.model_type_locations.audio,
            ),
            (
                "storage.auto_download_policy.target_storage_location_id",
                &self.storage.auto_download_policy.target_storage_location_id,
            ),
            (
                "security.remote_key_credential_id",
                &self.security.remote_key_credential_id,
            ),
        ];
        for (path, id) in ids {
            if let Some(id) = id {
                if !is_uuid_v7(id) {
                    return Err(config_error(format!("{path} no es un UUIDv7")));
                }
            }
        }

        // Listas cuyos elementos el schema exige no vacíos.
        let lists = [
            (
                "storage.auto_download_policy.allowed_source_ids",
                &self.storage.auto_download_policy.allowed_source_ids,
            ),
            (
                "storage.auto_download_policy.allowed_capabilities",
                &self.storage.auto_download_policy.allowed_capabilities,
            ),
            (
                "storage.auto_download_policy.accepted_license_hashes",
                &self.storage.auto_download_policy.accepted_license_hashes,
            ),
            ("security.allowed_origins", &self.security.allowed_origins),
            (
                "scheduler.warm_variant_ids",
                &self.scheduler.warm_variant_ids,
            ),
            (
                "service.credentials_pipe_allowed_sids",
                &self.service.credentials_pipe_allowed_sids,
            ),
            (
                "runtime_profiles.pinned_variant_ids",
                &self.runtime_profiles.pinned_variant_ids,
            ),
        ];
        for (path, list) in lists {
            if list.iter().any(String::is_empty) {
                return Err(config_error(format!("{path} contiene un elemento vacío")));
            }
        }

        // P028: los pins son claves públicas Ed25519 y las excepciones de
        // origen sólo pueden ampliar el transporte a orígenes HTTPS exactos.
        let security = &self.security;
        if let Some(registry_url) = &security.registry_url {
            if registry_url.len() > 4096 || !is_https_url_without_credentials(registry_url) {
                return Err(config_error(
                    "security.registry_url debe ser HTTPS sin credenciales, query ni fragmento",
                ));
            }
            if security.publisher_keys.is_empty() {
                return Err(config_error(
                    "security.registry_url exige al menos una clave fijada",
                ));
            }
        }
        for (key_id, encoded) in &security.publisher_keys {
            if key_id.is_empty()
                || key_id.len() > 128
                || !key_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || encoded.len() != 64
                || !encoded
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(config_error(
                    "security.publisher_keys contiene un id o pin Ed25519 inválido",
                ));
            }
        }
        let mut origins = std::collections::BTreeSet::new();
        for origin in &security.registry_allowed_origins {
            if origin.len() > 512 || !is_canonical_https_origin(origin) || !origins.insert(origin) {
                return Err(config_error("security.registry_allowed_origins debe contener orígenes HTTPS canónicos y únicos"));
            }
        }

        // Seguridad: el acceso remoto no se activa con el bind local por defecto.
        if self.api.remote_enabled && self.api.bind == "127.0.0.1" {
            return Err(config_error(
                "api.remote_enabled exige bind distinto de 127.0.0.1 (o permanecer off)",
            ));
        }
        Ok(())
    }
}

fn is_https_url_without_credentials(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

fn is_canonical_https_origin(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.origin().ascii_serialization() == value
}

/// Fija `value` en `root` siguiendo `path`.
fn set_json_path(root: &mut serde_json::Map<String, Value>, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cur = root;
    for key in parents {
        let entry = cur
            .entry(key.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(serde_json::Map::new());
        }
        match entry {
            Value::Object(next) => cur = next,
            _ => return,
        }
    }
    cur.insert(last.clone(), value);
}

/// Nivel de log admitido.
pub const LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];
/// Formato de log admitido.
pub const LOG_FORMATS: &[&str] = &["pretty", "json"];

/// Opciones de logging del proceso (no persistidas en `Settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingOptions {
    pub level: String,
    pub format: String,
}

impl LoggingOptions {
    /// `info` + `pretty` < `LARM_LOG_LEVEL`/`LARM_LOG_FORMAT` < CLI. Un valor
    /// fuera de la lista se rechaza: nunca se sustituye en silencio por el default.
    pub fn resolve(
        env_level: Option<&str>,
        env_format: Option<&str>,
        cli_level: Option<&str>,
        cli_format: Option<&str>,
    ) -> Result<Self, LarmError> {
        let level = cli_level
            .or(env_level)
            .unwrap_or("info")
            .to_ascii_lowercase();
        let format = cli_format
            .or(env_format)
            .unwrap_or("pretty")
            .to_ascii_lowercase();
        if !LOG_LEVELS.contains(&level.as_str()) {
            return Err(config_error(format!(
                "nivel de log {level:?} no válido; admitidos {LOG_LEVELS:?}"
            )));
        }
        if !LOG_FORMATS.contains(&format.as_str()) {
            return Err(config_error(format!(
                "formato de log {format:?} no válido; admitidos {LOG_FORMATS:?}"
            )));
        }
        Ok(Self { level, format })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serde_json::json;

    fn env_of(pairs: &[(&str, &str)]) -> Value {
        Settings::overrides_from_pairs(pairs.iter().copied())
    }

    fn compose_file(file: Value) -> Result<Settings, LarmError> {
        Settings::compose(Some(&file), None, None)
    }

    #[test]
    fn defaults_serializan_exactamente_el_contrato() {
        let contract: Value =
            serde_json::from_str(include_str!("../../../../docs/contracts/defaults.json")).unwrap();
        assert_eq!(
            serde_json::to_value(Settings::defaults()).unwrap(),
            contract
        );
        Settings::defaults().validate().unwrap();
    }

    #[test]
    fn precedencia_defaults_file_env_cli() {
        let file = json!({"api": {"port": 9000}});
        let s = Settings::compose(Some(&file), None, None).unwrap();
        assert_eq!(s.api.port, 9000);
        assert_eq!(s.api.bind, "127.0.0.1"); // resto de defaults intactos

        let env = env_of(&[("LARM_API__PORT", "9100")]);
        let s = Settings::compose(Some(&file), Some(&env), None).unwrap();
        assert_eq!(s.api.port, 9100);

        let cli = json!({"api": {"port": 9200}});
        let s = Settings::compose(Some(&file), Some(&env), Some(&cli)).unwrap();
        assert_eq!(s.api.port, 9200);
    }

    #[test]
    fn config_persistida_va_antes_de_file_env_y_cli() {
        let base = json!({"api": {"port": 8000}});
        let file = json!({"api": {"port": 9000}});
        let env = env_of(&[("LARM_API__PORT", "9100")]);
        let cli = json!({"api": {"port": 9200}});

        assert_eq!(
            Settings::compose_with_base(Some(&base), None, None, None)
                .unwrap()
                .api
                .port,
            8000
        );
        assert_eq!(
            Settings::compose_with_base(Some(&base), Some(&file), None, None)
                .unwrap()
                .api
                .port,
            9000
        );
        assert_eq!(
            Settings::compose_with_base(Some(&base), Some(&file), Some(&env), None)
                .unwrap()
                .api
                .port,
            9100
        );
        assert_eq!(
            Settings::compose_with_base(Some(&base), Some(&file), Some(&env), Some(&cli))
                .unwrap()
                .api
                .port,
            9200
        );
    }

    #[test]
    fn env_solo_secciones_conocidas_y_bytes_como_string() {
        let env = env_of(&[
            ("LARM_DESKTOP_SMOKE", "1"),
            ("LARM_LOG_LEVEL", "debug"),
            ("OTRA", "x"),
            ("LARM_STORAGE__MINIMUM_FREE_BYTES", "2147483648"),
            (
                "LARM_STORAGE__DOWNLOAD_BANDWIDTH_LIMIT_BYTES_PER_SECOND",
                "1048576",
            ),
            ("LARM_API__REMOTE_ENABLED", "false"),
        ]);
        assert_eq!(
            env,
            json!({"storage": {"minimum_free_bytes": "2147483648",
                "download_bandwidth_limit_bytes_per_second": "1048576"},
                "api": {"remote_enabled": false}})
        );
        let s = Settings::compose(None, Some(&env), None).unwrap();
        assert_eq!(s.storage.minimum_free_bytes.get(), 2_147_483_648);
        // un typo dentro de una sección conocida sí es error (config estricta)
        let typo = env_of(&[("LARM_API__PROT", "9000")]);
        assert!(Settings::compose(None, Some(&typo), None).is_err());
    }

    #[test]
    fn h08_limite_velocidad_null_o_positivo_cero_rechazado() {
        let d = serde_json::to_value(Settings::defaults()).unwrap();
        assert_eq!(
            d["storage"]["download_bandwidth_limit_bytes_per_second"],
            Value::Null
        );
        let s = compose_file(
            json!({"storage": {"download_bandwidth_limit_bytes_per_second": "1048576"}}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&s).unwrap()["storage"]
                ["download_bandwidth_limit_bytes_per_second"],
            json!("1048576")
        );
        assert!(compose_file(
            json!({"storage": {"download_bandwidth_limit_bytes_per_second": "0"}})
        )
        .is_err());
        // number JSON: el contrato exige string decimal
        assert!(compose_file(
            json!({"storage": {"download_bandwidth_limit_bytes_per_second": 1_048_576}})
        )
        .is_err());
    }

    #[test]
    fn h09_umbrales_percent_y_bytes_con_orden_valido() {
        let d = serde_json::to_value(Settings::defaults()).unwrap();
        assert_eq!(
            d["storage"]["low_disk_thresholds"],
            json!({"mode": "PERCENT", "warning_basis_points": 1500, "critical_basis_points": 500})
        );
        assert!(compose_file(json!({"storage": {"low_disk_thresholds": {
            "mode": "BYTES", "warning_bytes": "10737418240", "critical_bytes": "5368709120"}}}))
        .is_ok());
        for bad in [
            json!({"mode": "PERCENT", "warning_basis_points": 500, "critical_basis_points": 1500}),
            json!({"mode": "PERCENT", "warning_basis_points": 500, "critical_basis_points": 500}),
            json!({"mode": "PERCENT", "warning_basis_points": 10001, "critical_basis_points": 1}),
            json!({"mode": "BYTES", "warning_bytes": "100", "critical_bytes": "100"}),
            json!({"mode": "BYTES", "warning_bytes": "100", "critical_bytes": "200"}),
            // campos del otro modo mezclados: el schema no lo permite
            json!({"mode": "PERCENT", "warning_basis_points": 1500, "critical_basis_points": 500,
                   "warning_bytes": "1"}),
            json!({"mode": "BYTES", "warning_bytes": 100, "critical_bytes": 50}),
            json!({"mode": "BAR", "warning_basis_points": 1, "critical_basis_points": 0}),
        ] {
            let file = json!({"storage": {"low_disk_thresholds": bad.clone()}});
            assert!(compose_file(file).is_err(), "{bad}");
        }
    }

    #[test]
    fn p032_retencion_runtime_tiene_default_y_rango_contractual() {
        assert_eq!(
            Settings::defaults()
                .runtime_versions
                .retention
                .keep_last_validated,
            3
        );
        let env = Settings::overrides_from_pairs([(
            "LARM_RUNTIME_VERSIONS__RETENTION__KEEP_LAST_VALIDATED",
            "7",
        )]);
        assert_eq!(
            Settings::compose(None, Some(&env), None)
                .unwrap()
                .runtime_versions
                .retention
                .keep_last_validated,
            7
        );
        for keep_last_validated in [1, 51] {
            assert!(compose_file(json!({"runtime_versions": {"retention": {
                "keep_last_validated": keep_last_validated
            }}}))
            .is_err());
        }
    }

    #[test]
    fn negativos_de_schema_rechazados() {
        for bad in [
            json!({"api": {"puerto_typo": 1}}),
            json!({"log_level": "debug"}),
            json!({"api": {"port": 0}}),
            json!({"events": {"replay_limit": 0}}),
            json!({"events": {"replay_limit": 10001}}),
            json!({"storage": {"allocation_policy": "RANDOM"}}),
            json!({"scheduler": {"auto_unload_policy": "SOMETIMES"}}),
            json!({"service": {"mode": "DAEMON"}}),
            json!({"runtime_profiles": {"default_launch_profile": "TURBO"}}),
            json!({"storage": {"minimum_free_bytes": 1024}}),
            json!({"storage": {"preferred_location_id": "8f14e45f-ceea-4c3b-9e1d-2a7b5c3d4e5f"}}),
            json!({"security": {"allowed_origins": [""]}}),
            json!({"api": {"remote_enabled": true}}),
        ] {
            let e = compose_file(bad.clone()).expect_err(&bad.to_string());
            assert_eq!(e.code, ErrorCode::ValidationFailed, "{bad}");
            assert_eq!(e.http_status(), 400);
            // el detalle técnico no viaja en la serialización
            assert!(!serde_json::to_string(&e).unwrap().contains("tras aplicar"));
        }
    }

    #[test]
    fn p028_pins_y_origenes_del_registro_exigen_https_canonico() {
        let valid = json!({"security": {
            "registry_url": "https://registry.example.test/index.json",
            "publisher_keys": {"release-2026": "ab".repeat(32)},
            "registry_allowed_origins": ["https://cdn.example.test"]
        }});
        let settings = compose_file(valid).unwrap();
        assert_eq!(
            settings.security.registry_allowed_origins,
            ["https://cdn.example.test"]
        );

        for security in [
            json!({"registry_allowed_origins": ["http://cdn.example.test"]}),
            json!({"registry_allowed_origins": ["https://cdn.example.test/assets"]}),
            json!({"registry_allowed_origins": ["https://cdn.example.test/"]}),
            json!({"registry_allowed_origins": ["https://cdn.example.test", "https://cdn.example.test"]}),
            json!({"registry_url": "https://user@registry.example.test/index.json",
                  "publisher_keys": {"release": "ab".repeat(32)}}),
            json!({"registry_url": "https://registry.example.test/index.json?token=x",
                  "publisher_keys": {"release": "ab".repeat(32)}}),
            json!({"registry_url": "https://registry.example.test/index.json"}),
            json!({"publisher_keys": {"release": "AB".repeat(32)}}),
            json!({"publisher_keys": {"bad key": "ab".repeat(32)}}),
        ] {
            assert!(
                compose_file(json!({"security": security})).is_err(),
                "se aceptó configuración P028 inválida: {security}"
            );
        }
    }

    #[test]
    fn logging_options_precedencia_y_rechazo() {
        let o = LoggingOptions::resolve(None, None, None, None).unwrap();
        assert_eq!((o.level.as_str(), o.format.as_str()), ("info", "pretty"));
        let o = LoggingOptions::resolve(Some("debug"), Some("json"), Some("WARN"), None).unwrap();
        assert_eq!((o.level.as_str(), o.format.as_str()), ("warn", "json"));
        assert!(LoggingOptions::resolve(Some("verbose"), None, None, None).is_err());
        assert!(LoggingOptions::resolve(None, None, None, Some("xml")).is_err());
    }
}
