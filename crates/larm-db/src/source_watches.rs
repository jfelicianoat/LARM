//! Persistencia transaccional de vigilancia de fuentes y notificaciones (P050).

use async_trait::async_trait;
use chrono::{Duration, Utc};
use larm_domain::{
    bridge::timestamp,
    error::{ErrorCode, LarmError},
    ports::{MutationContext, SourceWatchRepository},
    sources::{SourceOperationalSnapshot, SourceOperationalStatus},
    watchers::{
        SourceWatch, SourceWatchCreate, SourceWatchSyncFailure, SourceWatchSyncSuccess,
        WatchDetection,
    },
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Row, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{
    events::{append, NewEvent},
    pool::db_error,
};

pub const CREATE_SOURCE_WATCH_OPERATION: &str = "P050_post__source_watches";
const WATCH_COLUMNS: &str = "id,created_at,updated_at,row_version,metadata,source_id,query,cursor,etag,interval_seconds,policy,next_run_at,enabled";

#[derive(Clone)]
pub struct SqlSourceWatches {
    pub pool: SqlitePool,
}

fn text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<String, LarmError> {
    row.try_get(column)
        .map_err(|error| db_error(column, &error))
}

fn opt_text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Option<String>, LarmError> {
    row.try_get(column)
        .map_err(|error| db_error(column, &error))
}

fn value(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Value, LarmError> {
    serde_json::from_str(&text(row, column)?).map_err(|error| db_error(column, &error))
}

fn object(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Map<String, Value>, LarmError> {
    serde_json::from_value(value(row, column)?).map_err(|error| db_error(column, &error))
}

fn watch_row(row: &sqlx::sqlite::SqliteRow) -> Result<SourceWatch, LarmError> {
    let interval: i64 = row
        .try_get("interval_seconds")
        .map_err(|error| db_error("interval_seconds", &error))?;
    let enabled: i64 = row
        .try_get("enabled")
        .map_err(|error| db_error("enabled", &error))?;
    let watch = SourceWatch {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|error| db_error("row_version", &error))?,
        metadata: object(row, "metadata")?,
        source_id: text(row, "source_id")?,
        query: object(row, "query")?,
        cursor: opt_text(row, "cursor")?,
        etag: opt_text(row, "etag")?,
        interval_seconds: u64::try_from(interval)
            .map_err(|error| db_error("interval_seconds", &error))?,
        policy: object(row, "policy")?,
        next_run_at: opt_text(row, "next_run_at")?,
        enabled: enabled == 1,
    };
    watch.validate()?;
    Ok(watch)
}

fn invalid(field: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración de vigilancia no válida",
        "source_watch_invalid",
    )
    .with_resource(field.to_string())
}

fn missing(id: &str) -> LarmError {
    LarmError::new(ErrorCode::NotFound, "Vigilancia desconocida", "not_found")
        .with_resource(id.to_owned())
}

fn stale(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "La vigilancia cambió desde que la leíste",
        "stale_revision",
    )
    .with_resource(id.to_owned())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn jitter(id: &str) -> u64 {
    let digest = Sha256::digest(id.as_bytes());
    u64::from(u32::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3],
    ])) % 301
}

fn scheduled_at(now: chrono::DateTime<Utc>, id: &str, interval_seconds: u64) -> String {
    let delay = interval_seconds.saturating_add(jitter(id));
    let seconds = i64::try_from(delay.min(i64::MAX as u64)).unwrap_or(i64::MAX);
    timestamp(now + Duration::seconds(seconds))
}

fn runtime_metadata(
    metadata: &mut Map<String, Value>,
    last_success_at: Option<&str>,
    last_error: Option<&str>,
    failure_count: u64,
) {
    let mut watcher = metadata
        .get("watcher")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(last_success_at) = last_success_at {
        watcher.insert("last_success_at".into(), json!(last_success_at));
    }
    watcher.insert(
        "last_error".into(),
        last_error.map_or(Value::Null, |reason| json!(reason)),
    );
    watcher.insert("failure_count".into(), json!(failure_count));
    metadata.insert("watcher".into(), Value::Object(watcher));
}

async fn require_source(
    conn: &mut SqliteConnection,
    source_id: &str,
    must_be_enabled: bool,
) -> Result<(), LarmError> {
    let enabled: Option<i64> = sqlx::query_scalar("SELECT enabled FROM model_source WHERE id=?")
        .bind(source_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|error| db_error("comprobando fuente de vigilancia", &error))?;
    match enabled {
        None => Err(LarmError::new(
            ErrorCode::NotFound,
            "La fuente de vigilancia no existe",
            "source_not_found",
        )
        .with_resource(source_id.to_owned())),
        Some(0) if must_be_enabled => Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "Activa la fuente antes de habilitar su vigilancia",
            "source_disabled",
        )
        .with_resource(source_id.to_owned())),
        Some(_) => Ok(()),
    }
}

async fn load(conn: &mut SqliteConnection, id: &str) -> Result<SourceWatch, LarmError> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {WATCH_COLUMNS} FROM source_watch WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|error| db_error("leyendo source_watch", &error))?
    .ok_or_else(|| missing(id))?;
    watch_row(&row)
}

async fn append_watch_event(
    conn: &mut SqliteConnection,
    watch: &SourceWatch,
    action: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "source.watch_changed",
            resource_type: "source_watch",
            resource_id: &watch.id,
            resource_version: watch.row_version,
            data: json!({"action":action,"source_id":watch.source_id,"enabled":watch.enabled}),
        },
        ctx,
    )
    .await?;
    Ok(())
}

async fn update_source_operational_snapshot(
    conn: &mut SqliteConnection,
    source_id: &str,
    snapshot: &SourceOperationalSnapshot,
    updated_at: &str,
) -> Result<(), LarmError> {
    snapshot.validate()?;
    let status = serde_json::to_value(snapshot.status)
        .map_err(|error| db_error("source operational status", &error))?;
    let status = status
        .as_str()
        .ok_or_else(|| invalid("operational_status"))?;
    let rate_limit = snapshot
        .rate_limit
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| db_error("source rate limit", &error))?;
    let changed = sqlx::query(
        "UPDATE model_source_operational_snapshot SET status=?,last_sync_at=?,rate_limit_json=?,updated_at=? WHERE source_id=?",
    )
    .bind(status)
    .bind(&snapshot.last_sync_at)
    .bind(rate_limit)
    .bind(updated_at)
    .bind(source_id)
    .execute(conn)
    .await
    .map_err(|error| db_error("actualizando observación operativa de fuente", &error))?;
    if changed.rows_affected() != 1 {
        return Err(invalid("operational_snapshot_missing"));
    }
    Ok(())
}

async fn latest_observation(
    conn: &mut SqliteConnection,
    source_id: &str,
    identity_hash: &str,
) -> Result<Option<String>, LarmError> {
    let metadata: Option<String> = sqlx::query_scalar(
        "SELECT metadata FROM notification WHERE json_extract(metadata,'$.source_id')=? \
         AND json_extract(metadata,'$.upstream_identity_hash')=? \
         ORDER BY created_at DESC,id DESC LIMIT 1",
    )
    .bind(source_id)
    .bind(identity_hash)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|error| db_error("leyendo última revisión observada", &error))?;
    metadata
        .map(|text| {
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| db_error("metadata de observación", &error))?;
            Ok(value
                .get("revision_fingerprint")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned())
        })
        .transpose()
}

async fn notification_seen_recently(
    conn: &mut SqliteConnection,
    dedupe_key: &str,
    now: &str,
) -> Result<bool, LarmError> {
    let observed_at = chrono::DateTime::parse_from_rfc3339(now)
        .map_err(|error| db_error("observed_at de notificación", &error))?
        .with_timezone(&Utc);
    let cutoff = timestamp(observed_at - Duration::hours(24));
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM notification WHERE dedupe_key=? AND created_at>=? AND created_at<=?)",
    )
    .bind(dedupe_key)
    .bind(cutoff)
    .bind(now)
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| db_error("deduplicando notificación", &error))?;
    Ok(exists != 0)
}

async fn record_detection(
    conn: &mut SqliteConnection,
    source_id: &str,
    watch_id: &str,
    detection: &WatchDetection,
    now: &str,
    ctx: &MutationContext,
) -> Result<bool, LarmError> {
    if detection.upstream_identity.trim().is_empty() || !is_sha256(&detection.revision_fingerprint)
    {
        return Err(invalid("detection"));
    }
    let identity_hash = hex::encode(Sha256::digest(detection.upstream_identity.as_bytes()));
    let previous = latest_observation(conn, source_id, &identity_hash).await?;
    let Some(classification) = larm_domain::watchers::classify_source_revision(
        previous.as_deref(),
        &detection.revision_fingerprint,
        detection.compatible,
    ) else {
        return Ok(false);
    };
    let event_type = classification.as_str();
    let dedupe_key = larm_domain::watchers::source_watch_dedupe_key(
        source_id,
        &detection.upstream_identity,
        &detection.revision_fingerprint,
        classification,
    );
    if notification_seen_recently(conn, &dedupe_key, now).await? {
        return Ok(false);
    }
    let notification_id = Uuid::now_v7().to_string();
    let metadata = json!({
        "source_id":source_id,
        "source_watch_id":watch_id,
        "upstream_identity_hash":identity_hash,
        "revision_fingerprint":detection.revision_fingerprint,
        "event_type":event_type
    });
    let action = json!({"route":format!("/sources/{source_id}"),"resource_id":source_id});
    sqlx::query(
        "INSERT INTO notification(id,created_at,updated_at,row_version,metadata,dedupe_key,severity,message_code,resource_id,action,read_at) \
         VALUES (?,?,?,1,?,?,'info',?,?,?,NULL)",
    )
    .bind(&notification_id)
    .bind(now)
    .bind(now)
    .bind(metadata.to_string())
    .bind(&dedupe_key)
    .bind(event_type)
    .bind(source_id)
    .bind(action.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| db_error("creando notificación", &error))?;
    append(
        conn,
        &NewEvent {
            event_type: "source.revision_detected",
            resource_type: "model_source",
            resource_id: source_id,
            resource_version: 1,
            data: json!({
                "event_type":event_type,
                "upstream_identity_hash":identity_hash,
                "revision_fingerprint":detection.revision_fingerprint
            }),
        },
        ctx,
    )
    .await?;
    append(
        conn,
        &NewEvent {
            event_type: "notification.created",
            resource_type: "notification",
            resource_id: &notification_id,
            resource_version: 1,
            data: json!({"message_code":event_type,"resource_id":source_id}),
        },
        ctx,
    )
    .await?;
    Ok(true)
}

#[async_trait]
impl SourceWatchRepository for SqlSourceWatches {
    async fn source_watch(&self, id: &str) -> Result<Option<SourceWatch>, LarmError> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {WATCH_COLUMNS} FROM source_watch WHERE id=?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| db_error("leyendo source_watch", &error))?;
        row.as_ref().map(watch_row).transpose()
    }

    async fn page_source_watches(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<SourceWatch>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(invalid("limit"));
        }
        let (at, id) = after.map_or((None, None), |(at, id)| (Some(at), Some(id)));
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {WATCH_COLUMNS} FROM source_watch WHERE (? IS NULL OR (created_at,id)>(?,?)) ORDER BY created_at,id LIMIT ?"
        )))
        .bind(&at)
        .bind(&at)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("listando source_watch", &error))?;
        rows.iter().map(watch_row).collect()
    }

    async fn create_source_watch(
        &self,
        request: &SourceWatchCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        request.validate()?;
        // P119: automatic_download=true es válido; la admisión se evalúa
        // contra la política vigente (opt-in) en el momento de la detección.
        if request
            .policy
            .get("automatic_download")
            .and_then(Value::as_bool)
            == Some(true)
            && request
                .policy
                .get("storage_location_id")
                .and_then(Value::as_str)
                .map(str::is_empty)
                != Some(false)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La descarga automática exige el destino de la política",
                "watch_auto_download_target_missing",
            ));
        }
        if !is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 240
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
            || ctx.actor_id.is_empty()
        {
            return Err(invalid("idempotency"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE model_source SET row_version=row_version WHERE id=?")
            .bind(&request.source_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando fuente de vigilancia", &error))?;
        let now = Utc::now();
        let now_text = timestamp(now);
        let replay: Option<(String, String)> = sqlx::query_as(
            "SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(CREATE_SOURCE_WATCH_OPERATION)
        .bind(key)
        .bind(&now_text)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("replay de source_watch", &error))?;
        if let Some((hash, response)) = replay {
            if hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            let original: SourceWatch = serde_json::from_str(&response)
                .map_err(|error| db_error("replay de source_watch", &error))?;
            if original.source_id != request.source_id
                || original.query != request.query
                || original.interval_seconds != request.interval_seconds
                || original.policy != request.policy
                || original.enabled != request.enabled
            {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit()
                .await
                .map_err(|error| db_error("COMMIT", &error))?;
            return Ok(original);
        }
        require_source(&mut tx, &request.source_id, request.enabled).await?;
        let id = Uuid::now_v7().to_string();
        let record = SourceWatch {
            id: id.clone(),
            created_at: now_text.clone(),
            updated_at: now_text.clone(),
            row_version: 1,
            metadata: Map::new(),
            source_id: request.source_id.clone(),
            query: request.query.clone(),
            cursor: None,
            etag: None,
            interval_seconds: request.interval_seconds,
            policy: request.policy.clone(),
            next_run_at: request
                .enabled
                .then(|| scheduled_at(now, &id, request.interval_seconds)),
            enabled: request.enabled,
        };
        record.validate()?;
        sqlx::query(
            "INSERT INTO source_watch(id,created_at,updated_at,row_version,metadata,source_id,query,cursor,etag,interval_seconds,policy,next_run_at,enabled) \
             VALUES (?,?,?,1,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&record.id)
        .bind(&record.created_at)
        .bind(&record.updated_at)
        .bind("{}")
        .bind(&record.source_id)
        .bind(Value::Object(record.query.clone()).to_string())
        .bind(&record.cursor)
        .bind(&record.etag)
        .bind(i64::try_from(record.interval_seconds).unwrap_or(i64::MAX))
        .bind(Value::Object(record.policy.clone()).to_string())
        .bind(&record.next_run_at)
        .bind(record.enabled)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("creando source_watch", &error))?;
        sqlx::query(
            "DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?",
        )
        .bind(&ctx.actor_id)
        .bind(CREATE_SOURCE_WATCH_OPERATION)
        .bind(key)
        .bind(&now_text)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("limpiando replay source_watch", &error))?;
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,201,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(CREATE_SOURCE_WATCH_OPERATION)
        .bind(key)
        .bind(body_hash)
        .bind(serde_json::to_string(&record).map_err(|error| db_error("replay", &error))?)
        .bind(timestamp(now + Duration::days(30)))
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("guardando replay source_watch", &error))?;
        append_watch_event(&mut tx, &record, "created", ctx).await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(record)
    }

    async fn update_source_watch(
        &self,
        id: &str,
        if_match: i64,
        request: &SourceWatchCreate,
        ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        request.validate()?;
        // P119: automatic_download=true es válido; la admisión se evalúa
        // contra la política vigente (opt-in) en el momento de la detección.
        if request
            .policy
            .get("automatic_download")
            .and_then(Value::as_bool)
            == Some(true)
            && request
                .policy
                .get("storage_location_id")
                .and_then(Value::as_str)
                .map(str::is_empty)
                != Some(false)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La descarga automática exige el destino de la política",
                "watch_auto_download_target_missing",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE source_watch SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando source_watch", &error))?;
        let mut record = load(&mut tx, id).await?;
        if record.row_version != if_match {
            return Err(stale(id));
        }
        require_source(&mut tx, &request.source_id, request.enabled).await?;
        let source_changed = record.source_id != request.source_id;
        let query_changed = record.query != request.query;
        let now = Utc::now();
        record.updated_at = timestamp(now);
        record.row_version = record
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version"))?;
        record.source_id = request.source_id.clone();
        record.query = request.query.clone();
        record.interval_seconds = request.interval_seconds;
        record.policy = request.policy.clone();
        record.enabled = request.enabled;
        if source_changed || query_changed {
            record.cursor = None;
            record.etag = None;
            record.metadata.clear();
        }
        record.next_run_at = request
            .enabled
            .then(|| scheduled_at(now, &record.id, record.interval_seconds));
        sqlx::query(
            "UPDATE source_watch SET updated_at=?,row_version=?,metadata=?,source_id=?,query=?,cursor=?,etag=?,interval_seconds=?,policy=?,next_run_at=?,enabled=? WHERE id=? AND row_version=?",
        )
        .bind(&record.updated_at)
        .bind(record.row_version)
        .bind(Value::Object(record.metadata.clone()).to_string())
        .bind(&record.source_id)
        .bind(Value::Object(record.query.clone()).to_string())
        .bind(&record.cursor)
        .bind(&record.etag)
        .bind(i64::try_from(record.interval_seconds).unwrap_or(i64::MAX))
        .bind(Value::Object(record.policy.clone()).to_string())
        .bind(&record.next_run_at)
        .bind(record.enabled)
        .bind(id)
        .bind(if_match)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("actualizando source_watch", &error))?;
        append_watch_event(&mut tx, &record, "updated", ctx).await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(record)
    }

    async fn delete_source_watch(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE source_watch SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando source_watch", &error))?;
        let mut record = load(&mut tx, id).await?;
        if record.row_version != if_match {
            return Err(stale(id));
        }
        sqlx::query("DELETE FROM source_watch WHERE id=? AND row_version=?")
            .bind(id)
            .bind(if_match)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("borrando source_watch", &error))?;
        record.row_version += 1;
        append_watch_event(&mut tx, &record, "deleted", ctx).await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(())
    }

    async fn due_source_watches(
        &self,
        now: &str,
        limit: u32,
    ) -> Result<Vec<SourceWatch>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(invalid("limit"));
        }
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {WATCH_COLUMNS} FROM source_watch WHERE enabled=1 AND next_run_at IS NOT NULL AND next_run_at<=? ORDER BY next_run_at,id LIMIT ?"
        )))
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("leyendo source_watch pendientes", &error))?;
        rows.iter().map(watch_row).collect()
    }

    async fn recover_interrupted_source_syncs(
        &self,
        recovered_at: &str,
        ctx: &MutationContext,
    ) -> Result<u64, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        let rows = sqlx::query(
            "SELECT o.source_id,s.row_version FROM model_source_operational_snapshot o JOIN model_source s ON s.id=o.source_id WHERE o.status='SYNCING' ORDER BY o.source_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| db_error("leyendo source.sync interrumpidos", &error))?;
        let mut recovered = 0_u64;
        for row in rows {
            let source_id: String = row
                .try_get("source_id")
                .map_err(|error| db_error("source_id", &error))?;
            let row_version: i64 = row
                .try_get("row_version")
                .map_err(|error| db_error("row_version", &error))?;
            let changed = sqlx::query(
                "UPDATE model_source_operational_snapshot SET status='ERROR',updated_at=? WHERE source_id=? AND status='SYNCING'",
            )
            .bind(recovered_at)
            .bind(&source_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("recuperando source.sync interrumpido", &error))?;
            if changed.rows_affected() != 1 {
                continue;
            }
            append(
                &mut tx,
                &NewEvent {
                    event_type: "source.sync_interrupted",
                    resource_type: "model_source",
                    resource_id: &source_id,
                    resource_version: row_version,
                    data: json!({"source_id":source_id,"status":SourceOperationalStatus::Error,"reason":"daemon_restarted"}),
                },
                ctx,
            )
            .await?;
            recovered = recovered.saturating_add(1);
        }
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(recovered)
    }

    async fn start_source_watch_sync(
        &self,
        id: &str,
        if_match: i64,
        started_at: &str,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE source_watch SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando source_watch", &error))?;
        let record = load(&mut tx, id).await?;
        if record.row_version != if_match || !record.enabled {
            return Err(stale(id));
        }
        let changed = sqlx::query(
            "UPDATE model_source_operational_snapshot SET status='SYNCING',updated_at=? WHERE source_id=?",
        )
        .bind(started_at)
        .bind(&record.source_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("marcando source.sync en curso", &error))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("operational_snapshot_missing"));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "source.sync_started",
                resource_type: "source_watch",
                resource_id: &record.id,
                resource_version: record.row_version,
                data: json!({"source_id":record.source_id,"status":SourceOperationalStatus::Syncing}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(())
    }

    async fn record_source_watch_success(
        &self,
        id: &str,
        if_match: i64,
        commit: &SourceWatchSyncSuccess,
        ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE source_watch SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando source_watch", &error))?;
        let mut record = load(&mut tx, id).await?;
        if record.row_version != if_match || !record.enabled {
            return Err(stale(id));
        }
        record.cursor = commit.cursor.clone();
        record.etag = commit.etag.clone();
        record.next_run_at = Some(commit.next_run_at.clone());
        record.updated_at = commit.observed_at.clone();
        record.row_version = record
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version"))?;
        runtime_metadata(&mut record.metadata, Some(&commit.observed_at), None, 0);
        sqlx::query(
            "UPDATE source_watch SET updated_at=?,row_version=?,metadata=?,cursor=?,etag=?,next_run_at=? WHERE id=? AND row_version=? AND enabled=1",
        )
        .bind(&record.updated_at)
        .bind(record.row_version)
        .bind(Value::Object(record.metadata.clone()).to_string())
        .bind(&record.cursor)
        .bind(&record.etag)
        .bind(&record.next_run_at)
        .bind(id)
        .bind(if_match)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("avanzando cursor source_watch", &error))?;
        update_source_operational_snapshot(
            &mut tx,
            &record.source_id,
            &SourceOperationalSnapshot {
                status: SourceOperationalStatus::Healthy,
                last_sync_at: Some(commit.observed_at.clone()),
                rate_limit: commit.rate_limit.clone(),
            },
            &commit.observed_at,
        )
        .await?;
        let mut notifications = 0_u64;
        for detection in &commit.detections {
            if record_detection(
                &mut tx,
                &record.source_id,
                &record.id,
                detection,
                &commit.observed_at,
                ctx,
            )
            .await?
            {
                notifications += 1;
            }
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "source.sync_completed",
                resource_type: "source_watch",
                resource_id: &record.id,
                resource_version: record.row_version,
                data: json!({"source_id":record.source_id,"notifications":notifications,"status":SourceOperationalStatus::Healthy,"last_sync_at":commit.observed_at}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(record)
    }

    async fn record_source_watch_failure(
        &self,
        id: &str,
        if_match: i64,
        commit: &SourceWatchSyncFailure,
        ctx: &MutationContext,
    ) -> Result<SourceWatch, LarmError> {
        if commit.reason.is_empty()
            || commit.reason.len() > 96
            || !commit
                .reason
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(invalid("failure_reason"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        sqlx::query("UPDATE source_watch SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando source_watch", &error))?;
        let mut record = load(&mut tx, id).await?;
        if record.row_version != if_match || !record.enabled {
            return Err(stale(id));
        }
        let failures = record
            .metadata
            .get("watcher")
            .and_then(Value::as_object)
            .and_then(|value| value.get("failure_count"))
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(1);
        record.next_run_at = Some(commit.retry_at.clone());
        record.updated_at = commit.observed_at.clone();
        record.row_version = record
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version"))?;
        runtime_metadata(&mut record.metadata, None, Some(&commit.reason), failures);
        sqlx::query(
            "UPDATE source_watch SET updated_at=?,row_version=?,metadata=?,next_run_at=? WHERE id=? AND row_version=? AND enabled=1",
        )
        .bind(&record.updated_at)
        .bind(record.row_version)
        .bind(Value::Object(record.metadata.clone()).to_string())
        .bind(&record.next_run_at)
        .bind(id)
        .bind(if_match)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("registrando error source_watch", &error))?;
        update_source_operational_snapshot(
            &mut tx,
            &record.source_id,
            &SourceOperationalSnapshot {
                status: commit.operational_status,
                last_sync_at: Some(commit.observed_at.clone()),
                rate_limit: commit.rate_limit.clone(),
            },
            &commit.observed_at,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "source.sync_failed",
                resource_type: "source_watch",
                resource_id: &record.id,
                resource_version: record.row_version,
                data: json!({"source_id":record.source_id,"reason":commit.reason,"retry_at":commit.retry_at,"status":commit.operational_status,"last_sync_at":commit.observed_at}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(record)
    }
}
