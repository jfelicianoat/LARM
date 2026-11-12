//! Escritura de eventos en el outbox dentro de la transacción de cada mutación
//! (contrato 07: «Outbox transacción con cada mutación»).
//!
//! `event_id` es decimal y monotónico: lo asigna `event_sequence_alloc`
//! (AUTOINCREMENT). La fila `event` y su entrada en `event_outbox` se escriben
//! con la mutación; el despacho at-least-once, SSE y la poda son de P029.

use larm_domain::error::LarmError;
use larm_domain::ports::MutationContext;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use super::pool::db_error;

/// Límite del batch que el dispatcher puede publicar y confirmar de una vez.
pub const MAX_DISPATCH_BATCH: u32 = 512;
pub const MAX_EVENT_REPLAY: u32 = 10_000;

/// Evento de dominio a registrar (`type` con forma `dominio.acción`).
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub event_type: &'a str,
    pub resource_type: &'a str,
    pub resource_id: &'a str,
    pub resource_version: i64,
    /// Datos sin contenido de usuario ni secretos.
    pub data: serde_json::Value,
}

/// Inserta el evento y su entrada de outbox; devuelve el `event_id` decimal.
///
/// # Errors
/// `INTERNAL` o `RESOURCE_UNAVAILABLE` (bloqueo) desde SQLite.
pub async fn append(
    conn: &mut SqliteConnection,
    event: &NewEvent<'_>,
    ctx: &MutationContext,
) -> Result<String, LarmError> {
    let allocated = sqlx::query("INSERT INTO event_sequence_alloc DEFAULT VALUES")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("asignando event_id", &e))?;
    if allocated.rows_affected() != 1 {
        return Err(db_error("asignando event_id", &"escritura no confirmada"));
    }
    let sequence = allocated.last_insert_rowid();
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let record_id = uuid::Uuid::now_v7().to_string();
    let inserted = sqlx::query(
        "INSERT INTO event(id,created_at,updated_at,row_version,metadata,event_id,event_type,actor_id,\
         resource_type,resource_id,resource_version,correlation_id,payload,occurred_at) \
         VALUES (?,?,?,1,'{}',?,?,?,?,?,?,?,?,?)",
    )
    .bind(&record_id)
    .bind(&now)
    .bind(&now)
    .bind(sequence)
    .bind(event.event_type)
    .bind(&ctx.actor_id)
    .bind(event.resource_type)
    .bind(event.resource_id)
    .bind(event.resource_version)
    .bind(&ctx.correlation_id)
    .bind(event.data.to_string())
    .bind(&now)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("insertando event", &e))?;
    if inserted.rows_affected() != 1 {
        return Err(db_error("insertando event", &"escritura no confirmada"));
    }
    let enqueued = sqlx::query(
        "INSERT INTO event_outbox(event_id, event_record_id, delivered_at) VALUES (?, ?, NULL)",
    )
    .bind(sequence)
    .bind(&record_id)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("insertando event_outbox", &e))?;
    if enqueued.rows_affected() != 1 {
        return Err(db_error(
            "insertando event_outbox",
            &"escritura no confirmada",
        ));
    }
    // rows_affected() does not prove that an AFTER trigger kept the mandatory
    // record, payload or outbox link. Read the exact pair back while the caller
    // still owns its business transaction; a failure must roll back all of it.
    let payload:Option<String>=sqlx::query_scalar("SELECT e.payload FROM event e JOIN event_outbox o ON o.event_record_id=e.id AND o.event_id=e.event_id WHERE e.id=? AND e.event_id=? AND e.event_type=? AND e.actor_id=? AND e.resource_type=? AND e.resource_id=? AND e.resource_version=? AND e.correlation_id=? AND e.occurred_at=? AND e.created_at=? AND e.updated_at=? AND e.row_version=1 AND json(e.metadata)=json('{}') AND o.delivered_at IS NULL")
        .bind(&record_id).bind(sequence).bind(event.event_type).bind(&ctx.actor_id).bind(event.resource_type)
        .bind(event.resource_id).bind(event.resource_version).bind(&ctx.correlation_id).bind(&now).bind(&now).bind(&now)
        .fetch_optional(&mut *conn).await.map_err(|e|db_error("confirmando evento/outbox",&e))?;
    let persisted = payload
        .as_deref()
        .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok());
    if persisted.as_ref() != Some(&event.data) {
        return Err(db_error(
            "confirmando evento/outbox",
            &"escritura incompleta o alterada",
        ));
    }
    Ok(sequence.to_string())
}

/// Evento persistido, tal como lo publica el bus.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub event_id: i64,
    pub event_type: String,
    pub occurred_at: String,
    pub correlation_id: String,
    pub actor_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resource_version: i64,
    pub data: serde_json::Value,
}

/// Columnas de `event` en el orden de la consulta de [`since`].
type EventRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
);

/// Una ventana consistente para reanudar un stream SSE. `through_event_id`
/// fija el high-water mark usado para filtrar mensajes que también llegaron
/// al bus mientras se leía el historial.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayWindow {
    pub events: Vec<StoredEvent>,
    pub through_event_id: i64,
}

fn decode_rows(rows: Vec<EventRow>, context: &str) -> Result<Vec<StoredEvent>, LarmError> {
    rows.into_iter()
        .map(|r| {
            Ok(StoredEvent {
                event_id: r.0,
                event_type: r.1,
                occurred_at: r.2,
                correlation_id: r.3,
                actor_id: r.4,
                resource_type: r.5,
                resource_id: r.6,
                resource_version: r.7,
                data: serde_json::from_str(&r.8).map_err(|e| db_error(context, &e))?,
            })
        })
        .collect()
}

/// Eventos con `event_id` mayor que `after`, en orden, como mucho `limit`.
///
/// # Errors
/// Errores de SQLite.
pub async fn since(
    pool: &sqlx::SqlitePool,
    after: i64,
    limit: i64,
) -> Result<Vec<StoredEvent>, LarmError> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT event_id, event_type, occurred_at, correlation_id, actor_id, resource_type, resource_id, \
         resource_version, payload FROM event WHERE event_id > ? ORDER BY event_id LIMIT ?",
    )
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| db_error("leyendo eventos", &e))?;
    decode_rows(rows, "payload de evento")
}

/// Lee una ventana SSE dentro de la retención y devuelve un high-water mark
/// consistente con los eventos. El límite se aplica antes de abrir el stream;
/// un cursor demasiado antiguo o con demasiado backlog exige resincronizar.
///
/// # Errors
/// Devuelve `VALIDATION_FAILED` para un cursor futuro/límite inválido,
/// `EVENT_CURSOR_EXPIRED` si el cursor ya no cabe en la ventana de replay y
/// errores de SQLite.
pub async fn replay_window(
    pool: &SqlitePool,
    after: Option<i64>,
    retention_days: u32,
    limit: u32,
) -> Result<ReplayWindow, LarmError> {
    if limit == 0 || limit > MAX_EVENT_REPLAY || after.is_some_and(|cursor| cursor < 0) {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "Cursor de eventos no válido",
            "event_cursor_invalid",
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| db_error("BEGIN replay de eventos", &e))?;
    let high_watermark: i64 =
        sqlx::query_scalar("SELECT coalesce((SELECT max(id) FROM event_sequence_alloc),0)")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_error("high-water mark de eventos", &e))?;
    let Some(after) = after else {
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT replay de eventos", &e))?;
        return Ok(ReplayWindow {
            events: Vec::new(),
            through_event_id: high_watermark,
        });
    };
    if after > high_watermark {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "El cursor de eventos está por delante del servidor",
            "event_cursor_future",
        ));
    }
    if after == high_watermark {
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT replay de eventos", &e))?;
        return Ok(ReplayWindow {
            events: Vec::new(),
            through_event_id: high_watermark,
        });
    }

    let cutoff = larm_domain::bridge::timestamp(
        chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days)),
    );
    let oldest_retained: Option<i64> = sqlx::query_scalar(
        "SELECT min(event_id) FROM event WHERE occurred_at >= ? AND event_id <= ?",
    )
    .bind(&cutoff)
    .bind(high_watermark)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| db_error("cursor mínimo retenido", &e))?;
    let expired = oldest_retained.is_none_or(|oldest| after < oldest.saturating_sub(1));
    if expired {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::EventCursorExpired,
            "El cursor de eventos ya no está disponible; resincroniza el estado antes de reconectar",
            "event_cursor_expired",
        ));
    }

    let fetch_limit = i64::from(limit) + 1;
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT event_id,event_type,occurred_at,correlation_id,actor_id,resource_type,resource_id,\
         resource_version,payload FROM event WHERE event_id > ? AND event_id <= ? AND occurred_at >= ?\
         ORDER BY event_id LIMIT ?",
    )
    .bind(after)
    .bind(high_watermark)
    .bind(&cutoff)
    .bind(fetch_limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| db_error("leyendo replay de eventos", &e))?;
    if rows.len() > limit as usize {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::EventCursorExpired,
            "El cursor requiere más eventos de los permitidos; resincroniza el estado antes de reconectar",
            "event_replay_limit_exceeded",
        ));
    }
    let events = decode_rows(rows, "payload de replay de evento")?;
    tx.commit()
        .await
        .map_err(|e| db_error("COMMIT replay de eventos", &e))?;
    Ok(ReplayWindow {
        events,
        through_event_id: high_watermark,
    })
}

/// Elimina eventos entregados que ya superaron la retención. Las filas aún
/// pendientes del outbox se conservan para no perder eventos confirmados.
///
/// # Errors
/// Errores de SQLite.
pub async fn prune_expired(pool: &SqlitePool, retention_days: u32) -> Result<u64, LarmError> {
    let cutoff = larm_domain::bridge::timestamp(
        chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days)),
    );
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| db_error("BEGIN poda de eventos", &e))?;
    sqlx::query(
        "DELETE FROM event_outbox WHERE delivered_at IS NOT NULL AND event_id IN \
         (SELECT event_id FROM event WHERE occurred_at < ?)",
    )
    .bind(&cutoff)
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("podando outbox de eventos", &e))?;
    let deleted = sqlx::query(
        "DELETE FROM event WHERE occurred_at < ? AND NOT EXISTS \
         (SELECT 1 FROM event_outbox o WHERE o.event_record_id=event.id AND o.delivered_at IS NULL)",
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("podando eventos", &e))?
    .rows_affected();
    tx.commit()
        .await
        .map_err(|e| db_error("COMMIT poda de eventos", &e))?;
    Ok(deleted)
}

/// Lee eventos todavía no confirmados por el dispatcher, siempre en el orden
/// del cursor durable `event_id`.
///
/// # Errors
/// Devuelve `VALIDATION_FAILED` para un límite no válido y errores de SQLite.
pub async fn pending(pool: &SqlitePool, limit: u32) -> Result<Vec<StoredEvent>, LarmError> {
    if !(1..=MAX_DISPATCH_BATCH).contains(&limit) {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "Límite de despacho de eventos no válido",
            "event_dispatch_batch_invalid",
        ));
    }
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT e.event_id, e.event_type, e.occurred_at, e.correlation_id, e.actor_id, \
         e.resource_type, e.resource_id, e.resource_version, e.payload \
         FROM event_outbox o JOIN event e ON e.id=o.event_record_id \
         WHERE o.delivered_at IS NULL ORDER BY o.event_id LIMIT ?",
    )
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(|e| db_error("leyendo eventos pendientes", &e))?;
    decode_rows(rows, "payload de evento pendiente")
}

/// Confirma un batch sólo después de publicarlo. El UPDATE es una única
/// sentencia atómica; si el daemon cae antes de confirmar SQLite, el mismo
/// `event_id` vuelve a publicarse y el consumidor puede deduplicarlo.
///
/// Repetir el ack es idempotente. Los IDs se validan para evitar batches
/// ambiguos o demasiado grandes antes de tocar la base.
///
/// # Errors
/// Devuelve `VALIDATION_FAILED` para un batch no válido y errores de SQLite.
pub async fn mark_dispatched(pool: &SqlitePool, event_ids: &[i64]) -> Result<(), LarmError> {
    if event_ids.len() > MAX_DISPATCH_BATCH as usize
        || event_ids.iter().any(|event_id| *event_id <= 0)
        || event_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != event_ids.len()
    {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "Batch de eventos no válido",
            "event_dispatch_batch_invalid",
        ));
    }
    if event_ids.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| db_error("BEGIN ack de eventos", &e))?;
    let mut pending_count = QueryBuilder::<Sqlite>::new(
        "SELECT count(*) FROM event_outbox WHERE delivered_at IS NULL AND event_id IN (",
    );
    {
        let mut separated = pending_count.separated(",");
        for event_id in event_ids {
            separated.push_bind(event_id);
        }
    }
    pending_count.push(")");
    let expected: i64 = pending_count
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("contando eventos por confirmar", &e))?;

    let delivered_at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let mut update = QueryBuilder::<Sqlite>::new("UPDATE event_outbox SET delivered_at = ");
    update
        .push_bind(delivered_at)
        .push(" WHERE delivered_at IS NULL AND event_id IN (");
    {
        let mut separated = update.separated(",");
        for event_id in event_ids {
            separated.push_bind(event_id);
        }
    }
    update.push(")");
    let changed = update
        .build()
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("confirmando despacho de eventos", &e))?
        .rows_affected();
    if changed != u64::try_from(expected).unwrap_or(0) {
        return Err(db_error(
            "confirmando despacho de eventos",
            &"ack del outbox no confirmó todas las filas pendientes",
        ));
    }
    tx.commit()
        .await
        .map_err(|e| db_error("COMMIT ack de eventos", &e))
}

/// Último `event_id` asignado (0 si no hay eventos).
///
/// # Errors
/// Errores de SQLite.
pub async fn last_event_id(pool: &sqlx::SqlitePool) -> Result<i64, LarmError> {
    sqlx::query_scalar("SELECT coalesce((SELECT max(id) FROM event_sequence_alloc), 0)")
        .fetch_one(pool)
        .await
        .map_err(|e| db_error("último event_id", &e))
}
