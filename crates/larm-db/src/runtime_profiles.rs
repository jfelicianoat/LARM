//! SQLite persistence for immutable, unqualified P099 runtime profile locks.

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    runtime_profiles::{
        RuntimeProfileLock, RuntimeProfileLockRepository, RuntimeProfileQualification,
        RuntimeProfileQualificationRepository,
    },
};
use sqlx::{Row, SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlRuntimeProfileLocks {
    pub pool: SqlitePool,
}

fn conflict() -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "Ya existe otro lock para esta identidad de perfil",
        "runtime_profile_lock_identity_conflict",
    )
}

fn corrupt() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El lock de perfil persistido no supera la verificación",
        "runtime_profile_lock_corrupt",
    )
}

async fn by_hash(
    conn: &mut SqliteConnection,
    lock_hash: &str,
) -> Result<Option<RuntimeProfileLock>, LarmError> {
    let row = sqlx::query("SELECT payload,runtime,source_url,source_revision,source_sha256,upstream_version,os,os_build,backend,protocol,qualification FROM runtime_profile_lock WHERE lock_sha256=?")
        .bind(lock_hash)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo runtime_profile_lock", &e))?;
    let Some(row) = row else { return Ok(None) };
    let raw: String = row.try_get("payload").map_err(|_| corrupt())?;
    let lock: RuntimeProfileLock = serde_json::from_str(&raw).map_err(|_| corrupt())?;
    lock.validate().map_err(|_| corrupt())?;
    let stored: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = (
        row.try_get("runtime").map_err(|_| corrupt())?,
        row.try_get("source_url").map_err(|_| corrupt())?,
        row.try_get("source_revision").map_err(|_| corrupt())?,
        row.try_get("source_sha256").map_err(|_| corrupt())?,
        row.try_get("upstream_version").map_err(|_| corrupt())?,
        row.try_get("os").map_err(|_| corrupt())?,
        row.try_get("os_build").map_err(|_| corrupt())?,
        row.try_get("backend").map_err(|_| corrupt())?,
        row.try_get("protocol").map_err(|_| corrupt())?,
        row.try_get("qualification").map_err(|_| corrupt())?,
    );
    let key = &lock.key;
    if lock.lock_sha256 != lock_hash
        || stored
            != (
                key.runtime.clone(),
                key.source_url.clone(),
                key.source_revision.clone(),
                key.source_sha256.clone(),
                key.upstream_version.clone(),
                key.os.clone(),
                key.os_build.clone(),
                key.backend.clone(),
                key.protocol.clone(),
                lock.qualification.clone(),
            )
    {
        return Err(corrupt());
    }
    Ok(Some(lock))
}

#[async_trait]
impl RuntimeProfileLockRepository for SqlRuntimeProfileLocks {
    async fn acquire(
        &self,
        lock: &RuntimeProfileLock,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError> {
        lock.validate()?;
        if ctx.actor_id.trim().is_empty() || !is_uuid_v7(&ctx.correlation_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Contexto de mutación no válido",
                "runtime_profile_lock_actor_invalid",
            ));
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN runtime_profile_lock", &e))?;
        // Reserve the SQLite writer before checking the immutable identity.
        sqlx::query("UPDATE runtime_profile_lock SET lock_sha256=lock_sha256 WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando runtime_profile_lock", &e))?;

        let existing = sqlx::query("SELECT lock_sha256,payload FROM runtime_profile_lock WHERE runtime=? AND source_url=? AND source_revision=? AND source_sha256=? AND upstream_version=? AND os=? AND os_build=? AND backend=? AND protocol=?")
            .bind(&lock.key.runtime)
            .bind(&lock.key.source_url)
            .bind(&lock.key.source_revision)
            .bind(&lock.key.source_sha256)
            .bind(&lock.key.upstream_version)
            .bind(&lock.key.os)
            .bind(&lock.key.os_build)
            .bind(&lock.key.backend)
            .bind(&lock.key.protocol)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("consultando identidad runtime_profile_lock", &e))?;
        if let Some(row) = existing {
            let existing_hash: String = row.try_get("lock_sha256").map_err(|_| corrupt())?;
            let existing_payload: String = row.try_get("payload").map_err(|_| corrupt())?;
            let parsed: RuntimeProfileLock =
                serde_json::from_str(&existing_payload).map_err(|_| corrupt())?;
            parsed.validate().map_err(|_| corrupt())?;
            if existing_hash != lock.lock_sha256 || parsed != *lock {
                return Err(conflict());
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT runtime_profile_lock replay", &e))?;
            return Ok(false);
        }

        let at = timestamp(chrono::Utc::now());
        let payload = serde_json::to_string(lock)
            .map_err(|e| db_error("serializando runtime_profile_lock", &e))?;
        let inserted = sqlx::query("INSERT INTO runtime_profile_lock(lock_sha256,runtime,source_url,source_revision,source_sha256,upstream_version,os,os_build,backend,protocol,qualification,payload,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&lock.lock_sha256)
            .bind(&lock.key.runtime)
            .bind(&lock.key.source_url)
            .bind(&lock.key.source_revision)
            .bind(&lock.key.source_sha256)
            .bind(&lock.key.upstream_version)
            .bind(&lock.key.os)
            .bind(&lock.key.os_build)
            .bind(&lock.key.backend)
            .bind(&lock.key.protocol)
            .bind(&lock.qualification)
            .bind(&payload)
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("insertando runtime_profile_lock", &e))?;
        if inserted.rows_affected() != 1 {
            return Err(db_error(
                "insertando runtime_profile_lock",
                &"escritura no confirmada",
            ));
        }
        let journal_id = uuid::Uuid::now_v7().to_string();
        let resources = serde_json::json!([lock.lock_sha256]).to_string();
        let journal_payload = serde_json::json!({"lock":lock,"actor_id":ctx.actor_id});
        let journal = sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'runtime.profile_lock', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(resources)
            .bind(&lock.lock_sha256)
            .bind(journal_payload.to_string())
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("registrando runtime.profile_lock en journal", &e))?;
        if journal.rows_affected() != 1 {
            return Err(db_error(
                "registrando runtime.profile_lock en journal",
                &"escritura no confirmada",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.profile_lock.acquired",
                resource_type: "runtime_profile_lock",
                resource_id: &lock.lock_sha256,
                resource_version: 1,
                data: serde_json::json!({
                    "runtime": lock.key.runtime,
                    "upstream_version": lock.key.upstream_version,
                    "qualification": "UNQUALIFIED",
                    "lock_sha256": lock.lock_sha256,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT runtime_profile_lock", &e))?;
        Ok(true)
    }

    async fn get_by_hash(
        &self,
        lock_sha256: &str,
    ) -> Result<Option<RuntimeProfileLock>, LarmError> {
        if lock_sha256.len() != 64
            || !lock_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Hash de lock no válido",
                "runtime_profile_lock_hash_invalid",
            ));
        }
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool runtime_profile_lock", &e))?;
        by_hash(&mut conn, lock_sha256).await
    }
}

#[derive(Clone)]
pub struct SqlRuntimeProfileQualifications {
    pub pool: SqlitePool,
}

async fn qualification_by_lock(
    conn: &mut SqliteConnection,
    lock_sha256: &str,
) -> Result<Option<RuntimeProfileQualification>, LarmError> {
    let row = sqlx::query(
        "SELECT q.payload,q.qualification_sha256,q.evidence_sha256,q.qualified_at,l.lock_sha256 \
         FROM runtime_profile_qualification q JOIN runtime_profile_lock l \
         ON l.lock_sha256=q.lock_sha256 WHERE q.lock_sha256=?",
    )
    .bind(lock_sha256)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("leyendo runtime_profile_qualification", &e))?;
    let Some(row) = row else { return Ok(None) };
    let raw: String = row.try_get("payload").map_err(|_| corrupt())?;
    let qualification: RuntimeProfileQualification =
        serde_json::from_str(&raw).map_err(|_| corrupt())?;
    qualification.validate().map_err(|_| corrupt())?;
    let stored: (String, String, String, String) = (
        row.try_get("qualification_sha256").map_err(|_| corrupt())?,
        row.try_get("evidence_sha256").map_err(|_| corrupt())?,
        row.try_get("qualified_at").map_err(|_| corrupt())?,
        row.try_get("lock_sha256").map_err(|_| corrupt())?,
    );
    if stored
        != (
            qualification.qualification_sha256.clone(),
            qualification.evidence_sha256.clone(),
            qualification.qualified_at.clone(),
            qualification.lock_sha256.clone(),
        )
    {
        return Err(corrupt());
    }
    let lock = by_hash(conn, lock_sha256).await?.ok_or_else(corrupt)?;
    qualification
        .validate_for_lock(&lock)
        .map_err(|_| corrupt())?;
    Ok(Some(qualification))
}

#[async_trait]
impl RuntimeProfileQualificationRepository for SqlRuntimeProfileQualifications {
    async fn record(
        &self,
        lock: &RuntimeProfileLock,
        qualification: &RuntimeProfileQualification,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError> {
        lock.validate()?;
        qualification.validate_for_lock(lock)?;
        if ctx.actor_id.trim().is_empty() || !is_uuid_v7(&ctx.correlation_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Contexto de mutación no válido",
                "runtime_profile_qualification_actor_invalid",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN runtime_profile_qualification", &e))?;
        sqlx::query("UPDATE runtime_profile_qualification SET lock_sha256=lock_sha256 WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando runtime_profile_qualification", &e))?;
        let stored_lock = by_hash(&mut tx, &lock.lock_sha256)
            .await?
            .ok_or_else(conflict)?;
        if stored_lock != *lock {
            return Err(conflict());
        }
        if let Some(existing) = qualification_by_lock(&mut tx, &lock.lock_sha256).await? {
            if existing != *qualification {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La evidencia de cualificación para este lock ya es inmutable",
                    "runtime_profile_qualification_conflict",
                ));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay runtime_profile_qualification", &e))?;
            return Ok(false);
        }
        let at = &qualification.qualified_at;
        let payload = serde_json::to_string(qualification)
            .map_err(|e| db_error("serializando runtime_profile_qualification", &e))?;
        let inserted = sqlx::query(
            "INSERT INTO runtime_profile_qualification(lock_sha256,qualification_sha256,evidence_sha256,payload,actor_id,qualified_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(&qualification.lock_sha256)
        .bind(&qualification.qualification_sha256)
        .bind(&qualification.evidence_sha256)
        .bind(payload)
        .bind(&ctx.actor_id)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando runtime_profile_qualification", &e))?;
        if inserted.rows_affected() != 1 {
            return Err(db_error(
                "insertando runtime_profile_qualification",
                &"escritura no confirmada",
            ));
        }
        let journal_id = uuid::Uuid::now_v7().to_string();
        let resources = serde_json::json!([qualification.lock_sha256]).to_string();
        let journal_payload = serde_json::json!({
            "qualification": qualification,
            "actor_id": ctx.actor_id
        });
        let journal = sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'runtime.profile_qualification', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(resources)
            .bind(&qualification.qualification_sha256)
            .bind(journal_payload.to_string())
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("registrando runtime.profile_qualification en journal", &e))?;
        if journal.rows_affected() != 1 {
            return Err(db_error(
                "registrando runtime.profile_qualification en journal",
                &"escritura no confirmada",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.profile_qualification.recorded",
                resource_type: "runtime_profile_lock",
                resource_id: &qualification.lock_sha256,
                resource_version: 1,
                data: serde_json::json!({
                    "qualification_sha256": qualification.qualification_sha256,
                    "evidence_sha256": qualification.evidence_sha256,
                    "strategies": qualification.strategy_test_ids.keys().collect::<Vec<_>>(),
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT runtime_profile_qualification", &e))?;
        Ok(true)
    }

    async fn get_by_lock_hash(
        &self,
        lock_sha256: &str,
    ) -> Result<Option<RuntimeProfileQualification>, LarmError> {
        if lock_sha256.len() != 64
            || !lock_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Hash de lock no válido",
                "runtime_profile_lock_hash_invalid",
            ));
        }
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool runtime_profile_qualification", &e))?;
        qualification_by_lock(&mut conn, lock_sha256).await
    }
}
