//! Utilidades HTTP comunes de la API: errores con el cuerpo del contrato 07,
//! correlación, cabeceras obligatorias y JSON estricto.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use larm_core::bridge::{ApiScope, Principal, PrincipalAuthenticator};
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::correlation::CORRELATION_HEADER;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Tamaño máximo de cuerpo (defaults.json `api.request_body_bytes`).
pub const REQUEST_BODY_BYTES: usize = 1_048_576;
pub(crate) const IDEMPOTENCY_KEY_MAX: usize = 240;

/// Error HTTP con el cuerpo exacto del contrato y su correlación.
pub(crate) struct Failure(Box<ApiError>);

impl Failure {
    /// Sustituye la correlación por la de la petición autenticada.
    #[must_use]
    pub(crate) fn recorrelate(mut self, correlation_id: &str) -> Self {
        self.0.correlation_id = correlation_id.to_string();
        self
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        Self(Box::new(error))
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let correlation = self.0.correlation_id.clone();
        let mut response = (status, axum::Json(*self.0)).into_response();
        if let Ok(value) = HeaderValue::from_str(&correlation) {
            response.headers_mut().insert(CORRELATION_HEADER, value);
        }
        response
    }
}

pub(crate) type Answer = Result<Response, Failure>;

pub(crate) fn correlated(mut error: ApiError, headers: &HeaderMap) -> ApiError {
    if let Some(incoming) = headers
        .get(CORRELATION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        if is_uuid_v7(incoming) {
            error.correlation_id = incoming.to_string();
        }
    }
    error
}

pub(crate) fn json<T: Serialize>(status: StatusCode, body: &T) -> Response {
    (status, axum::Json(body)).into_response()
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
}

pub(crate) async fn principal(
    auth: &dyn PrincipalAuthenticator,
    headers: &HeaderMap,
    scope: ApiScope,
) -> Result<Principal, Failure> {
    let Some(token) = bearer(headers) else {
        return Err(correlated(
            ApiError::new("AUTH_REQUIRED", "Falta el Bearer", "bearer_missing"),
            headers,
        )
        .into());
    };
    let authenticated = match auth.authenticate(token).await {
        Ok(found) => found,
        Err(error) => return Err(correlated(error, headers).into()),
    };
    let Some(principal) = authenticated else {
        return Err(correlated(
            ApiError::new("AUTH_REQUIRED", "Bearer no válido", "bearer_invalid"),
            headers,
        )
        .into());
    };
    if !principal.has_api_scope(scope) {
        let error = ApiError::new(
            "FORBIDDEN",
            format!("Falta el scope {}", scope.as_str()),
            "scope_missing",
        );
        return Err(correlated(error, headers).into());
    }
    Ok(principal)
}

pub(crate) fn uuid_param(value: &str, headers: &HeaderMap) -> Result<(), Failure> {
    if is_uuid_v7(value) {
        Ok(())
    } else {
        Err(correlated(
            ApiError::new(
                "VALIDATION_FAILED",
                "Identificador no es UUIDv7",
                "path_id_invalid",
            ),
            headers,
        )
        .into())
    }
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<&str, Failure> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let valid = !key.is_empty()
        && key.len() <= IDEMPOTENCY_KEY_MAX
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'));
    if valid {
        Ok(key)
    } else {
        Err(correlated(
            ApiError::new(
                "VALIDATION_FAILED",
                "Idempotency-Key obligatoria (1..240)",
                "idempotency_key_invalid",
            ),
            headers,
        )
        .into())
    }
}

pub(crate) fn if_match(headers: &HeaderMap) -> Result<u64, Failure> {
    let Some(raw) = headers.get("if-match").and_then(|v| v.to_str().ok()) else {
        let error = ApiError::new(
            "PRECONDITION_REQUIRED",
            "If-Match obligatorio",
            "if_match_missing",
        );
        return Err(correlated(error, headers).into());
    };
    raw.trim_matches('"').parse::<u64>().map_err(|_| {
        correlated(
            ApiError::new(
                "VALIDATION_FAILED",
                "If-Match debe ser la row_version",
                "if_match_invalid",
            ),
            headers,
        )
        .into()
    })
}

pub(crate) fn strict<T: DeserializeOwned>(body: &Bytes, headers: &HeaderMap) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|error| {
        let mut failure = ApiError::new(
            "VALIDATION_FAILED",
            "El cuerpo no cumple el schema",
            "schema_violation",
        );
        failure = failure.with_original("larm-api", "SERDE", &error.to_string());
        correlated(failure, headers).into()
    })
}

/// Parámetros de una lista del contrato 07: `cursor`, `limit` y los filtros
/// propios de la ruta; cualquier otro se rechaza.
pub(crate) fn list_params(
    params: &std::collections::BTreeMap<String, String>,
    filters: &[&str],
) -> Result<(Option<String>, Option<u32>), ApiError> {
    if let Some(other) = params
        .keys()
        .find(|k| !matches!(k.as_str(), "cursor" | "limit") && !filters.contains(&k.as_str()))
    {
        return Err(ApiError::new(
            "VALIDATION_FAILED",
            format!("Filtro no admitido: {other}"),
            "filter_not_allowed",
        ));
    }
    let limit = params
        .get("limit")
        .map(|l| l.parse::<u32>())
        .transpose()
        .map_err(|_| {
            ApiError::new(
                "VALIDATION_FAILED",
                "limit no es un entero",
                "limit_invalid",
            )
        })?;
    Ok((params.get("cursor").cloned(), limit))
}
