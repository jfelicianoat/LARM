//! Entrada estricta para guardar credenciales Basic de una cuenta de fuente.

use crate::bridge::is_uuid_v7;
use crate::error::{ErrorCode, LarmError};
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicCredentialWrite {
    pub source_id: String,
    pub account_id: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for BasicCredentialWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicCredentialWrite")
            .field("source_id", &self.source_id)
            .field("account_id", &self.account_id)
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl BasicCredentialWrite {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.source_id)
            || !is_uuid_v7(&self.account_id)
            || self.username.is_empty()
            || self.password.is_empty()
            || self.password.chars().count() > 65_536
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La credencial Basic no es válida",
                "basic_credential_invalid",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn request() -> BasicCredentialWrite {
        BasicCredentialWrite {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            account_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            username: "cuenta-privada".into(),
            password: "password-sentinel-never-log".into(),
        }
    }

    #[test]
    fn request_cerrada_valida_y_redacta_los_dos_campos_de_acceso() {
        let value = serde_json::json!({
            "source_id": "0192aaaa-0000-7000-8000-000000000001",
            "account_id": "0192aaaa-0000-7000-8000-000000000002",
            "username": "cuenta-privada",
            "password": "password-sentinel-never-log"
        });
        assert!(
            serde_json::from_value::<BasicCredentialWrite>(value.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        let mut extra = value;
        extra["token"] = serde_json::json!("no-admitido");
        assert!(serde_json::from_value::<BasicCredentialWrite>(extra).is_err());
        let debug = format!("{:?}", request());
        assert!(!debug.contains("cuenta-privada"));
        assert!(!debug.contains("password-sentinel-never-log"));
        assert!(debug.matches("[REDACTED]").count() == 2);
    }

    #[test]
    fn rechaza_identificadores_y_limites_invalidos_del_schema() {
        let mut bad = request();
        bad.source_id = "source".into();
        assert!(bad.validate().is_err());
        let mut bad = request();
        bad.username.clear();
        assert!(bad.validate().is_err());
        let mut bad = request();
        bad.password.clear();
        assert!(bad.validate().is_err());
        let mut bad = request();
        bad.password = "x".repeat(65_537);
        assert!(bad.validate().is_err());
    }
}
