//! Cuentas asociadas a una fuente y consentimientos de licencia P045.

use crate::bridge::is_uuid_v7;
use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSourceAccount {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub source_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub display_name: String,
    pub accepted_license_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSourceAccountCreate {
    pub source_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
    pub display_name: String,
    pub accepted_license_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSourceLicense {
    pub license_hash: String,
    pub source_account_id: String,
    pub revision_key: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLicenseReceipt {
    pub resource_id: String,
    pub row_version: i64,
    pub event_id: String,
}

impl CheckSourceLicense {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !crate::adapter_updates::is_sha256(&self.license_hash)
            || !is_uuid_v7(&self.source_account_id)
            || self.revision_key.is_empty()
        {
            return Err(account_invalid("license_request"));
        }
        Ok(())
    }
}

pub fn account_invalid(field: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración de cuenta de fuente no válida",
        "source_account_invalid",
    )
    .with_resource(field.to_string())
}

impl ModelSourceAccountCreate {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.source_id)
            || self
                .credential_reference_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
            || self.display_name.trim().is_empty()
            || self.display_name.chars().any(char::is_control)
            || self
                .accepted_license_hashes
                .iter()
                .any(|hash| !crate::adapter_updates::is_sha256(hash))
        {
            return Err(account_invalid("fields"));
        }
        crate::sources::validate_public_config(&Value::String(self.display_name.clone()))?;
        Ok(())
    }
}

impl ModelSourceAccount {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.id) || self.row_version < 1 {
            return Err(account_invalid("identity"));
        }
        ModelSourceAccountCreate {
            source_id: self.source_id.clone(),
            credential_reference_id: self.credential_reference_id.clone(),
            display_name: self.display_name.clone(),
            accepted_license_hashes: self.accepted_license_hashes.clone(),
        }
        .validate()?;
        crate::sources::validate_public_config(&Value::Object(self.metadata.clone()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn request() -> Value {
        json!({"source_id":"0192aaaa-0000-7000-8000-000000000001","credential_reference_id":null,
            "display_name":"Cuenta privada","accepted_license_hashes":["a".repeat(64)]})
    }

    #[test]
    fn dto_cerrado_y_roundtrip_con_nullable_obligatorio() {
        let value = request();
        let parsed: ModelSourceAccountCreate = serde_json::from_value(value.clone()).unwrap();
        parsed.validate().unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        let mut missing = value.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("credential_reference_id");
        assert!(serde_json::from_value::<ModelSourceAccountCreate>(missing).is_err());
        let mut extra = value;
        extra["secret"] = json!("must-not-be-accepted");
        assert!(serde_json::from_value::<ModelSourceAccountCreate>(extra).is_err());
    }

    #[test]
    fn rechaza_identidades_mal_formadas_hashes_y_nombres_no_utilizables() {
        for invalid in [
            json!({"source_id":"no-uuid","credential_reference_id":null,"display_name":"x","accepted_license_hashes":[]}),
            json!({"source_id":"0192aaaa-0000-7000-8000-000000000001","credential_reference_id":"no-uuid","display_name":"x","accepted_license_hashes":[]}),
            json!({"source_id":"0192aaaa-0000-7000-8000-000000000001","credential_reference_id":null,"display_name":"  ","accepted_license_hashes":[]}),
            json!({"source_id":"0192aaaa-0000-7000-8000-000000000001","credential_reference_id":null,"display_name":"x","accepted_license_hashes":["A".repeat(64)]}),
        ] {
            let parsed: ModelSourceAccountCreate = serde_json::from_value(invalid).unwrap();
            assert!(parsed.validate().is_err());
        }
    }

    #[test]
    fn licencia_acepta_solo_hash_sha256_y_referencia_uuidv7() {
        let valid = CheckSourceLicense {
            license_hash: "b".repeat(64),
            source_account_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            revision_key: "revision:main".into(),
            accepted: true,
        };
        valid.validate().unwrap();
        for invalid in [
            CheckSourceLicense {
                license_hash: "B".repeat(64),
                ..valid.clone()
            },
            CheckSourceLicense {
                source_account_id: "not-uuid".into(),
                ..valid.clone()
            },
            CheckSourceLicense {
                revision_key: String::new(),
                ..valid.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
}
