//! P043: catálogo de fuentes. Registrar/configurar no cualifica ni ejecuta un adaptador.
use crate::bridge::is_uuid_v7;
use crate::error::{ErrorCode, LarmError};
use crate::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CREATE_SOURCE_OPERATION: &str = "P043_post__sources";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthMode {
    None,
    Token,
    Bearer,
    ApiToken,
    OauthDevice,
    OauthAuthCodePkce,
    Basic,
    Ssh,
    Cookie,
    GitLfs,
    WindowsIntegrated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceOperationalStatus {
    #[default]
    Unknown,
    Syncing,
    Healthy,
    RateLimited,
    AuthRequired,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRateLimit {
    #[serde(deserialize_with = "Option::deserialize")]
    pub limit: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub remaining: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub reset_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub retry_after_seconds: Option<DecimalU64>,
}

impl SourceRateLimit {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.reset_at.as_deref().is_some_and(|value| {
            chrono::DateTime::parse_from_rfc3339(value).map_or(true, |parsed| {
                crate::bridge::timestamp(parsed.with_timezone(&chrono::Utc)) != value
            })
        }) || matches!((self.limit, self.remaining), (Some(limit), Some(left)) if left > limit)
        {
            return Err(source_invalid("rate_limit"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOperationalSnapshot {
    pub status: SourceOperationalStatus,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_sync_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub rate_limit: Option<SourceRateLimit>,
}

impl SourceOperationalSnapshot {
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), LarmError> {
        if self.last_sync_at.as_deref().is_some_and(|value| {
            value.len() != 24
                || !value.ends_with('Z')
                || chrono::DateTime::parse_from_rfc3339(value).is_err()
        }) {
            return Err(source_invalid("last_sync_at"));
        }
        if let Some(rate_limit) = &self.rate_limit {
            rate_limit.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSource {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: String,
    pub adapter_id: String,
    pub base_url: String,
    pub priority: u64,
    pub enabled: bool,
    pub auth_mode: AuthMode,
    #[serde(deserialize_with = "Option::deserialize")]
    pub account_id: Option<String>,
    pub policy: Map<String, Value>,
    /// Estado operativo persistido fuera de la configuración de la fuente.
    /// El default permite reproducir respuestas de idempotencia anteriores a P127.
    #[serde(default)]
    pub operational: SourceOperationalSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSourceCreate {
    pub name: String,
    pub adapter_id: String,
    pub base_url: String,
    pub priority: u64,
    pub enabled: bool,
    pub auth_mode: AuthMode,
    #[serde(deserialize_with = "Option::deserialize")]
    pub account_id: Option<String>,
    pub policy: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSourceAdapter {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub adapter_installation_id: String,
    pub source_type: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFilter {
    pub q: Option<String>,
    pub capability: Option<String>,
}

pub fn source_invalid(field: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración de fuente no válida",
        "source_invalid",
    )
    .with_resource(field.to_string())
}

/// Los objetos de configuración abiertos no son un almacén de credenciales.
/// Se conservan campos desconocidos no sensibles, no se redondean ni redactan
/// silenciosamente requests válidas antes de guardarlas.
pub fn validate_public_config(value: &Value) -> Result<(), LarmError> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase().replace(['_', '-'], "");
                if [
                    "authorization",
                    "proxyauthorization",
                    "cookie",
                    "setcookie",
                    "xapikey",
                    "apikey",
                    "apitoken",
                    "bearertoken",
                    "token",
                    "accesstoken",
                    "refreshtoken",
                    "password",
                    "passwd",
                    "secret",
                    "clientsecret",
                    "credentials",
                ]
                .contains(&key.as_str())
                {
                    return Err(source_invalid("secret_payload_forbidden"));
                }
                validate_public_config(value)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                validate_public_config(value)?;
            }
        }
        Value::String(text) if crate::redact::redact(text) != *text => {
            return Err(source_invalid("secret_payload_forbidden"))
        }
        _ => {}
    }
    Ok(())
}

impl ModelSourceCreate {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.name.is_empty()
            || self.base_url.is_empty()
            || !is_uuid_v7(&self.adapter_id)
            || self.priority > i64::MAX as u64
            || self.account_id.as_deref().is_some_and(|id| !is_uuid_v7(id))
        {
            return Err(source_invalid("fields"));
        }
        validate_public_config(&Value::Object(self.policy.clone()))?;
        validate_public_config(&Value::String(self.name.clone()))?;
        validate_public_config(&Value::String(self.base_url.clone()))
    }
}
impl ModelSource {
    pub fn configuration(&self) -> ModelSourceCreate {
        ModelSourceCreate {
            name: self.name.clone(),
            adapter_id: self.adapter_id.clone(),
            base_url: self.base_url.clone(),
            priority: self.priority,
            enabled: self.enabled,
            auth_mode: self.auth_mode.clone(),
            account_id: self.account_id.clone(),
            policy: self.policy.clone(),
        }
    }
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.id) || self.row_version < 1 {
            return Err(source_invalid("identity"));
        }
        self.configuration().validate()?;
        self.operational.validate()?;
        validate_public_config(&Value::Object(self.metadata.clone()))
    }
}
impl ModelSourceAdapter {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.id)
            || !is_uuid_v7(&self.adapter_installation_id)
            || self.row_version < 1
            || self.source_type.is_empty()
        {
            return Err(source_invalid("adapter_identity"));
        }
        validate_public_config(&Value::Object(self.metadata.clone()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;
    fn input() -> Value {
        json!({"name":"public","adapter_id":"0192aaaa-0000-7000-8000-000000000001","base_url":"https://example.invalid","priority":0,"enabled":false,"auth_mode":"NONE","account_id":null,"policy":{"vendor":{"unknown":[1,true,null]}}})
    }
    #[test]
    fn dto_cerrado_y_nullable_obligatorio_conserva_politica_abierta() {
        let original = input();
        let request: ModelSourceCreate = serde_json::from_value(original.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), original);
        for field in original.as_object().unwrap().keys() {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<ModelSourceCreate>(missing).is_err(),
                "{field}"
            );
        }
        let mut extra = original;
        extra["row_version"] = json!(2);
        assert!(serde_json::from_value::<ModelSourceCreate>(extra).is_err());
    }
    #[test]
    fn credenciales_embebidas_y_enteros_fuera_de_sqlite_se_rechazan() {
        for (field, value) in [
            ("name", json!("")),
            ("adapter_id", json!("name-not-uuid")),
            ("account_id", json!("plain-secret")),
            ("base_url", json!("")),
            ("priority", json!(u64::MAX)),
            ("policy", json!({"nested":[{"api_key":"fixture"}]})),
            ("policy", json!({"nested":[{"api_token":"fixture"}]})),
            ("policy", json!({"nested":[{"Bearer-Token":"fixture"}]})),
            (
                "policy",
                json!({"Authorization":"Bearer fixture-credential"}),
            ),
        ] {
            let mut invalid = input();
            invalid[field] = value;
            let request: ModelSourceCreate = serde_json::from_value(invalid).unwrap();
            assert!(request.validate().is_err(), "{field}");
        }
        for value in [json!(-1), json!("1"), json!(1.1)] {
            let mut invalid = input();
            invalid["priority"] = value;
            assert!(serde_json::from_value::<ModelSourceCreate>(invalid).is_err());
        }
    }

    #[test]
    fn operational_snapshot_preserves_unknown_and_validates_only_observed_data() {
        let unknown = SourceOperationalSnapshot::unknown();
        unknown.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&unknown).unwrap(),
            json!({"status":"UNKNOWN","last_sync_at":null,"rate_limit":null})
        );

        let observed = SourceOperationalSnapshot {
            status: SourceOperationalStatus::RateLimited,
            last_sync_at: Some("2026-10-25T12:00:00.000Z".into()),
            rate_limit: Some(SourceRateLimit {
                limit: Some(DecimalU64::new(120).unwrap()),
                remaining: Some(DecimalU64::new(0).unwrap()),
                reset_at: Some("2026-10-25T12:05:00.000Z".into()),
                retry_after_seconds: None,
            }),
        };
        observed.validate().unwrap();

        let invalid_limit = SourceOperationalSnapshot {
            rate_limit: Some(SourceRateLimit {
                limit: Some(DecimalU64::new(10).unwrap()),
                remaining: Some(DecimalU64::new(11).unwrap()),
                reset_at: None,
                retry_after_seconds: None,
            }),
            ..observed.clone()
        };
        assert!(invalid_limit.validate().is_err());
        let invalid_reset = SourceOperationalSnapshot {
            rate_limit: Some(SourceRateLimit {
                reset_at: Some("2026-10-25T13:05:00.000+01:00".into()),
                ..observed.rate_limit.clone().unwrap()
            }),
            ..observed
        };
        assert!(invalid_reset.validate().is_err());
    }
}
