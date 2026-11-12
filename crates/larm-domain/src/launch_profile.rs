//! Perfiles reproducibles de lanzamiento (P034, contratos 04/07/11).

use crate::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
    launch::LaunchParameters,
    ports::MutationContext,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchProfileKind {
    #[serde(rename = "ECO")]
    Eco,
    #[serde(rename = "BALANCED")]
    Balanced,
    #[serde(rename = "PERFORMANCE")]
    Performance,
    #[serde(rename = "LONG_CONTEXT")]
    LongContext,
    #[serde(rename = "CUSTOM")]
    Custom,
}

impl LaunchProfileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eco => "ECO",
            Self::Balanced => "BALANCED",
            Self::Performance => "PERFORMANCE",
            Self::LongContext => "LONG_CONTEXT",
            Self::Custom => "CUSTOM",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchProfileCreate {
    pub name: LaunchProfileKind,
    pub parameters: LaunchParameters,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub backend_id: Option<String>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchProfile {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: LaunchProfileKind,
    pub parameters: LaunchParameters,
    pub profile_hash: String,
    pub backend_id: Option<String>,
}

impl LaunchProfileCreate {
    /// Comprueba los invariantes comunes del perfil antes de tocar SQLite.
    pub fn validate(&self) -> Result<(), LarmError> {
        self.parameters
            .validate()
            .map_err(|error| invalid(error.reason, error.detail))?;
        if self.backend_id.as_deref().is_some_and(|id| !is_uuid_v7(id)) {
            return Err(invalid(
                "launch_profile_backend_invalid",
                "backend_id debe ser UUIDv7 o null",
            ));
        }
        Ok(())
    }

    /// El hash identifica todas las opciones del perfil que pueden cambiar
    /// una selección de backend o los parámetros efectivos.
    pub fn profile_hash(&self) -> Result<String, LarmError> {
        self.validate()?;
        let value = serde_json::json!({
            "backend_id": self.backend_id,
            "parameters": self.parameters,
        });
        crate::canonical::sha256_hex(&value).map_err(|_| {
            invalid(
                "launch_profile_hash_invalid",
                "no se pudo canonicalizar el perfil",
            )
        })
    }

    pub fn into_record(
        self,
        id: String,
        at: String,
        row_version: i64,
        metadata: Map<String, Value>,
    ) -> Result<LaunchProfile, LarmError> {
        let profile_hash = self.profile_hash()?;
        Ok(LaunchProfile {
            id,
            created_at: at.clone(),
            updated_at: at,
            row_version,
            metadata,
            name: self.name,
            parameters: self.parameters,
            profile_hash,
            backend_id: self.backend_id,
        })
    }
}

impl LaunchProfile {
    /// Valida la fila y vuelve a calcular el hash al leer datos durables.
    pub fn validate_record(&self) -> Result<(), LarmError> {
        let create = LaunchProfileCreate {
            name: self.name,
            parameters: self.parameters.clone(),
            backend_id: self.backend_id.clone(),
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
            || self.profile_hash != create.profile_hash()?
        {
            return Err(invalid(
                "launch_profile_corrupt",
                "el registro no coincide con su identidad, versión o hash",
            ));
        }
        Ok(())
    }

    /// Campos efectivos que difieren, en orden estable y con rutas JSON Pointer.
    pub fn diff(&self, other: &Self) -> Result<Vec<String>, LarmError> {
        self.validate_record()?;
        other.validate_record()?;
        let left = serde_json::json!({
            "backend_id": self.backend_id,
            "parameters": self.parameters,
        });
        let right = serde_json::json!({
            "backend_id": other.backend_id,
            "parameters": other.parameters,
        });
        let mut changed = Vec::new();
        diff_values("", &left, &right, &mut changed);
        Ok(changed)
    }
}

fn diff_values(path: &str, left: &Value, right: &Value, output: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys: std::collections::BTreeSet<_> = left.keys().chain(right.keys()).collect();
            for key in keys {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let next = format!("{path}/{escaped}");
                match (left.get(key), right.get(key)) {
                    (Some(a), Some(b)) => diff_values(&next, a, b, output),
                    _ => output.push(next),
                }
            }
        }
        _ if left != right => output.push(path.to_string()),
        _ => {}
    }
}

fn invalid(reason: &'static str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

#[async_trait]
pub trait LaunchProfileRepository: Send + Sync {
    async fn list_launch_profiles(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<LaunchProfile>, LarmError>;

    async fn launch_profile(&self, id: &str) -> Result<Option<LaunchProfile>, LarmError>;

    async fn create_launch_profile(
        &self,
        profile: &LaunchProfileCreate,
        body_hash: &str,
        idempotency_key: &str,
        ctx: &MutationContext,
    ) -> Result<LaunchProfile, LarmError>;

    async fn update_launch_profile(
        &self,
        id: &str,
        expected_row_version: i64,
        profile: &LaunchProfileCreate,
        ctx: &MutationContext,
    ) -> Result<LaunchProfile, LarmError>;

    async fn delete_launch_profile(
        &self,
        id: &str,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{launch::LaunchParameters, units::DecimalU64};

    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000001";

    fn profile(batch: u32) -> LaunchProfile {
        LaunchProfileCreate {
            name: LaunchProfileKind::Balanced,
            parameters: LaunchParameters::builtin(BACKEND, 4, Some(4096), batch),
            backend_id: None,
        }
        .into_record(
            "0192aaaa-0000-7000-8000-000000000002".into(),
            "2026-10-22T12:00:00.000Z".into(),
            1,
            Map::new(),
        )
        .unwrap()
    }

    #[test]
    fn profile_hash_and_diff_cover_only_effective_changes() {
        let first = profile(512);
        let second = profile(256);
        assert_ne!(first.profile_hash, second.profile_hash);
        assert_eq!(first.diff(&second).unwrap(), vec!["/parameters/batch"]);
        assert!(first.validate_record().is_ok());
    }

    #[test]
    fn profile_hash_includes_backend_preference_and_memory_limit() {
        let first = profile(512);
        let mut second_create = LaunchProfileCreate {
            name: first.name,
            parameters: first.parameters.clone(),
            backend_id: Some(BACKEND.into()),
        };
        second_create.parameters.memory_limit_bytes = Some(DecimalU64::new(4096).unwrap());
        let second = second_create
            .into_record(
                "0192aaaa-0000-7000-8000-000000000003".into(),
                first.created_at.clone(),
                1,
                Map::new(),
            )
            .unwrap();
        assert_ne!(first.profile_hash, second.profile_hash);
        assert!(first.diff(&second).unwrap().contains(&"/backend_id".into()));
        assert!(first
            .diff(&second)
            .unwrap()
            .contains(&"/parameters/memory_limit_bytes".into()));
    }

    #[test]
    fn request_requires_nullable_backend_field_and_rejects_unknown_fields() {
        let mut value = serde_json::to_value(LaunchProfileCreate {
            name: LaunchProfileKind::Balanced,
            parameters: LaunchParameters::builtin(BACKEND, 4, Some(4096), 512),
            backend_id: None,
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("backend_id");
        assert!(serde_json::from_value::<LaunchProfileCreate>(value).is_err());
        let mut value = serde_json::to_value(LaunchProfileCreate {
            name: LaunchProfileKind::Balanced,
            parameters: LaunchParameters::builtin(BACKEND, 4, Some(4096), 512),
            backend_id: None,
        })
        .unwrap();
        value["script"] = Value::String("must_not_run".into());
        assert!(serde_json::from_value::<LaunchProfileCreate>(value).is_err());
    }

    #[test]
    fn record_hash_tampering_is_rejected() {
        let mut record = profile(512);
        record.profile_hash.replace_range(..1, "b");
        assert_eq!(
            record.validate_record().unwrap_err().details.reason,
            "launch_profile_corrupt"
        );
    }
}
