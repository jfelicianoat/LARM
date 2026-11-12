//! P026: commit de conformidad sobre el snapshot registrado. No ejecuta suites
//! ni comprueba archivos: el coordinador debe medirlos y esperar su IO primero.
use crate::adapters::{to_installation, COLUMNS};
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{
    committed_installation_hash, hash, Outcome, Receipt, Run, JOURNAL_KIND, METADATA_KEY,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterConformanceRepository, MutationContext};
use serde_json::{json, Value};
use sqlx::{SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlAdapterConformance {
    pub pool: SqlitePool,
}

fn conflict(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo confirmar la conformidad del adaptador",
        reason,
    )
}
async fn journal(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<(String, String, String, String, String)>, LarmError> {
    sqlx::query_as(
        "SELECT kind,phase,plan_hash,payload,resource_ids FROM operation_journal WHERE id=?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo conformidad", &e))
}
fn parse_receipt(row: &(String, String, String, String, String)) -> Result<Receipt, LarmError> {
    let payload: Value = serde_json::from_str(&row.3)
        .map_err(|e| db_error("leyendo evidencia de conformidad", &e))?;
    let receipt: Receipt = serde_json::from_value(payload["receipt"].clone())
        .map_err(|e| db_error("leyendo recibo de conformidad", &e))?;
    let run: Run = serde_json::from_value(payload["run"].clone())
        .map_err(|e| db_error("leyendo binding de conformidad", &e))?;
    let resources: Vec<String> = serde_json::from_str(&row.4)
        .map_err(|e| db_error("leyendo recursos de conformidad", &e))?;
    let revision = run
        .installation_revision
        .parse::<i64>()
        .ok()
        .and_then(|r| r.checked_add(1))
        .map(|r| r.to_string());
    let evidence = if receipt.passed {
        Some(hash(&payload["outcome"]["proof"])?)
    } else {
        None
    };
    if row.0 != JOURNAL_KIND
        || row.1
            != if receipt.passed {
                "COMPLETED"
            } else {
                "FAILED"
            }
        || receipt.run_sha256 != row.2
        || run.digest()? != row.2
        || receipt.run_id != run.id
        || receipt.installation_id != run.installation_id
        || Some(&receipt.installation_revision) != revision.as_ref()
        || receipt.package_sha256 != run.package_sha256
        || receipt.committed_installation_sha256.len() != 64
        || !receipt
            .committed_installation_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || hash(&payload["outcome"])? != receipt.outcome_sha256
        || receipt.evidence_sha256 != evidence
        || payload["outcome"]["status"].as_str()
            != Some(if receipt.passed { "PASSED" } else { "FAILED" })
        || payload["schema_version"] != "1.0"
        || resources != vec![run.installation_id.clone()]
        || payload["actor_id"].as_str() != Some(run.actor_id.as_str())
        || chrono::DateTime::parse_from_rfc3339(&receipt.completed_at).is_err()
        || !receipt.completed_at.ends_with('Z')
    {
        return Err(conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_journal_invalid",
        ));
    }
    Ok(receipt)
}

/// Comprueba el recibo y su journal dentro de la transacción que consume la
/// instalación. No sustituye la comprobación física del paquete del host.
pub(crate) async fn require_current(
    conn: &mut SqliteConnection,
    installation: &AdapterInstallation,
) -> Result<Run, LarmError> {
    let receipt: Receipt = installation
        .metadata
        .get(METADATA_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| {
            conflict(
                ErrorCode::InvalidStateTransition,
                "conformance_receipt_missing",
            )
        })?;
    if !receipt.matches_committed(installation)? {
        return Err(conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_receipt_stale",
        ));
    }
    let row = journal(conn, &receipt.run_id).await?.ok_or_else(|| {
        conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_journal_missing",
        )
    })?;
    let recorded = parse_receipt(&row).map_err(|_| {
        conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_journal_invalid",
        )
    })?;
    if recorded != receipt {
        return Err(conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_receipt_mismatch",
        ));
    }
    let payload: Value = serde_json::from_str(&row.3).map_err(|_| {
        conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_journal_invalid",
        )
    })?;
    serde_json::from_value(payload["run"].clone()).map_err(|_| {
        conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_journal_invalid",
        )
    })
}

/// Resuelve journal y sondeos pendientes; no concede autorización física de IO.
pub(crate) async fn require_consumer_current(
    conn: &mut SqliteConnection,
    installation: &AdapterInstallation,
) -> Result<Run, LarmError> {
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operation_journal WHERE kind='adapter.probe' AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.target')=?)").bind(&installation.id).fetch_one(&mut *conn).await.map_err(|e|db_error("sondeo activo",&e))?;
    if active {
        return Err(conflict(
            ErrorCode::InvalidStateTransition,
            "adapter_probe_active",
        ));
    }
    let run = require_current(conn, installation).await?;
    let pending:Option<String>=sqlx::query_scalar("SELECT id FROM operation_journal WHERE kind='adapter.probe' AND phase='FAILED' AND json_extract(payload,'$.target')=? AND json_extract(payload,'$.installation_sha256')=? AND json_extract(payload,'$.receipt') IS NULL AND COALESCE(json_extract(payload,'$.authority_pending'),1)=1 ORDER BY id LIMIT 1").bind(&installation.id).bind(larm_domain::adapter_probes::snapshot(installation)?).fetch_optional(&mut *conn).await.map_err(|e|db_error("sondeo sin confirmar",&e))?;
    if let Some(id) = pending {
        crate::adapter_probes::load(conn, &id)
            .await?
            .ok_or_else(|| conflict(ErrorCode::InvalidStateTransition, "probe_journal_invalid"))?;
        return Err(conflict(
            ErrorCode::InvalidStateTransition,
            "adapter_probe_unconfirmed",
        ));
    }
    Ok(run)
}

/// El guard físico vive hasta COMMIT; su ausencia deniega el consumo.
pub(crate) async fn qualify_current(
    conn: &mut SqliteConnection,
    installation: &AdapterInstallation,
    ctx: &MutationContext,
    gate: Option<&std::sync::Arc<dyn larm_domain::ports::AdapterQualification>>,
) -> Result<Box<dyn larm_domain::ports::AdapterQualificationGuard>, LarmError> {
    let run = require_consumer_current(conn, installation).await?;
    let gate = gate.ok_or_else(|| {
        conflict(
            ErrorCode::InvalidStateTransition,
            "conformance_physical_gate_unavailable",
        )
    })?;
    gate.acquire(installation, &run, ctx).await
}
impl SqlAdapterConformance {
    /// Resuelve la instalación y el `Run` actual desde SQLite para un
    /// consumidor. Verifica recibo, journal y sondas pendientes en la misma
    /// transacción; el caller aún debe adquirir la cualificación física P022.
    pub async fn current_for_consumer(
        &self,
        installation_id: &str,
    ) -> Result<Option<(AdapterInstallation, Run)>, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(installation_id) {
            return Err(conflict(
                ErrorCode::ValidationFailed,
                "conformance_installation_id_invalid",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN consumer conformance", &e))?;
        let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
            .bind(installation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("leyendo instalación cualificada", &e))?;
        let Some(row) = row else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT consumer conformance", &e))?;
            return Ok(None);
        };
        let installation = crate::adapters::to_installation(&row)?;
        let run = require_consumer_current(&mut tx, &installation).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT consumer conformance", &e))?;
        Ok(Some((installation, run)))
    }

    async fn commit_with_probe(
        &self,
        run: &Run,
        outcome: &Outcome,
        ctx: &MutationContext,
        probe: Option<&str>,
    ) -> Result<Receipt, LarmError> {
        run.validate()?;
        outcome.check_run(run)?;
        if run.actor_id != ctx.actor_id {
            return Err(conflict(ErrorCode::Forbidden, "conformance_actor_mismatch"));
        }
        if ctx.actor_id.is_empty() || !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id) {
            return Err(conflict(
                ErrorCode::ValidationFailed,
                "conformance_context_invalid",
            ));
        }
        let run_hash = run.digest()?;
        let outcome_hash = hash(outcome)?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // Toma el lock de escritura antes de leer. Dos commits del mismo
        // snapshot no pueden observar ambos la revisión inicial.
        sqlx::query("UPDATE adapter_installation SET row_version=row_version WHERE id=?")
            .bind(&run.installation_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando instalación para conformidad", &e))?;
        if let Some(row) = journal(&mut tx, &run.id).await? {
            if row.0 != JOURNAL_KIND {
                return Err(conflict(ErrorCode::Conflict, "conformance_run_collision"));
            }
            let receipt = parse_receipt(&row)?;
            let payload: Value =
                serde_json::from_str(&row.3).map_err(|e| db_error("evidencia", &e))?;
            if receipt.run_sha256 != run_hash
                || receipt.outcome_sha256 != outcome_hash
                || payload["actor_id"].as_str() != Some(ctx.actor_id.as_str())
            {
                return Err(conflict(ErrorCode::Conflict, "conformance_replay_mismatch"));
            }
            if let Some(id) = probe {
                crate::adapter_probes::finish_with_receipt(
                    &mut tx, id, run, outcome, &receipt, ctx,
                )
                .await?;
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(receipt);
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM adapter_installation WHERE id=?"
        )))
        .bind(&run.installation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo instalación para conformidad", &e))?
        .ok_or_else(|| conflict(ErrorCode::NotFound, "adapter_installation_not_found"))?;
        let mut installation: AdapterInstallation = to_installation(&row)?;
        if installation.trust == AdapterTrust::Revoked {
            return Err(conflict(ErrorCode::Forbidden, "adapter_revoked"));
        }
        if !matches!(
            installation.state,
            AdapterState::Installed | AdapterState::Validated
        ) {
            return Err(conflict(
                ErrorCode::InvalidStateTransition,
                "adapter_not_installed",
            ));
        }
        if !run.matches(&installation)? {
            return Err(conflict(
                ErrorCode::StaleRevision,
                "conformance_snapshot_stale",
            ));
        }
        let revision = installation.row_version.checked_add(1).ok_or_else(|| {
            conflict(
                ErrorCode::InvalidStateTransition,
                "conformance_revision_exhausted",
            )
        })?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        installation.state = if outcome.passed() {
            AdapterState::Validated
        } else {
            AdapterState::Installed
        };
        installation.row_version = revision;
        installation.updated_at = now.clone();
        let receipt = Receipt {
            run_id: run.id.clone(),
            installation_id: installation.id.clone(),
            installation_revision: revision.to_string(),
            committed_installation_sha256: committed_installation_hash(&installation)?,
            package_sha256: installation.package_hash.clone(),
            run_sha256: run_hash.clone(),
            outcome_sha256: outcome_hash,
            evidence_sha256: outcome.evidence_hash()?,
            passed: outcome.passed(),
            completed_at: now.clone(),
        };
        installation.metadata.insert(
            METADATA_KEY.into(),
            serde_json::to_value(&receipt).map_err(|e| db_error("recibo de conformidad", &e))?,
        );
        let done=sqlx::query("UPDATE adapter_installation SET state=?,metadata=?,row_version=?,updated_at=? WHERE id=? AND row_version=?")
            .bind(installation.state.as_str()).bind(serde_json::to_string(&installation.metadata).map_err(|e|db_error("metadata",&e))?)
            .bind(revision).bind(&now).bind(&installation.id).bind(revision - 1)
            .execute(&mut *tx).await.map_err(|e|db_error("confirmando conformidad",&e))?;
        if done.rows_affected() != 1 {
            return Err(conflict(
                ErrorCode::StaleRevision,
                "conformance_snapshot_stale",
            ));
        }
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES (?,?,?,?,?,?,?)")
            .bind(&run.id).bind(JOURNAL_KIND).bind(json!([installation.id]).to_string())
            .bind(if receipt.passed {"COMPLETED"}else{"FAILED"}).bind(&run_hash)
            .bind(json!({"schema_version":"1.0","run":run,"outcome":outcome,"receipt":receipt,"actor_id":ctx.actor_id,"correlation_id":ctx.correlation_id}).to_string())
            .bind(&now).execute(&mut *tx).await.map_err(|e|db_error("registrando journal de conformidad",&e))?;
        append(&mut tx,&NewEvent{event_type:"adapter.updated",resource_type:"adapter_installation",resource_id:&installation.id,resource_version:revision,
            data:json!({"action":if receipt.passed {"conformance_passed"}else{"conformance_failed"},"run_id":run.id,"run_sha256":run_hash,"state":installation.state.as_str()})},ctx).await?;
        if let Some(id) = probe {
            crate::adapter_probes::finish_with_receipt(&mut tx, id, run, outcome, &receipt, ctx)
                .await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(receipt)
    }
}
#[async_trait]
impl AdapterConformanceRepository for SqlAdapterConformance {
    async fn commit_conformance(
        &self,
        run: &Run,
        outcome: &Outcome,
        ctx: &MutationContext,
    ) -> Result<Receipt, LarmError> {
        self.commit_with_probe(run, outcome, ctx, None).await
    }
    async fn commit_probe_conformance(
        &self,
        job_id: &str,
        run: &Run,
        outcome: &Outcome,
        ctx: &MutationContext,
    ) -> Result<Receipt, LarmError> {
        self.commit_with_probe(run, outcome, ctx, Some(job_id))
            .await
    }
    async fn conformance_receipt(&self, id: &str) -> Result<Option<Receipt>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        let row = journal(&mut conn, id).await?;
        if row.as_ref().is_some_and(|r| r.0 != JOURNAL_KIND) {
            return Ok(None);
        }
        let receipt = row.as_ref().map(parse_receipt).transpose()?;
        if receipt.as_ref().is_some_and(|r| r.run_id != id) {
            return Err(conflict(
                ErrorCode::InvalidStateTransition,
                "conformance_journal_invalid",
            ));
        }
        Ok(receipt)
    }
}
