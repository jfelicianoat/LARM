//! Administración local de principales (P009). Se ejecuta en la máquina del
//! daemon con la identidad del operador: el token solo sale por stdout de
//! este comando (o entra por stdin), nunca por argumentos ni logs.

use std::path::Path;
use std::sync::Arc;

use larm_core::config::Settings;
use larm_core::security::{principal_from_reference, Principals};
use larm_db::pool::Db;
use larm_db::security::SqlCredentials;
use larm_domain::auth::{ApiScope, McpPermission, PrincipalKind};
use larm_domain::credential::PRINCIPAL_PREFIX;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{CredentialRepository, MutationContext};

use crate::bridge::KeyringSecrets;

/// Base de la raíz de datos, abierta y migrada.
///
/// # Errors
/// Errores de apertura o migración.
pub async fn open_db(settings: &Settings, data_root: &Path) -> Result<Db, LarmError> {
    let db = Db::open(&data_root.join(&settings.paths.database_relative_path)).await?;
    larm_db::migrate::migrate(&db.pool).await?;
    Ok(db)
}

/// Registro de principales sobre la base y Credential Manager.
#[must_use]
pub fn principals(db: &Db) -> Arc<Principals> {
    Arc::new(Principals::new(
        Arc::new(SqlCredentials {
            pool: db.pool.clone(),
        }),
        Arc::new(KeyringSecrets),
    ))
}

fn operator_ctx() -> MutationContext {
    MutationContext {
        actor_id: "local-operator".into(),
        correlation_id: larm_domain::correlation::CorrelationId::new().to_string(),
    }
}

fn parse_list<T>(raw: &str, parse: fn(&str) -> Option<T>, what: &str) -> Result<Vec<T>, LarmError> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            parse(s).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    format!("{what} desconocido: {s}"),
                    "unknown_scope",
                )
            })
        })
        .collect()
}

/// Alta de un principal. Devuelve el JSON a imprimir (con el token una sola vez).
///
/// # Errors
/// Validación de tipo y scopes, nombre repetido o fallo de Credential Manager.
pub async fn enroll(
    db: &Db,
    name: &str,
    kind: &str,
    scopes: &str,
    mcp_scopes: &str,
    token: Option<String>,
) -> Result<serde_json::Value, LarmError> {
    let kind = PrincipalKind::parse(&kind.to_ascii_uppercase()).ok_or_else(|| {
        LarmError::new(
            ErrorCode::ValidationFailed,
            "Tipo de principal desconocido",
            "kind_invalid",
        )
    })?;
    let api = parse_list(scopes, ApiScope::parse, "scope")?;
    let mcp = parse_list(mcp_scopes, McpPermission::parse, "permiso MCP")?;
    let given = token.is_some();
    let (principal, token) = principals(db)
        .enroll(kind, name, api, mcp, token, &operator_ctx())
        .await?;
    Ok(serde_json::json!({
        "principal": principal,
        "token": if given { serde_json::Value::Null } else { token.into() },
        "token_source": if given { "stdin" } else { "generated" },
    }))
}

/// Rotación: emite un token nuevo y el anterior deja de valer.
///
/// # Errors
/// `NOT_FOUND` si no existe; fallo de Credential Manager.
pub async fn rotate(db: &Db, name: &str) -> Result<serde_json::Value, LarmError> {
    let registry = principals(db);
    let principal = registry
        .find(name)
        .await?
        .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "Principal desconocido", "not_found"))?;
    let token = registry.rotate(&principal.id, &operator_ctx()).await?;
    Ok(serde_json::json!({"principal": principal, "token": token}))
}

/// Revoca un principal (token y referencia).
///
/// # Errors
/// `NOT_FOUND`; fallo de Credential Manager.
pub async fn revoke(db: &Db, name: &str) -> Result<serde_json::Value, LarmError> {
    let registry = principals(db);
    let principal = registry
        .find(name)
        .await?
        .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "Principal desconocido", "not_found"))?;
    registry.revoke(&principal.id, &operator_ctx()).await?;
    Ok(serde_json::json!({"revoked": principal.name, "id": principal.id}))
}

/// Principales registrados, sin tokens.
///
/// # Errors
/// Errores de la base.
pub async fn list(db: &Db) -> Result<serde_json::Value, LarmError> {
    let repo = SqlCredentials {
        pool: db.pool.clone(),
    };
    let mut items = Vec::new();
    for reference in repo.by_service_prefix(PRINCIPAL_PREFIX).await? {
        items.push(match principal_from_reference(&reference) {
            Ok(p) => serde_json::json!({"principal": p, "row_version": reference.row_version, "updated_at": reference.updated_at}),
            Err(e) => serde_json::json!({"id": reference.id, "error": e.details.reason}),
        });
    }
    Ok(serde_json::json!({ "principals": items }))
}
