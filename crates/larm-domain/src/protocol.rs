//! Protocol profiles and instance inference endpoints (P036).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
};

/// Protocol contract published by a runtime or adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolProfile {
    pub protocol: String,
    pub profile_hash: String,
    pub capabilities: Vec<String>,
    pub streaming: bool,
    pub tools: bool,
    pub request_schema_id: String,
    pub response_schema_id: String,
}

/// State of protocol support in the local registry. Unknown identifiers retain
/// their original spelling and are never promoted by a URL or health response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolResolution<'a> {
    Known(&'a ProtocolProfile),
    UnknownProtocol(&'a str),
    UnknownProfile {
        protocol: &'a str,
        profile_hash: &'a str,
    },
}

/// Registry keyed by the exact protocol identifier and profile hash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolRegistry {
    profiles: BTreeMap<(String, String), ProtocolProfile>,
}

impl ProtocolRegistry {
    /// Adds a validated profile. A protocol can publish multiple versions.
    pub fn register(&mut self, profile: ProtocolProfile) -> Result<(), LarmError> {
        validate_protocol_profile(&profile)?;
        let key = (profile.protocol.clone(), profile.profile_hash.clone());
        if let Some(existing) = self.profiles.get(&key) {
            if existing == &profile {
                return Ok(());
            }
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El perfil de protocolo ya existe con otra definición",
                "protocol_profile_conflict",
            ));
        }
        self.profiles.insert(key, profile);
        Ok(())
    }

    #[must_use]
    pub fn resolve<'a>(
        &'a self,
        protocol: &'a str,
        profile_hash: &'a str,
    ) -> ProtocolResolution<'a> {
        if let Some(profile) = self
            .profiles
            .get(&(protocol.to_owned(), profile_hash.to_owned()))
        {
            return ProtocolResolution::Known(profile);
        }
        if self
            .profiles
            .keys()
            .any(|(registered, _)| registered == protocol)
        {
            ProtocolResolution::UnknownProfile {
                protocol,
                profile_hash,
            }
        } else {
            ProtocolResolution::UnknownProtocol(protocol)
        }
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ProtocolProfile> {
        self.profiles.values()
    }
}

/// Qualification retained for each observed endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndpointQualification {
    #[serde(rename = "UNTESTED")]
    Untested,
    #[serde(rename = "QUALIFIED")]
    Qualified,
    #[serde(rename = "FAILED")]
    Failed,
}

/// Strict create representation from `larm.schema.json#/$defs/inference_endpointCreate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceEndpointCreate {
    pub instance_id: String,
    pub protocol: String,
    pub protocol_profile_hash: String,
    pub base_url: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub capabilities: Vec<String>,
    pub qualification: EndpointQualification,
    pub evidence_refs: Vec<String>,
}

impl InferenceEndpointCreate {
    /// Validates identity and digest shape while allowing unknown protocol IDs.
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.instance_id)
            || !valid_protocol_id(&self.protocol)
            || !valid_sha256(&self.protocol_profile_hash)
            || self.base_url.trim().is_empty()
            || self
                .credential_reference_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
            || self.capabilities.iter().any(|capability| {
                capability.trim().is_empty() || capability.chars().any(char::is_control)
            })
            || self.evidence_refs.iter().any(|reference| {
                reference.trim().is_empty() || reference.chars().any(char::is_control)
            })
        {
            return Err(invalid("inference_endpoint_create_invalid"));
        }
        Ok(())
    }
}

/// Durable endpoint record from `domain.schema.json#/$defs/inference_endpoint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceEndpoint {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub instance_id: String,
    pub protocol: String,
    pub protocol_profile_hash: String,
    pub base_url: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub capabilities: Vec<String>,
    pub qualification: EndpointQualification,
    pub evidence_refs: Vec<String>,
}

impl InferenceEndpoint {
    pub fn validate_record(&self) -> Result<(), LarmError> {
        let create = InferenceEndpointCreate {
            instance_id: self.instance_id.clone(),
            protocol: self.protocol.clone(),
            protocol_profile_hash: self.protocol_profile_hash.clone(),
            base_url: self.base_url.clone(),
            credential_reference_id: self.credential_reference_id.clone(),
            capabilities: self.capabilities.clone(),
            qualification: self.qualification,
            evidence_refs: self.evidence_refs.clone(),
        };
        create.validate()?;
        let valid_time = |value: &str| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .is_some_and(|parsed| timestamp(parsed.with_timezone(&chrono::Utc)) == value)
        };
        if !is_uuid_v7(&self.id)
            || !valid_time(&self.created_at)
            || !valid_time(&self.updated_at)
            || self.updated_at < self.created_at
            || self.row_version < 1
            || (self.qualification != EndpointQualification::Untested
                && self.evidence_refs.is_empty())
        {
            return Err(invalid("inference_endpoint_record_invalid"));
        }
        Ok(())
    }
}

pub fn validate_protocol_profile(profile: &ProtocolProfile) -> Result<(), LarmError> {
    if !valid_protocol_id(&profile.protocol)
        || !valid_sha256(&profile.profile_hash)
        || profile
            .capabilities
            .iter()
            .any(|value| value.trim().is_empty())
        || profile.request_schema_id.trim().is_empty()
        || profile.response_schema_id.trim().is_empty()
    {
        return Err(invalid("protocol_profile_invalid"));
    }
    Ok(())
}

fn valid_protocol_id(value: &str) -> bool {
    !value.trim().is_empty() && value == value.trim() && !value.chars().any(char::is_control)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Endpoint o perfil de protocolo no válido",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn profile(protocol: &str, hash: &str) -> ProtocolProfile {
        ProtocolProfile {
            protocol: protocol.into(),
            profile_hash: hash.into(),
            capabilities: vec!["text.generate".into()],
            streaming: true,
            tools: false,
            request_schema_id: "openai.chat_completions.request.v1".into(),
            response_schema_id: "openai.chat_completions.response.v1".into(),
        }
    }

    #[test]
    fn registry_keeps_multiple_profiles_and_distinguishes_unknown_ids_and_versions() {
        let mut registry = ProtocolRegistry::default();
        let first = profile("openai", &"a".repeat(64));
        let second = profile("openai", &"b".repeat(64));
        let ollama = profile("ollama", &"c".repeat(64));
        registry.register(first.clone()).unwrap();
        registry.register(second).unwrap();
        registry.register(ollama).unwrap();

        assert_eq!(registry.profiles().count(), 3);
        assert_eq!(
            registry.resolve("openai", &"a".repeat(64)),
            ProtocolResolution::Known(&first)
        );
        assert_eq!(
            registry.resolve("openai", &"d".repeat(64)),
            ProtocolResolution::UnknownProfile {
                protocol: "openai",
                profile_hash: &"d".repeat(64),
            }
        );
        assert_eq!(
            registry.resolve("vendor.custom/v7", &"e".repeat(64)),
            ProtocolResolution::UnknownProtocol("vendor.custom/v7")
        );
    }

    #[test]
    fn unknown_protocol_survives_strict_json_roundtrip_without_becoming_supported() {
        let create: InferenceEndpointCreate = serde_json::from_value(json!({
            "instance_id": "0192aaaa-0000-7000-8000-000000000001",
            "protocol": "vendor.custom/v7",
            "protocol_profile_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "base_url": "http://127.0.0.1:18080/v7",
            "credential_reference_id": null,
            "capabilities": ["vendor.text.generate"],
            "qualification": "FAILED",
            "evidence_refs": ["probe:unsupported-protocol"]
        }))
        .unwrap();
        create.validate().unwrap();
        let encoded = serde_json::to_value(&create).unwrap();
        let roundtrip: InferenceEndpointCreate = serde_json::from_value(encoded).unwrap();
        assert_eq!(roundtrip.protocol, "vendor.custom/v7");
        assert_eq!(
            ProtocolRegistry::default()
                .resolve(&roundtrip.protocol, &roundtrip.protocol_profile_hash),
            ProtocolResolution::UnknownProtocol("vendor.custom/v7")
        );
    }

    #[test]
    fn strict_endpoint_schema_rejects_secret_fields_and_invalid_hashes() {
        let base = json!({
            "instance_id": "0192aaaa-0000-7000-8000-000000000001",
            "protocol": "openai",
            "protocol_profile_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "base_url": "http://127.0.0.1:18080/v1",
            "credential_reference_id": null,
            "capabilities": ["text.generate"],
            "qualification": "QUALIFIED",
            "evidence_refs": ["probe:health", "probe:protocol"]
        });
        let mut with_secret = base.clone();
        with_secret["api_key"] = json!("must-not-be-stored");
        assert!(serde_json::from_value::<InferenceEndpointCreate>(with_secret).is_err());

        let mut invalid = base;
        invalid["protocol_profile_hash"] = json!("not-a-sha256");
        let endpoint: InferenceEndpointCreate = serde_json::from_value(invalid).unwrap();
        assert_eq!(
            endpoint.validate().unwrap_err().details.reason,
            "inference_endpoint_create_invalid"
        );
    }
}
