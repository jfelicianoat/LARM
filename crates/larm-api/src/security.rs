//! Autenticación, autorización y rutas de seguridad (P009).
//!
//! [`guard`] es el middleware que P005 instala delante de todas las rutas:
//! resuelve la operación del contrato por método y ruta, exige Bearer salvo en
//! las públicas, autentica, aplica [`authorize`] (scopes de API y permisos MCP)
//! y solo entonces deja que el handler lea el cuerpo. La identidad y la
//! audiencia salen de la credencial; ninguna cabecera del cliente las cambia.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use larm_core::security::{Approvals, CommandReceipt, Credentials, CursorSigner};
use larm_domain::approval::ApprovalRequest;
use larm_domain::auth::{authorize, Catalog, OperationScope, Principal};
use larm_domain::bridge::{is_uuid_v7, timestamp, ApiError};
use larm_domain::correlation::{CorrelationId, CORRELATION_HEADER};
use larm_domain::credential::CredentialWrite;
use larm_domain::ports::{
    IdempotencyStore, MutationContext, PrincipalAuthenticator, StoredResponse,
};
use sha2::{Digest, Sha256};

use crate::http::{
    bearer, idempotency_key, if_match, json, strict, Answer, Failure, REQUEST_BODY_BYTES,
};

/// Días que se conserva una respuesta idempotente (contrato 07).
pub const IDEMPOTENCY_RETENTION_DAYS: i64 = 30;

/// Dependencias de las rutas de seguridad.
#[derive(Clone)]
pub struct SecurityState {
    pub auth: Arc<dyn PrincipalAuthenticator>,
    pub credentials: Arc<Credentials>,
    pub approvals: Arc<Approvals>,
    pub idempotency: Arc<dyn IdempotencyStore>,
    pub cursors: Arc<CursorSigner>,
}

/// Contexto autenticado que el guard deja en la petición.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub principal: Principal,
    pub operation_id: String,
    pub correlation_id: String,
}

impl AuthContext {
    #[must_use]
    pub fn mutation(&self) -> MutationContext {
        MutationContext {
            actor_id: self.principal.id.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }
}

fn correlation(headers: &HeaderMap) -> String {
    headers
        .get(CORRELATION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| CorrelationId::parse(v).ok())
        .unwrap_or_default()
        .to_string()
}

/// Middleware de autenticación y autorización.
pub async fn guard(
    State(auth): State<Arc<dyn PrincipalAuthenticator>>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let correlation_id = correlation(&headers);
    let fail = |error: ApiError| {
        let mut error = error;
        error.correlation_id.clone_from(&correlation_id);
        Failure::from(error).into_response()
    };
    let method = if request.method() == axum::http::Method::HEAD {
        "GET"
    } else {
        request.method().as_str()
    };
    let Some(operation) = Catalog::contract().find(method, request.uri().path()) else {
        let mut methods: Vec<&str> = Catalog::contract()
            .operations
            .iter()
            .filter(|op| op.matches(&op.method, request.uri().path()))
            .map(|op| op.method.as_str())
            .collect();
        if methods.contains(&"GET") {
            methods.push("HEAD");
        }
        methods.sort_unstable();
        methods.dedup();
        if !methods.is_empty() {
            let mut response = fail(ApiError::new(
                "VALIDATION_FAILED",
                "Método HTTP no permitido para esta ruta",
                "method_not_allowed",
            ));
            *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            if let Ok(value) = axum::http::HeaderValue::from_str(&methods.join(", ")) {
                response
                    .headers_mut()
                    .insert(axum::http::header::ALLOW, value);
            }
            return response;
        }
        return fail(ApiError::new(
            "NOT_FOUND",
            "Operación desconocida",
            "operation_unknown",
        ));
    };
    if operation.scope == OperationScope::Public {
        return next.run(request).await;
    }
    let Some(token) = bearer(&headers) else {
        return fail(ApiError::new(
            "AUTH_REQUIRED",
            "Falta el Bearer",
            "bearer_missing",
        ));
    };
    let principal = match auth.authenticate(token).await {
        Ok(Some(principal)) => principal,
        Ok(None) => {
            return fail(ApiError::new(
                "AUTH_REQUIRED",
                "Bearer no válido",
                "bearer_invalid",
            ))
        }
        Err(error) => return fail(error),
    };
    if let Err(error) = authorize(&principal, operation) {
        tracing::warn!(principal = %principal.name, operation = %operation.id, reason = %error.details.reason, "acceso denegado");
        return fail(error);
    }
    request.extensions_mut().insert(AuthContext {
        principal,
        operation_id: operation.id.clone(),
        correlation_id,
    });
    next.run(request).await
}

/// Rutas de credenciales y aprobaciones, sin guard: la aplicación (P005)
/// pone el guard delante de todas las rutas administrativas.
pub fn routes(state: SecurityState) -> Router {
    Router::new()
        .route(
            "/api/v1/credentials",
            post(create_credential).get(list_credentials),
        )
        .route("/api/v1/credentials/{id}", get(get_credential))
        .route("/api/v1/credentials/{id}", delete(delete_credential))
        .route("/api/v1/approvals", post(create_approval))
        .with_state(state)
}

/// Las mismas rutas con su propio guard (pruebas y usos aislados).
pub fn router(state: SecurityState) -> Router {
    let auth = state.auth.clone();
    routes(state)
        .layer(axum::middleware::from_fn_with_state(auth, guard))
        .layer(DefaultBodyLimit::max(REQUEST_BODY_BYTES))
}

fn with_correlation(error: ApiError, ctx: &AuthContext) -> Failure {
    let mut error = error;
    error.correlation_id.clone_from(&ctx.correlation_id);
    Failure::from(error)
}

fn id_param(id: &str, ctx: &AuthContext) -> Result<(), Failure> {
    if is_uuid_v7(id) {
        Ok(())
    } else {
        Err(with_correlation(
            ApiError::new(
                "VALIDATION_FAILED",
                "Identificador no es UUIDv7",
                "path_id_invalid",
            ),
            ctx,
        ))
    }
}

/// Repite la respuesta guardada o, si no hay, ejecuta y la guarda (contrato 07).
/// Ejecuta `run` una sola vez por (principal, operación, Idempotency-Key):
/// la misma clave con el mismo cuerpo repite la respuesta guardada y con otro
/// cuerpo es `IDEMPOTENCY_CONFLICT` (contrato 07).
pub(crate) async fn idempotent<F, Fut>(
    store: &dyn IdempotencyStore,
    ctx: &AuthContext,
    headers: &HeaderMap,
    body: &Bytes,
    run: F,
) -> Answer
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(u16, serde_json::Value), ApiError>>,
{
    let key = idempotency_key(headers)?;
    let body_hash = hex::encode(Sha256::digest(body));
    let fail = |e: ApiError| with_correlation(e, ctx);
    if let Some(stored) = store
        .lookup(&ctx.principal.id, &ctx.operation_id, key)
        .await
        .map_err(fail)?
    {
        if stored.body_hash != body_hash {
            return Err(fail(ApiError::new(
                "IDEMPOTENCY_CONFLICT",
                "Idempotency-Key ya usada con otro cuerpo",
                "idempotency_body_mismatch",
            )));
        }
        let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
        return Ok(json(status, &stored.response));
    }
    let (status, response) = run().await.map_err(fail)?;
    let expires =
        timestamp(chrono::Utc::now() + chrono::Duration::days(IDEMPOTENCY_RETENTION_DAYS));
    let stored = StoredResponse {
        body_hash,
        status,
        response: response.clone(),
    };
    if let Err(e) = store
        .save(&ctx.principal.id, &ctx.operation_id, key, &stored, &expires)
        .await
    {
        tracing::error!(detail = %e.technical_detail(), "no se guardó la respuesta idempotente");
    }
    Ok(json(
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        &response,
    ))
}

async fn create_credential(
    State(state): State<SecurityState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let write: CredentialWrite =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let credentials = state.credentials.clone();
    let mutation = ctx.mutation();
    idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let reference = credentials.create(write, &mutation).await?;
            Ok((201, serde_json::to_value(reference).unwrap_or_default()))
        },
    )
    .await
}

async fn list_credentials(
    State(state): State<SecurityState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(&params, &[]).map_err(|e| with_correlation(e, &ctx))?;
    let page = state
        .cursors
        .page_credentials(&state.credentials, cursor.as_deref(), limit)
        .await
        .map_err(|e| with_correlation(e, &ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn get_credential(
    State(state): State<SecurityState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let reference = state
        .credentials
        .get(&id)
        .await
        .map_err(|e| with_correlation(e, &ctx))?;
    Ok(json(StatusCode::OK, &reference))
}

async fn delete_credential(
    State(state): State<SecurityState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    id_param(&id, &ctx)?;
    let version = if_match(&headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let version = i64::try_from(version).map_err(|_| {
        with_correlation(
            ApiError::new(
                "VALIDATION_FAILED",
                "If-Match fuera de rango",
                "if_match_invalid",
            ),
            &ctx,
        )
    })?;
    state
        .credentials
        .delete(&id, version, &ctx.mutation())
        .await
        .map_err(|e| with_correlation(e, &ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn create_approval(
    State(state): State<SecurityState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ApprovalRequest =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let approvals = state.approvals.clone();
    let mutation = ctx.mutation();
    idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let receipt: CommandReceipt = approvals.grant(&request, &mutation).await?;
            Ok((201, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}
