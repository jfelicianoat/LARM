//! Immutable external runtime profile locks (P099).
//!
//! Acquisition locks record a version-specific, source-pinned profile. This
//! layer only accepts UNQUALIFIED records; qualifying a runtime is a separate
//! workflow backed by real probes and smokes.

use crate::error::{ErrorCode, LarmError};
use crate::ports::MutationContext;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LOCK_SCHEMA: &str = "larm.runtime-profile-lock/1";
pub const QUALIFICATION_SCHEMA: &str = "larm.runtime-profile-qualification/1";
pub const REQUIRED_FIELDS: &[&str] = &[
    "upstream_version",
    "commit_or_build",
    "artifact_hash",
    "os_build_range",
    "architecture",
    "install_strategy",
    "dependencies_lock",
    "executable",
    "args_template",
    "env_allowlist",
    "backend_probe",
    "model_layout_schema",
    "protocol_mapping",
    "request_schema",
    "response_schema",
    "error_mapping",
    "fixture_hashes",
    "evidence_refs",
    "installation_strategies",
    "launch_options_schema",
    "runtime_arguments_schema",
    "environment_allowlist",
];

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Lock de perfil runtime no válido",
        reason,
    )
}

fn text(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn known_runtime(runtime: &str) -> Result<bool, LarmError> {
    #[derive(Deserialize)]
    struct Catalog {
        profiles: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        runtime: String,
        profile_required_fields: Vec<String>,
    }
    let catalog: Catalog = serde_json::from_str(include_str!(
        "../../../docs/contracts/runtime-profiles.json"
    ))
    .map_err(|_| invalid("runtime_profile_catalog_invalid"))?;
    if catalog.profiles.len() != 20
        || catalog.profiles.iter().any(|entry| {
            entry.profile_required_fields.len() != REQUIRED_FIELDS.len()
                || entry
                    .profile_required_fields
                    .iter()
                    .any(|field| !REQUIRED_FIELDS.contains(&field.as_str()))
        })
    {
        return Err(invalid("runtime_profile_catalog_invalid"));
    }
    Ok(catalog
        .profiles
        .iter()
        .any(|entry| entry.runtime == runtime))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfileKey {
    pub runtime: String,
    pub source_url: String,
    pub source_revision: String,
    pub source_sha256: String,
    pub upstream_version: String,
    pub os: String,
    pub os_build: String,
    pub backend: String,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfileLock {
    pub schema_version: String,
    pub key: RuntimeProfileKey,
    pub fields: Value,
    pub qualification: String,
    pub lock_sha256: String,
}

impl RuntimeProfileLock {
    /// Validate against the shared 20-runtime catalog and the exact 22 fields.
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.schema_version != LOCK_SCHEMA || self.qualification != "UNQUALIFIED" {
            return Err(invalid("runtime_profile_lock_status_invalid"));
        }
        let key = &self.key;
        if !known_runtime(&key.runtime)? {
            return Err(invalid("runtime_profile_runtime_unknown"));
        }
        for value in [
            key.runtime.as_str(),
            key.source_url.as_str(),
            key.source_revision.as_str(),
            key.upstream_version.as_str(),
            key.os.as_str(),
            key.os_build.as_str(),
            key.backend.as_str(),
            key.protocol.as_str(),
        ] {
            if !text(value) {
                return Err(invalid("runtime_profile_lock_key_invalid"));
            }
        }
        let url = key.source_url.strip_prefix("https://");
        if url.is_none_or(|rest| {
            let host = rest.split(['/', '?']).next().unwrap_or_default();
            host.is_empty() || host.contains('@') || rest.contains('#')
        }) {
            return Err(invalid("runtime_profile_source_url_invalid"));
        }
        if ["main", "master", "head", "latest", "stable"]
            .contains(&key.source_revision.to_ascii_lowercase().as_str())
        {
            return Err(invalid("runtime_profile_source_revision_unpinned"));
        }
        if !sha256(&key.source_sha256) {
            return Err(invalid("runtime_profile_source_hash_invalid"));
        }
        let Some(fields) = self.fields.as_object() else {
            return Err(invalid("runtime_profile_required_fields_mismatch"));
        };
        if fields.len() != REQUIRED_FIELDS.len()
            || REQUIRED_FIELDS
                .iter()
                .any(|field| !fields.contains_key(*field))
        {
            return Err(invalid("runtime_profile_required_fields_mismatch"));
        }
        if fields.get("upstream_version").is_some_and(|value| {
            !value.is_null() && value.as_str() != Some(key.upstream_version.as_str())
        }) {
            return Err(invalid("runtime_profile_upstream_version_mismatch"));
        }
        let strategies = fields
            .get("installation_strategies")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("runtime_profile_installation_strategies_invalid"))?;
        let allowed = [
            "PREBUILT_BINARY",
            "GITHUB_RELEASE",
            "SOURCE_BUILD",
            "WINGET",
            "PIP",
            "UV",
            "CONDA",
            "MAMBA",
            "NUGET",
            "WSL",
            "CONTAINER",
            "CUSTOM",
            "MSI_MANAGED",
            "PYTHON_VENV",
            "EXTERNAL_ADOPTION",
        ];
        let mut seen = std::collections::HashSet::new();
        for strategy in strategies {
            let Some(strategy) = strategy.as_str() else {
                return Err(invalid("runtime_profile_installation_strategies_invalid"));
            };
            if !allowed.contains(&strategy) || !seen.insert(strategy) {
                return Err(invalid("runtime_profile_installation_strategies_invalid"));
            }
        }
        if let Some(strategy) = fields.get("install_strategy") {
            if !strategy.is_null()
                && strategy
                    .as_str()
                    .is_none_or(|value| !allowed.contains(&value))
            {
                return Err(invalid("runtime_profile_install_strategy_invalid"));
            }
        }
        let fixture_hashes = fields
            .get("fixture_hashes")
            .ok_or_else(|| invalid("runtime_profile_fixture_hash_invalid"))?;
        let valid_fixture = |value: &Value| value.as_str().is_some_and(sha256);
        match fixture_hashes {
            Value::Array(values) if !values.is_empty() && values.iter().all(valid_fixture) => {}
            Value::Object(values) if !values.is_empty() && values.values().all(valid_fixture) => {}
            _ => return Err(invalid("runtime_profile_fixture_hash_invalid")),
        }
        let evidence = fields
            .get("evidence_refs")
            .and_then(Value::as_array)
            .filter(|values| {
                !values.is_empty() && values.iter().all(|value| value.as_str().is_some_and(text))
            });
        if evidence.is_none() {
            return Err(invalid("runtime_profile_evidence_refs_missing"));
        }
        if !sha256(&self.lock_sha256) || self.computed_hash()? != self.lock_sha256 {
            return Err(invalid("runtime_profile_lock_hash_mismatch"));
        }
        Ok(())
    }

    pub fn computed_hash(&self) -> Result<String, LarmError> {
        let mut payload = serde_json::to_value(self)
            .map_err(|_| invalid("runtime_profile_canonicalization_failed"))?;
        payload
            .as_object_mut()
            .ok_or_else(|| invalid("runtime_profile_lock_invalid"))?
            .remove("lock_sha256");
        crate::canonical::sha256_hex(&payload)
            .map_err(|_| invalid("runtime_profile_canonicalization_failed"))
    }

    /// Hash identity without the digest field, using the contract's RFC 8785 implementation.
    pub fn identity(&self) -> Result<Value, LarmError> {
        serde_json::to_value(&self.key).map_err(|_| invalid("runtime_profile_lock_invalid"))
    }
}

#[async_trait]
pub trait RuntimeProfileLockRepository: Send + Sync {
    /// Insert an immutable lock. An exact repeat returns `false`; a changed
    /// lock for the same profile key returns CONFLICT.
    async fn acquire(
        &self,
        lock: &RuntimeProfileLock,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError>;

    async fn get_by_hash(&self, lock_sha256: &str)
        -> Result<Option<RuntimeProfileLock>, LarmError>;
}

/// Evidencia inmutable obtenida por el cualificador local. Esta evidencia se
/// almacena separada del lock de adquisición: guardar un candidato nunca lo
/// convierte en compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfileQualification {
    pub schema_version: String,
    pub lock_sha256: String,
    pub host_id: String,
    pub qualified_at: String,
    pub fixture_hashes: std::collections::BTreeMap<String, String>,
    /// Estrategias que pasaron sus pruebas reales; las claves son valores
    /// canónicos de InstallationStrategy y los valores identifican los casos.
    pub strategy_test_ids: std::collections::BTreeMap<String, Vec<String>>,
    pub evidence_sha256: String,
    pub qualification_sha256: String,
}

impl RuntimeProfileQualification {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.schema_version != QUALIFICATION_SCHEMA
            || !sha256(&self.lock_sha256)
            || !sha256(&self.evidence_sha256)
            || !text(&self.host_id)
        {
            return Err(invalid("runtime_profile_qualification_invalid"));
        }
        let qualified_at = chrono::DateTime::parse_from_rfc3339(&self.qualified_at)
            .map_err(|_| invalid("runtime_profile_qualification_timestamp_invalid"))?;
        if crate::bridge::timestamp(qualified_at.with_timezone(&chrono::Utc)) != self.qualified_at {
            return Err(invalid("runtime_profile_qualification_timestamp_invalid"));
        }
        if self.fixture_hashes.is_empty()
            || self
                .fixture_hashes
                .iter()
                .any(|(name, hash)| !text(name) || !sha256(hash))
            || self.strategy_test_ids.is_empty()
            || self.strategy_test_ids.iter().any(|(strategy, tests)| {
                ![
                    "PREBUILT_BINARY",
                    "GITHUB_RELEASE",
                    "SOURCE_BUILD",
                    "WINGET",
                    "PIP",
                    "UV",
                    "CONDA",
                    "MAMBA",
                    "NUGET",
                    "WSL",
                    "CONTAINER",
                    "CUSTOM",
                    "MSI_MANAGED",
                    "PYTHON_VENV",
                    "EXTERNAL_ADOPTION",
                ]
                .contains(&strategy.as_str())
                    || tests.is_empty()
                    || tests.iter().any(|test| !text(test))
                    || tests.iter().collect::<std::collections::HashSet<_>>().len() != tests.len()
            })
        {
            return Err(invalid("runtime_profile_qualification_evidence_invalid"));
        }
        if !sha256(&self.qualification_sha256) || self.computed_hash()? != self.qualification_sha256
        {
            return Err(invalid("runtime_profile_qualification_hash_mismatch"));
        }
        Ok(())
    }

    pub fn validate_for_lock(&self, lock: &RuntimeProfileLock) -> Result<(), LarmError> {
        self.validate()?;
        lock.validate()?;
        if self.lock_sha256 != lock.lock_sha256 {
            return Err(invalid("runtime_profile_qualification_lock_mismatch"));
        }
        let declared_strategies = lock.fields["installation_strategies"]
            .as_array()
            .ok_or_else(|| invalid("runtime_profile_installation_strategies_invalid"))?;
        if self.strategy_test_ids.keys().any(|strategy| {
            !declared_strategies
                .iter()
                .any(|v| v.as_str() == Some(strategy))
        }) {
            return Err(invalid("runtime_profile_qualification_strategy_mismatch"));
        }
        let mut declared_fixtures = match &lock.fields["fixture_hashes"] {
            Value::Array(values) => values.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
            Value::Object(values) => values
                .values()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            _ => return Err(invalid("runtime_profile_fixture_hash_invalid")),
        };
        let mut qualified_fixtures = self
            .fixture_hashes
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>();
        declared_fixtures.sort_unstable();
        qualified_fixtures.sort_unstable();
        if declared_fixtures != qualified_fixtures {
            return Err(invalid("runtime_profile_qualification_fixture_mismatch"));
        }
        Ok(())
    }

    pub fn computed_hash(&self) -> Result<String, LarmError> {
        let mut value = serde_json::to_value(self)
            .map_err(|_| invalid("runtime_profile_qualification_invalid"))?;
        value
            .as_object_mut()
            .ok_or_else(|| invalid("runtime_profile_qualification_invalid"))?
            .remove("qualification_sha256");
        crate::canonical::sha256_hex(&value)
            .map_err(|_| invalid("runtime_profile_qualification_invalid"))
    }
}

#[async_trait]
pub trait RuntimeProfileQualificationRepository: Send + Sync {
    /// Sólo el flujo de cualificación local escribe evidencias. El lock siempre
    /// se conserva como UNQUALIFIED y se consulta por separado en la admisión.
    async fn record(
        &self,
        lock: &RuntimeProfileLock,
        qualification: &RuntimeProfileQualification,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError>;

    async fn get_by_lock_hash(
        &self,
        lock_sha256: &str,
    ) -> Result<Option<RuntimeProfileQualification>, LarmError>;
}
