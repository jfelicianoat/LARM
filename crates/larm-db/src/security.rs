//! Repositorios SQLite de seguridad (P009): `credential_reference`,
//! aprobaciones en `operation_journal` e `idempotency`. Ningún secreto llega
//! aquí: solo referencias, hashes y respuestas sin secreto.

use async_trait::async_trait;
use larm_domain::approval::ApprovalRecord;
use larm_domain::credential::{CredentialReference, SecretType};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    ApprovalRepository, CredentialRepository, IdempotencyStore, MutationContext, StoredResponse,
};
use sqlx::{Row, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

const COLUMNS: &str =
    "id,created_at,updated_at,row_version,metadata,service,username,backend,secret_type,scopes,expires_at";

fn secret_type(text: &str) -> Result<SecretType, LarmError> {
    serde_json::from_value(serde_json::Value::String(text.into()))
        .map_err(|e| db_error("secret_type desconocido en la base", &e))
}

fn json_map(text: &str) -> Result<serde_json::Map<String, serde_json::Value>, LarmError> {
    serde_json::from_str(text).map_err(|e| db_error("metadata ilegible", &e))
}

fn to_reference(row: &sqlx::sqlite::SqliteRow) -> Result<CredentialReference, LarmError> {
    let get = |name: &str| -> Result<String, LarmError> {
        row.try_get::<String, _>(name)
            .map_err(|e| db_error(name, &e))
    };
    Ok(CredentialReference {
        id: get("id")?,
        created_at: get("created_at")?,
        updated_at: get("updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: json_map(&get("metadata")?)?,
        service: get("service")?,
        username: get("username")?,
        backend: get("backend")?,
        secret_type: secret_type(&get("secret_type")?)?,
        scopes: serde_json::from_str(&get("scopes")?).map_err(|e| db_error("scopes", &e))?,
        expires_at: row
            .try_get("expires_at")
            .map_err(|e| db_error("expires_at", &e))?,
    })
}

/// `credential_reference` sobre SQLite.
#[derive(Clone)]
pub struct SqlCredentials {
    pub pool: SqlitePool,
}

#[async_trait]
impl CredentialRepository for SqlCredentials {
    async fn insert(
        &self,
        c: &CredentialReference,
        ctx: &MutationContext,
    ) -> Result<String, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let duplicate: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM credential_reference WHERE service=? AND username=?",
        )
        .bind(&c.service)
        .bind(&c.username)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("buscando duplicado", &e))?;
        if duplicate > 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe una credencial con ese servicio y usuario",
                "credential_exists",
            ));
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO credential_reference({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&c.id)
        .bind(&c.created_at)
        .bind(&c.updated_at)
        .bind(c.row_version)
        .bind(serde_json::Value::Object(c.metadata.clone()).to_string())
        .bind(&c.service)
        .bind(&c.username)
        .bind(&c.backend)
        .bind(c.secret_type.as_str())
        .bind(serde_json::to_string(&c.scopes).unwrap_or_else(|_| "[]".into()))
        .bind(&c.expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando credential_reference", &e))?;
        let event_id = append(
            &mut tx,
            &NewEvent {
                event_type: "credential.created",
                resource_type: "credential_reference",
                resource_id: &c.id,
                resource_version: c.row_version,
                data: serde_json::json!({"service": c.service, "secret_type": c.secret_type.as_str()}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(event_id)
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialReference>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM credential_reference WHERE id=?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo credential_reference", &e))?
        .as_ref()
        .map(to_reference)
        .transpose()
    }

    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<CredentialReference>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM credential_reference WHERE (created_at, id) > (?, ?) \
             ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando credential_reference", &e))?;
        rows.iter().map(to_reference).collect()
    }

    async fn by_service_prefix(&self, prefix: &str) -> Result<Vec<CredentialReference>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM credential_reference WHERE substr(service, 1, ?) = ? ORDER BY created_at, id"
        )))
        .bind(i64::try_from(prefix.len()).unwrap_or(i64::MAX))
        .bind(prefix)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("buscando por prefijo", &e))?;
        rows.iter().map(to_reference).collect()
    }

    async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<String, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT row_version FROM credential_reference WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo row_version", &e))?;
        match current {
            None => {
                return Err(LarmError::new(
                    ErrorCode::NotFound,
                    "Credencial desconocida",
                    "not_found",
                )
                .with_resource(id))
            }
            Some(v) if v != if_match => {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La credencial cambió desde que se leyó",
                    "row_version_mismatch",
                )
                .with_resource(id))
            }
            Some(_) => {}
        }
        sqlx::query("DELETE FROM credential_reference WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando credential_reference", &e))?;
        let event_id = append(
            &mut tx,
            &NewEvent {
                event_type: "credential.deleted",
                resource_type: "credential_reference",
                resource_id: id,
                resource_version: if_match + 1,
                data: serde_json::json!({}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(event_id)
    }

    async fn touch(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed = sqlx::query(
            "UPDATE credential_reference SET row_version=row_version+1, updated_at=? WHERE id=?",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando credential_reference", &e))?
        .rows_affected();
        if changed == 0 {
            return Err(
                LarmError::new(ErrorCode::NotFound, "Credencial desconocida", "not_found")
                    .with_resource(id),
            );
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM credential_reference WHERE id=?"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("releyendo credential_reference", &e))?;
        let reference = to_reference(&row)?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "credential.rotated",
                resource_type: "credential_reference",
                resource_id: id,
                resource_version: reference.row_version,
                data: serde_json::json!({"service": reference.service}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(reference)
    }
}

/// Aprobaciones en `operation_journal` (`kind='approval'`, `plan_hash=operation_digest`).
#[derive(Clone)]
pub struct SqlApprovals {
    pub pool: SqlitePool,
}

#[async_trait]
impl ApprovalRepository for SqlApprovals {
    async fn insert(&self, a: &ApprovalRecord, ctx: &MutationContext) -> Result<String, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let payload =
            serde_json::to_string(a).map_err(|e| db_error("serializando aprobación", &e))?;
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES (?, 'approval', ?, 'APPROVED', ?, ?, ?)",
        )
        .bind(&a.id)
        .bind(serde_json::to_string(&a.resource_ids).unwrap_or_else(|_| "[]".into()))
        .bind(&a.operation_digest)
        .bind(payload)
        .bind(&a.created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando aprobación", &e))?;
        let event_id = append(
            &mut tx,
            &NewEvent {
                event_type: "approval.created",
                resource_type: "approval",
                resource_id: &a.id,
                resource_version: 1,
                data: serde_json::json!({"operation_digest": a.operation_digest, "expires_at": a.expires_at}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(event_id)
    }

    async fn get(&self, id: &str) -> Result<Option<ApprovalRecord>, LarmError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT payload, phase FROM operation_journal WHERE id=? AND kind='approval'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo aprobación", &e))?;
        row.map(|(payload, _)| {
            serde_json::from_str(&payload).map_err(|e| db_error("aprobación ilegible", &e))
        })
        .transpose()
    }

    async fn consume(&self, id: &str, at: &str) -> Result<bool, LarmError> {
        // Una sola sentencia condicional: dos usos simultáneos no pueden consumirla ambos.
        let changed = sqlx::query(
            "UPDATE operation_journal SET phase='CONSUMED', updated_at=?, \
             payload=json_set(payload, '$.consumed_at', ?) \
             WHERE id=? AND kind='approval' AND phase='APPROVED'",
        )
        .bind(at)
        .bind(at)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| db_error("consumiendo aprobación", &e))?
        .rows_affected();
        Ok(changed == 1)
    }
}

/// Tabla `idempotency` del contrato.
#[derive(Clone)]
pub struct SqlIdempotency {
    pub pool: SqlitePool,
}

#[async_trait]
impl IdempotencyStore for SqlIdempotency {
    async fn lookup(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
    ) -> Result<Option<StoredResponse>, LarmError> {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let row: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash, status_code, response_json FROM idempotency \
             WHERE principal_id=? AND operation=? AND key=? AND expires_at > ?",
        )
        .bind(principal_id)
        .bind(operation)
        .bind(key)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo idempotency", &e))?;
        row.map(|(body_hash, status, response)| {
            Ok(StoredResponse {
                body_hash,
                status: u16::try_from(status).map_err(|e| db_error("status_code", &e))?,
                response: serde_json::from_str(&response)
                    .map_err(|e| db_error("response_json", &e))?,
            })
        })
        .transpose()
    }

    async fn save(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
        stored: &StoredResponse,
        expires_at: &str,
    ) -> Result<(), LarmError> {
        sqlx::query(
            "INSERT OR REPLACE INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES (?,?,?,?,?,?,?)",
        )
        .bind(principal_id)
        .bind(operation)
        .bind(key)
        .bind(&stored.body_hash)
        .bind(i64::from(stored.status))
        .bind(stored.response.to_string())
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| db_error("guardando idempotency", &e))?;
        Ok(())
    }
}
