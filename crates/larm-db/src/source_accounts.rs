//! Persistencia P045 de cuentas de fuente, referencias y licencias aceptadas.

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::bridge::{is_uuid_v7, timestamp};
use larm_domain::credential::{CredentialReference, SecretType, BACKEND};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, SourceAccountRepository};
use larm_domain::source_accounts::{account_invalid, CheckSourceLicense, ModelSourceAccount};
use sqlx::{Row, SqliteConnection, SqlitePool};

const COLUMNS: &str = "id,created_at,updated_at,row_version,metadata,source_id,credential_reference_id,display_name,accepted_license_hashes";
const CREDENTIAL_COLUMNS: &str =
    "id,created_at,updated_at,row_version,metadata,service,username,backend,secret_type,scopes,expires_at";
const LICENSE_OPERATION: &str = "P045_post__sources_id_license";

#[derive(Clone)]
pub struct SqlSourceAccounts {
    pub pool: SqlitePool,
}

fn not_found(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::NotFound,
        "Cuenta o fuente desconocida",
        "not_found",
    )
    .with_resource(id)
}

fn row_account(row: &sqlx::sqlite::SqliteRow) -> Result<ModelSourceAccount, LarmError> {
    let text = |name: &str| -> Result<String, LarmError> {
        row.try_get(name).map_err(|e| db_error(name, &e))
    };
    let account: ModelSourceAccount = serde_json::from_value(serde_json::json!({
        "id": text("id")?,
        "created_at": text("created_at")?,
        "updated_at": text("updated_at")?,
        "row_version": row.try_get::<i64, _>("row_version").map_err(|e| db_error("row_version", &e))?,
        "metadata": serde_json::from_str::<serde_json::Value>(&text("metadata")?).map_err(|e| db_error("metadata", &e))?,
        "source_id": text("source_id")?,
        "credential_reference_id": row.try_get::<Option<String>, _>("credential_reference_id").map_err(|e| db_error("credential_reference_id", &e))?,
        "display_name": text("display_name")?,
        "accepted_license_hashes": serde_json::from_str::<serde_json::Value>(&text("accepted_license_hashes")?).map_err(|e| db_error("accepted_license_hashes", &e))?
    }))
    .map_err(|e| db_error("model_source_account", &e))?;
    account.validate()?;
    Ok(account)
}

async fn account(conn: &mut SqliteConnection, id: &str) -> Result<ModelSourceAccount, LarmError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM model_source_account WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo cuenta de fuente", &e))?
    .ok_or_else(|| not_found(id))?;
    row_account(&row)
}

async fn write_lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE model_source_account SET row_version=row_version WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando cuentas de fuente", &e))?;
    Ok(())
}

#[async_trait]
impl SourceAccountRepository for SqlSourceAccounts {
    async fn create_source_account(
        &self,
        record: &ModelSourceAccount,
        ctx: &MutationContext,
    ) -> Result<ModelSourceAccount, LarmError> {
        record.validate()?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let source_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_source WHERE id=?)")
                .bind(&record.source_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| db_error("comprobando fuente", &e))?;
        if !source_exists {
            return Err(not_found(&record.source_id));
        }
        if let Some(credential_id) = &record.credential_reference_id {
            if !is_uuid_v7(credential_id) {
                return Err(account_invalid("credential_reference_id"));
            }
            let service: Option<String> =
                sqlx::query_scalar("SELECT service FROM credential_reference WHERE id=?")
                    .bind(credential_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| db_error("resolviendo referencia de credencial", &e))?;
            let Some(service) = service else {
                return Err(not_found(credential_id));
            };
            if service != format!("larm/source/{}", record.source_id) {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "La credencial no pertenece a esta fuente",
                    "source_credential_scope_mismatch",
                )
                .with_resource(credential_id));
            }
        }

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO model_source_account({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&record.id)
        .bind(&record.created_at)
        .bind(&record.updated_at)
        .bind(record.row_version)
        .bind(serde_json::Value::Object(record.metadata.clone()).to_string())
        .bind(&record.source_id)
        .bind(&record.credential_reference_id)
        .bind(&record.display_name)
        .bind(
            serde_json::to_string(&record.accepted_license_hashes)
                .map_err(|e| db_error("serializando licencias", &e))?,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("creando cuenta de fuente", &e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "source_account.created",
                resource_type: "model_source_account",
                resource_id: &record.id,
                resource_version: record.row_version,
                data: serde_json::json!({
                    "source_id": record.source_id,
                    "credential_reference_id": record.credential_reference_id,
                    "accepted_license_hashes": record.accepted_license_hashes
                }),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record.clone())
    }

    async fn source_account(&self, id: &str) -> Result<Option<ModelSourceAccount>, LarmError> {
        if !is_uuid_v7(id) {
            return Err(account_invalid("id"));
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM model_source_account WHERE id=?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo cuenta de fuente", &e))?
        .as_ref()
        .map(row_account)
        .transpose()
    }

    async fn page_source_accounts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<ModelSourceAccount>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(account_invalid("limit"));
        }
        let (created_at, id) =
            after.map_or((None, None), |(created, id)| (Some(created), Some(id)));
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM model_source_account WHERE (? IS NULL OR (created_at,id)>(?,?)) ORDER BY created_at,id LIMIT ?"
        )))
        .bind(&created_at)
        .bind(&created_at)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando cuentas de fuente", &e))?;
        rows.iter().map(row_account).collect()
    }

    async fn set_basic_credential(
        &self,
        account_id: &str,
        credential: &CredentialReference,
        ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        if !is_uuid_v7(account_id)
            || !is_uuid_v7(&credential.id)
            || credential.username != account_id
            || credential.backend != BACKEND
            || credential.secret_type != SecretType::Password
            || !credential.scopes.is_empty()
            || credential.expires_at.is_some()
        {
            return Err(account_invalid("basic_credential"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = account(&mut tx, account_id).await?;
        let expected_service = format!("larm/source/{}", record.source_id);
        if credential.service != expected_service {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La credencial no pertenece a esta fuente",
                "source_credential_scope_mismatch",
            )
            .with_resource(&credential.id));
        }
        let selected_basic_source: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM model_source WHERE id=? AND auth_mode='BASIC' AND account_id=?)",
        )
        .bind(&record.source_id)
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("validando la fuente Basic seleccionada", &e))?;
        if !selected_basic_source {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La cuenta debe estar seleccionada en una fuente Basic",
                "source_account_mismatch",
            )
            .with_resource(account_id));
        }

        let now = timestamp(chrono::Utc::now());
        let saved = if let Some(current_id) = &record.credential_reference_id {
            if current_id != &credential.id {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La cuenta ya tiene otra referencia de credencial",
                    "source_credential_reference_changed",
                )
                .with_resource(account_id));
            }
            let current: Option<(i64, String, String)> = sqlx::query_as(
                "SELECT row_version,service,username FROM credential_reference WHERE id=?",
            )
            .bind(current_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("validando la referencia Basic", &e))?;
            let Some((current_version, service, username)) = current else {
                return Err(LarmError::new(
                    ErrorCode::AuthBackendUnavailable,
                    "La referencia de credencial de la cuenta no está disponible",
                    "source_credential_reference_missing",
                )
                .with_resource(current_id));
            };
            if service != expected_service || username != account_id {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "La credencial vinculada no pertenece a esta cuenta",
                    "source_credential_scope_mismatch",
                )
                .with_resource(current_id));
            }
            let next_version = current_version.checked_add(1).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::Conflict,
                    "La referencia de credencial alcanzó su límite de revisiones",
                    "credential_version_exhausted",
                )
            })?;
            if credential.row_version != next_version {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La credencial cambió durante la escritura",
                    "credential_version_mismatch",
                )
                .with_resource(current_id));
            }
            sqlx::query("UPDATE credential_reference SET updated_at=?,row_version=?,secret_type='PASSWORD',scopes='[]',expires_at=NULL WHERE id=? AND row_version=?")
                .bind(&now)
                .bind(next_version)
                .bind(current_id)
                .bind(current_version)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("rotando credencial Basic", &e))?;
            let mut saved = credential.clone();
            saved.updated_at = now.clone();
            saved.row_version = next_version;
            append(
                &mut tx,
                &NewEvent {
                    event_type: "credential.rotated",
                    resource_type: "credential_reference",
                    resource_id: current_id,
                    resource_version: next_version,
                    data: serde_json::json!({"service": expected_service, "secret_type": "PASSWORD"}),
                },
                ctx,
            )
            .await?;
            saved
        } else {
            if credential.row_version != 1 {
                return Err(account_invalid("basic_credential_version"));
            }
            let duplicate: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM credential_reference WHERE service=? AND username=?)",
            )
            .bind(&expected_service)
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_error("comprobando referencia Basic duplicada", &e))?;
            if duplicate {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya existe una credencial sin vincular para esta cuenta",
                    "credential_exists",
                )
                .with_resource(account_id));
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO credential_reference({CREDENTIAL_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?)"
            )))
            .bind(&credential.id)
            .bind(&credential.created_at)
            .bind(&now)
            .bind(1_i64)
            .bind(serde_json::Value::Object(credential.metadata.clone()).to_string())
            .bind(&credential.service)
            .bind(&credential.username)
            .bind(&credential.backend)
            .bind(SecretType::Password.as_str())
            .bind("[]")
            .bind(Option::<String>::None)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("guardando referencia Basic", &e))?;
            append(
                &mut tx,
                &NewEvent {
                    event_type: "credential.created",
                    resource_type: "credential_reference",
                    resource_id: &credential.id,
                    resource_version: 1,
                    data: serde_json::json!({"service": expected_service, "secret_type": "PASSWORD"}),
                },
                ctx,
            )
            .await?;

            let next_account_version = record.row_version.checked_add(1).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::Conflict,
                    "La cuenta alcanzó su límite de revisiones",
                    "source_account_version_exhausted",
                )
            })?;
            sqlx::query("UPDATE model_source_account SET updated_at=?,row_version=?,credential_reference_id=? WHERE id=? AND row_version=?")
                .bind(&now)
                .bind(next_account_version)
                .bind(&credential.id)
                .bind(account_id)
                .bind(record.row_version)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("asociando credencial Basic", &e))?;
            record.credential_reference_id = Some(credential.id.clone());
            record.row_version = next_account_version;
            record.updated_at = now.clone();
            append(
                &mut tx,
                &NewEvent {
                    event_type: "source_account.credential_updated",
                    resource_type: "model_source_account",
                    resource_id: account_id,
                    resource_version: next_account_version,
                    data: serde_json::json!({
                        "source_id": record.source_id,
                        "credential_reference_id": credential.id,
                    }),
                },
                ctx,
            )
            .await?;
            let mut saved = credential.clone();
            saved.updated_at = now.clone();
            saved.row_version = 1;
            saved
        };

        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(saved)
    }

    async fn set_source_license(
        &self,
        source_id: &str,
        request: &CheckSourceLicense,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<larm_domain::source_accounts::SourceLicenseReceipt, LarmError> {
        if !is_uuid_v7(source_id) {
            return Err(account_invalid("source_id"));
        }
        request.validate()?;
        if idempotency_key.is_empty()
            || idempotency_key.len() > 240
            || !idempotency_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
            || !larm_domain::adapter_updates::is_sha256(body_hash)
        {
            return Err(account_invalid("idempotency"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let now_text = timestamp(now);
        let replay: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(LICENSE_OPERATION)
        .bind(idempotency_key)
        .bind(&now_text)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo replay de licencia", &e))?;
        if let Some((stored_hash, response)) = replay {
            if stored_hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otra petición",
                    "idempotency_body_mismatch",
                ));
            }
            let receipt: larm_domain::source_accounts::SourceLicenseReceipt =
                serde_json::from_str(&response).map_err(|e| db_error("replay de licencia", &e))?;
            if receipt.resource_id != request.source_account_id {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada para otro recurso",
                    "idempotency_resource_mismatch",
                ));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay", &e))?;
            return Ok(receipt);
        }
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id)
            .bind(LICENSE_OPERATION)
            .bind(idempotency_key)
            .bind(&now_text)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando replay caducado", &e))?;
        let mut record = account(&mut tx, &request.source_account_id).await?;
        if record.source_id != source_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La cuenta no pertenece a esta fuente",
                "source_account_mismatch",
            )
            .with_resource(&record.id));
        }
        if request.accepted {
            if !record
                .accepted_license_hashes
                .iter()
                .any(|hash| hash == &request.license_hash)
            {
                record
                    .accepted_license_hashes
                    .push(request.license_hash.clone());
            }
        } else {
            record
                .accepted_license_hashes
                .retain(|hash| hash != &request.license_hash);
        }
        let next_version = record.row_version.checked_add(1).ok_or_else(|| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo actualizar la cuenta",
                "source_account_version_exhausted",
            )
        })?;
        let updated_at = timestamp(chrono::Utc::now());
        let update = sqlx::query(
            "UPDATE model_source_account SET updated_at=?,row_version=?,accepted_license_hashes=? WHERE id=? AND row_version=?",
        )
        .bind(&updated_at)
        .bind(next_version)
        .bind(
            serde_json::to_string(&record.accepted_license_hashes)
                .map_err(|e| db_error("serializando licencias", &e))?,
        )
        .bind(&record.id)
        .bind(record.row_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando licencias aceptadas", &e))?;
        if update.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La cuenta cambió durante la operación",
                "stale_revision",
            )
            .with_resource(&record.id));
        }
        let event_id = append(
            &mut tx,
            &NewEvent {
                event_type: "source.license_checked",
                resource_type: "model_source_account",
                resource_id: &record.id,
                resource_version: next_version,
                data: serde_json::json!({
                    "source_id": source_id,
                    "source_account_id": record.id,
                    "license_hash": request.license_hash,
                    "revision_key": request.revision_key,
                    "accepted": request.accepted
                }),
            },
            ctx,
        )
        .await?;
        let receipt = larm_domain::source_accounts::SourceLicenseReceipt {
            resource_id: record.id,
            row_version: next_version,
            event_id,
        };
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,201,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(LICENSE_OPERATION)
        .bind(idempotency_key)
        .bind(body_hash)
        .bind(serde_json::to_string(&receipt).map_err(|e| db_error("serializando recibo de licencia", &e))?)
        .bind(timestamp(now + chrono::Duration::days(30)))
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando replay de licencia", &e))?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(receipt)
    }

    async fn delete_source_account(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        if !is_uuid_v7(id) {
            return Err(account_invalid("id"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let record = account(&mut tx, id).await?;
        let selected: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_source WHERE account_id=?)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| db_error("comprobando cuenta en uso", &e))?;
        if selected {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Desvincula la cuenta de la fuente antes de borrarla",
                "source_account_in_use",
            )
            .with_resource(id));
        }
        sqlx::query("DELETE FROM model_source_account WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando cuenta de fuente", &e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "source_account.deleted",
                resource_type: "model_source_account",
                resource_id: id,
                resource_version: record.row_version,
                data: serde_json::json!({"source_id": record.source_id}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }
}
