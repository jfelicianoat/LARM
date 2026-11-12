//! Descriptores privados del paquete; no son otro DTO de API ni una cualificación.
//! Mapping y estimador deben estar implementados por el handler antes de anunciar.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jsonschema::Validator;
use larm_domain::canonical::sha256_hex;
use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contracts::Contracts;
mod builtin;

pub type Estimator = Arc<dyn Fn(&Value) -> Result<Value, LarmError> + Send + Sync>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub input: Value,
    pub output: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub id: String,
    pub version: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub input_sha256: String,
    pub output_sha256: String,
    pub mapping: String,
    pub estimator_id: String,
    pub fixture: Fixture,
    pub fixture_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_version: String,
    entries: Vec<Descriptor>,
}
struct Entry {
    descriptor: Descriptor,
    input: Validator,
    output: Validator,
}
pub struct CapabilityRegistry {
    entries: BTreeMap<String, Entry>,
    estimators: BTreeMap<String, Estimator>,
    aliases: BTreeMap<String, String>,
    pub profile_hash: String,
}
fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "Contrato de capacidad no disponible o inválido",
        reason,
    )
}
pub fn digest(value: &Value) -> Result<String, LarmError> {
    sha256_hex(value)
        .map_err(|_| error(ErrorCode::ValidationFailed, "capability_descriptor_invalid"))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    matches!(parts.len(), 2 | 3)
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
impl CapabilityRegistry {
    pub fn new(
        value: Value,
        estimators: BTreeMap<String, Estimator>,
        mappings: &BTreeSet<String>,
    ) -> Result<Self, LarmError> {
        let bundle: Bundle = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ValidationFailed, "capability_descriptor_invalid"))?;
        if bundle.schema_version != "1.0" || bundle.entries.is_empty() || bundle.entries.len() > 128
        {
            return Err(error(
                ErrorCode::ValidationFailed,
                "capability_descriptor_invalid",
            ));
        }
        let table: Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/capability-contracts.json"
        ))
        .map_err(|_| error(ErrorCode::ProtocolError, "capability_descriptor_invalid"))?;
        let aliases: BTreeMap<String, String> =
            serde_json::from_value(table["aliases_read_only"].clone())
                .map_err(|_| error(ErrorCode::ProtocolError, "capability_descriptor_invalid"))?;
        let mut entries = BTreeMap::new();
        for descriptor in bundle.entries {
            let identity = larm_domain::model::capability(&descriptor.id)?;
            if identity.id != descriptor.id
                || (!identity.known && !descriptor.id.contains('.'))
                || !version(&descriptor.version)
                || !hash(&descriptor.input_sha256)
                || !hash(&descriptor.output_sha256)
                || !hash(&descriptor.fixture_sha256)
                || descriptor.mapping.is_empty()
                || descriptor.estimator_id.is_empty()
                || !descriptor.fixture.input.is_object()
                || !descriptor.fixture.output.is_object()
            {
                return Err(error(
                    ErrorCode::ValidationFailed,
                    "capability_descriptor_invalid",
                ));
            }
            if entries.contains_key(&descriptor.id) {
                return Err(error(
                    ErrorCode::Conflict,
                    "capability_descriptor_duplicate",
                ));
            }
            if let Some((_, v, input, output)) = builtin::BUILTIN
                .iter()
                .find(|(id, _, _, _)| *id == descriptor.id)
            {
                if descriptor.version != *v
                    || descriptor.input_sha256 != *input
                    || descriptor.output_sha256 != *output
                {
                    return Err(error(
                        ErrorCode::UnsupportedCapability,
                        "capability_builtin_schema_mismatch",
                    ));
                }
            }
            if !mappings.contains(&descriptor.mapping) {
                return Err(error(
                    ErrorCode::UnsupportedCapability,
                    "capability_mapping_missing",
                ));
            }
            if !estimators.contains_key(&descriptor.estimator_id) {
                return Err(error(
                    ErrorCode::UnsupportedCapability,
                    "capability_estimator_missing",
                ));
            }
            let compile = |schema: &Value, expected: &str| -> Result<Validator, LarmError> {
                if !schema.is_object() {
                    return Err(error(
                        ErrorCode::ValidationFailed,
                        "capability_descriptor_invalid",
                    ));
                }
                let raw = larm_domain::canonical::canonicalize(schema).map_err(|_| {
                    error(ErrorCode::ValidationFailed, "capability_descriptor_invalid")
                })?;
                if raw.len() > 1 << 20 || digest(schema)? != expected {
                    return Err(error(
                        ErrorCode::IntegrityFailed,
                        "capability_schema_hash_mismatch",
                    ));
                }
                Contracts::compile(&raw).map_err(|_| {
                    error(ErrorCode::ValidationFailed, "capability_descriptor_invalid")
                })
            };
            let input = compile(&descriptor.input_schema, &descriptor.input_sha256)?;
            let output = compile(&descriptor.output_schema, &descriptor.output_sha256)?;
            if !input.is_valid(&descriptor.fixture.input)
                || !output.is_valid(&descriptor.fixture.output)
            {
                return Err(error(
                    ErrorCode::ValidationFailed,
                    "capability_fixture_invalid",
                ));
            }
            let fixture = serde_json::to_value(&descriptor.fixture)
                .map_err(|_| error(ErrorCode::ValidationFailed, "capability_descriptor_invalid"))?;
            if digest(&fixture)? != descriptor.fixture_sha256 {
                return Err(error(
                    ErrorCode::IntegrityFailed,
                    "capability_fixture_hash_mismatch",
                ));
            }
            entries.insert(
                descriptor.id.clone(),
                Entry {
                    descriptor,
                    input,
                    output,
                },
            );
        }
        let all: Vec<_> = entries.values().map(|entry| &entry.descriptor).collect();
        let profile_hash = digest(&serde_json::json!({"schema_version":"1.0","entries":all}))?;
        Ok(Self {
            entries,
            estimators,
            aliases,
            profile_hash,
        })
    }
    fn resolve(&self, identity: &str) -> Result<&Entry, LarmError> {
        let canonical = self.aliases.get(identity).map_or(identity, String::as_str);
        self.entries.get(canonical).ok_or_else(|| {
            error(
                ErrorCode::UnsupportedCapability,
                "capability_descriptor_missing",
            )
        })
    }
    pub fn descriptor(&self, identity: &str) -> Result<&Descriptor, LarmError> {
        Ok(&self.resolve(identity)?.descriptor)
    }
    pub fn check_advertised(&self, capabilities: &[String]) -> Result<(), LarmError> {
        let names: BTreeSet<_> = capabilities.iter().collect();
        if names.len() != capabilities.len()
            || capabilities.iter().any(|id| !self.entries.contains_key(id))
        {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                "capability_descriptor_missing",
            ));
        }
        for id in capabilities {
            self.estimate(id, &self.resolve(id)?.descriptor.fixture.input)?;
        }
        Ok(())
    }
    pub fn input(&self, identity: &str, value: &Value) -> Result<(), LarmError> {
        if self.resolve(identity)?.input.is_valid(value) {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                "capability_input_invalid",
            ))
        }
    }
    pub fn output(&self, identity: &str, value: &Value) -> Result<(), LarmError> {
        if self.resolve(identity)?.output.is_valid(value) {
            Ok(())
        } else {
            Err(error(ErrorCode::ProtocolError, "capability_output_invalid"))
        }
    }
    pub fn estimate(&self, identity: &str, value: &Value) -> Result<Value, LarmError> {
        self.input(identity, value)?;
        let entry = self.resolve(identity)?;
        let estimate = self.estimators[&entry.descriptor.estimator_id](value)?;
        Contracts::shared()?
            .response("runtime.estimate_memory", &estimate)
            .map_err(|_| error(ErrorCode::ProtocolError, "capability_estimator_failed"))?;
        Ok(estimate)
    }
    pub fn schema_id(&self, identity: &str, output: bool) -> Result<String, LarmError> {
        let entry = &self.resolve(identity)?.descriptor;
        Ok(format!(
            "urn:larm:capability:{}:{}:{}:sha256:{}",
            entry.id,
            entry.version,
            if output { "output" } else { "input" },
            if output {
                &entry.output_sha256
            } else {
                &entry.input_sha256
            }
        ))
    }
}
