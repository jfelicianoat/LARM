//! Tipos del contrato Broker–LARM 1.0 (contratos 04, 06 y 07; operaciones
//! P100/P101 de `api-operations.json`).
//!
//! Representación exacta de `schemas/larm.schema.json`: las peticiones son
//! estrictas (`deny_unknown_fields`, equivalente a `additionalProperties:
//! false`) y los tamaños en bytes viajan como cadena decimal. Aquí no hay IO:
//! la validación de forma vive con el tipo y la de negocio en los casos de uso.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use crate::protocol::ProtocolProfile;

/// Versión del contrato que publica `GET /api/v1/broker/catalog`.
pub const BROKER_CONTRACT_VERSION: &str = "1.0";

/// Estado de evidencia de un componente de huella (contrato 05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EvidenceStatus {
    Verified,
    Declared,
    Unavailable,
}

/// Componente de huella con su procedencia. Sin valor, siempre UNAVAILABLE:
/// cero o cadena vacía no representan "desconocido".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceComponent {
    pub value: Option<String>,
    pub status: EvidenceStatus,
    pub evidence_refs: Vec<String>,
}

impl EvidenceComponent {
    #[must_use]
    pub fn verified(value: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self::with_status(Some(value.into()), EvidenceStatus::Verified, evidence)
    }

    #[must_use]
    pub fn declared(value: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self::with_status(Some(value.into()), EvidenceStatus::Declared, evidence)
    }

    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            value: None,
            status: EvidenceStatus::Unavailable,
            evidence_refs: vec![reason.into()],
        }
    }

    fn with_status(
        value: Option<String>,
        status: EvidenceStatus,
        evidence: impl Into<String>,
    ) -> Self {
        match value {
            Some(v) if !v.trim().is_empty() => Self {
                value: Some(v),
                status,
                evidence_refs: vec![evidence.into()],
            },
            _ => Self::unavailable(evidence),
        }
    }
}

/// Huella técnica `{hash, components}` (schema `Fingerprint`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub hash: String,
    pub components: BTreeMap<String, EvidenceComponent>,
}

/// Restricciones exactas de una lease. Nunca se relajan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    #[serde(deserialize_with = "Option::deserialize")]
    pub runtime_version_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub backend_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub execution_fingerprint: Option<String>,
    pub allow_backend_fallback: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub max_memory_bytes: Option<String>,
}

/// Una variante BROKER_ELIGIBLE del catálogo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogItem {
    pub variant_id: String,
    pub capabilities: Vec<String>,
    pub runtime_version_ids: Vec<String>,
    pub backend_ids: Vec<String>,
    pub protocol_profiles: Vec<ProtocolProfile>,
    pub fingerprint: Fingerprint,
    pub constraints: Constraints,
}

/// Respuesta de `GET /api/v1/broker/catalog`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerCatalog {
    pub contract_version: String,
    pub items: Vec<CatalogItem>,
    pub snapshot_at: String,
}

/// Requisitos de recursos de una petición (null = no aplicable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequirements {
    pub capability: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub context_tokens: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub max_output_tokens: Option<u64>,
    pub batch: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub width: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub height: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub frames: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub sample_rate: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub audio_duration_ms: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_memory_budget_bytes: Option<String>,
}

/// Cuerpo estricto de `POST /api/v1/leases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub variant_id: Option<String>,
    pub capability: Option<String>,
    pub launch_profile_id: String,
    pub constraints: Constraints,
    pub ttl_seconds: u64,
    pub owner_id: String,
    pub request_id: String,
    pub resource_requirements: Option<ResourceRequirements>,
}

/// Endpoint de una lease ACTIVE. Nunca rutas de pesos ni secretos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseEndpoint {
    pub base_url: String,
    pub protocol: String,
    pub credential_reference: Option<String>,
}

/// Respuesta ACTIVE de una lease (schema `LeaseResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseResponse {
    pub id: String,
    pub owner_id: String,
    pub instance_id: String,
    pub generation: u64,
    pub endpoint_id: String,
    pub endpoint: LeaseEndpoint,
    pub execution_fingerprint: Fingerprint,
    pub expires_at: String,
    pub renew_after_seconds: u64,
}

/// Estados de lease del contrato 01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LeaseState {
    Requested,
    Admitting,
    Active,
    Expiring,
    Expired,
    Released,
    Failed,
}

impl LeaseState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Expired | Self::Released | Self::Failed)
    }
}

/// Registro de lease (schema `runtime_lease`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLease {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: u64,
    pub metadata: serde_json::Map<String, serde_json::Value>,
    pub owner_id: String,
    pub request_id: String,
    pub instance_id: Option<String>,
    pub generation: u64,
    pub state: LeaseState,
    pub expires_at: Option<String>,
    pub endpoint_id: Option<String>,
    pub constraints: Constraints,
}

/// Cuerpo estricto de `POST /api/v1/leases/{id}/renew`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renew {
    pub generation: u64,
    pub ttl_seconds: u64,
}

/// Lista paginada `{items, next_cursor, snapshot_at}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub snapshot_at: String,
}

/// Estados de job (schema `JobReceipt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum JobPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Respuesta 202 de una operación larga.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobReceipt {
    pub job_id: String,
    pub status_url: String,
    pub state: JobPhase,
}

/// Estado de un job (schema `JobState`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobState {
    pub job_id: String,
    pub state: JobPhase,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub progress: serde_json::Map<String, serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<ApiError>,
    pub evidence_refs: Vec<String>,
}

/// Salud (schema `Health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    pub status: HealthStatus,
    pub schema_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HealthStatus {
    Live,
    Ready,
    Degraded,
}

pub use crate::error::{ApiError, ErrorDetails, OriginalError, ORIGINAL_ERROR_MAX_CHARS};

/// UTC RFC 3339 con milisegundos y `Z` (contrato 00).
#[must_use]
pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `true` si `value` es un UUIDv7 en minúsculas como exigen los schemas.
#[must_use]
pub fn is_uuid_v7(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || c == '-')
        && uuid::Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 7)
}

/// Cadena decimal de bytes acotada a i64::MAX (contrato 00), o None si no lo es.
#[must_use]
pub fn parse_byte_count(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 19 || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    let parsed: u64 = value.parse().ok()?;
    (parsed <= i64::MAX as u64).then_some(parsed)
}

/// `renew_after_seconds = min(40, max(1, floor(ttl/3)))` (contrato 04).
#[must_use]
pub fn renew_after_seconds(ttl_seconds: u64) -> u64 {
    (ttl_seconds / 3).clamp(1, 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renew_cadence_matches_contract() {
        assert_eq!(renew_after_seconds(120), 40);
        assert_eq!(renew_after_seconds(30), 10);
        assert_eq!(renew_after_seconds(600), 40);
        assert_eq!(renew_after_seconds(2), 1);
    }

    #[test]
    fn byte_counts_are_bounded_decimal_strings() {
        assert_eq!(parse_byte_count("0"), Some(0));
        assert_eq!(
            parse_byte_count("9223372036854775807"),
            Some(9_223_372_036_854_775_807)
        );
        assert_eq!(parse_byte_count("9223372036854775808"), None);
        assert_eq!(parse_byte_count("01"), None);
        assert_eq!(parse_byte_count("-1"), None);
        assert_eq!(parse_byte_count(""), None);
    }

    #[test]
    fn uuid_v7_check_is_strict() {
        let v7 = uuid::Uuid::now_v7().to_string();
        assert!(is_uuid_v7(&v7));
        assert!(!is_uuid_v7(&uuid::Uuid::new_v4().to_string()));
        assert!(!is_uuid_v7(&v7.to_uppercase()));
        assert!(!is_uuid_v7("qwen3:8b"));
    }

    #[test]
    fn lease_request_rejects_unknown_fields() {
        let raw = serde_json::json!({
            "variant_id": null, "capability": "text.generate",
            "launch_profile_id": uuid::Uuid::now_v7().to_string(),
            "constraints": {"runtime_version_id": null, "backend_id": null, "execution_fingerprint": null,
                            "allow_backend_fallback": false, "max_memory_bytes": null},
            "ttl_seconds": 120, "owner_id": "ai-broker", "request_id": "t:i",
            "resource_requirements": null, "extra": 1
        });
        assert!(serde_json::from_value::<LeaseRequest>(raw).is_err());
    }

    #[test]
    fn error_shape_and_status() {
        let error = ApiError::new("RESOURCE_UNAVAILABLE", "sin memoria", "memory_budget")
            .retryable(true)
            .with_original("ollama", "500", &"x".repeat(5000));
        let value = serde_json::to_value(&error).unwrap_or_default();
        assert_eq!(
            value["details"]["original_error"]["message"]
                .as_str()
                .map(str::len),
            Some(4096)
        );
        assert_eq!(error.http_status(), 409);
        assert_eq!(ApiError::new("TIMEOUT", "x", "y").http_status(), 504);
        assert!(is_uuid_v7(&error.correlation_id));
    }

    #[test]
    fn empty_value_is_unavailable_not_verified() {
        assert_eq!(
            EvidenceComponent::verified("", "probe").status,
            EvidenceStatus::Unavailable
        );
        assert_eq!(
            EvidenceComponent::verified("x", "probe").status,
            EvidenceStatus::Verified
        );
    }
}
