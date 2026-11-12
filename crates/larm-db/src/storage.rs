//! Último estado observado de cada `storage_location` (P012).
//!
//! - `free_bytes` y `last_seen_at` son observación del sistema, no campos que
//!   edite el usuario: se refrescan sin subir `row_version`, así un `PATCH`
//!   con `If-Match` no choca con cada sondeo.
//! - Un cambio de estado, capacidad, sistema de ficheros, clase o letra sí es
//!   una mutación: sube `row_version` y escribe `storage.state_changed` en la
//!   misma transacción.
//! - Una ubicación cuyo volumen no aparece pasa a OFFLINE y conserva sus
//!   datos y su `last_seen_at` (contrato 08: una unidad offline conserva
//!   registros). La letra no interviene: solo la `volume_key`.

use async_trait::async_trait;
use larm_domain::error::LarmError;
use larm_domain::locations::{disk_level, DiskLevel};
use larm_domain::ports::{
    LocationRoot, MutationContext, ObservationContext, StorageStateChange, StorageStateRepository,
};
use larm_domain::storage::{location_state, StorageObservation};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

#[derive(Clone)]
pub struct SqlStorageState {
    pub pool: SqlitePool,
}

struct Current {
    id: String,
    row_version: i64,
    state: String,
    volume_key: String,
    filesystem: String,
    capacity: i64,
    class: String,
    letter: Option<String>,
}

fn to_i64(value: Option<larm_domain::units::DecimalU64>, fallback: i64) -> i64 {
    value
        .and_then(|v| i64::try_from(v.get()).ok())
        .unwrap_or(fallback)
}

fn level_name(level: DiskLevel) -> &'static str {
    match level {
        DiskLevel::Normal => "NORMAL",
        DiskLevel::Warning => "WARNING",
        DiskLevel::Critical => "CRITICAL",
    }
}

async fn apply_volume_alerts(
    conn: &mut SqliteConnection,
    observation: &StorageObservation,
    volume_keys: impl IntoIterator<Item = String>,
    context: &ObservationContext,
    now: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let Some(thresholds) = context.thresholds else {
        return Ok(());
    };
    let settings_revision = context.settings_revision.unwrap_or(0).max(0);
    for volume_key in volume_keys {
        if volume_key.starts_with(larm_domain::locations::UNKNOWN_VOLUME_PREFIX) {
            continue;
        }
        let Some(volume) = observation
            .by_key(&volume_key)
            .filter(|volume| volume.online)
        else {
            // Offline and absent volumes retain the last verified level.
            continue;
        };
        let (Some(free), Some(capacity)) = (volume.free_bytes, volume.capacity_bytes) else {
            continue;
        };
        let Some(level) = disk_level(free.get(), capacity.get(), thresholds) else {
            continue;
        };
        let next = level_name(level);
        let previous_row =
            sqlx::query("SELECT level FROM storage_volume_alert_state WHERE volume_key=?")
                .bind(&volume_key)
                .fetch_optional(&mut *conn)
                .await
                .map_err(|error| db_error("leyendo nivel H09", &error))?;
        let previous = previous_row
            .as_ref()
            .map(|row| row.try_get::<String, _>("level"))
            .transpose()
            .map_err(|error| db_error("decodificando nivel H09", &error))?;

        sqlx::query(
            "INSERT INTO storage_volume_alert_state(volume_key,level,settings_revision,updated_at) VALUES(?,?,?,?) \
             ON CONFLICT(volume_key) DO UPDATE SET level=excluded.level,settings_revision=excluded.settings_revision,updated_at=excluded.updated_at",
        )
        .bind(&volume_key)
        .bind(next)
        .bind(settings_revision)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(|error| db_error("guardando nivel H09", &error))?;

        if previous.as_deref() == Some(next) || (previous.is_none() && next == "NORMAL") {
            continue;
        }

        let recovering = next == "NORMAL";
        let severity = if recovering {
            "info"
        } else if next == "CRITICAL" {
            "error"
        } else {
            "warning"
        };
        let location_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM storage_location WHERE volume_key=? ORDER BY id LIMIT 1",
        )
        .bind(&volume_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|error| db_error("buscando detalle de volumen H09", &error))?;
        let notification_id = uuid::Uuid::now_v7().to_string();
        let previous_label = previous.as_deref().unwrap_or("UNKNOWN");
        let fingerprint = hex::encode(Sha256::digest(volume_key.as_bytes()));
        let dedupe_material = format!(
            "{fingerprint}\0{previous_label}\0{next}\0{settings_revision}\0{}",
            observation.observed_at
        );
        let dedupe_key = hex::encode(Sha256::digest(dedupe_material.as_bytes()));
        let metadata = serde_json::json!({
            "volume_key_hash": fingerprint,
            "from": previous_label,
            "level": next,
            "free_bytes": free.get().to_string(),
            "capacity_bytes": capacity.get().to_string(),
            "drive_letter": volume.identity.drive_letter,
            "settings_revision": settings_revision,
            "observed_at": observation.observed_at,
        });
        let route = location_id
            .as_ref()
            .map_or_else(|| "/storage".to_string(), |id| format!("/storage/{id}"));
        let action = serde_json::json!({"route":route,"resource_id":location_id});
        sqlx::query(
            "INSERT INTO notification(id,created_at,updated_at,row_version,metadata,dedupe_key,severity,message_code,resource_id,action,read_at) \
             VALUES (?,?,?,1,?,?,?,?,?,?,NULL)",
        )
        .bind(&notification_id)
        .bind(now)
        .bind(now)
        .bind(metadata.to_string())
        .bind(&dedupe_key)
        .bind(severity)
        .bind("LOW_DISK_SPACE")
        .bind(location_id.as_deref())
        .bind(action.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|error| db_error("creando alerta H09", &error))?;
        append(
            conn,
            &NewEvent {
                event_type: "notification.created",
                resource_type: "notification",
                resource_id: &notification_id,
                resource_version: 1,
                data: serde_json::json!({
                    "message_code": "LOW_DISK_SPACE",
                    "resource_id": location_id,
                    "level": next,
                }),
            },
            ctx,
        )
        .await?;
    }
    Ok(())
}

#[async_trait]
impl StorageStateRepository for SqlStorageState {
    async fn roots(&self) -> Result<Vec<LocationRoot>, LarmError> {
        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, canonical_path, unc_path FROM storage_location ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| db_error("leyendo raíces", &e))?;
        Ok(rows
            .into_iter()
            .map(|(id, canonical_path, unc_path)| LocationRoot {
                id,
                canonical_path,
                unc_path,
            })
            .collect())
    }

    async fn apply(
        &self,
        observation: &StorageObservation,
        context: &ObservationContext,
        ctx: &MutationContext,
    ) -> Result<Vec<StorageStateChange>, LarmError> {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // Toma el bloqueo de escritura antes de leer: un PATCH concurrente
        // espera y no se mezcla con esta observación.
        sqlx::query("UPDATE storage_location SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando storage_location", &e))?;
        let rows = sqlx::query(
            "SELECT id, row_version, state, volume_key, filesystem, capacity_bytes, performance_class, drive_letter \
             FROM storage_location ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo storage_location", &e))?;
        let mut current = Vec::with_capacity(rows.len());
        for r in &rows {
            let get = |c: &str| r.try_get::<String, _>(c).map_err(|e| db_error(c, &e));
            current.push(Current {
                id: get("id")?,
                row_version: r
                    .try_get("row_version")
                    .map_err(|e| db_error("row_version", &e))?,
                state: get("state")?,
                volume_key: get("volume_key")?,
                filesystem: get("filesystem")?,
                capacity: r
                    .try_get("capacity_bytes")
                    .map_err(|e| db_error("capacity_bytes", &e))?,
                class: get("performance_class")?,
                letter: r
                    .try_get("drive_letter")
                    .map_err(|e| db_error("drive_letter", &e))?,
            });
        }

        let mut changes = Vec::new();
        let volume_keys: std::collections::BTreeSet<String> = observation
            .volumes
            .iter()
            .filter_map(|volume| volume.identity.volume_key.clone())
            .collect();
        for row in current {
            if context.known.as_ref().is_some_and(|k| !k.contains(&row.id)) {
                continue;
            }
            let volume = observation.by_key(&row.volume_key);
            let state = location_state(
                volume,
                context.root_present.get(&row.id).copied(),
                context.thresholds,
            )
            .as_str()
            .to_string();
            let seen = volume.filter(|v| v.online);
            // Solo lo observado cambia los datos; lo desconocido conserva lo anterior.
            let filesystem = seen
                .and_then(|v| v.filesystem.clone())
                .unwrap_or(row.filesystem.clone());
            let capacity = to_i64(seen.and_then(|v| v.capacity_bytes), row.capacity);
            let class = seen.map_or(row.class.clone(), |v| {
                serde_json::to_value(v.performance_class)
                    .ok()
                    .and_then(|c| c.as_str().map(String::from))
                    .unwrap_or(row.class.clone())
            });
            let letter = seen.map_or(row.letter.clone(), |v| v.identity.drive_letter.clone());
            let material = state != row.state
                || filesystem != row.filesystem
                || capacity != row.capacity
                || class != row.class
                || letter != row.letter;
            let free = seen
                .and_then(|v| v.free_bytes)
                .and_then(|f| i64::try_from(f.get()).ok());
            let last_seen = seen.map(|_| observation.observed_at.clone());
            if material {
                let version = row.row_version + 1;
                sqlx::query(
                    "UPDATE storage_location SET state = ?, filesystem = ?, capacity_bytes = ?, performance_class = ?, \
                     drive_letter = ?, free_bytes = COALESCE(?, free_bytes), last_seen_at = COALESCE(?, last_seen_at), \
                     row_version = ?, updated_at = ? WHERE id = ? AND row_version = ?",
                )
                .bind(&state)
                .bind(&filesystem)
                .bind(capacity)
                .bind(&class)
                .bind(&letter)
                .bind(free)
                .bind(&last_seen)
                .bind(version)
                .bind(&now)
                .bind(&row.id)
                .bind(row.row_version)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("actualizando storage_location", &e))?;
                append(
                    &mut tx,
                    &NewEvent {
                        event_type: "storage.state_changed",
                        resource_type: "storage_location",
                        resource_id: &row.id,
                        resource_version: version,
                        data: serde_json::json!({
                            "from": row.state,
                            "to": state,
                            "volume_key": row.volume_key,
                            "observed_at": observation.observed_at,
                        }),
                    },
                    ctx,
                )
                .await?;
                changes.push(StorageStateChange {
                    location_id: row.id,
                    from: row.state,
                    to: state,
                    row_version: version,
                });
            } else if seen.is_some() {
                sqlx::query(
                    "UPDATE storage_location SET free_bytes = COALESCE(?, free_bytes), last_seen_at = ? WHERE id = ?",
                )
                .bind(free)
                .bind(&last_seen)
                .bind(&row.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("refrescando storage_location", &e))?;
            }
        }
        apply_volume_alerts(&mut tx, observation, volume_keys, context, &now, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(changes)
    }
}
