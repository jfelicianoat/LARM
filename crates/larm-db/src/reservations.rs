//! Reservas de espacio por volumen (P015, `volume_reservation`).
//!
//! La admisión ocurre en una sola transacción que toma el bloqueo de
//! escritura antes de leer: dos trabajos simultáneos no pueden reservar el
//! mismo espacio. La instantánea del espacio libre debe ser reciente (< 5 s).

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{VolumeDemand, VolumeReservationRepository};
use sqlx::SqlitePool;

use super::pool::db_error;

/// Antigüedad máxima de la instantánea de espacio libre (contrato 02).
pub const SNAPSHOT_MAX_AGE_MS: i64 = 5_000;

/// Una reserva por volumen físico: varias demandas del mismo volumen (dos
/// raíces en el mismo disco) se suman en una.
fn merge(demands: &[VolumeDemand]) -> Result<Vec<VolumeDemand>, LarmError> {
    let mut out: Vec<VolumeDemand> = Vec::new();
    for d in demands {
        if let Some(existing) = out.iter_mut().find(|e| e.volume_key == d.volume_key) {
            existing.required_bytes = existing
                .required_bytes
                .checked_add(d.required_bytes)
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ValidationFailed,
                        "Reserva fuera de rango",
                        "size_overflow",
                    )
                })?;
            existing.os_free = existing.os_free.min(d.os_free);
            existing.policy_minimum = existing.policy_minimum.max(d.policy_minimum);
        } else {
            out.push(d.clone());
        }
    }
    Ok(out)
}

#[derive(Clone)]
pub struct SqlVolumeReservations {
    pub pool: SqlitePool,
}

async fn others_in(
    conn: &mut sqlx::SqliteConnection,
    volume_key: &str,
    exclude_job: Option<&str>,
    now: &str,
) -> Result<u64, LarmError> {
    let sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(remaining_bytes), 0) FROM volume_reservation \
         WHERE volume_key = ? AND (? IS NULL OR job_id <> ?) AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(volume_key)
    .bind(exclude_job)
    .bind(exclude_job)
    .bind(now)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| db_error("sumando reservas", &e))?;
    Ok(u64::try_from(sum).unwrap_or(0))
}

/// Admisión reutilizable en una transacción de negocio ya abierta.
pub(crate) async fn admit_in(
    conn: &mut sqlx::SqliteConnection,
    job_id: &str,
    demands: &[VolumeDemand],
    snapshot_at: chrono::DateTime<chrono::Utc>,
    expires_at: Option<String>,
) -> Result<(), LarmError> {
    // Bloqueo de escritura primero (equivalente a BEGIN IMMEDIATE).
    sqlx::query("UPDATE volume_reservation SET remaining_bytes = remaining_bytes WHERE 0")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("bloqueando reservas", &e))?;
    let now_dt = chrono::Utc::now();
    let age = (now_dt - snapshot_at).num_milliseconds();
    if !(0..SNAPSHOT_MAX_AGE_MS).contains(&age) {
        return Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "La medida del espacio libre es demasiado antigua; vuelve a medir",
            "free_snapshot_stale",
        )
        .retryable(true)
        .with_technical(format!("{age} ms")));
    }
    let now = larm_domain::bridge::timestamp(now_dt);
    let demands = merge(demands)?;
    let demands = demands.as_slice();
    let mut shortfalls = Vec::new();
    for d in demands {
        let others = others_in(&mut *conn, &d.volume_key, Some(job_id), &now).await?;
        let usable = d
            .os_free
            .saturating_sub(d.policy_minimum)
            .saturating_sub(others);
        if usable < d.required_bytes {
            shortfalls.push(format!(
                "{}: requiere {} y hay {usable}",
                d.volume_key, d.required_bytes
            ));
        }
    }
    if !shortfalls.is_empty() {
        return Err(LarmError::new(
            ErrorCode::StorageInsufficient,
            "No hay espacio suficiente en el destino",
            "insufficient_space",
        )
        .with_technical(shortfalls.join("; ")));
    }
    // Reemplaza reservas previas del mismo trabajo (readmisión idempotente).
    sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
        .bind(job_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("limpiando reservas", &e))?;
    for d in demands {
        let bytes = i64::try_from(d.required_bytes).map_err(|_| {
            LarmError::new(
                ErrorCode::ValidationFailed,
                "Reserva fuera de rango",
                "size_overflow",
            )
        })?;
        sqlx::query(
            "INSERT INTO volume_reservation (id, volume_key, job_id, total_bytes, remaining_bytes, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&d.volume_key)
        .bind(job_id)
        .bind(bytes)
        .bind(bytes)
        .bind(&expires_at)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("insertando reserva", &e))?;
    }
    Ok(())
}

#[async_trait]
impl VolumeReservationRepository for SqlVolumeReservations {
    async fn others(&self, volume_key: &str, exclude_job: Option<&str>) -> Result<u64, LarmError> {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("conexión", &e))?;
        others_in(&mut conn, volume_key, exclude_job, &now).await
    }

    async fn admit(
        &self,
        job_id: &str,
        demands: &[VolumeDemand],
        snapshot_at: chrono::DateTime<chrono::Utc>,
        expires_at: Option<String>,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        admit_in(&mut tx, job_id, demands, snapshot_at, expires_at).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn consume(
        &self,
        job_id: &str,
        volume_key: &str,
        created_live: u64,
    ) -> Result<(), LarmError> {
        let created = i64::try_from(created_live).unwrap_or(i64::MAX);
        let done = sqlx::query(
            "UPDATE volume_reservation SET remaining_bytes = MAX(total_bytes - ?, 0) WHERE job_id = ? AND volume_key = ?",
        )
        .bind(created)
        .bind(job_id)
        .bind(volume_key)
        .execute(&self.pool)
        .await
        .map_err(|e| db_error("consumiendo reserva", &e))?;
        if done.rows_affected() == 0 {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Reserva desconocida",
                "reservation_not_found",
            )
            .with_resource(job_id.to_string()));
        }
        Ok(())
    }

    async fn release(&self, job_id: &str) -> Result<u64, LarmError> {
        let done = sqlx::query("DELETE FROM volume_reservation WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(|e| db_error("liberando reservas", &e))?;
        Ok(done.rows_affected())
    }
}
