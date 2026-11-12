//! P030: inventario de runtimes. Registrar no instala, valida ni concede ejecución.
//! Los DTO mantienen las cinco dimensiones del contrato; los providers son abiertos.

use crate::bridge::{is_uuid_v7, timestamp};
use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

macro_rules! contract_enum {
    ($name:ident { $($(#[$attr:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name { $($(#[$attr])* #[serde(rename = $wire)] $variant),+ }
        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
    };
}

contract_enum!(RuntimeChannel {
    Stable => "stable", Prerelease => "prerelease", Nightly => "nightly",
    Commit => "commit", Custom => "custom",
});
contract_enum!(RuntimePromotion {
    None => "NONE", Candidate => "CANDIDATE", Default => "DEFAULT",
    Pinned => "PINNED", Rejected => "REJECTED",
});
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum RuntimeVersionLifecycle {
    #[default]
    #[serde(rename = "CANDIDATE")]
    Candidate,
    #[serde(rename = "INSTALLING")]
    Installing,
    #[serde(rename = "VALIDATING")]
    Validating,
    #[serde(rename = "VALIDATED")]
    Validated,
    #[serde(rename = "DEFAULT")]
    Default,
    #[serde(rename = "RETAINED")]
    Retained,
    #[serde(rename = "BROKEN")]
    Broken,
    #[serde(rename = "REJECTED")]
    Rejected,
}
impl RuntimeVersionLifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "CANDIDATE",
            Self::Installing => "INSTALLING",
            Self::Validating => "VALIDATING",
            Self::Validated => "VALIDATED",
            Self::Default => "DEFAULT",
            Self::Retained => "RETAINED",
            Self::Broken => "BROKEN",
            Self::Rejected => "REJECTED",
        }
    }
}
contract_enum!(RegistrationMode {
    Managed => "MANAGED", Adopted => "ADOPTED", External => "EXTERNAL",
});
contract_enum!(RuntimeOwnership { LarmManaged => "LARM_MANAGED", External => "EXTERNAL" });
contract_enum!(ExternalOwnership { External => "EXTERNAL" });
contract_enum!(ExternalHostState { Online => "ONLINE", Offline => "OFFLINE", Unknown => "UNKNOWN" });
contract_enum!(BackendStatus { Available => "AVAILABLE", Unavailable => "UNAVAILABLE", Unknown => "UNKNOWN" });
contract_enum!(InstallationStrategy {
    PrebuiltBinary => "PREBUILT_BINARY", GithubRelease => "GITHUB_RELEASE",
    SourceBuild => "SOURCE_BUILD", Winget => "WINGET", Pip => "PIP", Uv => "UV",
    Conda => "CONDA", Mamba => "MAMBA", Nuget => "NUGET", Wsl => "WSL",
    Container => "CONTAINER", Custom => "CUSTOM", MsiManaged => "MSI_MANAGED",
    PythonVenv => "PYTHON_VENV", ExternalAdoption => "EXTERNAL_ADOPTION",
});
impl InstallationStrategy {
    #[must_use]
    pub const fn is_extension(self) -> bool {
        matches!(
            self,
            Self::MsiManaged | Self::PythonVenv | Self::ExternalAdoption
        )
    }
}
contract_enum!(InstallationStage {
    Discovered => "DISCOVERED", #[serde(alias = "PLANNED")] InstallPlanned => "INSTALL_PLANNED",
    Installing => "INSTALLING", Installed => "INSTALLED", Validating => "VALIDATING",
    Validated => "VALIDATED", Failed => "FAILED", Retired => "RETIRED",
});
contract_enum!(InstallationState {
    Discovered => "DISCOVERED", #[serde(alias = "PLANNED")] InstallPlanned => "INSTALL_PLANNED",
    Installing => "INSTALLING", Installed => "INSTALLED", Validating => "VALIDATING",
    Validated => "VALIDATED", Active => "ACTIVE", Degraded => "DEGRADED",
    Broken => "BROKEN", RolledBack => "ROLLED_BACK", Failed => "FAILED", Retired => "RETIRED",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChannelPolicy {
    pub allowed_channels: Vec<RuntimeChannel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRetentionPolicy {
    pub keep_last_validated: u32,
}

impl Default for RuntimeRetentionPolicy {
    fn default() -> Self {
        Self {
            keep_last_validated: 3,
        }
    }
}

impl RuntimeRetentionPolicy {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !(2..=50).contains(&self.keep_last_validated) {
            return Err(invalid("runtime_retention_policy_invalid"));
        }
        Ok(())
    }
}

impl Default for RuntimeChannelPolicy {
    fn default() -> Self {
        Self {
            allowed_channels: vec![RuntimeChannel::Stable],
        }
    }
}

impl RuntimeChannelPolicy {
    pub fn validate(&self) -> Result<(), LarmError> {
        let unique: std::collections::BTreeSet<_> = self.allowed_channels.iter().collect();
        if unique.is_empty() || unique.len() != self.allowed_channels.len() {
            return Err(invalid("channel_policy_invalid"));
        }
        Ok(())
    }

    /// Filtro de selección automática; nunca filtra el inventario instalado.
    #[must_use]
    pub fn allows(&self, channel: RuntimeChannel) -> bool {
        self.allowed_channels.contains(&channel)
    }
}

/// Observación de upstream, separada de los enums estrictos de la API/DB.
/// Un nombre desconocido conserva el original y no obtiene promoción automática.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedChannel {
    pub channel: RuntimeChannel,
    pub upstream_value: String,
    pub reason: Option<&'static str>,
}

impl ObservedChannel {
    #[must_use]
    pub fn from_upstream(value: &str) -> Self {
        let channel = match value.to_ascii_lowercase().as_str() {
            "stable" => Some(RuntimeChannel::Stable),
            "prerelease" => Some(RuntimeChannel::Prerelease),
            "nightly" => Some(RuntimeChannel::Nightly),
            "commit" => Some(RuntimeChannel::Commit),
            "custom" => Some(RuntimeChannel::Custom),
            _ => None,
        };
        Self {
            channel: channel.unwrap_or(RuntimeChannel::Custom),
            upstream_value: value.to_owned(),
            reason: channel.is_none().then_some("UNKNOWN"),
        }
    }

    #[must_use]
    pub fn automatic_selection_allowed(&self, policy: &RuntimeChannelPolicy) -> bool {
        self.reason.is_none() && policy.allows(self.channel)
    }

    /// Dato descriptivo exportable; no usar una clave libre como permiso.
    #[must_use]
    pub fn metadata(&self) -> Value {
        serde_json::json!({"value":self.upstream_value,"reason":self.reason})
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCreate {
    pub name: String,
    pub adapter_id: String,
    pub capabilities: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub default_version_id: Option<String>,
    pub channel_policy: RuntimeChannelPolicy,
    #[serde(default)]
    pub retention_policy: RuntimeRetentionPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: String,
    /// FK a runtime_adapter, cuyo binding apunta a adapter_installation.
    pub adapter_id: String,
    pub capabilities: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub default_version_id: Option<String>,
    pub channel_policy: RuntimeChannelPolicy,
    pub retention_policy: RuntimeRetentionPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAdapter {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub adapter_installation_id: String,
    pub runtime_family: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeVersion {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub runtime_id: String,
    /// Identidad upstream, sin interpretación SemVer obligatoria.
    pub version_string: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub commit: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub build: Option<String>,
    pub channel: RuntimeChannel,
    #[serde(deserialize_with = "Option::deserialize")]
    pub released_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub artifact_hash: Option<String>,
    pub promotion: RuntimePromotion,
    /// Campo añadido en el contrato 3.11. Los payloads de adaptadores previos
    /// no lo conocen y se normalizan a CANDIDATE en el host.
    #[serde(default)]
    pub lifecycle: RuntimeVersionLifecycle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeVersionPin {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub launch_profile_id: String,
    pub runtime_version_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeUpdateRequest {
    pub storage_location_id: String,
    pub strategy: InstallationStrategy,
    pub profile_lock_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub external_host_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRollbackRequest {
    #[serde(deserialize_with = "Option::deserialize")]
    pub expected_default_version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePinRequest {
    pub variant_id: String,
    pub launch_profile_id: String,
}

/// Recibo contractual de una mutación síncrona del catálogo runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMutationReceipt {
    pub resource_id: String,
    pub row_version: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstallation {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub runtime_version_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub storage_location_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub relative_path: Option<String>,
    pub strategy: InstallationStrategy,
    #[serde(deserialize_with = "Option::deserialize")]
    pub external_host_id: Option<String>,
    pub state: InstallationState,
    pub ownership: RuntimeOwnership,
    #[serde(deserialize_with = "Option::deserialize")]
    pub lock_hash: Option<String>,
    pub stage: InstallationStage,
    pub registration_mode: RegistrationMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendLimits {
    pub max_instances: u64,
    pub multi_model_support: bool,
    pub parallel_requests: u64,
    pub gpu_sharing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBackend {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub runtime_installation_id: String,
    /// Provider/capability abierto: un valor desconocido no se cambia a CPU.
    pub backend_type: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub device_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub driver_version: Option<String>,
    pub status: BackendStatus,
    pub evidence_refs: Vec<String>,
    pub limits: BackendLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHost {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: String,
    pub base_url: String,
    pub host_type: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub state: ExternalHostState,
    pub ownership: ExternalOwnership,
    #[serde(deserialize_with = "Option::deserialize")]
    pub observed_version: Option<String>,
    pub registration_mode: RegistrationMode,
}

/// Contractual request for registering a reachable or adopted external host.
/// Registration records UNKNOWN connectivity; it never probes or starts a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHostCreate {
    pub name: String,
    pub base_url: String,
    pub host_type: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub registration_mode: RegistrationMode,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Inventario de runtime no válido",
        reason,
    )
}

fn nonempty(value: &str, field: &str) -> Result<(), LarmError> {
    if value.is_empty() {
        Err(invalid(field))
    } else {
        Ok(())
    }
}
fn id(value: &str, field: &str) -> Result<(), LarmError> {
    if is_uuid_v7(value) {
        Ok(())
    } else {
        Err(invalid(field))
    }
}
fn optional_id(value: &Option<String>, field: &str) -> Result<(), LarmError> {
    value.as_deref().map(|s| id(s, field)).unwrap_or(Ok(()))
}
fn optional_text(value: &Option<String>, field: &str) -> Result<(), LarmError> {
    value
        .as_deref()
        .map(|s| nonempty(s, field))
        .unwrap_or(Ok(()))
}
fn optional_hash(value: &Option<String>, field: &str) -> Result<(), LarmError> {
    if value.as_ref().is_some_and(|s| {
        s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(invalid(field));
    }
    Ok(())
}
fn utc(value: &str) -> Result<(), LarmError> {
    let at =
        chrono::DateTime::parse_from_rfc3339(value).map_err(|_| invalid("timestamp_invalid"))?;
    if timestamp(at.with_timezone(&chrono::Utc)) != value {
        return Err(invalid("timestamp_invalid"));
    }
    Ok(())
}
fn record(identity: &str, created: &str, updated: &str, revision: i64) -> Result<(), LarmError> {
    id(identity, "id_invalid")?;
    utc(created)?;
    utc(updated)?;
    if revision < 1 {
        return Err(invalid("row_version_invalid"));
    }
    Ok(())
}

impl RuntimeCreate {
    pub fn validate(&self) -> Result<(), LarmError> {
        nonempty(&self.name, "name_invalid")?;
        id(&self.adapter_id, "runtime_adapter_id_invalid")?;
        optional_id(&self.default_version_id, "default_version_id_invalid")?;
        self.channel_policy.validate()?;
        self.retention_policy.validate()
    }
}
impl Runtime {
    pub fn configuration(&self) -> RuntimeCreate {
        RuntimeCreate {
            name: self.name.clone(),
            adapter_id: self.adapter_id.clone(),
            capabilities: self.capabilities.clone(),
            default_version_id: self.default_version_id.clone(),
            channel_policy: self.channel_policy.clone(),
            retention_policy: self.retention_policy,
        }
    }
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        self.configuration().validate()
    }
}
impl RuntimeAdapter {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        id(
            &self.adapter_installation_id,
            "adapter_installation_id_invalid",
        )?;
        nonempty(&self.runtime_family, "runtime_family_invalid")
    }
}
impl RuntimeVersion {
    /// Clasifica el canal que observó un adaptador sin convertir metadata en
    /// autoridad. El marcador legado `channel_reason` se conserva como UNKNOWN
    /// aunque el adaptador no conozca el valor original.
    pub fn observed_channel(&self) -> Result<ObservedChannel, LarmError> {
        if let Some(value) = self.metadata.get("upstream_channel") {
            let object = value.as_object().ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ProtocolError,
                    "El adaptador devolvió una clasificación de canal inválida",
                    "runtime_upstream_channel_invalid",
                )
            })?;
            let upstream_value = object.get("value").and_then(Value::as_str).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ProtocolError,
                    "El adaptador devolvió una clasificación de canal inválida",
                    "runtime_upstream_channel_invalid",
                )
            })?;
            let reason = object.get("reason").and_then(Value::as_str);
            let observed = ObservedChannel::from_upstream(upstream_value);
            if reason != Some("UNKNOWN")
                || observed.reason != Some("UNKNOWN")
                || self.channel != RuntimeChannel::Custom
            {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "La clasificación de canal contradice la versión observada",
                    "runtime_upstream_channel_mismatch",
                ));
            }
            return Ok(observed);
        }

        if self.metadata.get("channel_reason").and_then(Value::as_str) == Some("UNKNOWN") {
            if self.channel != RuntimeChannel::Custom {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "La clasificación de canal contradice la versión observada",
                    "runtime_upstream_channel_mismatch",
                ));
            }
            return Ok(ObservedChannel {
                channel: RuntimeChannel::Custom,
                upstream_value: String::new(),
                reason: Some("UNKNOWN"),
            });
        }

        Ok(ObservedChannel::from_upstream(self.channel.as_str()))
    }

    /// Emite la forma canónica que consume LARM cuando un adaptador antiguo
    /// sólo proporciona `channel_reason`; no inventa el valor upstream.
    pub fn normalize_observed_channel_metadata(&mut self) -> Result<(), LarmError> {
        let observed = self.observed_channel()?;
        if observed.reason.is_some() {
            self.metadata
                .entry("upstream_channel")
                .or_insert_with(|| observed.metadata());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        id(&self.runtime_id, "runtime_id_invalid")?;
        nonempty(&self.version_string, "version_string_invalid")?;
        optional_text(&self.commit, "commit_invalid")?;
        optional_text(&self.build, "build_invalid")?;
        optional_hash(&self.artifact_hash, "artifact_hash_invalid")?;
        if let Some(at) = &self.released_at {
            utc(at)?;
        }
        self.observed_channel()?;
        Ok(())
    }
}
impl RuntimeVersionPin {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        id(&self.variant_id, "runtime_pin_variant_id_invalid")?;
        id(
            &self.launch_profile_id,
            "runtime_pin_launch_profile_id_invalid",
        )?;
        id(&self.runtime_version_id, "runtime_pin_version_id_invalid")
    }
}
impl RuntimeUpdateRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.storage_location_id, "storage_location_id_invalid")?;
        optional_id(&self.external_host_id, "external_host_id_invalid")?;
        if self.profile_lock_hash.len() != 64
            || !self
                .profile_lock_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("profile_lock_hash_invalid"));
        }
        Ok(())
    }
}
impl RuntimeRollbackRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        optional_id(
            &self.expected_default_version_id,
            "expected_default_version_id_invalid",
        )
    }
}
impl RuntimePinRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        id(&self.variant_id, "runtime_pin_variant_id_invalid")?;
        id(
            &self.launch_profile_id,
            "runtime_pin_launch_profile_id_invalid",
        )
    }
}
impl RuntimeInstallation {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        id(&self.runtime_version_id, "runtime_version_id_invalid")?;
        optional_id(&self.storage_location_id, "storage_location_id_invalid")?;
        optional_id(&self.external_host_id, "external_host_id_invalid")?;
        optional_hash(&self.lock_hash, "lock_hash_invalid")?;
        if let Some(path) = &self.relative_path {
            crate::artifacts::normalize_relative(path)?;
        }
        let correct = if self.stage == InstallationStage::Validated {
            matches!(
                self.state,
                InstallationState::Validated
                    | InstallationState::Active
                    | InstallationState::Degraded
                    | InstallationState::Broken
                    | InstallationState::RolledBack
            )
        } else {
            self.state == InstallationState::for_stage(self.stage)
        };
        if !correct {
            return Err(invalid("installation_state_stage_mismatch"));
        }
        Ok(())
    }
}
impl RuntimeBackend {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        id(
            &self.runtime_installation_id,
            "runtime_installation_id_invalid",
        )?;
        nonempty(&self.backend_type, "backend_type_invalid")?;
        optional_text(&self.device_id, "device_id_invalid")?;
        optional_text(&self.driver_version, "driver_version_invalid")?;
        if self.limits.max_instances == 0 || self.limits.parallel_requests == 0 {
            return Err(invalid("backend_limits_invalid"));
        }
        Ok(())
    }
}
impl ExternalHost {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(
            &self.id,
            &self.created_at,
            &self.updated_at,
            self.row_version,
        )?;
        nonempty(&self.name, "name_invalid")?;
        nonempty(&self.base_url, "base_url_invalid")?;
        nonempty(&self.host_type, "host_type_invalid")?;
        optional_id(
            &self.credential_reference_id,
            "credential_reference_id_invalid",
        )?;
        optional_text(&self.observed_version, "observed_version_invalid")
    }
}

impl ExternalHostCreate {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.name.trim().is_empty() {
            return Err(invalid("name_invalid"));
        }
        if self.host_type.trim().is_empty() {
            return Err(invalid("host_type_invalid"));
        }
        let parsed = url::Url::parse(&self.base_url)
            .map_err(|_| invalid("external_host_base_url_invalid"))?;
        if self.base_url.trim() != self.base_url
            || !matches!(parsed.scheme(), "http" | "https")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(invalid("external_host_base_url_invalid"));
        }
        optional_id(
            &self.credential_reference_id,
            "credential_reference_id_invalid",
        )?;
        if self.registration_mode == RegistrationMode::Managed {
            return Err(invalid("external_host_registration_mode_invalid"));
        }
        Ok(())
    }

    /// Builds a record without claiming connectivity or remote version.
    pub fn to_record(&self) -> Result<ExternalHost, LarmError> {
        self.validate()?;
        let now = timestamp(chrono::Utc::now());
        let host = ExternalHost {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: Map::new(),
            name: self.name.trim().to_owned(),
            base_url: self.base_url.clone(),
            host_type: self.host_type.trim().to_owned(),
            credential_reference_id: self.credential_reference_id.clone(),
            state: ExternalHostState::Unknown,
            ownership: ExternalOwnership::External,
            observed_version: None,
            registration_mode: self.registration_mode,
        };
        host.validate()?;
        Ok(host)
    }
}

impl InstallationStage {
    /// El caller debe obtener las referencias activas bajo el lock de escritura.
    pub fn transition(self, next: Self, active_references: bool) -> Result<(), LarmError> {
        let progress = matches!(
            (self, next),
            (Self::Discovered, Self::InstallPlanned)
                | (Self::InstallPlanned, Self::Installing)
                | (Self::Installing, Self::Installed)
                | (Self::Installed, Self::Validating)
                | (Self::Validating, Self::Validated)
        );
        let failure = next == Self::Failed
            && matches!(
                self,
                Self::Discovered
                    | Self::InstallPlanned
                    | Self::Installing
                    | Self::Installed
                    | Self::Validating
            );
        let retire = self == Self::Validated && next == Self::Retired;
        if retire && active_references {
            return Err(LarmError::new(
                ErrorCode::ActiveLease,
                "La instalación tiene referencias activas",
                "runtime_in_use",
            ));
        }
        if progress || failure || retire {
            return Ok(());
        }
        Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "Transición de instalación no permitida",
            "runtime_transition_invalid",
        ))
    }
}

/// Hechos de instancias obtenidos del repositorio; no son datos libres de metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallationActivity {
    pub failed_instances: u64,
    pub operable_instances: u64,
    pub active_instances: u64,
    pub rolled_back: bool,
}

impl InstallationState {
    #[must_use]
    pub const fn for_stage(stage: InstallationStage) -> Self {
        match stage {
            InstallationStage::Discovered => Self::Discovered,
            InstallationStage::InstallPlanned => Self::InstallPlanned,
            InstallationStage::Installing => Self::Installing,
            InstallationStage::Installed => Self::Installed,
            InstallationStage::Validating => Self::Validating,
            InstallationStage::Validated => Self::Validated,
            InstallationStage::Failed => Self::Failed,
            InstallationStage::Retired => Self::Retired,
        }
    }

    pub fn observed(
        stage: InstallationStage,
        activity: InstallationActivity,
    ) -> Result<Self, LarmError> {
        if activity.active_instances > activity.operable_instances {
            return Err(invalid("installation_activity_invalid"));
        }
        if stage != InstallationStage::Validated {
            return Ok(Self::for_stage(stage));
        }
        Ok(
            if activity.failed_instances > 0 && activity.operable_instances == 0 {
                Self::Broken
            } else if activity.failed_instances > 0 {
                Self::Degraded
            } else if activity.rolled_back {
                Self::RolledBack
            } else if activity.active_instances > 0 {
                Self::Active
            } else {
                Self::Validated
            },
        )
    }
}

#[cfg(test)]
mod external_host_create_tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn request() -> ExternalHostCreate {
        ExternalHostCreate {
            name: "  Host remoto  ".into(),
            base_url: "https://inference.example.test/v1".into(),
            host_type: "vendor.custom/v1".into(),
            credential_reference_id: Some("0192aaaa-0000-7000-8000-000000000002".into()),
            registration_mode: RegistrationMode::External,
        }
    }

    #[test]
    fn registration_stores_reference_and_keeps_connectivity_unknown() {
        let host = request().to_record().unwrap();
        assert_eq!(host.state, ExternalHostState::Unknown);
        assert_eq!(host.ownership, ExternalOwnership::External);
        assert_eq!(host.observed_version, None);
        assert_eq!(host.registration_mode, RegistrationMode::External);
        assert_eq!(
            host.credential_reference_id.as_deref(),
            Some("0192aaaa-0000-7000-8000-000000000002")
        );
        assert_eq!(host.name, "Host remoto");
        assert!(host.validate().is_ok());
    }

    #[test]
    fn rejects_secret_bearing_or_non_http_urls_and_invalid_registration_values() {
        for base_url in [
            "https://user:secret@inference.example.test/v1",
            "https://inference.example.test/v1?token=secret",
            "https://inference.example.test/v1#fragment",
            "file:///C:/runtime",
            "not a url",
        ] {
            let mut candidate = request();
            candidate.base_url = base_url.into();
            assert_eq!(
                candidate.validate().unwrap_err().details.reason,
                "external_host_base_url_invalid"
            );
        }
        let mut candidate = request();
        candidate.credential_reference_id = Some("not-a-uuid".into());
        assert_eq!(
            candidate.validate().unwrap_err().details.reason,
            "credential_reference_id_invalid"
        );
        let mut candidate = request();
        candidate.registration_mode = RegistrationMode::Managed;
        assert_eq!(
            candidate.validate().unwrap_err().details.reason,
            "external_host_registration_mode_invalid"
        );
    }

    #[test]
    fn request_is_closed_to_uncontracted_secret_fields() {
        let mut body = json!({
            "name":"Remote",
            "base_url":"https://inference.example.test/v1",
            "host_type":"vendor.custom/v1",
            "credential_reference_id":null,
            "registration_mode":"ADOPTED"
        });
        body["api_key"] = json!("must-not-be-stored");
        assert!(serde_json::from_value::<ExternalHostCreate>(body).is_err());
    }
}
