//! P119: persistencia de la política opt-in de descargas automáticas y del
//! historial inmutable de admisiones. La política es un singleton versionado
//! con journal/evento/outbox; las admisiones se insertan inmutables salvo la
//! transición a REVOKED (revocación inmediata de futuras admisiones).
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use larm_domain::download_admission::{
    evaluate_admission, AdmissionCandidate, DownloadAdmission, DownloadAdmissionPolicy,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::MutationContext;
use serde_json::json;
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct SqlDownloadAdmissions {
    pub pool: SqlitePool,
}

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo gestionar la admisión de descargas",
        reason,
    )
}

fn invalid(reason: &str) -> LarmError {
    error(ErrorCode::ValidationFailed, reason)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredPolicy {
    pub policy: DownloadAdmissionPolicy,
    pub row_version: i64,
    pub updated_at: String,
}

fn row_to_policy(row: &sqlx::sqlite::SqliteRow) -> Result<StoredPolicy, LarmError> {
    let parse_list = |name: &str| -> Result<Vec<String>, LarmError> {
        let raw: String = row.try_get(name).map_err(|e| db_error(name, &e))?;
        serde_json::from_str(&raw)
            .map_err(|_| error(ErrorCode::IntegrityFailed, "admission_policy_corrupt"))
    };
    let daily: Option<String> = row
        .try_get("daily_quota_bytes")
        .map_err(|e| db_error("daily_quota_bytes", &e))?;
    Ok(StoredPolicy {
        policy: DownloadAdmissionPolicy {
            enabled: row
                .try_get::<i64, _>("enabled")
                .map_err(|e| db_error("enabled", &e))?
                != 0,
            source_allowlist: parse_list("source_allowlist")?,
            capability_allowlist: parse_list("capability_allowlist")?,
            license_allowlist: parse_list("license_allowlist")?,
            min_trust: row
                .try_get("min_trust")
                .map_err(|e| db_error("min_trust", &e))?,
            daily_quota_bytes: daily,
            storage_location_id: row
                .try_get("storage_location_id")
                .map_err(|e| db_error("storage_location_id", &e))?,
        },
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|e| db_error("updated_at", &e))?,
    })
}

async fn read_policy(conn: &mut sqlx::SqliteConnection) -> Result<Option<StoredPolicy>, LarmError> {
    let found = sqlx::query("SELECT * FROM download_admission_policy WHERE singleton=1")
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo download_admission_policy", &e))?;
    found.as_ref().map(row_to_policy).transpose()
}

async fn used_bytes_on(conn: &mut sqlx::SqliteConnection, day_key: &str) -> Result<u64, LarmError> {
    let used: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(cast(estimated_bytes AS INTEGER)),0) FROM download_admission \
         WHERE day_key=? AND decision='ADMITTED'",
    )
    .bind(day_key)
    .fetch_one(conn)
    .await
    .map_err(|e| db_error("sumando cuota diaria", &e))?;
    Ok(u64::try_from(used).unwrap_or(u64::MAX))
}

impl SqlDownloadAdmissions {
    /// Persiste o actualiza la política (singleton versionado). El aumento de
    /// row_version invalida la aprobación anterior (aprobación versionada).
    pub async fn put_policy(
        &self,
        policy: &DownloadAdmissionPolicy,
        ctx: &MutationContext,
    ) -> Result<StoredPolicy, LarmError> {
        policy
            .validate()
            .map_err(|_| invalid("admission_policy_invalid"))?;
        if ctx.actor_id.is_empty() || ctx.actor_id.len() > 256 {
            return Err(invalid("actor_invalid"));
        }
        let at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN policy", &e))?;
        let sources = json!(policy.source_allowlist).to_string();
        let capabilities = json!(policy.capability_allowlist).to_string();
        let licenses = json!(policy.license_allowlist).to_string();
        let stored = sqlx::query(
            "INSERT INTO download_admission_policy(singleton,enabled,source_allowlist,capability_allowlist,license_allowlist,min_trust,daily_quota_bytes,storage_location_id,row_version,updated_at) \
             VALUES(1,?,?,?,?,?,?,?,1,?) \
             ON CONFLICT(singleton) DO UPDATE SET enabled=excluded.enabled,source_allowlist=excluded.source_allowlist,capability_allowlist=excluded.capability_allowlist,license_allowlist=excluded.license_allowlist,min_trust=excluded.min_trust,daily_quota_bytes=excluded.daily_quota_bytes,storage_location_id=excluded.storage_location_id,row_version=row_version+1,updated_at=excluded.updated_at \
             RETURNING row_version, updated_at",
        )
        .bind(i64::from(policy.enabled))
        .bind(&sources)
        .bind(&capabilities)
        .bind(&licenses)
        .bind(&policy.min_trust)
        .bind(policy.daily_quota_bytes.as_deref())
        .bind(policy.storage_location_id.as_deref())
        .bind(&at)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("escribiendo download_admission_policy", &e))?;
        let row_version: i64 = stored
            .try_get("row_version")
            .map_err(|e| db_error("row_version policy", &e))?;
        let updated_at: String = stored
            .try_get("updated_at")
            .map_err(|e| db_error("updated_at policy", &e))?;
        let journal_id = uuid::Uuid::now_v7().to_string();
        let payload =
            json!({"policy": policy, "row_version": row_version, "actor_id": ctx.actor_id});
        let journal = sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'download.admission_policy', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(json!(["download-admission-policy"]).to_string())
            .bind(larm_domain::canonical::sha256_hex(&payload).map_err(|_| invalid("policy_hash_invalid"))?)
            .bind(payload.to_string())
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("journal download.admission_policy", &e))?;
        if journal.rows_affected() != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "admission_policy_journal_unconfirmed",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "download.admission_policy.updated",
                resource_type: "download_admission_policy",
                resource_id: "download-admission-policy",
                resource_version: row_version,
                data: json!({"enabled": policy.enabled, "row_version": row_version}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT policy", &e))?;
        Ok(StoredPolicy {
            policy: policy.clone(),
            row_version,
            updated_at,
        })
    }

    pub async fn get_policy(&self) -> Result<Option<StoredPolicy>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool policy", &e))?;
        read_policy(&mut conn).await
    }

    /// Evalúa el candidato contra la política vigente y registra la admisión
    /// (ADMITTED/BLOCKED) en una transacción con la cuota diaria agregada.
    pub async fn admit(
        &self,
        candidate: &AdmissionCandidate,
        ctx: &MutationContext,
    ) -> Result<(DownloadAdmission, Option<i64>), LarmError> {
        let at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let day_key = &at[..10];
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN admit", &e))?;
        let stored = read_policy(&mut tx).await?;
        let Some(stored_policy) = stored else {
            return Err(invalid("admission_policy_missing"));
        };
        let used = used_bytes_on(&mut tx, day_key).await?;
        let decision = evaluate_admission(&stored_policy.policy, candidate, used, day_key)?;
        let admission = DownloadAdmission {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            variant_id: candidate.variant_id.clone(),
            source_id: candidate.source_id.clone(),
            estimated_bytes: candidate.estimated_bytes.to_string(),
            decision: decision.decision.to_string(),
            reason: decision.reason.clone(),
            policy_row_version: stored_policy.row_version,
            day_key: day_key.to_string(),
            job_id: None,
        };
        let metadata_json = json!(admission.metadata).to_string();
        let inserted = sqlx::query(
            "INSERT INTO download_admission(id,created_at,updated_at,row_version,metadata,variant_id,source_id,estimated_bytes,decision,reason,policy_row_version,day_key,job_id) \
             VALUES(?,?,?,1,?,?,?,?,?,?,?,?,NULL)",
        )
        .bind(&admission.id)
        .bind(&admission.created_at)
        .bind(&admission.updated_at)
        .bind(&metadata_json)
        .bind(&admission.variant_id)
        .bind(&admission.source_id)
        .bind(&admission.estimated_bytes)
        .bind(&admission.decision)
        .bind(&admission.reason)
        .bind(admission.policy_row_version)
        .bind(&admission.day_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando download_admission", &e))?;
        if inserted.rows_affected() != 1 {
            return Err(error(ErrorCode::Conflict, "admission_insert_unconfirmed"));
        }
        let journal_id = uuid::Uuid::now_v7().to_string();
        let payload = json!({"admission": admission, "actor_id": ctx.actor_id});
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'download.admission', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(json!([admission.id]).to_string())
            .bind(larm_domain::canonical::sha256_hex(&payload).map_err(|_| invalid("admission_hash_invalid"))?)
            .bind(payload.to_string())
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("journal download.admission", &e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "download.admission.evaluated",
                resource_type: "download_admission",
                resource_id: &admission.id,
                resource_version: 1,
                data: json!({"decision": admission.decision, "reason": admission.reason, "variant_id": admission.variant_id}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT admit", &e))?;
        let job_row_version = if admission.decision == "ADMITTED" {
            Some(stored_policy.row_version)
        } else {
            None
        };
        Ok((admission, job_row_version))
    }

    /// Revocación inmediata: marca la admisión REVOKED (futuras admisiones del
    /// mismo variant quedan excluidas por el evaluador del watcher).
    pub async fn revoke(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<DownloadAdmission, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(id) {
            return Err(invalid("admission_id_invalid"));
        }
        let at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN revoke", &e))?;
        let updated = sqlx::query(
            "UPDATE download_admission SET decision='REVOKED', reason='admission_revoked_by_operator', updated_at=?, row_version=row_version+1 WHERE id=? AND decision='ADMITTED' RETURNING *",
        )
        .bind(&at)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("revocando download_admission", &e))?;
        let Some(row) = updated else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Admisión no encontrada o ya terminal",
                "download_admission_not_admitted",
            ));
        };
        let admission = row_to_admission(&row)?;
        let journal_id = uuid::Uuid::now_v7().to_string();
        let payload = json!({"admission": admission, "actor_id": ctx.actor_id});
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'download.admission', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(json!([admission.id]).to_string())
            .bind(larm_domain::canonical::sha256_hex(&payload).map_err(|_| invalid("admission_hash_invalid"))?)
            .bind(payload.to_string())
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("journal revoke", &e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "download.admission.revoked",
                resource_type: "download_admission",
                resource_id: &admission.id,
                resource_version: admission.row_version,
                data: json!({"variant_id": admission.variant_id, "day_key": admission.day_key}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT revoke", &e))?;
        Ok(admission)
    }

    pub async fn list(&self, limit: u32) -> Result<Vec<DownloadAdmission>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(invalid("admission_filter_invalid"));
        }
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool admissions", &e))?;
        let rows = sqlx::query(
            "SELECT * FROM download_admission ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_error("listando download_admission", &e))?;
        rows.iter().map(row_to_admission).collect()
    }

    /// ¿Hay alguna admisión REVOKED vigente para este variant? El watcher
    /// consulta esto para la revalidación (revocación inmediata).
    pub async fn is_revoked(&self, variant_id: &str) -> Result<bool, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool admissions", &e))?;
        let revoked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM download_admission WHERE variant_id=? AND decision='REVOKED'",
        )
        .bind(variant_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("consultando revocados", &e))?;
        Ok(revoked > 0)
    }
}

fn row_to_admission(row: &sqlx::sqlite::SqliteRow) -> Result<DownloadAdmission, LarmError> {
    let metadata_raw: String = row
        .try_get("metadata")
        .map_err(|e| db_error("metadata admission", &e))?;
    let admission = DownloadAdmission {
        id: row
            .try_get("id")
            .map_err(|e| db_error("id admission", &e))?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| db_error("created_at", &e))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|e| db_error("updated_at", &e))?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: serde_json::from_str(&metadata_raw)
            .map_err(|_| error(ErrorCode::IntegrityFailed, "admission_record_corrupt"))?,
        variant_id: row
            .try_get("variant_id")
            .map_err(|e| db_error("variant_id", &e))?,
        source_id: row
            .try_get("source_id")
            .map_err(|e| db_error("source_id", &e))?,
        estimated_bytes: row
            .try_get("estimated_bytes")
            .map_err(|e| db_error("estimated_bytes", &e))?,
        decision: row
            .try_get("decision")
            .map_err(|e| db_error("decision", &e))?,
        reason: row.try_get("reason").map_err(|e| db_error("reason", &e))?,
        policy_row_version: row
            .try_get("policy_row_version")
            .map_err(|e| db_error("policy_row_version", &e))?,
        day_key: row
            .try_get("day_key")
            .map_err(|e| db_error("day_key", &e))?,
        job_id: row.try_get("job_id").map_err(|e| db_error("job_id", &e))?,
    };
    admission
        .validate()
        .map_err(|_| error(ErrorCode::IntegrityFailed, "admission_record_corrupt"))?;
    Ok(admission)
}
