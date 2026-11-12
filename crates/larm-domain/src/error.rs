//! Modelo de errores de LARM: la forma exacta del schema `Error` (contrato 07).
//!
//! `{code, message, retryable, correlation_id, details{reason, evidence_refs,
//! resource_ids, original_error}}`, sin campos adicionales. El `message` es
//! seguro para el usuario; el detalle técnico vive en un campo que nunca se
//! serializa y solo va a los logs. `original_error` se guarda redactado y
//! truncado a 4096 caracteres. Hay un único tipo de error para API, núcleo y
//! adaptadores: [`ApiError`] es un alias de [`LarmError`].

use serde::{Deserialize, Serialize};

/// Códigos del catálogo del contrato 07 y de §154, con su estado HTTP.
/// Los códigos no reconocidos (p. ej. registrados por un adaptador) se
/// conservan en [`ErrorCode::Other`] y responden 500.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ValidationFailed,
    AuthRequired,
    Forbidden,
    NotFound,
    EventCursorExpired,
    PreconditionRequired,
    Conflict,
    InvalidStateTransition,
    ActiveLease,
    StaleRevision,
    IdempotencyConflict,
    StorageInsufficient,
    ResourceUnavailable,
    ApprovalRequired,
    LicenseRequired,
    ModelOffline,
    RuntimeNotInstalled,
    BackendUnavailable,
    LoadOom,
    UnsupportedCapability,
    Incompatible,
    IntegrityFailed,
    UnknownSize,
    ModelNotFound,
    ModelCorrupt,
    RuntimeIncompatible,
    AdapterCrashed,
    RuntimeCrash,
    DownloadFailed,
    ProtocolError,
    UpstreamUnavailable,
    AuthBackendUnavailable,
    Timeout,
    Internal,
    /// Extensibilidad: código fuera del catálogo; nunca se descarta.
    Other(String),
}

/// (código, texto, estado HTTP) del catálogo. Fuente única de verdad.
const CATALOG: &[(ErrorCode, &str, u16)] = &[
    (ErrorCode::ValidationFailed, "VALIDATION_FAILED", 400),
    (ErrorCode::AuthRequired, "AUTH_REQUIRED", 401),
    (ErrorCode::Forbidden, "FORBIDDEN", 403),
    (ErrorCode::NotFound, "NOT_FOUND", 404),
    (ErrorCode::ModelNotFound, "MODEL_NOT_FOUND", 404),
    (ErrorCode::EventCursorExpired, "EVENT_CURSOR_EXPIRED", 410),
    (
        ErrorCode::PreconditionRequired,
        "PRECONDITION_REQUIRED",
        428,
    ),
    (ErrorCode::Conflict, "CONFLICT", 409),
    (
        ErrorCode::InvalidStateTransition,
        "INVALID_STATE_TRANSITION",
        409,
    ),
    (ErrorCode::ActiveLease, "ACTIVE_LEASE", 409),
    (ErrorCode::StaleRevision, "STALE_REVISION", 409),
    (ErrorCode::IdempotencyConflict, "IDEMPOTENCY_CONFLICT", 409),
    (ErrorCode::StorageInsufficient, "STORAGE_INSUFFICIENT", 409),
    (ErrorCode::ResourceUnavailable, "RESOURCE_UNAVAILABLE", 409),
    (ErrorCode::ApprovalRequired, "APPROVAL_REQUIRED", 409),
    (ErrorCode::LicenseRequired, "LICENSE_REQUIRED", 409),
    (ErrorCode::ModelOffline, "MODEL_OFFLINE", 409),
    (ErrorCode::RuntimeNotInstalled, "RUNTIME_NOT_INSTALLED", 409),
    (ErrorCode::BackendUnavailable, "BACKEND_UNAVAILABLE", 409),
    (ErrorCode::LoadOom, "LOAD_OOM", 409),
    (
        ErrorCode::UnsupportedCapability,
        "UNSUPPORTED_CAPABILITY",
        422,
    ),
    (ErrorCode::Incompatible, "INCOMPATIBLE", 422),
    (ErrorCode::IntegrityFailed, "INTEGRITY_FAILED", 422),
    (ErrorCode::UnknownSize, "UNKNOWN_SIZE", 422),
    (ErrorCode::ModelCorrupt, "MODEL_CORRUPT", 422),
    (ErrorCode::RuntimeIncompatible, "RUNTIME_INCOMPATIBLE", 422),
    (ErrorCode::AdapterCrashed, "ADAPTER_CRASHED", 502),
    (ErrorCode::RuntimeCrash, "RUNTIME_CRASH", 502),
    (ErrorCode::DownloadFailed, "DOWNLOAD_FAILED", 502),
    (ErrorCode::ProtocolError, "PROTOCOL_ERROR", 502),
    (ErrorCode::UpstreamUnavailable, "UPSTREAM_UNAVAILABLE", 503),
    (
        ErrorCode::AuthBackendUnavailable,
        "AUTH_BACKEND_UNAVAILABLE",
        503,
    ),
    (ErrorCode::Timeout, "TIMEOUT", 504),
    (ErrorCode::Internal, "INTERNAL", 500),
];

impl ErrorCode {
    /// Código textual estable (p. ej. `"STORAGE_INSUFFICIENT"`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::Other(s) => s.as_str(),
            known => CATALOG
                .iter()
                .find(|(c, _, _)| c == known)
                .map_or("INTERNAL", |(_, s, _)| s),
        }
    }

    /// Estado HTTP del catálogo; un código desconocido responde 500.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        CATALOG
            .iter()
            .find(|(c, _, _)| c == self)
            .map_or(500, |(_, _, status)| *status)
    }

    /// Códigos que el contrato declara no reintentables (licencia, trust,
    /// incompatibilidad, integridad): `retryable` siempre es `false`.
    #[must_use]
    pub fn never_retryable(&self) -> bool {
        matches!(
            self,
            ErrorCode::LicenseRequired
                | ErrorCode::ApprovalRequired
                | ErrorCode::UnsupportedCapability
                | ErrorCode::Incompatible
                | ErrorCode::RuntimeIncompatible
                | ErrorCode::IntegrityFailed
                | ErrorCode::ModelCorrupt
                | ErrorCode::ValidationFailed
                | ErrorCode::Forbidden
        )
    }

    /// Todos los códigos del catálogo, en el orden del contrato.
    pub fn catalog() -> impl Iterator<Item = &'static ErrorCode> {
        CATALOG.iter().map(|(c, _, _)| c)
    }
}

impl From<&str> for ErrorCode {
    fn from(value: &str) -> Self {
        CATALOG.iter().find(|(_, s, _)| *s == value).map_or_else(
            || ErrorCode::Other(value.to_string()),
            |(c, _, _)| c.clone(),
        )
    }
}

impl From<ErrorCode> for String {
    fn from(value: ErrorCode) -> Self {
        value.as_str().to_string()
    }
}

impl PartialEq<&str> for ErrorCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(serde::de::Error::custom("code vacío"));
        }
        Ok(ErrorCode::from(s.as_str()))
    }
}

/// Error original conservado, redactado y truncado (máximo 4096 caracteres).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalError {
    pub system: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetails {
    pub reason: String,
    pub evidence_refs: Vec<String>,
    pub resource_ids: Vec<String>,
    pub original_error: Option<OriginalError>,
}

/// Límite de `original_error.message` del contrato 07.
pub const ORIGINAL_ERROR_MAX_CHARS: usize = 4096;

/// Error de LARM con la forma exacta del schema `Error` del contrato 07.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("{code}: {message}")]
pub struct LarmError {
    pub code: ErrorCode,
    /// Mensaje seguro para el usuario (sin rutas internas, credenciales ni stack).
    pub message: String,
    pub retryable: bool,
    /// UUIDv7; lo fija la frontera que recibe la petición ([`LarmError::with_correlation`]).
    pub correlation_id: String,
    /// En caja: el error viaja en `Result` por toda la cadena y el caso de
    /// éxito no debe pagar su tamaño.
    pub details: Box<ErrorDetails>,
    /// Detalle técnico solo para logs; nunca se serializa ni llega a la API.
    #[serde(skip)]
    technical: Option<String>,
}

/// Alias histórico del puente Broker–LARM: es el mismo tipo.
pub type ApiError = LarmError;

impl LarmError {
    /// Error con código, mensaje para el usuario y `details.reason` (texto
    /// técnico breve y seguro: sin rutas ni secretos).
    #[must_use]
    pub fn new(
        code: impl Into<ErrorCode>,
        message: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            correlation_id: uuid::Uuid::now_v7().to_string(),
            details: Box::new(ErrorDetails {
                reason: reason.into(),
                evidence_refs: Vec::new(),
                resource_ids: Vec::new(),
                original_error: None,
            }),
            technical: None,
        }
    }

    /// Marca el error como reintentable salvo en los códigos que el contrato
    /// declara definitivos (licencia, trust, incompatibilidad...).
    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable && !self.code.never_retryable();
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.details.evidence_refs.push(evidence.into());
        self
    }

    #[must_use]
    pub fn with_resource(mut self, id: impl Into<String>) -> Self {
        self.details.resource_ids.push(id.into());
        self
    }

    /// Conserva el error original del sistema externo, redactado y truncado.
    #[must_use]
    pub fn with_original(mut self, system: &str, code: &str, message: &str) -> Self {
        let message: String = crate::redact::redact(message)
            .chars()
            .take(ORIGINAL_ERROR_MAX_CHARS)
            .collect();
        self.details.original_error = Some(OriginalError {
            system: system.to_string(),
            code: code.to_string(),
            message,
        });
        self
    }

    /// Detalle técnico para logs (redactado); no se serializa.
    #[must_use]
    pub fn with_technical(mut self, detail: impl Into<String>) -> Self {
        self.technical = Some(crate::redact::redact(&detail.into()));
        self
    }

    /// Correlation id de la petición que originó el error (propagación API→core→worker).
    #[must_use]
    pub fn with_correlation(mut self, id: crate::correlation::CorrelationId) -> Self {
        self.correlation_id = id.to_string();
        self
    }

    /// Mensaje seguro para el usuario.
    #[must_use]
    pub fn user_message(&self) -> &str {
        &self.message
    }

    /// Detalle técnico para log: el explícito o, si no hay, `details.reason`.
    #[must_use]
    pub fn technical_detail(&self) -> &str {
        self.technical.as_deref().unwrap_or(&self.details.reason)
    }

    /// Código HTTP del catálogo del contrato 07 y §154.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serde_json::json;

    #[test]
    fn serializa_la_forma_exacta_del_contrato_sin_detalle_tecnico() {
        let id = crate::correlation::CorrelationId::new();
        let e = LarmError::new(
            ErrorCode::ModelNotFound,
            "Modelo no encontrado",
            "variant_not_in_registry",
        )
        .with_technical("C:/Users/x/AppData/Local/LARM/models no contiene abc")
        .with_resource("0192aaaa-0000-7000-8000-000000000000")
        .with_correlation(id);
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(
            j,
            json!({
                "code": "MODEL_NOT_FOUND",
                "message": "Modelo no encontrado",
                "retryable": false,
                "correlation_id": id.to_string(),
                "details": {
                    "reason": "variant_not_in_registry",
                    "evidence_refs": [],
                    "resource_ids": ["0192aaaa-0000-7000-8000-000000000000"],
                    "original_error": null
                }
            })
        );
        assert!(!j.to_string().contains("AppData"));
        assert!(e.technical_detail().contains("AppData"));
    }

    #[test]
    fn catalogo_y_estados_http_del_contrato() {
        let expected = [
            ("VALIDATION_FAILED", 400),
            ("AUTH_REQUIRED", 401),
            ("FORBIDDEN", 403),
            ("NOT_FOUND", 404),
            ("CONFLICT", 409),
            ("INVALID_STATE_TRANSITION", 409),
            ("ACTIVE_LEASE", 409),
            ("STALE_REVISION", 409),
            ("IDEMPOTENCY_CONFLICT", 409),
            ("STORAGE_INSUFFICIENT", 409),
            ("RESOURCE_UNAVAILABLE", 409),
            ("APPROVAL_REQUIRED", 409),
            ("LICENSE_REQUIRED", 409),
            ("UNSUPPORTED_CAPABILITY", 422),
            ("INCOMPATIBLE", 422),
            ("INTEGRITY_FAILED", 422),
            ("UNKNOWN_SIZE", 422),
            ("ADAPTER_CRASHED", 502),
            ("UPSTREAM_UNAVAILABLE", 503),
            ("AUTH_BACKEND_UNAVAILABLE", 503),
            ("TIMEOUT", 504),
            ("INTERNAL", 500),
            // §154
            ("MODEL_NOT_FOUND", 404),
            ("MODEL_OFFLINE", 409),
            ("MODEL_CORRUPT", 422),
            ("RUNTIME_NOT_INSTALLED", 409),
            ("RUNTIME_INCOMPATIBLE", 422),
            ("RUNTIME_CRASH", 502),
            ("BACKEND_UNAVAILABLE", 409),
            ("LICENSE_REQUIRED", 409),
            ("DOWNLOAD_FAILED", 502),
            ("LOAD_OOM", 409),
            ("PROTOCOL_ERROR", 502),
            // contrato 07: If-Match ausente y cursor podado
            ("PRECONDITION_REQUIRED", 428),
            ("EVENT_CURSOR_EXPIRED", 410),
        ];
        for (code, status) in expected {
            let parsed = ErrorCode::from(code);
            assert!(
                !matches!(parsed, ErrorCode::Other(_)),
                "{code} fuera del catálogo"
            );
            assert_eq!(parsed.as_str(), code);
            assert_eq!(parsed.http_status(), status, "{code}");
        }
        assert_eq!(ErrorCode::catalog().count(), 34);
    }

    #[test]
    fn codigo_desconocido_se_conserva_y_responde_500() {
        let raw = json!({"code": "FUTURE_CODE_42", "message": "x", "retryable": true,
            "correlation_id": "0192aaaa-0000-7000-8000-000000000000",
            "details": {"reason": "r", "evidence_refs": [], "resource_ids": [], "original_error": null}});
        let e: LarmError = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(e.code, ErrorCode::Other("FUTURE_CODE_42".into()));
        assert_eq!(e.http_status(), 500);
        assert_eq!(serde_json::to_value(&e).unwrap(), raw);
    }

    #[test]
    fn campos_adicionales_o_codigo_vacio_se_rechazan() {
        let extra = json!({"code": "TIMEOUT", "message": "x", "retryable": true,
            "correlation_id": "c", "detail": "fuga",
            "details": {"reason": "r", "evidence_refs": [], "resource_ids": [], "original_error": null}});
        assert!(serde_json::from_value::<LarmError>(extra).is_err());
        let empty = json!({"code": "", "message": "x", "retryable": true, "correlation_id": "c",
            "details": {"reason": "r", "evidence_refs": [], "resource_ids": [], "original_error": null}});
        assert!(serde_json::from_value::<LarmError>(empty).is_err());
    }

    #[test]
    fn retryable_nunca_en_licencia_ni_incompatibles() {
        for code in [
            "LICENSE_REQUIRED",
            "INCOMPATIBLE",
            "RUNTIME_INCOMPATIBLE",
            "UNSUPPORTED_CAPABILITY",
        ] {
            assert!(!LarmError::new(code, "m", "r").retryable(true).retryable);
        }
        assert!(
            LarmError::new("TIMEOUT", "m", "r")
                .retryable(true)
                .retryable
        );
    }

    #[test]
    fn original_error_redactado_y_truncado() {
        let msg = format!("Authorization: Bearer abcdefghijkl\n{}", "x".repeat(5000));
        let e =
            LarmError::new("UPSTREAM_UNAVAILABLE", "m", "r").with_original("ollama", "500", &msg);
        let original = e.details.original_error.unwrap();
        assert_eq!(original.message.chars().count(), ORIGINAL_ERROR_MAX_CHARS);
        assert!(!original.message.contains("abcdefghijkl"));
        assert!(original.message.starts_with("[REDACTED]"));
    }

    #[test]
    fn correlation_id_es_uuidv7_y_se_propaga() {
        let e = LarmError::new("TIMEOUT", "m", "r");
        assert!(crate::bridge::is_uuid_v7(&e.correlation_id));
        let id = crate::correlation::CorrelationId::new();
        assert_eq!(e.with_correlation(id).correlation_id, id.to_string());
    }
}
