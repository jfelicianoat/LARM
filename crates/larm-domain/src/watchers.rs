//! Suscripciones de modelos a cambios en fuentes (P050).

use crate::{
    error::{ErrorCode, LarmError},
    sources::{SourceOperationalStatus, SourceRateLimit},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRevisionEvent {
    NewModel,
    NewCompatibleModel,
    ModelUpdate,
}

impl SourceRevisionEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewModel => "NEW_MODEL",
            Self::NewCompatibleModel => "NEW_COMPATIBLE_MODEL",
            Self::ModelUpdate => "MODEL_UPDATE",
        }
    }
}

/// Clasifica sólo cuando aparece una revisión que no se había observado.
#[must_use]
pub fn classify_source_revision(
    previous_fingerprint: Option<&str>,
    current_fingerprint: &str,
    compatible: bool,
) -> Option<SourceRevisionEvent> {
    match previous_fingerprint {
        Some(previous) if previous == current_fingerprint => None,
        Some(_) => Some(SourceRevisionEvent::ModelUpdate),
        None if compatible => Some(SourceRevisionEvent::NewCompatibleModel),
        None => Some(SourceRevisionEvent::NewModel),
    }
}

/// Clave opaca y estable de deduplicación. La identidad upstream nunca queda
/// en claro en la clave persistida.
#[must_use]
pub fn source_watch_dedupe_key(
    source_id: &str,
    upstream_identity: &str,
    revision_fingerprint: &str,
    event_type: SourceRevisionEvent,
) -> String {
    let identity_hash = Sha256::digest(upstream_identity.as_bytes());
    let material = format!(
        "{source_id}\0{}\0{revision_fingerprint}\0{}",
        hex::encode(identity_hash),
        event_type.as_str()
    );
    hex::encode(Sha256::digest(material.as_bytes()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWatch {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub source_id: String,
    pub query: Map<String, Value>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub etag: Option<String>,
    pub interval_seconds: u64,
    pub policy: Map<String, Value>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub next_run_at: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWatchCreate {
    pub source_id: String,
    pub query: Map<String, Value>,
    pub interval_seconds: u64,
    pub policy: Map<String, Value>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchDetection {
    pub upstream_identity: String,
    pub revision_fingerprint: String,
    pub compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWatchSyncSuccess {
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub etag: Option<String>,
    pub observed_at: String,
    pub next_run_at: String,
    pub detections: Vec<WatchDetection>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub rate_limit: Option<SourceRateLimit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWatchSyncFailure {
    pub reason: String,
    pub retry_at: String,
    pub observed_at: String,
    pub operational_status: SourceOperationalStatus,
    #[serde(deserialize_with = "Option::deserialize")]
    pub rate_limit: Option<SourceRateLimit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub dedupe_key: String,
    pub severity: String,
    pub message_code: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub resource_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub action: Option<Value>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub read_at: Option<String>,
}

impl SourceWatchCreate {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !crate::bridge::is_uuid_v7(&self.source_id)
            || !(60..=i64::MAX as u64).contains(&self.interval_seconds)
        {
            return Err(invalid("fields"));
        }
        crate::sources::validate_public_config(&Value::Object(self.query.clone()))?;
        validate_watch_query(&self.query)?;
        crate::sources::validate_public_config(&Value::Object(self.policy.clone()))?;
        if self
            .policy
            .get("automatic_download")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "La descarga automática requiere la política P119",
                "watch_auto_download_not_implemented",
            ));
        }
        Ok(())
    }
}

/// Valida los filtros compartidos por búsqueda y vigilancia. Las claves
/// específicas de adaptador siguen siendo extensibles en `query`.
pub fn validate_watch_query(query: &Map<String, Value>) -> Result<(), LarmError> {
    if query
        .get("q")
        .is_some_and(|value| value.as_str().is_none_or(|text| text.chars().count() > 500))
    {
        return Err(invalid("query.q"));
    }
    if let Some(value) = query.get("capabilities") {
        let Some(capabilities) = value.as_array() else {
            return Err(invalid("query.capabilities"));
        };
        if capabilities
            .iter()
            .any(|item| item.as_str().is_none_or(str::is_empty))
        {
            return Err(invalid("query.capabilities"));
        }
    }
    if let Some(value) = query.get("compatibility") {
        if !matches!(
            value.as_str(),
            Some("ALL" | "COMPATIBLE" | "UNKNOWN" | "INCOMPATIBLE")
        ) {
            return Err(invalid("query.compatibility"));
        }
    }
    if let Some(value) = query.get("size_bytes") {
        let Some(range) = value.as_object() else {
            return Err(invalid("query.size_bytes"));
        };
        let minimum = watch_range_value(range.get("minimum"))?;
        let maximum = watch_range_value(range.get("maximum"))?;
        if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
            return Err(invalid("query.size_bytes"));
        }
    }
    Ok(())
}

fn watch_range_value(value: Option<&Value>) -> Result<Option<u64>, LarmError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text))
            if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let parsed = text
                .parse::<u64>()
                .map_err(|_| invalid("query.size_bytes"))?;
            if parsed > i64::MAX as u64 {
                return Err(invalid("query.size_bytes"));
            }
            Ok(Some(parsed))
        }
        _ => Err(invalid("query.size_bytes")),
    }
}

impl SourceWatch {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !crate::bridge::is_uuid_v7(&self.id)
            || !crate::bridge::is_uuid_v7(&self.source_id)
            || self.row_version < 1
            || !(60..=i64::MAX as u64).contains(&self.interval_seconds)
        {
            return Err(invalid("identity_or_interval"));
        }
        crate::sources::validate_public_config(&Value::Object(self.metadata.clone()))?;
        crate::sources::validate_public_config(&Value::Object(self.query.clone()))?;
        validate_watch_query(&self.query)?;
        crate::sources::validate_public_config(&Value::Object(self.policy.clone()))
    }
}

fn invalid(field: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración de vigilancia no válida",
        "source_watch_invalid",
    )
    .with_resource(field.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn watch_query_accepts_exact_ranges_and_rejects_ambiguous_ranges() {
        let valid = serde_json::from_value::<SourceWatchCreate>(serde_json::json!({
            "source_id":"0192f000-0000-7000-8000-000000000301",
            "query":{"q":"fixture","capabilities":["chat"],"compatibility":"UNKNOWN","size_bytes":{"minimum":"10","maximum":"20"}},
            "interval_seconds":3600,"policy":{},"enabled":true
        })).unwrap();
        assert!(valid.validate().is_ok());

        let reversed = serde_json::json!({"size_bytes":{"minimum":"21","maximum":"20"}});
        assert_eq!(
            validate_watch_query(reversed.as_object().unwrap())
                .unwrap_err()
                .details
                .reason,
            "source_watch_invalid"
        );
        let estimate = serde_json::json!({"size_bytes":{"minimum":10,"maximum":20}});
        assert!(
            validate_watch_query(estimate.as_object().unwrap()).is_err(),
            "los rangos usan bytes decimales exactos"
        );
        let unknown_mode = serde_json::json!({"compatibility":"LIKELY"});
        assert!(validate_watch_query(unknown_mode.as_object().unwrap()).is_err());
    }
}
