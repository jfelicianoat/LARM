//! P024: selección CAS, conservación del último bueno y rollback persistente.
//! La selección, el journal y el outbox se confirman juntos. Este módulo no
//! mide ficheros directamente ni cambia el lifecycle de instalaciones: exige
//! el puerto físico inyectado además del recibo/journal vigentes.

use async_trait::async_trait;
use larm_domain::adapters::{AdapterSelection, AdapterSelectionChange};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterQualification, AdapterQualificationGuard};
use larm_domain::ports::{AdapterSelectionRepository, MutationContext};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::sync::Arc;

use crate::events::{append, NewEvent};
use crate::pool::db_error;

#[derive(Clone)]
pub struct SqlAdapterSelections {
    pub pool: SqlitePool,
    pub qualification: Option<Arc<dyn AdapterQualification>>,
}

async fn read(
    conn: &mut SqliteConnection,
    adapter_id: &str,
) -> Result<Option<AdapterSelection>, LarmError> {
    let row = sqlx::query(
        "SELECT adapter_id,active_installation_id,previous_installation_id,enabled,row_version,updated_at \
         FROM adapter_selection WHERE adapter_id=?",
    )
    .bind(adapter_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo selección de adaptador", &e))?;
    row.map(|r| {
        let err = |e| db_error("selección de adaptador", &e);
        Ok(AdapterSelection {
            adapter_id: r.try_get("adapter_id").map_err(err)?,
            active_installation_id: r.try_get("active_installation_id").map_err(err)?,
            previous_installation_id: r.try_get("previous_installation_id").map_err(err)?,
            enabled: r.try_get("enabled").map_err(err)?,
            row_version: r.try_get("row_version").map_err(err)?,
            updated_at: r.try_get("updated_at").map_err(err)?,
        })
    })
    .transpose()
}

fn invalid(reason: &str, id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "La instalación no permite ese cambio de selección",
        reason,
    )
    .with_resource(id)
}

/// Verifica bajo el mismo bloqueo que la selección. P026 es quien puede
/// producir VALIDATED tras su suite; INSTALLED nunca basta para promover.
async fn eligible(
    conn: &mut SqliteConnection,
    id: &str,
    adapter_id: &str,
    ctx: &MutationContext,
    gate: Option<&Arc<dyn AdapterQualification>>,
    guards: &mut Vec<Box<dyn AdapterQualificationGuard>>,
) -> Result<(String, i64), LarmError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM adapter_installation WHERE id=?",
        crate::adapters::COLUMNS
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("comprobando instalación seleccionable", &e))?;
    let row = row.ok_or_else(|| {
        LarmError::new(
            ErrorCode::NotFound,
            "Instalación de adaptador desconocida",
            "not_found",
        )
        .with_resource(id)
    })?;
    let installation = crate::adapters::to_installation(&row)?;
    if !installation.adapter_id.eq_ignore_ascii_case(adapter_id) {
        return Err(invalid("adapter_identity_mismatch", id));
    }
    if installation.trust == larm_domain::adapters::AdapterTrust::Revoked {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "El paquete está revocado",
            "adapter_revoked",
        )
        .with_resource(id));
    }
    if installation.state != larm_domain::adapters::AdapterState::Validated {
        return Err(invalid("adapter_not_validated", id));
    }
    guards.push(crate::conformance::qualify_current(conn, &installation, ctx, gate).await?);
    Ok((installation.family, installation.row_version))
}

#[async_trait]
impl AdapterSelectionRepository for SqlAdapterSelections {
    async fn selection(&self, adapter_id: &str) -> Result<Option<AdapterSelection>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        read(&mut conn, adapter_id).await
    }

    async fn change_selection(
        &self,
        adapter_id: &str,
        expected_row_version: i64,
        change: &AdapterSelectionChange,
        ctx: &MutationContext,
    ) -> Result<AdapterSelection, LarmError> {
        let mut guards = Vec::new();
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // El UPDATE toma el bloqueo de escritura incluso si aún no hay fila.
        // Así dos primeras promociones tampoco pueden leer ambas revisión 0.
        sqlx::query("UPDATE adapter_selection SET row_version=row_version WHERE adapter_id=?")
            .bind(adapter_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando selección de adaptador", &e))?;
        let before = read(&mut tx, adapter_id).await?;
        let version = before.as_ref().map_or(0, |s| s.row_version);
        if expected_row_version < 0 || version != expected_row_version {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La selección cambió; vuelve a consultarla",
                "adapter_selection_stale",
            )
            .with_resource(adapter_id));
        }
        let mut next = before.clone().unwrap_or(AdapterSelection {
            adapter_id: adapter_id.into(),
            active_installation_id: None,
            previous_installation_id: None,
            enabled: false,
            row_version: 0,
            updated_at: String::new(),
        });
        let selected_revision;
        match change {
            AdapterSelectionChange::Promote { installation_id } => {
                if next.active_installation_id.as_ref() == Some(installation_id) {
                    return Err(invalid("adapter_already_selected", installation_id));
                }
                let (family, revision) = eligible(
                    &mut tx,
                    installation_id,
                    adapter_id,
                    ctx,
                    self.qualification.as_ref(),
                    &mut guards,
                )
                .await?;
                if let Some(old) = &next.active_installation_id {
                    let old_family: String =
                        sqlx::query_scalar("SELECT family FROM adapter_installation WHERE id=?")
                            .bind(old)
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(|e| db_error("leyendo familia activa", &e))?;
                    if family != old_family {
                        return Err(invalid("adapter_family_mismatch", installation_id));
                    }
                    // Sólo se conserva como último bueno si sigue siendo
                    // elegible. No se pierde un anterior sano por uno fallido.
                    match eligible(
                        &mut tx,
                        old,
                        adapter_id,
                        ctx,
                        self.qualification.as_ref(),
                        &mut guards,
                    )
                    .await
                    {
                        Ok(_) => next.previous_installation_id = Some(old.clone()),
                        Err(e)
                            if matches!(
                                e.code,
                                ErrorCode::InvalidStateTransition
                                    | ErrorCode::Forbidden
                                    | ErrorCode::StaleRevision
                                    | ErrorCode::IntegrityFailed
                                    | ErrorCode::ResourceUnavailable
                                    | ErrorCode::ValidationFailed
                                    | ErrorCode::ApprovalRequired
                            ) => {}
                        Err(e) => return Err(e),
                    }
                }
                next.active_installation_id = Some(installation_id.clone());
                // Re-promover el anterior no puede dejarlo en ambos campos.
                if next.previous_installation_id == next.active_installation_id {
                    next.previous_installation_id = None;
                }
                selected_revision = revision;
            }
            AdapterSelectionChange::Rollback => {
                let target = next
                    .previous_installation_id
                    .clone()
                    .ok_or_else(|| invalid("adapter_rollback_unavailable", adapter_id))?;
                let (_, revision) = eligible(
                    &mut tx,
                    &target,
                    adapter_id,
                    ctx,
                    self.qualification.as_ref(),
                    &mut guards,
                )
                .await?;
                next.active_installation_id = Some(target);
                next.previous_installation_id = None;
                selected_revision = revision;
            }
            AdapterSelectionChange::SetEnabled { enabled } => {
                let active = next
                    .active_installation_id
                    .as_deref()
                    .ok_or_else(|| invalid("adapter_selection_missing", adapter_id))?;
                // Deshabilitar sigue siendo posible después de una revocación
                // o fallo. Habilitar exige una instalación aún válida.
                selected_revision = if *enabled {
                    eligible(
                        &mut tx,
                        active,
                        adapter_id,
                        ctx,
                        self.qualification.as_ref(),
                        &mut guards,
                    )
                    .await?
                    .1
                } else {
                    sqlx::query_scalar("SELECT row_version FROM adapter_installation WHERE id=?")
                        .bind(active)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| db_error("leyendo revisión activa", &e))?
                };
                if next.enabled == *enabled {
                    for guard in &guards {
                        guard.recheck().await?;
                    }
                    tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
                    return Ok(next);
                }
                next.enabled = *enabled;
            }
        }
        next.row_version = version
            .checked_add(1)
            .ok_or_else(|| invalid("adapter_selection_revision_exhausted", adapter_id))?;
        next.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        sqlx::query(
            "INSERT INTO adapter_selection(adapter_id,active_installation_id,previous_installation_id,enabled,row_version,updated_at) \
             VALUES (?,?,?,?,?,?) ON CONFLICT(adapter_id) DO UPDATE SET active_installation_id=excluded.active_installation_id,\
             previous_installation_id=excluded.previous_installation_id,enabled=excluded.enabled,row_version=excluded.row_version,updated_at=excluded.updated_at",
        )
        .bind(&next.adapter_id)
        .bind(&next.active_installation_id)
        .bind(&next.previous_installation_id)
        .bind(next.enabled)
        .bind(next.row_version)
        .bind(&next.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando selección de adaptador", &e))?;
        let resources: Vec<&str> = next
            .active_installation_id
            .iter()
            .chain(next.previous_installation_id.iter())
            .map(String::as_str)
            .collect();
        let plan_hash = larm_domain::canonical::sha256_hex(&serde_json::json!({
            "adapter_id":next.adapter_id,"expected_row_version":expected_row_version.to_string(),"change":change
        })).map_err(|e| db_error("canonizando cambio de selección", &e))?;
        sqlx::query(
            "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
             VALUES (?,'adapter.selection',?,'COMPLETED',?,?,?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(serde_json::to_string(&resources).map_err(|e| db_error("resources", &e))?)
        .bind(plan_hash)
        .bind(serde_json::json!({"actor_id":ctx.actor_id,"correlation_id":ctx.correlation_id,
            "change":change,"before":before,"result":next}).to_string())
        .bind(&next.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("registrando cambio de selección", &e))?;
        let active = next
            .active_installation_id
            .as_deref()
            .ok_or_else(|| invalid("adapter_selection_missing", adapter_id))?;
        append(&mut tx, &NewEvent {
            event_type: "adapter.updated",
            resource_type: "adapter_installation",
            resource_id: active,
            resource_version: selected_revision,
            data: serde_json::json!({"action":"selection_changed","change":change,"selection":next}),
        }, ctx).await?;
        for guard in &guards {
            guard.recheck().await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(next)
    }
}
