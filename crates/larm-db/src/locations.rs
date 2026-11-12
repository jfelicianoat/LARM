//! Repositorio SQLite de `storage_location` (P014). Cada mutación escribe
//! `storage.state_changed` en el outbox en la misma transacción.
//!
//! El borrado se niega si la ubicación tiene artefactos
//! (`location_has_artifacts`) o la usa una instalación de runtime o una
//! descarga (`location_in_use`): quitar una ubicación con datos es una
//! migración explícita, no un `DELETE`.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{LocationFilter, MutationContext, StorageLocationRepository};
use larm_domain::units::DecimalU64;
use sqlx::{Row, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

const COLUMNS: &str = "id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,volume_guid,\
drive_letter,unc_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,\
last_seen_at,volume_key,performance_class";

fn enum_from<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T, LarmError> {
    serde_json::from_value(serde_json::Value::String(text.into())).map_err(|e| db_error(what, &e))
}

fn bytes(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<DecimalU64, LarmError> {
    let raw: i64 = row.try_get(column).map_err(|e| db_error(column, &e))?;
    u64::try_from(raw)
        .ok()
        .and_then(DecimalU64::new)
        .ok_or_else(|| db_error(column, &format!("{raw} fuera de rango")))
}

pub(crate) fn to_location(row: &sqlx::sqlite::SqliteRow) -> Result<StorageLocation, LarmError> {
    let text = |c: &str| row.try_get::<String, _>(c).map_err(|e| db_error(c, &e));
    let opt = |c: &str| {
        row.try_get::<Option<String>, _>(c)
            .map_err(|e| db_error(c, &e))
    };
    let priority: i64 = row
        .try_get("priority")
        .map_err(|e| db_error("priority", &e))?;
    let auto: i64 = row
        .try_get("auto_placement")
        .map_err(|e| db_error("auto_placement", &e))?;
    Ok(StorageLocation {
        id: text("id")?,
        created_at: text("created_at")?,
        updated_at: text("updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: serde_json::from_str(&text("metadata")?).map_err(|e| db_error("metadata", &e))?,
        name: text("name")?,
        root_path: text("root_path")?,
        canonical_path: text("canonical_path")?,
        volume_guid: opt("volume_guid")?,
        drive_letter: opt("drive_letter")?,
        unc_path: opt("unc_path")?,
        filesystem: text("filesystem")?,
        storage_type: text("storage_type")?,
        capacity_bytes: bytes(row, "capacity_bytes")?,
        free_bytes: bytes(row, "free_bytes")?,
        reserved_bytes: bytes(row, "reserved_bytes")?,
        priority: u32::try_from(priority).map_err(|e| db_error("priority", &e))?,
        auto_placement: auto != 0,
        state: enum_from(&text("state")?, "state")?,
        last_seen_at: opt("last_seen_at")?,
        volume_key: text("volume_key")?,
        performance_class: enum_from(&text("performance_class")?, "performance_class")?,
    })
}

fn as_i64(v: DecimalU64) -> i64 {
    // DecimalU64 nunca supera i64::MAX.
    i64::try_from(v.get()).unwrap_or(i64::MAX)
}

fn class_text(l: &StorageLocation) -> String {
    serde_json::to_value(l.performance_class)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "UNKNOWN".into())
}

async fn event(
    conn: &mut sqlx::SqliteConnection,
    l: &StorageLocation,
    action: &str,
    from: Option<LocationState>,
    ctx: &MutationContext,
) -> Result<String, LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "storage.state_changed",
            resource_type: "storage_location",
            resource_id: &l.id,
            resource_version: l.row_version,
            data: serde_json::json!({
                "action": action,
                "from": from.map(LocationState::as_str),
                "to": l.state.as_str(),
                "volume_key": l.volume_key,
            }),
        },
        ctx,
    )
    .await
}

#[derive(Clone)]
pub struct SqlStorageLocations {
    pub pool: SqlitePool,
}

#[async_trait]
impl StorageLocationRepository for SqlStorageLocations {
    async fn insert(&self, l: &StorageLocation, ctx: &MutationContext) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO storage_location ({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&l.id)
        .bind(&l.created_at)
        .bind(&l.updated_at)
        .bind(l.row_version)
        .bind(serde_json::to_string(&l.metadata).unwrap_or_else(|_| "{}".into()))
        .bind(&l.name)
        .bind(&l.root_path)
        .bind(&l.canonical_path)
        .bind(&l.volume_guid)
        .bind(&l.drive_letter)
        .bind(&l.unc_path)
        .bind(&l.filesystem)
        .bind(&l.storage_type)
        .bind(as_i64(l.capacity_bytes))
        .bind(as_i64(l.free_bytes))
        .bind(as_i64(l.reserved_bytes))
        .bind(i64::from(l.priority))
        .bind(i64::from(l.auto_placement))
        .bind(l.state.as_str())
        .bind(&l.last_seen_at)
        .bind(&l.volume_key)
        .bind(class_text(l))
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando storage_location", &e))?;
        event(&mut tx, l, "created", None, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn get(&self, id: &str) -> Result<Option<StorageLocation>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM storage_location WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo storage_location", &e))?;
        row.as_ref().map(to_location).transpose()
    }

    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &LocationFilter,
    ) -> Result<Vec<StorageLocation>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let q = filter
            .q
            .as_deref()
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{}%", q.to_lowercase()));
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM storage_location WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR lower(name) LIKE ? OR lower(root_path) LIKE ?) \
             AND (? IS NULL OR state = ?) ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(&q)
        .bind(&q)
        .bind(&q)
        .bind(filter.state.map(LocationState::as_str))
        .bind(filter.state.map(LocationState::as_str))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando storage_location", &e))?;
        rows.iter().map(to_location).collect()
    }

    async fn all(&self) -> Result<Vec<StorageLocation>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM storage_location ORDER BY created_at, id"
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando storage_location", &e))?;
        rows.iter().map(to_location).collect()
    }

    async fn update(
        &self,
        l: &StorageLocation,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<StorageLocation, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let previous: Option<(i64, String)> =
            sqlx::query_as("SELECT row_version, state FROM storage_location WHERE id = ?")
                .bind(&l.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo storage_location", &e))?;
        let Some((version, state)) = previous else {
            return Err(
                LarmError::new(ErrorCode::NotFound, "Ubicación desconocida", "not_found")
                    .with_resource(l.id.clone()),
            );
        };
        if version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La ubicación cambió desde que la leíste",
                "stale_revision",
            )
            .with_resource(l.id.clone()));
        }
        let mut updated = l.clone();
        updated.row_version = version + 1;
        sqlx::query(
            "UPDATE storage_location SET updated_at = ?, row_version = ?, metadata = ?, name = ?, root_path = ?, canonical_path = ?, \
             volume_guid = ?, drive_letter = ?, unc_path = ?, filesystem = ?, storage_type = ?, capacity_bytes = ?, free_bytes = ?, \
             reserved_bytes = ?, priority = ?, auto_placement = ?, state = ?, last_seen_at = ?, volume_key = ?, performance_class = ? \
             WHERE id = ? AND row_version = ?",
        )
        .bind(&updated.updated_at)
        .bind(updated.row_version)
        .bind(serde_json::to_string(&updated.metadata).unwrap_or_else(|_| "{}".into()))
        .bind(&updated.name)
        .bind(&updated.root_path)
        .bind(&updated.canonical_path)
        .bind(&updated.volume_guid)
        .bind(&updated.drive_letter)
        .bind(&updated.unc_path)
        .bind(&updated.filesystem)
        .bind(&updated.storage_type)
        .bind(as_i64(updated.capacity_bytes))
        .bind(as_i64(updated.free_bytes))
        .bind(as_i64(updated.reserved_bytes))
        .bind(i64::from(updated.priority))
        .bind(i64::from(updated.auto_placement))
        .bind(updated.state.as_str())
        .bind(&updated.last_seen_at)
        .bind(&updated.volume_key)
        .bind(class_text(&updated))
        .bind(&updated.id)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando storage_location", &e))?;
        let from: Option<LocationState> =
            serde_json::from_value(serde_json::Value::String(state)).ok();
        event(&mut tx, &updated, "updated", from, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(updated)
    }

    async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // Toma el bloqueo de escritura antes de comprobar referencias.
        sqlx::query("UPDATE storage_location SET row_version = row_version WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando storage_location", &e))?;
        let Some(row) = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM storage_location WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo storage_location", &e))?
        else {
            return Err(
                LarmError::new(ErrorCode::NotFound, "Ubicación desconocida", "not_found")
                    .with_resource(id.to_string()),
            );
        };
        let mut location = to_location(&row)?;
        if location.row_version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La ubicación cambió desde que la leíste",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        }
        let artifacts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM artifact_location WHERE storage_location_id = ?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("contando artefactos", &e))?;
        if artifacts > 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La ubicación tiene modelos: muévelos antes de quitarla",
                "location_has_artifacts",
            )
            .with_resource(id.to_string()));
        }
        let in_use: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM runtime_installation WHERE storage_location_id = ?) + \
             (SELECT COUNT(*) FROM download_job WHERE storage_location_id = ?)",
        )
        .bind(id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("contando usos", &e))?;
        if in_use > 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La ubicación la usan runtimes o descargas",
                "location_in_use",
            )
            .with_resource(id.to_string()));
        }
        sqlx::query("DELETE FROM storage_location WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando storage_location", &e))?;
        location.row_version += 1;
        let from = location.state;
        event(&mut tx, &location, "deleted", Some(from), ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn artifact_count(&self, id: &str) -> Result<u64, LarmError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM artifact_location WHERE storage_location_id = ?",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| db_error("contando artefactos", &e))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}
