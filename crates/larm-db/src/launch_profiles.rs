//! CRUD transaccional de launch_profile (P034).

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
    launch_profile::{
        LaunchProfile, LaunchProfileCreate, LaunchProfileKind, LaunchProfileRepository,
    },
    ports::MutationContext,
};
use serde_json::Map;
use sqlx::{Row, SqliteConnection, SqlitePool};

const CREATE_OPERATION: &str = "P034_post__launch_profiles";

#[derive(Clone)]
pub struct SqlLaunchProfiles {
    pub pool: SqlitePool,
}

fn error(code: ErrorCode, message: &str, reason: &'static str) -> LarmError {
    LarmError::new(code, message, reason)
}

fn invalid(reason: &'static str) -> LarmError {
    error(
        ErrorCode::ValidationFailed,
        "Perfil de lanzamiento no válido",
        reason,
    )
}

fn corrupt() -> LarmError {
    error(
        ErrorCode::IntegrityFailed,
        "El perfil persistido no supera la verificación",
        "launch_profile_corrupt",
    )
}

fn stale() -> LarmError {
    error(
        ErrorCode::StaleRevision,
        "El perfil cambió desde que se leyó",
        "stale_revision",
    )
}

fn not_found(id: &str) -> LarmError {
    error(
        ErrorCode::NotFound,
        "Perfil de lanzamiento desconocido",
        "launch_profile_not_found",
    )
    .with_resource(id)
}

fn validate_context(ctx: &MutationContext) -> Result<(), LarmError> {
    if ctx.actor_id.trim().is_empty() || !is_uuid_v7(&ctx.correlation_id) {
        return Err(invalid("launch_profile_actor_invalid"));
    }
    Ok(())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

fn kind_from_str(value: &str) -> Option<LaunchProfileKind> {
    match value {
        "ECO" => Some(LaunchProfileKind::Eco),
        "BALANCED" => Some(LaunchProfileKind::Balanced),
        "PERFORMANCE" => Some(LaunchProfileKind::Performance),
        "LONG_CONTEXT" => Some(LaunchProfileKind::LongContext),
        "CUSTOM" => Some(LaunchProfileKind::Custom),
        _ => None,
    }
}

async fn read_row(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<LaunchProfile>, LarmError> {
    let row = sqlx::query("SELECT id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id FROM launch_profile WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo launch_profile", &e))?;
    row.map(parse_profile_row).transpose()
}

fn parse_profile_row(row: sqlx::sqlite::SqliteRow) -> Result<LaunchProfile, LarmError> {
    let name: String = row.try_get("name").map_err(|_| corrupt())?;
    let profile = LaunchProfile {
        id: row.try_get("id").map_err(|_| corrupt())?,
        created_at: row.try_get("created_at").map_err(|_| corrupt())?,
        updated_at: row.try_get("updated_at").map_err(|_| corrupt())?,
        row_version: row.try_get("row_version").map_err(|_| corrupt())?,
        metadata: serde_json::from_str(
            &row.try_get::<String, _>("metadata")
                .map_err(|_| corrupt())?,
        )
        .map_err(|_| corrupt())?,
        name: kind_from_str(&name).ok_or_else(corrupt)?,
        parameters: serde_json::from_str(
            &row.try_get::<String, _>("parameters")
                .map_err(|_| corrupt())?,
        )
        .map_err(|_| corrupt())?,
        profile_hash: row.try_get("profile_hash").map_err(|_| corrupt())?,
        backend_id: row.try_get("backend_id").map_err(|_| corrupt())?,
    };
    profile.validate_record().map_err(|_| corrupt())?;
    Ok(profile)
}

async fn reserve_writer(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE launch_profile SET id=id WHERE 0")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("reservando escritura launch_profile", &e))?;
    Ok(())
}

async fn append_profile_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_type: &'static str,
    id: &str,
    row_version: i64,
    name: LaunchProfileKind,
    profile_hash: Option<&str>,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    append(
        &mut *tx,
        &NewEvent {
            event_type,
            resource_type: "launch_profile",
            resource_id: id,
            resource_version: row_version,
            data: serde_json::json!({
                "name": name.as_str(),
                "profile_hash": profile_hash,
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

#[async_trait]
impl LaunchProfileRepository for SqlLaunchProfiles {
    async fn list_launch_profiles(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<LaunchProfile>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(invalid("limit_invalid"));
        }
        let rows = if let Some((created_at, id)) = after {
            sqlx::query("SELECT id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id FROM launch_profile WHERE (created_at>? OR (created_at=? AND id>?)) ORDER BY created_at,id LIMIT ?")
                .bind(&created_at)
                .bind(&created_at)
                .bind(&id)
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| db_error("listando launch_profile", &e))?
        } else {
            sqlx::query("SELECT id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id FROM launch_profile ORDER BY created_at,id LIMIT ?")
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| db_error("listando launch_profile", &e))?
        };
        rows.into_iter().map(parse_profile_row).collect()
    }

    async fn launch_profile(&self, id: &str) -> Result<Option<LaunchProfile>, LarmError> {
        if !is_uuid_v7(id) {
            return Err(invalid("path_id_invalid"));
        }
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("abriendo launch_profile", &e))?;
        read_row(&mut conn, id).await
    }

    async fn create_launch_profile(
        &self,
        profile: &LaunchProfileCreate,
        body_hash: &str,
        idempotency_key: &str,
        ctx: &MutationContext,
    ) -> Result<LaunchProfile, LarmError> {
        profile.validate()?;
        if !valid_hash(body_hash) || !valid_key(idempotency_key) {
            return Err(invalid("launch_profile_idempotency_invalid"));
        }
        validate_context(ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("iniciando create launch_profile", &e))?;
        reserve_writer(&mut tx).await?;
        let now = timestamp(chrono::Utc::now());
        let replay: Option<(String, String)> = sqlx::query_as("SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?")
            .bind(&ctx.actor_id)
            .bind(CREATE_OPERATION)
            .bind(idempotency_key)
            .bind(&now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("consultando idempotencia launch_profile", &e))?;
        if let Some((stored_hash, response)) = replay {
            if stored_hash != body_hash {
                return Err(error(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            let profile: LaunchProfile = serde_json::from_str(&response).map_err(|_| corrupt())?;
            profile.validate_record().map_err(|_| corrupt())?;
            tx.commit()
                .await
                .map_err(|e| db_error("confirmando replay launch_profile", &e))?;
            return Ok(profile);
        }

        let id = uuid::Uuid::now_v7().to_string();
        let record = profile
            .clone()
            .into_record(id, now.clone(), 1, Map::new())?;
        let inserted = sqlx::query("INSERT INTO launch_profile(id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(&record.id)
            .bind(&record.created_at)
            .bind(&record.updated_at)
            .bind(record.row_version)
            .bind("{}")
            .bind(record.name.as_str())
            .bind(serde_json::to_string(&record.parameters).map_err(|_| invalid("serialization_invalid"))?)
            .bind(&record.profile_hash)
            .bind(&record.backend_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("insertando launch_profile", &e))?;
        if inserted.rows_affected() != 1 {
            return Err(db_error(
                "insertando launch_profile",
                &"escritura no confirmada",
            ));
        }
        append_profile_event(
            &mut tx,
            "runtime.profile.created",
            &record.id,
            1,
            record.name,
            Some(&record.profile_hash),
            ctx,
        )
        .await?;
        let expiry = timestamp(chrono::Utc::now() + chrono::Duration::days(30));
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?")
            .bind(&ctx.actor_id)
            .bind(CREATE_OPERATION)
            .bind(idempotency_key)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("limpiando idempotencia launch_profile", &e))?;
        let response =
            serde_json::to_string(&record).map_err(|_| invalid("serialization_invalid"))?;
        let saved = sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES(?,?,?,?,201,?,?)")
            .bind(&ctx.actor_id)
            .bind(CREATE_OPERATION)
            .bind(idempotency_key)
            .bind(body_hash)
            .bind(response)
            .bind(expiry)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("guardando idempotencia launch_profile", &e))?;
        if saved.rows_affected() != 1 {
            return Err(db_error(
                "guardando idempotencia launch_profile",
                &"escritura no confirmada",
            ));
        }
        tx.commit()
            .await
            .map_err(|e| db_error("confirmando create launch_profile", &e))?;
        Ok(record)
    }

    async fn update_launch_profile(
        &self,
        id: &str,
        expected_row_version: i64,
        profile: &LaunchProfileCreate,
        ctx: &MutationContext,
    ) -> Result<LaunchProfile, LarmError> {
        profile.validate()?;
        if !is_uuid_v7(id) || expected_row_version < 1 {
            return Err(invalid("launch_profile_update_invalid"));
        }
        validate_context(ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("iniciando update launch_profile", &e))?;
        reserve_writer(&mut tx).await?;
        let current = read_row(&mut tx, id).await?.ok_or_else(|| not_found(id))?;
        if current.row_version != expected_row_version {
            return Err(stale().with_resource(id));
        }
        let next_version = expected_row_version.checked_add(1).ok_or_else(corrupt)?;
        let mut updated = profile.clone().into_record(
            id.to_string(),
            timestamp(chrono::Utc::now()),
            next_version,
            current.metadata.clone(),
        )?;
        updated.created_at.clone_from(&current.created_at);
        updated.validate_record()?;
        let result = sqlx::query("UPDATE launch_profile SET updated_at=?,row_version=?,name=?,parameters=?,profile_hash=?,backend_id=? WHERE id=? AND row_version=?")
            .bind(&updated.updated_at)
            .bind(updated.row_version)
            .bind(updated.name.as_str())
            .bind(serde_json::to_string(&updated.parameters).map_err(|_| invalid("serialization_invalid"))?)
            .bind(&updated.profile_hash)
            .bind(&updated.backend_id)
            .bind(&updated.id)
            .bind(expected_row_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("actualizando launch_profile", &e))?;
        if result.rows_affected() != 1 {
            return Err(stale().with_resource(id));
        }
        append_profile_event(
            &mut tx,
            "runtime.profile.updated",
            id,
            next_version,
            updated.name,
            Some(&updated.profile_hash),
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("confirmando update launch_profile", &e))?;
        Ok(updated)
    }

    async fn delete_launch_profile(
        &self,
        id: &str,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        if !is_uuid_v7(id) || expected_row_version < 1 {
            return Err(invalid("launch_profile_delete_invalid"));
        }
        validate_context(ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("iniciando delete launch_profile", &e))?;
        reserve_writer(&mut tx).await?;
        let current = read_row(&mut tx, id).await?.ok_or_else(|| not_found(id))?;
        if current.row_version != expected_row_version {
            return Err(stale().with_resource(id));
        }
        let references: i64 =
            sqlx::query_scalar("SELECT count(*) FROM runtime_instance WHERE launch_profile_id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| db_error("comprobando uso de launch_profile", &e))?;
        if references > 0 {
            return Err(error(
                ErrorCode::Conflict,
                "El perfil se usa en instancias de runtime",
                "launch_profile_in_use",
            )
            .with_resource(id));
        }
        let deleted = sqlx::query("DELETE FROM launch_profile WHERE id=? AND row_version=?")
            .bind(id)
            .bind(expected_row_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando launch_profile", &e))?;
        if deleted.rows_affected() != 1 {
            return Err(stale().with_resource(id));
        }
        let deleted_version = expected_row_version.checked_add(1).ok_or_else(corrupt)?;
        append_profile_event(
            &mut tx,
            "runtime.profile.deleted",
            id,
            deleted_version,
            current.name,
            None,
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("confirmando delete launch_profile", &e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::panic)]

    use super::*;
    use larm_domain::launch::LaunchParameters;

    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000001";

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "0192aaaa-0000-7000-8000-000000000010".into(),
            correlation_id: "0192aaaa-0000-7000-8000-000000000011".into(),
        }
    }

    fn request(batch: u32) -> LaunchProfileCreate {
        LaunchProfileCreate {
            name: LaunchProfileKind::Balanced,
            parameters: LaunchParameters::builtin(BACKEND, 4, Some(4096), batch),
            backend_id: None,
        }
    }

    fn expect_error<T>(result: Result<T, LarmError>) -> LarmError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("se esperaba un error"),
        }
    }

    #[tokio::test]
    async fn create_replay_update_y_delete_persisten_hash_cas_y_outbox() {
        let (_directory, db) = crate::testing::temp_migrated().await.unwrap();
        let repo = SqlLaunchProfiles {
            pool: db.pool.clone(),
        };
        let first = repo
            .create_launch_profile(&request(512), &"a".repeat(64), "profile:create", &context())
            .await
            .unwrap();
        assert_eq!(first.row_version, 1);
        assert_eq!(first.profile_hash, request(512).profile_hash().unwrap());
        let replay = repo
            .create_launch_profile(&request(512), &"a".repeat(64), "profile:create", &context())
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            expect_error(
                repo.create_launch_profile(
                    &request(256),
                    &"b".repeat(64),
                    "profile:create",
                    &context()
                )
                .await
            )
            .code,
            ErrorCode::IdempotencyConflict
        );

        let updated = repo
            .update_launch_profile(&first.id, 1, &request(256), &context())
            .await
            .unwrap();
        assert_eq!(updated.row_version, 2);
        assert_ne!(updated.profile_hash, first.profile_hash);
        assert_eq!(updated.created_at, first.created_at);
        assert_eq!(
            expect_error(
                repo.update_launch_profile(&first.id, 1, &request(512), &context())
                    .await
            )
            .code,
            ErrorCode::StaleRevision
        );
        let rows = repo.list_launch_profiles(None, 2).await.unwrap();
        assert_eq!(rows, vec![updated.clone()]);
        assert_eq!(
            repo.launch_profile(&first.id).await.unwrap(),
            Some(updated.clone())
        );

        repo.delete_launch_profile(&first.id, 2, &context())
            .await
            .unwrap();
        assert_eq!(repo.launch_profile(&first.id).await.unwrap(), None);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=?")
            .bind(&first.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(events, 3);
    }

    #[tokio::test]
    async fn fallo_outbox_revierte_creacion_y_clave_idempotente() {
        let (_directory, db) = crate::testing::temp_migrated().await.unwrap();
        let repo = SqlLaunchProfiles {
            pool: db.pool.clone(),
        };
        sqlx::query("CREATE TRIGGER fail_launch_profile_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fault injected'); END")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(repo
            .create_launch_profile(&request(512), &"a".repeat(64), "profile:create", &context())
            .await
            .is_err());
        let profiles: i64 = sqlx::query_scalar("SELECT count(*) FROM launch_profile")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency WHERE operation=?")
            .bind(CREATE_OPERATION)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!((profiles, keys), (0, 0));
    }

    #[tokio::test]
    async fn dos_perfiles_guardados_con_parametro_distinto_recargan_hashes_distintos() {
        let (_directory, db) = crate::testing::temp_migrated().await.unwrap();
        let repo = SqlLaunchProfiles {
            pool: db.pool.clone(),
        };
        let first = repo
            .create_launch_profile(&request(512), &"a".repeat(64), "profile:one", &context())
            .await
            .unwrap();
        let second = repo
            .create_launch_profile(&request(256), &"b".repeat(64), "profile:two", &context())
            .await
            .unwrap();
        let first_loaded = repo.launch_profile(&first.id).await.unwrap().unwrap();
        let second_loaded = repo.launch_profile(&second.id).await.unwrap().unwrap();
        assert_eq!(first_loaded.parameters.batch, 512);
        assert_eq!(second_loaded.parameters.batch, 256);
        assert_ne!(first_loaded.profile_hash, second_loaded.profile_hash);
        assert_eq!(first_loaded.profile_hash, first.profile_hash);
        assert_eq!(second_loaded.profile_hash, second.profile_hash);
    }

    #[tokio::test]
    async fn lectura_rechaza_hash_persistido_alterado() {
        let (_directory, db) = crate::testing::temp_migrated().await.unwrap();
        let repo = SqlLaunchProfiles {
            pool: db.pool.clone(),
        };
        let row = repo
            .create_launch_profile(&request(512), &"a".repeat(64), "profile:create", &context())
            .await
            .unwrap();
        sqlx::query("UPDATE launch_profile SET profile_hash=? WHERE id=?")
            .bind("0".repeat(64))
            .bind(&row.id)
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            expect_error(repo.launch_profile(&row.id).await).code,
            ErrorCode::IntegrityFailed
        );
    }
}
