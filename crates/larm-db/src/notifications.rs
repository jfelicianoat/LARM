//! Lectura y actualización CAS de notificaciones persistidas (P113).

use async_trait::async_trait;
use larm_domain::{
    error::{ErrorCode, LarmError},
    ports::{MutationContext, NotificationRepository},
    watchers::Notification,
};
use serde_json::{Map, Value};
use sqlx::{Row, SqlitePool};

use super::{
    events::{append, NewEvent},
    pool::db_error,
};

#[derive(Clone)]
pub struct SqlNotifications {
    pub pool: SqlitePool,
}

const SELECT_BY_ID: &str = "SELECT id,created_at,updated_at,row_version,metadata,dedupe_key,severity,message_code,resource_id,action,read_at FROM notification WHERE id=?";
const SELECT_PAGE: &str = "SELECT id,created_at,updated_at,row_version,metadata,dedupe_key,severity,message_code,resource_id,action,read_at FROM notification WHERE (? IS NULL OR created_at < ? OR (created_at = ? AND id < ?)) ORDER BY created_at DESC,id DESC LIMIT ?";

fn text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<String, LarmError> {
    row.try_get(column)
        .map_err(|error| db_error(column, &error))
}

fn opt_text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Option<String>, LarmError> {
    row.try_get(column)
        .map_err(|error| db_error(column, &error))
}

fn object(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Map<String, Value>, LarmError> {
    let raw = text(row, column)?;
    serde_json::from_str(&raw).map_err(|error| db_error(column, &error))
}

fn notification_row(row: &sqlx::sqlite::SqliteRow) -> Result<Notification, LarmError> {
    let action = opt_text(row, "action")?
        .map(|raw| serde_json::from_str(&raw).map_err(|error| db_error("action", &error)))
        .transpose()?;
    let item = Notification {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|error| db_error("row_version", &error))?,
        metadata: object(row, "metadata")?,
        dedupe_key: text(row, "dedupe_key")?,
        severity: text(row, "severity")?,
        message_code: text(row, "message_code")?,
        resource_id: opt_text(row, "resource_id")?,
        action,
        read_at: opt_text(row, "read_at")?,
    };
    if item.row_version < 1
        || item.dedupe_key.is_empty()
        || !matches!(item.severity.as_str(), "info" | "warning" | "error")
        || item.message_code.is_empty()
    {
        return Err(db_error(
            "notification row",
            &"invalid persisted notification",
        ));
    }
    Ok(item)
}

async fn load(
    pool: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<Notification>, LarmError> {
    sqlx::query(SELECT_BY_ID)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|error| db_error("leyendo notificación", &error))?
        .as_ref()
        .map(notification_row)
        .transpose()
}

fn missing(id: &str) -> LarmError {
    LarmError::new(ErrorCode::NotFound, "Notificación desconocida", "not_found")
        .with_resource(id.to_string())
}

fn stale(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "La notificación cambió desde que se abrió",
        "stale_revision",
    )
    .with_resource(id.to_string())
}

#[async_trait]
impl NotificationRepository for SqlNotifications {
    async fn notification(&self, id: &str) -> Result<Option<Notification>, LarmError> {
        sqlx::query(SELECT_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| db_error("leyendo notificación", &error))?
            .as_ref()
            .map(notification_row)
            .transpose()
    }

    async fn page_notifications(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<Notification>, LarmError> {
        let count = i64::from(limit.clamp(1, 200));
        let after_at = after.as_ref().map(|value| value.0.as_str());
        let after_id = after.as_ref().map(|value| value.1.as_str());
        let rows = sqlx::query(SELECT_PAGE)
            .bind(after_at)
            .bind(after_at)
            .bind(after_at)
            .bind(after_id)
            .bind(count)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| db_error("paginando notificaciones", &error))?;
        rows.iter().map(notification_row).collect()
    }

    async fn set_notification_read(
        &self,
        id: &str,
        if_match: i64,
        read: bool,
        ctx: &MutationContext,
    ) -> Result<Notification, LarmError> {
        if if_match < 1 {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "If-Match fuera de rango",
                "if_match_invalid",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN", &error))?;
        // Bloquea la escritura antes de comparar la revisión para que dos
        // sesiones no confirmen el mismo If-Match.
        sqlx::query("UPDATE notification SET row_version=row_version WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando notificación", &error))?;
        let mut item = load(&mut tx, id).await?.ok_or_else(|| missing(id))?;
        if item.row_version != if_match {
            return Err(stale(id));
        }
        if item.read_at.is_some() == read {
            tx.rollback()
                .await
                .map_err(|error| db_error("ROLLBACK", &error))?;
            return Ok(item);
        }

        let updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let next_version = item.row_version.checked_add(1).ok_or_else(|| {
            LarmError::new(
                ErrorCode::Conflict,
                "No se pudo actualizar la notificación",
                "row_version_overflow",
            )
        })?;
        let read_at = read.then(|| updated_at.clone());
        let updated = sqlx::query(
            "UPDATE notification SET updated_at=?,row_version=?,read_at=? WHERE id=? AND row_version=?",
        )
        .bind(&updated_at)
        .bind(next_version)
        .bind(&read_at)
        .bind(id)
        .bind(if_match)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("actualizando lectura de notificación", &error))?;
        if updated.rows_affected() != 1 {
            return Err(stale(id));
        }
        item.updated_at = updated_at.clone();
        item.row_version = next_version;
        item.read_at = read_at.clone();
        append(
            &mut tx,
            &NewEvent {
                event_type: "notification.read_state_changed",
                resource_type: "notification",
                resource_id: id,
                resource_version: next_version,
                data: serde_json::json!({"read": read, "read_at": read_at}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT", &error))?;
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    async fn seeded() -> (tempfile::TempDir, SqlNotifications, String) {
        let (dir, db) = crate::testing::temp_migrated().await.unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        sqlx::query(
            "INSERT INTO notification(id,created_at,updated_at,row_version,metadata,dedupe_key,severity,message_code,resource_id,action,read_at) VALUES(?,?,?,1,'{}','disk:volume:warning','warning','LOW_DISK',NULL,'{}',NULL)",
        )
        .bind(&id)
        .bind(&now)
        .bind(&now)
        .execute(&db.pool)
        .await
        .unwrap();
        (dir, SqlNotifications { pool: db.pool }, id)
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "notification-test".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    #[tokio::test]
    async fn lectura_cas_y_outbox_son_atomicos_y_no_reemiten_en_noop() {
        let (_dir, repository, id) = seeded().await;
        let read = repository
            .set_notification_read(&id, 1, true, &context())
            .await
            .unwrap();
        assert_eq!(read.row_version, 2);
        assert!(read.read_at.is_some());
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        assert_eq!(events, 1);

        let noop = repository
            .set_notification_read(&id, 2, true, &context())
            .await
            .unwrap();
        assert_eq!(noop.row_version, 2);
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        assert_eq!(events, 1);
        let (kind, payload): (String, String) =
            sqlx::query_as("SELECT event_type,payload FROM event ORDER BY event_id")
                .fetch_one(&repository.pool)
                .await
                .unwrap();
        assert_eq!(kind, "notification.read_state_changed");
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["read"], true);
    }

    #[tokio::test]
    async fn revision_obsoleta_no_mutate_y_page_respeta_cursor_estable() {
        let (_dir, repository, id) = seeded().await;
        repository
            .set_notification_read(&id, 1, true, &context())
            .await
            .unwrap();
        let error = repository
            .set_notification_read(&id, 1, false, &context())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleRevision);
        let page = repository.page_notifications(None, 1).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, id);
        assert_eq!(page[0].row_version, 2);
    }
}
