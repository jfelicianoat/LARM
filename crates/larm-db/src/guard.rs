//! Resource Guard y reservas de pool sobre SQLite (P019; contrato 02).
//!
//! Toda adquisición y liberación ocurre en una transacción que toma el
//! bloqueo de escritura antes de leer (equivalente a `BEGIN IMMEDIATE`):
//! dos procesos no pueden ver la misma tabla libre y escribir a la vez.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::{
    compatible, normalize, JournalStep, LockMode, Resource, ResourceKind, TERMINAL_LEASE_STATES,
    TERMINAL_PHASES,
};
use larm_domain::ports::{
    HeldLock, PoolReservationRequest, PoolReservations, Reconciliation, ResourceGuard,
};
use sqlx::SqlitePool;

use super::pool::db_error;

fn mode_from(text: &str) -> Result<LockMode, LarmError> {
    match text {
        "SHARED" => Ok(LockMode::Shared),
        "EXCLUSIVE" => Ok(LockMode::Exclusive),
        other => Err(db_error("mode", &format!("modo desconocido {other}"))),
    }
}

async fn write_lock(tx: &mut sqlx::SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE resource_lock SET generation = generation WHERE 0")
        .execute(&mut *tx)
        .await
        .map(|_| ())
        .map_err(|e| db_error("bloqueando resource_lock", &e))
}

pub(crate) async fn write_journal(
    tx: &mut sqlx::SqliteConnection,
    step: &JournalStep,
) -> Result<(), LarmError> {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let resource_ids = serde_json::to_string(&step.resource_ids).unwrap_or_else(|_| "[]".into());
    if !step.from_phases.is_empty() {
        let placeholders = vec!["?"; step.from_phases.len()].join(",");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE operation_journal SET phase = ?, payload = ?, updated_at = ?, resource_ids = ?, plan_hash = ?              WHERE id = ? AND kind = ? AND phase IN ({placeholders})"
        )))
        .bind(&step.phase)
        .bind(step.payload.to_string())
        .bind(&now)
        .bind(&resource_ids)
        .bind(&step.plan_hash)
        .bind(&step.id)
        .bind(&step.kind);
        for phase in &step.from_phases {
            query = query.bind(phase);
        }
        let done = query
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("escribiendo operation_journal", &e))?;
        if done.rows_affected() == 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La operación ya no está en la fase esperada",
                "journal_phase_conflict",
            )
            .with_resource(step.id.clone()));
        }
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO operation_journal (id, kind, resource_ids, phase, plan_hash, payload, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)          ON CONFLICT(id) DO UPDATE SET phase = excluded.phase, payload = excluded.payload, updated_at = excluded.updated_at,          resource_ids = excluded.resource_ids",
    )
    .bind(&step.id)
    .bind(&step.kind)
    .bind(resource_ids)
    .bind(&step.phase)
    .bind(&step.plan_hash)
    .bind(step.payload.to_string())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map(|_| ())
    .map_err(|e| db_error("escribiendo operation_journal", &e))
}

/// Lease viva de una instancia que sirve la variante (spec §38: un modelo con
/// una lease activa no se mueve ni se borra). Se mira dentro de la misma
/// transacción que toma los locks.
async fn live_lease_on_variant(
    tx: &mut sqlx::SqliteConnection,
    variant_id: &str,
) -> Result<Option<String>, LarmError> {
    let placeholders = vec!["?"; TERMINAL_LEASE_STATES.len()].join(",");
    let mut query = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT l.id FROM runtime_lease l JOIN runtime_instance i ON l.instance_id = i.id          WHERE i.variant_id = ? AND l.state NOT IN ({placeholders}) ORDER BY l.id LIMIT 1"
    )))
    .bind(variant_id);
    for state in TERMINAL_LEASE_STATES {
        query = query.bind(state);
    }
    query
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo runtime_lease", &e))
}

/// ¿Es el titular una lease viva?
async fn is_live_lease(tx: &mut sqlx::SqliteConnection, owner: &str) -> Result<bool, LarmError> {
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM runtime_lease WHERE id = ?")
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo runtime_lease", &e))?;
    Ok(state.is_some_and(|s| !TERMINAL_LEASE_STATES.contains(&s.as_str())))
}

#[derive(Clone)]
pub struct SqlResourceGuard {
    pub pool: SqlitePool,
}

/// Adquisición del mismo Resource Guard dentro de otra transacción.
pub(crate) async fn acquire_in(
    conn: &mut sqlx::SqliteConnection,
    owner_id: &str,
    generation: i64,
    requests: &[(Resource, LockMode)],
    journal: Option<&JournalStep>,
) -> Result<(), LarmError> {
    if generation < 0 {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Generación negativa",
            "generation_invalid",
        ));
    }
    let wanted = normalize(requests);
    write_lock(&mut *conn).await?;
    for (resource, mode) in &wanted {
        let key = resource.key();
        if resource.kind == ResourceKind::Variant && *mode == LockMode::Exclusive {
            if let Some(lease) = live_lease_on_variant(&mut *conn, &resource.id).await? {
                return Err(LarmError::new(
                    ErrorCode::ActiveLease,
                    "El modelo tiene una lease activa: espera, detén la instancia o cancela",
                    "active_lease",
                )
                .with_resource(key)
                .with_resource(lease)
                .retryable(true));
            }
        }
        let held: Vec<(String, String)> = sqlx::query_as(
            "SELECT owner_id, mode FROM resource_lock WHERE resource_id = ? AND owner_id <> ?",
        )
        .bind(&key)
        .bind(owner_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo resource_lock", &e))?;
        for (other, held_mode) in held {
            if !compatible(*mode, mode_from(&held_mode)?) {
                // Todo o nada: la transacción se revierte al soltarse.
                return Err(if is_live_lease(&mut *conn, &other).await? {
                    LarmError::new(
                        ErrorCode::ActiveLease,
                        "El recurso lo está usando una lease activa",
                        "active_lease",
                    )
                } else {
                    LarmError::new(
                        ErrorCode::Conflict,
                        "El recurso está bloqueado por otra operación",
                        "resource_locked",
                    )
                }
                .with_resource(key)
                .retryable(true));
            }
        }
        sqlx::query(
            "INSERT INTO resource_lock (resource_id, owner_id, generation, mode) VALUES (?, ?, ?, ?) \
             ON CONFLICT(resource_id, owner_id, generation) DO UPDATE SET mode = excluded.mode",
        )
        .bind(&key)
        .bind(owner_id)
        .bind(generation)
        .bind(mode.as_str())
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("insertando resource_lock", &e))?;
    }
    if let Some(step) = journal {
        write_journal(&mut *conn, step).await?;
    }
    Ok(())
}

#[async_trait]
impl ResourceGuard for SqlResourceGuard {
    async fn acquire(
        &self,
        owner_id: &str,
        generation: i64,
        requests: &[(Resource, LockMode)],
        journal: Option<&JournalStep>,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        acquire_in(&mut tx, owner_id, generation, requests, journal).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn release(
        &self,
        owner_id: &str,
        generation: i64,
        journal: Option<&JournalStep>,
    ) -> Result<u64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let done = sqlx::query("DELETE FROM resource_lock WHERE owner_id = ? AND generation = ?")
            .bind(owner_id)
            .bind(generation)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando resource_lock", &e))?;
        if let Some(step) = journal {
            write_journal(&mut tx, step).await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(done.rows_affected())
    }

    async fn holds(
        &self,
        owner_id: &str,
        generation: i64,
        resource: &Resource,
        mode: LockMode,
    ) -> Result<bool, LarmError> {
        let held: Option<String> =
            sqlx::query_scalar("SELECT mode FROM resource_lock WHERE resource_id = ? AND owner_id = ? AND generation = ?")
                .bind(resource.key())
                .bind(owner_id)
                .bind(generation)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo resource_lock", &e))?;
        Ok(match held.as_deref() {
            Some("EXCLUSIVE") => true,
            Some("SHARED") => mode == LockMode::Shared,
            _ => false,
        })
    }

    async fn holders(&self, resource: &Resource) -> Result<Vec<HeldLock>, LarmError> {
        let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
            "SELECT resource_id, owner_id, generation, mode FROM resource_lock WHERE resource_id = ? ORDER BY owner_id, generation",
        )
        .bind(resource.key())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo resource_lock", &e))?;
        rows.into_iter()
            .map(|(resource_id, owner_id, generation, mode)| {
                Ok(HeldLock {
                    resource_id,
                    owner_id,
                    generation,
                    mode: mode_from(&mode)?,
                })
            })
            .collect()
    }

    async fn reconcile(&self) -> Result<Reconciliation, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let owners: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT owner_id FROM resource_lock ORDER BY owner_id")
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo titulares", &e))?;
        let mut out = Reconciliation::default();
        for owner in owners {
            let phase: Option<String> =
                sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = ?")
                    .bind(&owner)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| db_error("leyendo operation_journal", &e))?;
            let lease: Option<String> =
                sqlx::query_scalar("SELECT state FROM runtime_lease WHERE id = ?")
                    .bind(&owner)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| db_error("leyendo runtime_lease", &e))?;
            let finished = match (&phase, &lease) {
                (Some(p), _) if TERMINAL_PHASES.contains(&p.as_str()) => Some(true),
                (_, Some(s)) if TERMINAL_LEASE_STATES.contains(&s.as_str()) => Some(true),
                (Some(_), _) | (_, Some(_)) => Some(false),
                (None, None) => None,
            };
            match finished {
                Some(true) => {
                    sqlx::query("DELETE FROM resource_lock WHERE owner_id = ?")
                        .bind(&owner)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| db_error("liberando resource_lock", &e))?;
                    out.released.push(owner);
                }
                Some(false) => out.kept.push(owner),
                None => out.unverified.push(owner),
            }
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(out)
    }

    async fn delete_blockers(&self, artifact_id: &str) -> Result<Vec<String>, LarmError> {
        let variants: Vec<String> = sqlx::query_scalar(
            "SELECT variant_id FROM artifact_reference WHERE artifact_id = ? ORDER BY variant_id",
        )
        .bind(artifact_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_reference", &e))?;
        let locks = self
            .holders(&Resource::new(
                larm_domain::guard::ResourceKind::Artifact,
                artifact_id,
            ))
            .await?;
        Ok(variants
            .into_iter()
            .map(|v| format!("referenced_by_variant:{v}"))
            .chain(
                locks
                    .into_iter()
                    .map(|l| format!("locked_by:{}:{}", l.owner_id, l.mode.as_str())),
            )
            .collect())
    }
}

/// Reservas de memoria por pool (`memory_reservation`).
#[derive(Clone)]
pub struct SqlPoolReservations {
    pub pool: SqlitePool,
}

async fn reserve_memory_pools(
    pool: &SqlitePool,
    owner_id: &str,
    requests: &[PoolReservationRequest],
    replace_all_pools: bool,
) -> Result<(), LarmError> {
    if owner_id.is_empty() || requests.is_empty() {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "La reserva de memoria no es válida",
            "pool_reservation_invalid",
        ));
    }
    let mut requests = requests.to_vec();
    requests.sort_by(|left, right| left.pool_key.cmp(&right.pool_key));
    if requests.iter().any(|request| {
        request.pool_key.is_empty()
            || request.bytes > larm_domain::units::DECIMAL_U64_MAX
            || request.budget > larm_domain::units::DECIMAL_U64_MAX
    }) || requests
        .windows(2)
        .any(|pair| pair[0].pool_key == pair[1].pool_key)
    {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "La reserva de memoria no es válida",
            "pool_reservation_invalid",
        ));
    }

    let mut tx = pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
    sqlx::query("UPDATE memory_reservation SET bytes = bytes WHERE 0")
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("bloqueando memory_reservation", &e))?;
    for request in &requests {
        let others: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(bytes), 0) FROM memory_reservation WHERE pool_key = ? AND owner_id <> ?",
        )
        .bind(&request.pool_key)
        .bind(owner_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("sumando memory_reservation", &e))?;
        let others =
            u64::try_from(others).map_err(|e| db_error("validando memory_reservation", &e))?;
        let total = others.checked_add(request.bytes);
        if total.is_none_or(|total| total > request.budget) {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No hay memoria libre en el pool",
                "pool_budget_exceeded",
            )
            .retryable(true)
            .with_technical(format!(
                "{}: reservado {others} + {} > {}",
                request.pool_key, request.bytes, request.budget
            )));
        }
    }

    if replace_all_pools {
        sqlx::query("DELETE FROM memory_reservation WHERE owner_id = ?")
            .bind(owner_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("sustituyendo memory_reservation", &e))?;
    } else {
        for request in &requests {
            sqlx::query("DELETE FROM memory_reservation WHERE owner_id = ? AND pool_key = ?")
                .bind(owner_id)
                .bind(&request.pool_key)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("sustituyendo memory_reservation", &e))?;
        }
    }
    for request in &requests {
        sqlx::query(
            "INSERT INTO memory_reservation (id, pool_key, owner_id, bytes) VALUES (?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&request.pool_key)
        .bind(owner_id)
        .bind(
            i64::try_from(request.bytes)
                .map_err(|e| db_error("convirtiendo bytes de memory_reservation", &e))?,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando memory_reservation", &e))?;
    }
    tx.commit().await.map_err(|e| db_error("COMMIT", &e))
}

#[async_trait]
impl PoolReservations for SqlPoolReservations {
    async fn reserve(
        &self,
        owner_id: &str,
        pool_key: &str,
        bytes: u64,
        budget: u64,
    ) -> Result<(), LarmError> {
        reserve_memory_pools(
            &self.pool,
            owner_id,
            &[PoolReservationRequest {
                pool_key: pool_key.to_string(),
                bytes,
                budget,
            }],
            false,
        )
        .await
    }

    async fn reserve_many(
        &self,
        owner_id: &str,
        requests: &[PoolReservationRequest],
    ) -> Result<(), LarmError> {
        reserve_memory_pools(&self.pool, owner_id, requests, true).await
    }

    async fn release(&self, owner_id: &str) -> Result<u64, LarmError> {
        let done = sqlx::query("DELETE FROM memory_reservation WHERE owner_id = ?")
            .bind(owner_id)
            .execute(&self.pool)
            .await
            .map_err(|e| db_error("liberando memory_reservation", &e))?;
        Ok(done.rows_affected())
    }

    async fn reserved(&self, pool_key: &str) -> Result<u64, LarmError> {
        let sum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(bytes), 0) FROM memory_reservation WHERE pool_key = ?",
        )
        .bind(pool_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| db_error("sumando memory_reservation", &e))?;
        Ok(u64::try_from(sum).unwrap_or(0))
    }
}
