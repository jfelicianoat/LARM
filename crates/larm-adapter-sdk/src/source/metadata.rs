//! Extensiones tipadas dentro de metadata abierta, sin cambiar ModelPage/Error.
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{invalid, wire};

pub const SOURCE_METADATA: &str = "source_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    #[serde(deserialize_with = "Option::deserialize")]
    pub limit: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub remaining: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub reset_at: Option<String>,
    /// Observación original; el retry local la acota a 60 s, sin alterarla.
    #[serde(deserialize_with = "Option::deserialize")]
    pub retry_after_seconds: Option<DecimalU64>,
}
impl RateLimit {
    pub fn validate(&self) -> Result<(), LarmError> {
        if let Some(reset) = &self.reset_at {
            wire::time(reset)?;
        }
        if matches!((self.limit, self.remaining), (Some(limit), Some(left)) if left > limit) {
            return Err(invalid("rate_limit"));
        }
        Ok(())
    }
    pub fn retry_delay(&self) -> Option<std::time::Duration> {
        self.retry_after_seconds
            .map(|n| std::time::Duration::from_secs(n.get().min(60)))
    }
    /// Un 429 no se convierte en un éxito vacío. También funciona sin items.
    pub fn to_error(&self) -> Result<LarmError, LarmError> {
        self.validate()?;
        let original = serde_json::to_string(self).map_err(|_| invalid("rate_limit"))?;
        Ok(LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "La fuente ha limitado las solicitudes",
            "source_rate_limited",
        )
        .retryable(true)
        .with_original("source", "429", &original))
    }
    pub fn from_error(error: &LarmError) -> Option<Self> {
        if error.code != ErrorCode::UpstreamUnavailable
            || error.details.reason != "source_rate_limited"
        {
            return None;
        }
        let original = error.details.original_error.as_ref()?;
        if original.system != "source" || original.code != "429" {
            return None;
        }
        let value = serde_json::from_str(&original.message).ok()?;
        let rate: Self = wire::decode(value).ok()?;
        rate.validate().ok()?;
        Some(rate)
    }
}

/// Se guarda en model_variant.metadata/source-adapter.metadata.source_v1.
/// `original` conserva datos de modelo upstream, nunca headers de transporte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub original: Map<String, Value>,
    pub normalized: Map<String, Value>,
    pub limitations: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub rate_limit: Option<RateLimit>,
}

fn redact(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                    let sensitive = [
                        "authorization",
                        "proxyauthorization",
                        "cookie",
                        "setcookie",
                        "xapikey",
                        "apikey",
                        "accesstoken",
                        "refreshtoken",
                        "token",
                        "secret",
                        "password",
                        "passwd",
                        "clientsecret",
                    ]
                    .contains(&normalized.as_str());
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        Value::String(text) => Value::String(larm_domain::redact::redact(text)),
        other => other.clone(),
    }
}
impl SourceMetadata {
    /// No descarta claves nuevas ni reemplaza null por valores inventados.
    /// Rechaza límites incoherentes antes de adjuntar; redacta campos sensibles.
    pub fn attach(&self, metadata: &mut Map<String, Value>) -> Result<(), LarmError> {
        if let Some(rate) = &self.rate_limit {
            rate.validate()?;
        }
        let value = serde_json::to_value(self).map_err(|_| invalid("metadata"))?;
        metadata.insert(SOURCE_METADATA.into(), redact(&value));
        Ok(())
    }
    pub fn read(metadata: &Map<String, Value>) -> Result<Option<Self>, LarmError> {
        let value: Option<Self> = metadata
            .get(SOURCE_METADATA)
            .cloned()
            .map(wire::decode)
            .transpose()?;
        if let Some(rate) = value.as_ref().and_then(|v| v.rate_limit.as_ref()) {
            rate.validate()?;
        }
        Ok(value)
    }
}
