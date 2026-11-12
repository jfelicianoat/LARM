//! `artifact_location` y journal de movimientos sobre SQLite (P016).
//!
//! El cambio de registro de un movimiento es una sola transacción que toma
//! el bloqueo de escritura antes de leer, comprueba el fencing del guard,
//! aplica el CAS por `row_version` de cada fila, escribe el paso del journal
//! y un evento `artifact.moved` por fila: o todo o nada.

use async_trait::async_trait;
use larm_domain::artifacts::{ArtifactLocation, MovePhase, MOVE_JOB_KIND};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::JournalStep;
use larm_domain::ports::{
    ArtifactLocationRepository, DuplicateCopy, DuplicateGroup, Fence, LocationSwitch, MoveJournal,
    MoveRecord, MutationContext, VariantArtifact,
};
use larm_domain::units::DecimalU64;
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::events::{append, NewEvent};
use super::guard::write_journal;
use super::pool::db_error;

const COLUMNS: &str =
    "id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,\
relative_path,file_size,sha256,state,verified_at,ownership";

fn enum_from<T: serde::de::DeserializeOwned>(text: &str, column: &str) -> Result<T, LarmError> {
    serde_json::from_value(serde_json::Value::String(text.to_string()))
        .map_err(|e| db_error(column, &e))
}

pub(crate) fn to_location(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactLocation, LarmError> {
    let text = |c: &str| row.try_get::<String, _>(c).map_err(|e| db_error(c, &e));
    let opt = |c: &str| {
        row.try_get::<Option<String>, _>(c)
            .map_err(|e| db_error(c, &e))
    };
    let size: Option<i64> = row
        .try_get("file_size")
        .map_err(|e| db_error("file_size", &e))?;
    Ok(ArtifactLocation {
        id: text("id")?,
        created_at: text("created_at")?,
        updated_at: text("updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: serde_json::from_str(&text("metadata")?).map_err(|e| db_error("metadata", &e))?,
        artifact_id: text("artifact_id")?,
        storage_location_id: text("storage_location_id")?,
        relative_path: text("relative_path")?,
        file_size: size
            .map(|s| {
                u64::try_from(s)
                    .ok()
                    .and_then(DecimalU64::new)
                    .ok_or_else(|| db_error("file_size", &"negativo"))
            })
            .transpose()?,
        sha256: opt("sha256")?,
        state: enum_from(&text("state")?, "state")?,
        verified_at: opt("verified_at")?,
        ownership: enum_from(&text("ownership")?, "ownership")?,
    })
}

/// Inserta ubicaciones desde una transacción compartida. Lo usa también el
/// cierre de una descarga para confirmar catálogo, procedencia y job juntos.
pub(crate) async fn insert_locations_in(
    conn: &mut SqliteConnection,
    rows: &[ArtifactLocation],
    ctx: &MutationContext,
) -> Result<usize, LarmError> {
    let mut created = 0;
    for location in rows {
        let relative = larm_domain::artifacts::normalize_relative(&location.relative_path)?;
        if !larm_domain::bridge::is_uuid_v7(&location.id)
            || !larm_domain::bridge::is_uuid_v7(&location.artifact_id)
            || !larm_domain::bridge::is_uuid_v7(&location.storage_location_id)
            || location.row_version < 1
            || location.sha256.as_deref().is_some_and(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Ubicación de artefacto no válida",
                "artifact_location_invalid",
            ));
        }
        let exists: Option<(Option<i64>, Option<String>, String)> = sqlx::query_as(
            "SELECT file_size,sha256,state FROM artifact_location WHERE artifact_id=? AND storage_location_id=? AND relative_path=?",
        )
        .bind(&location.artifact_id)
        .bind(&location.storage_location_id)
        .bind(&relative)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo artifact_location", &e))?;
        if let Some((size, hash, state)) = exists {
            if size
                != location
                    .file_size
                    .map(|value| i64::try_from(value.get()).unwrap_or(i64::MAX))
                || hash != location.sha256
                || state != location.state.as_str()
            {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La ubicación existente no coincide con el artefacto verificado",
                    "artifact_location_replay_mismatch",
                )
                .with_resource(location.artifact_id.clone()));
            }
            continue;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO artifact_location ({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&location.id)
        .bind(&location.created_at)
        .bind(&location.updated_at)
        .bind(location.row_version)
        .bind(serde_json::to_string(&location.metadata).unwrap_or_else(|_| "{}".into()))
        .bind(&location.artifact_id)
        .bind(&location.storage_location_id)
        .bind(&relative)
        .bind(
            location
                .file_size
                .map(|size| i64::try_from(size.get()).unwrap_or(i64::MAX)),
        )
        .bind(&location.sha256)
        .bind(location.state.as_str())
        .bind(&location.verified_at)
        .bind(location.ownership.as_str())
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("insertando artifact_location", &e))?;
        append(
            conn,
            &NewEvent {
                event_type: "model.state_changed",
                resource_type: "artifact_location",
                resource_id: &location.id,
                resource_version: location.row_version,
                data: serde_json::json!({
                    "action": "registered",
                    "artifact_id": location.artifact_id,
                    "storage_location_id": location.storage_location_id,
                    "ownership": location.ownership.as_str(),
                    "registration_mode": location.metadata.get("registration_mode"),
                }),
            },
            ctx,
        )
        .await?;
        created += 1;
    }
    Ok(created)
}

#[derive(Clone)]
pub struct SqlArtifactLocations {
    pub pool: SqlitePool,
}

#[async_trait]
impl ArtifactLocationRepository for SqlArtifactLocations {
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<ArtifactLocation>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM artifact_location WHERE (created_at, id) > (?, ?) \
             ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando artifact_location", &e))?;
        rows.iter().map(to_location).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<ArtifactLocation>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM artifact_location WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_location", &e))?;
        row.as_ref().map(to_location).transpose()
    }

    async fn for_variant(
        &self,
        variant_id: &str,
    ) -> Result<Option<Vec<VariantArtifact>>, LarmError> {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM model_variant WHERE id = ?")
                .bind(variant_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo model_variant", &e))?;
        if exists.is_none() {
            return Ok(None);
        }
        // Artefactos propios y referenciados; `referenced_by` cuenta variantes
        // distintas (la dueña y las de artifact_reference).
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH arts AS ( \
               SELECT id AS artifact_id FROM model_artifact WHERE variant_id = ?1 \
               UNION SELECT artifact_id FROM artifact_reference WHERE variant_id = ?1), \
             users AS ( \
               SELECT a.artifact_id, m.variant_id AS v FROM arts a JOIN model_artifact m ON m.id = a.artifact_id \
               UNION SELECT r.artifact_id, r.variant_id FROM artifact_reference r JOIN arts a ON a.artifact_id = r.artifact_id) \
             SELECT {cols}, (SELECT count(*) FROM users u WHERE u.artifact_id = l.artifact_id) AS referenced_by \
             FROM artifact_location l JOIN arts a ON a.artifact_id = l.artifact_id ORDER BY l.artifact_id, l.id",
            cols = COLUMNS
                .split(',')
                .map(|c| format!("l.{c}"))
                .collect::<Vec<_>>()
                .join(",")
        )))
        .bind(variant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo ubicaciones de la variante", &e))?;
        rows.iter()
            .map(|row| {
                let referenced_by: i64 = row
                    .try_get("referenced_by")
                    .map_err(|e| db_error("referenced_by", &e))?;
                Ok(VariantArtifact {
                    location: to_location(row)?,
                    referenced_by: u64::try_from(referenced_by).unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>, LarmError>>()
            .map(Some)
    }

    async fn insert_locations(
        &self,
        rows: &[ArtifactLocation],
        ctx: &MutationContext,
    ) -> Result<usize, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE artifact_location SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando artifact_location", &e))?;
        let created = insert_locations_in(&mut tx, rows, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(created)
    }

    async fn at_path(
        &self,
        storage_location_id: &str,
        relative_path: &str,
    ) -> Result<Vec<ArtifactLocation>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM artifact_location WHERE storage_location_id = ? AND relative_path = ? ORDER BY id"
        )))
        .bind(storage_location_id)
        .bind(relative_path)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_location", &e))?;
        rows.iter().map(to_location).collect()
    }

    async fn mark_shared(
        &self,
        storage_location_id: &str,
        relative_path: &str,
        ctx: &MutationContext,
    ) -> Result<usize, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed: Vec<(String, i64)> = sqlx::query_as(
            "UPDATE artifact_location SET ownership = 'SHARED', row_version = row_version + 1, updated_at = ? \
             WHERE storage_location_id = ? AND relative_path = ? AND ownership = 'LARM_MANAGED' RETURNING id, row_version",
        )
        .bind(&now)
        .bind(storage_location_id)
        .bind(relative_path)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("marcando SHARED", &e))?;
        for (id, version) in &changed {
            append(
                &mut tx,
                &NewEvent {
                    event_type: "model.state_changed",
                    resource_type: "artifact_location",
                    resource_id: id,
                    resource_version: *version,
                    data: serde_json::json!({"action": "shared"}),
                },
                ctx,
            )
            .await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(changed.len())
    }

    async fn by_location(
        &self,
        storage_location_id: &str,
    ) -> Result<Vec<ArtifactLocation>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM artifact_location WHERE storage_location_id = ? ORDER BY relative_path, id"
        )))
        .bind(storage_location_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_location", &e))?;
        rows.iter().map(to_location).collect()
    }

    async fn set_state(
        &self,
        id: &str,
        expected: i64,
        state: larm_domain::artifacts::ArtifactState,
        verified_at: Option<&str>,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let from: Option<String> = sqlx::query_scalar(
            "UPDATE artifact_location SET state = ?, verified_at = COALESCE(?, verified_at), updated_at = ?, \
             row_version = row_version + 1 WHERE id = ? AND row_version = ? RETURNING id",
        )
        .bind(state.as_str())
        .bind(verified_at)
        .bind(&now)
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando artifact_location", &e))?;
        if from.is_none() {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La ubicación del artefacto cambió",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "storage.state_changed",
                resource_type: "artifact_location",
                resource_id: id,
                resource_version: expected + 1,
                data: serde_json::json!({"to": state.as_str(), "reverified": verified_at.is_some()}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(expected + 1)
    }

    async fn variants_at(&self, storage_location_id: &str) -> Result<Vec<String>, LarmError> {
        sqlx::query_scalar(
            "SELECT DISTINCT v FROM ( \
               SELECT m.variant_id AS v FROM artifact_location l JOIN model_artifact m ON m.id = l.artifact_id \
               WHERE l.storage_location_id = ?1 \
               UNION SELECT r.variant_id FROM artifact_location l JOIN artifact_reference r ON r.artifact_id = l.artifact_id \
               WHERE l.storage_location_id = ?1) ORDER BY v",
        )
        .bind(storage_location_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo variantes de la ubicación", &e))
    }

    async fn duplicates(&self) -> Result<Vec<DuplicateGroup>, LarmError> {
        let rows: Vec<(String, i64, String, String, String, String)> = sqlx::query_as(
            "SELECT a.sha256, coalesce(a.file_size, 0), a.storage_location_id, s.volume_key, a.relative_path, a.id \
             FROM artifact_location a JOIN storage_location s ON s.id = a.storage_location_id \
             WHERE a.sha256 IN (SELECT sha256 FROM artifact_location WHERE sha256 IS NOT NULL \
                 GROUP BY sha256 HAVING count(DISTINCT storage_location_id || '|' || relative_path) > 1) \
             ORDER BY a.sha256, a.storage_location_id, a.relative_path, a.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("buscando duplicados", &e))?;
        let mut groups: Vec<DuplicateGroup> = Vec::new();
        for (sha256, size, location, volume, path, id) in rows {
            let size = u64::try_from(size).unwrap_or(0);
            if groups.last().is_none_or(|g| g.sha256 != sha256) {
                groups.push(DuplicateGroup {
                    sha256: sha256.clone(),
                    size_bytes: size,
                    copies: vec![],
                    duplicate_bytes: 0,
                    reclaimable_bytes: 0,
                });
            }
            let Some(group) = groups.last_mut() else {
                continue;
            };
            match group
                .copies
                .iter_mut()
                .find(|c| c.storage_location_id == location && c.relative_path == path)
            {
                Some(copy) => copy.artifact_location_ids.push(id),
                None => group.copies.push(DuplicateCopy {
                    storage_location_id: location,
                    volume_key: volume,
                    relative_path: path,
                    artifact_location_ids: vec![id],
                }),
            }
        }
        for g in &mut groups {
            let copies = g.copies.len() as u64;
            g.duplicate_bytes = g.size_bytes.saturating_mul(copies.saturating_sub(1));
            let mut per_volume: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for c in &g.copies {
                *per_volume.entry(c.volume_key.as_str()).or_default() += 1;
            }
            g.reclaimable_bytes = per_volume
                .values()
                .map(|n| g.size_bytes.saturating_mul(n.saturating_sub(1)))
                .sum();
        }
        Ok(groups)
    }

    async fn switch(
        &self,
        switches: &[LocationSwitch],
        fence: &Fence,
        journal: &JournalStep,
        ctx: &MutationContext,
    ) -> Result<Vec<i64>, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // Bloqueo de escritura antes de leer (como BEGIN IMMEDIATE).
        sqlx::query("UPDATE artifact_location SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando artifact_location", &e))?;
        let held: Option<String> = sqlx::query_scalar(
            "SELECT mode FROM resource_lock WHERE resource_id = ? AND owner_id = ? AND generation = ?",
        )
        .bind(fence.resource.key())
        .bind(&fence.owner_id)
        .bind(fence.generation)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo resource_lock", &e))?;
        if held.as_deref() != Some("EXCLUSIVE") {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La operación perdió su lock: no puede cambiar el registro",
                "fencing_lost",
            )
            .with_resource(fence.resource.key()));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut versions = Vec::with_capacity(switches.len());
        for s in switches {
            let relative = larm_domain::artifacts::normalize_relative(&s.relative_path)?;
            let row: Option<(i64, String)> = sqlx::query_as(
                "UPDATE artifact_location SET storage_location_id = ?, relative_path = ?, \
                 sha256 = COALESCE(?, sha256), file_size = COALESCE(?, file_size), \
                 verified_at = COALESCE(?, verified_at), state = 'ONLINE', updated_at = ?, \
                 row_version = row_version + 1 \
                 WHERE id = ? AND row_version = ? RETURNING row_version, artifact_id",
            )
            .bind(&s.storage_location_id)
            .bind(&relative)
            .bind(&s.sha256)
            .bind(s.file_size.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
            .bind(&s.verified_at)
            .bind(&now)
            .bind(&s.artifact_location_id)
            .bind(s.expected_row_version)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("cambiando artifact_location", &e))?;
            let Some((version, artifact_id)) = row else {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La ubicación del artefacto cambió durante el movimiento",
                    "stale_revision",
                )
                .with_resource(s.artifact_location_id.clone()));
            };
            append(
                &mut tx,
                &NewEvent {
                    event_type: "artifact.moved",
                    resource_type: "artifact_location",
                    resource_id: &s.artifact_location_id,
                    resource_version: version,
                    data: serde_json::json!({
                        "artifact_id": artifact_id,
                        "storage_location_id": s.storage_location_id,
                        "job_id": journal.id,
                        "phase": journal.phase,
                    }),
                },
                ctx,
            )
            .await?;
            versions.push(version);
        }
        write_journal(&mut tx, journal).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(versions)
    }
}

/// Journal de movimientos (`operation_journal`, `kind = artifact.move`).
#[derive(Clone)]
pub struct SqlMoveJournal {
    pub pool: SqlitePool,
}

fn to_record(
    (id, phase, plan_hash, resource_ids, payload, updated_at): (
        String,
        String,
        String,
        String,
        String,
        String,
    ),
) -> Result<MoveRecord, LarmError> {
    Ok(MoveRecord {
        id,
        phase,
        plan_hash,
        resource_ids: serde_json::from_str(&resource_ids)
            .map_err(|e| db_error("resource_ids", &e))?,
        payload: serde_json::from_str(&payload).map_err(|e| db_error("payload", &e))?,
        updated_at,
    })
}

#[async_trait]
impl MoveJournal for SqlMoveJournal {
    async fn write(&self, step: &JournalStep) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_journal(&mut tx, step).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn get(&self, id: &str) -> Result<Option<MoveRecord>, LarmError> {
        let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(MOVE_JOB_KIND)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        row.map(to_record).transpose()
    }

    async fn find_planned(&self, plan_hash: &str) -> Result<Option<MoveRecord>, LarmError> {
        let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal              WHERE kind = ? AND phase = ? AND plan_hash = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(MOVE_JOB_KIND)
        .bind(MovePhase::Planned.as_str())
        .bind(plan_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        row.map(to_record).transpose()
    }

    async fn unfinished(&self) -> Result<Vec<MoveRecord>, LarmError> {
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal \
             WHERE kind = ? AND phase NOT IN (?, ?, ?, ?) ORDER BY id",
        )
        .bind(MOVE_JOB_KIND)
        .bind(MovePhase::Planned.as_str())
        .bind(MovePhase::Completed.as_str())
        .bind(MovePhase::Failed.as_str())
        .bind(MovePhase::Cancelled.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        rows.into_iter().map(to_record).collect()
    }
}

/// Journal genérico de un tipo de operación con fases (P018: limpieza y
/// borrado de modelos).
#[derive(Clone)]
pub struct SqlJournal {
    pub pool: SqlitePool,
    pub kind: &'static str,
}

#[async_trait]
impl MoveJournal for SqlJournal {
    async fn write(&self, step: &JournalStep) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_journal(&mut tx, step).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn get(&self, id: &str) -> Result<Option<MoveRecord>, LarmError> {
        let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal \
             WHERE id = ? AND kind = ?",
        )
        .bind(id)
        .bind(self.kind)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        row.map(to_record).transpose()
    }

    async fn find_planned(&self, plan_hash: &str) -> Result<Option<MoveRecord>, LarmError> {
        let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal \
             WHERE kind = ? AND phase = 'PLANNED' AND plan_hash = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(self.kind)
        .bind(plan_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        row.map(to_record).transpose()
    }

    async fn unfinished(&self) -> Result<Vec<MoveRecord>, LarmError> {
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, phase, plan_hash, resource_ids, payload, updated_at FROM operation_journal \
             WHERE kind = ? AND phase NOT IN ('PLANNED','COMPLETED','FAILED','CANCELLED') ORDER BY id",
        )
        .bind(self.kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo operation_journal", &e))?;
        rows.into_iter().map(to_record).collect()
    }
}

/// Jobs vivos de `operation_journal`.
#[derive(Clone)]
pub struct SqlLiveJobs {
    pub pool: SqlitePool,
}

#[async_trait]
impl larm_domain::ports::LiveJobs for SqlLiveJobs {
    async fn live_jobs(&self) -> Result<std::collections::BTreeSet<String>, LarmError> {
        let placeholders = vec!["?"; larm_domain::guard::TERMINAL_PHASES.len() + 1].join(",");
        let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM operation_journal WHERE phase NOT IN ({placeholders})"
        )));
        for p in larm_domain::guard::TERMINAL_PHASES {
            query = query.bind(p);
        }
        query = query.bind("PLANNED");
        Ok(query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error("leyendo operation_journal", &e))?
            .into_iter()
            .collect())
    }
}
