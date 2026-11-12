//! Referencias a credenciales (spec §16, §91; contrato 03; entidad `credential_reference`).
//!
//! El secreto vive solo en Windows Credential Manager; SQLite guarda la
//! referencia (servicio, usuario, tipo, scopes, caducidad). Todo servicio está
//! en el espacio `larm/`. `larm/principal/` (tokens de principales de la API) y
//! `larm/lease/` (tokens de endpoint) los gestiona LARM: crearlos o borrarlos
//! por `/credentials` permitiría a un `credentials:write` fabricarse un
//! principal con más scopes o revocar el de otro.

use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};

/// Tipos de secreto (schema `CredentialWrite.secret_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SecretType {
    Token,
    Password,
    PrivateKey,
    Cookie,
    OauthRefresh,
}

impl SecretType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SecretType::Token => "TOKEN",
            SecretType::Password => "PASSWORD",
            SecretType::PrivateKey => "PRIVATE_KEY",
            SecretType::Cookie => "COOKIE",
            SecretType::OauthRefresh => "OAUTH_REFRESH",
        }
    }
}

/// Único backend del contrato.
pub const BACKEND: &str = "WINDOWS_CREDENTIAL_MANAGER";
/// Prefijo de los tokens de principales de la API.
pub const PRINCIPAL_PREFIX: &str = "larm/principal/";
/// Prefijo de los tokens de endpoint de leases (contrato 04).
pub const LEASE_PREFIX: &str = "larm/lease/";

/// Entidad `credential_reference` (sin secreto).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialReference {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: serde_json::Map<String, serde_json::Value>,
    pub service: String,
    pub username: String,
    pub backend: String,
    pub secret_type: SecretType,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
}

/// `POST /credentials` (schema `CredentialWrite`). `Debug` oculta el secreto.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialWrite {
    pub service: String,
    pub username: String,
    pub secret_type: SecretType,
    pub secret: String,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for CredentialWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialWrite")
            .field("service", &self.service)
            .field("username", &self.username)
            .field("secret_type", &self.secret_type)
            .field("secret", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// `true` si el servicio lo gestiona LARM y no se toca por `/credentials`.
#[must_use]
pub fn is_reserved_service(service: &str) -> bool {
    service.starts_with(PRINCIPAL_PREFIX) || service.starts_with(LEASE_PREFIX)
}

impl CredentialWrite {
    /// Restricciones del schema y espacios reservados.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` si el servicio no es `larm/...` sin espacios, el
    /// usuario está vacío o el secreto no mide 1..=65536; `FORBIDDEN` si el
    /// servicio es de un espacio reservado.
    pub fn validate(&self) -> Result<(), LarmError> {
        let invalid = |reason: &str, message: &str| {
            Err(LarmError::new(ErrorCode::ValidationFailed, message, reason))
        };
        let tail = self.service.strip_prefix("larm/").unwrap_or_default();
        if tail.is_empty() || tail.chars().any(char::is_whitespace) {
            return invalid(
                "service_namespace_invalid",
                "El servicio debe estar en el espacio larm/ y sin espacios",
            );
        }
        if self.username.is_empty() {
            return invalid("username_missing", "Falta el usuario");
        }
        if self.secret.is_empty() || self.secret.chars().count() > 65_536 {
            return invalid(
                "secret_length_invalid",
                "El secreto debe tener entre 1 y 65536 caracteres",
            );
        }
        if is_reserved_service(&self.service) {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Ese espacio de credenciales lo gestiona LARM",
                "reserved_credential_namespace",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn write(service: &str) -> CredentialWrite {
        CredentialWrite {
            service: service.into(),
            username: "acc".into(),
            secret_type: SecretType::Token,
            secret: "hf_secreto".into(),
            scopes: vec![],
        }
    }

    #[test]
    fn espacio_larm_y_reservados() {
        write("larm/source/0192aaaa").validate().unwrap();
        for bad in [
            "ai-broker/token",
            "larm/",
            "larm/con espacio",
            "LARM/x",
            "dashboard",
        ] {
            assert_eq!(
                write(bad).validate().unwrap_err().code,
                ErrorCode::ValidationFailed,
                "{bad}"
            );
        }
        for reserved in ["larm/principal/x", "larm/lease/y"] {
            assert_eq!(
                write(reserved).validate().unwrap_err().code,
                ErrorCode::Forbidden
            );
        }
    }

    #[test]
    fn el_debug_no_muestra_el_secreto() {
        let text = format!("{:?}", write("larm/source/a"));
        assert!(!text.contains("hf_secreto") && text.contains("[REDACTED]"));
    }

    #[test]
    fn secreto_vacio_o_enorme_y_campos_extra_rechazados() {
        let mut w = write("larm/source/a");
        w.secret = String::new();
        assert!(w.validate().is_err());
        w.secret = "x".repeat(65_537);
        assert!(w.validate().is_err());
        let extra = serde_json::json!({"service": "larm/a", "username": "u", "secret_type": "TOKEN",
            "secret": "s", "scopes": [], "backend": "OTRO"});
        assert!(serde_json::from_value::<CredentialWrite>(extra).is_err());
        let bad_type = serde_json::json!({"service": "larm/a", "username": "u", "secret_type": "API_KEY",
            "secret": "s", "scopes": []});
        assert!(serde_json::from_value::<CredentialWrite>(bad_type).is_err());
    }
}
