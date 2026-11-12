//! Persist public owner identity before a helper can execute its handshake.
//! No private seed/launch document and no persisted closure authority.
use super::*;
use larm_domain::transformation_execution::TransformationNativePreparation;
const KIND: &str = "transformation.owner";
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema_version: String,
    owner: TransformationNativeOwner,
    actor_id: String,
    correlation_id: String,
}
struct Stored {
    intent: Intent,
    phase: String,
}
async fn load(conn: &mut SqliteConnection, session: &str) -> Result<Option<Stored>, LarmError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT phase,plan_hash,resource_ids,payload FROM operation_journal WHERE id=? AND kind=?",
    )
    .bind(session)
    .bind(KIND)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo intención nativa", &e))?;
    row.map(|(phase, hash, resources, payload)| {
        let intent: Intent = serde_json::from_str(&payload).map_err(|_| corrupt())?;
        intent.owner.validate()?;
        let ids: Vec<String> = serde_json::from_str(&resources).map_err(|_| corrupt())?;
        if intent.schema_version != "1.0"
            || intent.owner.session_id != session
            || hash != intent.owner.fence.plan_hash
            || ids != vec![intent.owner.fence.job_id.clone()]
            || !matches!(phase.as_str(), "PREPARING" | "REGISTERED" | "CLOSED")
            || intent.actor_id.is_empty()
            || !is_uuid_v7(&intent.correlation_id)
        {
            return Err(corrupt());
        }
        Ok(Stored { intent, phase })
    })
    .transpose()
}
async fn active(conn: &mut SqliteConnection, job: &str) -> Result<Vec<String>, LarmError> {
    sqlx::query_scalar("SELECT id FROM operation_journal WHERE kind=? AND phase IN ('PREPARING','REGISTERED') AND json_extract(payload,'$.owner.fence.job_id')=? ORDER BY id")
        .bind(KIND).bind(job).fetch_all(conn).await.map_err(|e|db_error("buscando propietario pendiente",&e))
}
pub(super) async fn require_unprepared(
    conn: &mut SqliteConnection,
    job: &str,
) -> Result<(), LarmError> {
    if !active(conn, job).await?.is_empty() {
        return Err(stale());
    }
    Ok(())
}
fn same(intent: &Intent, owner: &TransformationNativeOwner, ctx: &MutationContext) -> bool {
    intent.owner == *owner
        && intent.actor_id == ctx.actor_id
        && intent.correlation_id == ctx.correlation_id
}
pub(super) async fn require_pending(
    conn: &mut SqliteConnection,
    owner: &TransformationNativeOwner,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let stored = load(conn, &owner.session_id).await?.ok_or_else(stale)?;
    if stored.phase != "PREPARING"
        || !same(&stored.intent, owner, ctx)
        || active(conn, &owner.fence.job_id).await? != vec![owner.session_id.clone()]
    {
        return Err(stale());
    }
    Ok(())
}
pub(super) async fn check_record(conn: &mut SqliteConnection, r: &Record) -> Result<(), LarmError> {
    let sessions = active(conn, &r.job.id).await?;
    if let Some(owner) = &r.journal.native_owner {
        if let Some(stored) = load(conn, &owner.session_id).await? {
            let expected = if r.journal.attempt_closed.is_some() {
                "CLOSED"
            } else {
                "REGISTERED"
            };
            if stored.phase != expected
                || !same(&stored.intent, owner, &r.context)
                || sessions
                    != if expected == "CLOSED" {
                        vec![]
                    } else {
                        vec![owner.session_id.clone()]
                    }
            {
                return Err(corrupt());
            }
        } else if !sessions.is_empty() {
            return Err(corrupt());
        }
        // Existing registered v2 journals may predate preparation intents.
        // Their exact live guard is still mandatory for any resource release.
    } else if !sessions.is_empty() {
        // La cancelación en cola (CANCELLING) cierra la preparación activa en
        // la misma transacción que la convierte en terminal.
        if sessions.len() != 1
            || !matches!(
                r.job.state,
                TransformationState::Queued | TransformationState::Cancelling
            )
        {
            return Err(corrupt());
        }
        let stored = load(conn, &sessions[0]).await?.ok_or_else(corrupt)?;
        // La preparación activa sólo proyecta claim en un trabajo aún
        // reclamable; en CANCELLING el cierre en cola la retira en la misma
        // transacción y el binding ya se contrastó al preparar.
        if r.job.state == TransformationState::Queued {
            let projected = project_claim(r.clone(), &stored.intent.owner.fence.claim_id)?;
            check_native_binding(&projected, &stored.intent.owner)?;
        }
        if stored.phase != "PREPARING" || !same(&stored.intent, &stored.intent.owner, &r.context) {
            return Err(corrupt());
        }
    }
    Ok(())
}
pub(super) async fn register(
    conn: &mut SqliteConnection,
    owner: &TransformationNativeOwner,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    require_pending(conn, owner, ctx).await?;
    changed(sqlx::query("UPDATE operation_journal SET phase='REGISTERED',updated_at=? WHERE id=? AND kind=? AND phase='PREPARING'")
        .bind(timestamp(chrono::Utc::now())).bind(&owner.session_id).bind(KIND).execute(conn).await.map_err(|e|db_error("registrando intención nativa",&e))?)
}
pub(super) async fn close(conn: &mut SqliteConnection, r: &Record) -> Result<(), LarmError> {
    if let Some(owner) = &r.journal.native_owner {
        if let Some(stored) = load(conn, &owner.session_id).await? {
            if stored.phase != "REGISTERED" || !same(&stored.intent, owner, &r.context) {
                return Err(corrupt());
            }
            changed(sqlx::query("UPDATE operation_journal SET phase='CLOSED',updated_at=? WHERE id=? AND kind=? AND phase='REGISTERED'")
                .bind(&r.job.updated_at).bind(&owner.session_id).bind(KIND).execute(conn).await.map_err(|e|db_error("cerrando intención nativa",&e))?)?;
        }
    }
    Ok(())
}
/// Cierra las intenciones activas de un trabajo que termina CANCELLED sin
/// intento reclamado: la preparación pudo quedar PREPARING (el consumidor
/// seguía preparando cuando llegó la cancelación) o REGISTERED, y el journal
/// del trabajo aún no referencia a ninguna. El testigo vivo queda sin
/// autoridad y lo retira la reconciliación de owners cerrados.
pub(super) async fn close_unclaimed(
    conn: &mut SqliteConnection,
    r: &Record,
) -> Result<(), LarmError> {
    for session in active(&mut *conn, &r.job.id).await? {
        let stored = load(&mut *conn, &session).await?.ok_or_else(corrupt)?;
        if !matches!(stored.phase.as_str(), "PREPARING" | "REGISTERED")
            || !same(&stored.intent, &stored.intent.owner, &r.context)
        {
            return Err(corrupt());
        }
        changed(sqlx::query("UPDATE operation_journal SET phase='CLOSED',updated_at=? WHERE id=? AND kind=? AND phase IN ('PREPARING','REGISTERED')")
            .bind(&r.job.updated_at).bind(&session).bind(KIND).execute(&mut *conn).await.map_err(|e|db_error("cerrando intención nativa en cola",&e))?)?;
    }
    Ok(())
}
fn check_proposal(r: &Record, p: &TransformationClaimProposal) -> Result<(), LarmError> {
    let c = claim(r.clone())?;
    let mut expected = json!(p.claim.job);
    expected["updated_at"] = json!(c.job.updated_at);
    if c.fence != p.claim.fence
        || c.plan != p.claim.plan
        || c.context != p.claim.context
        || c.input_identity != p.claim.input_identity
        || c.input_locations != p.claim.input_locations
        || json!(c.job) != expected
    {
        return Err(stale());
    }
    Ok(())
}

fn known_closed(
    r: &Record,
    owner: &TransformationNativeOwner,
    ctx: &MutationContext,
) -> Result<bool, LarmError> {
    if owner.fence.job_id != r.job.id || ctx.actor_id != r.context.actor_id {
        return Ok(false);
    }
    if r.journal.native_owner.as_ref() == Some(owner)
        && r.context == *ctx
        && r.journal
            .attempt_closed
            .as_ref()
            .is_some_and(|c| c.fence == owner.fence)
    {
        return Ok(true);
    }
    for h in meta(&r.job, "attempt_history")
        .as_array()
        .ok_or_else(corrupt)?
    {
        if h.get("native_owner") == Some(&json!(owner))
            && h["correlation_id"] == json!(ctx.correlation_id)
        {
            let closed: Closed = json(&h["attempt_closed"])?;
            return Ok(closed.fence == owner.fence
                && h["attempt_execution"] == json!(owner.fence)
                && h["execution_binding"] == json!(owner.binding)
                && h["plan_hash"] == json!(owner.fence.plan_hash)
                && h["state"] == json!(closed.state)
                && h["attempt"] == json!(owner.fence.attempt)
                && matches!(
                    closed.state,
                    TransformationState::Failed
                        | TransformationState::FailedRecoverable
                        | TransformationState::Completed
                )
                && chrono::DateTime::parse_from_rfc3339(&closed.closed_at).is_ok());
        }
    }
    Ok(false)
}

impl SqlTransformationJobs {
    /// Read-only projection of the revision AFTER persisting preparation.
    /// This candidate grants no permission to launch; prepare must commit first.
    pub async fn propose_native_preparation(
        &self,
        id: &str,
    ) -> Result<TransformationClaimProposal, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN preparación propuesta", &e))?;
        let mut r = read(&mut tx, id).await?;
        require_unprepared(&mut tx, id).await?;
        r.job.row_version = r.job.row_version.checked_add(1).ok_or_else(corrupt)?;
        let version = r.job.row_version;
        let r = project_claim(r, &uuid::Uuid::now_v7().to_string())?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT preparación propuesta", &e))?;
        Ok(TransformationClaimProposal {
            queued_row_version: version,
            claim: claim(r)?,
        })
    }
    /// Durable intent, queued CAS revision and outbox precede any helper start.
    pub async fn prepare_native_owner(
        &self,
        p: &TransformationClaimProposal,
        owner: &TransformationNativeOwner,
    ) -> Result<(), LarmError> {
        owner.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN intención nativa", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &owner.fence.job_id).await?;
        if let Some(stored) = load(&mut tx, &owner.session_id).await? {
            if stored.phase != "PREPARING" || !same(&stored.intent, owner, &p.claim.context) {
                return Err(stale());
            }
            let projected = project_claim(r, &owner.fence.claim_id)?;
            check_native_binding(&projected, owner)?;
            check_proposal(&projected, p)?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay intención", &e))?;
            return Ok(());
        }
        require_unprepared(&mut tx, &r.job.id).await?;
        let old = r.job.row_version;
        r.job.row_version = old.checked_add(1).ok_or_else(corrupt)?;
        r.job.updated_at = timestamp(chrono::Utc::now());
        if r.job.row_version != p.queued_row_version
            || owner.queued_row_version != p.queued_row_version
        {
            return Err(stale());
        }
        let projected = project_claim(r.clone(), &owner.fence.claim_id)?;
        check_native_binding(&projected, owner)?;
        check_proposal(&projected, p)?;
        let intent = Intent {
            schema_version: "1.0".into(),
            owner: owner.clone(),
            actor_id: r.context.actor_id.clone(),
            correlation_id: r.context.correlation_id.clone(),
        };
        changed(sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,?,?,'PREPARING',?,?,?)")
            .bind(&owner.session_id).bind(KIND).bind(json!([r.job.id]).to_string()).bind(&owner.fence.plan_hash)
            .bind(serde_json::to_string(&intent).map_err(|_|corrupt())?).bind(&r.job.updated_at).execute(&mut *tx).await.map_err(|e|db_error("guardando intención nativa",&e))?)?;
        persist(
            &mut tx,
            &r,
            old,
            TransformationState::Queued,
            "owner_preparing",
        )
        .await?;
        let after = read(&mut tx, &r.job.id).await?;
        if json!(after.job) != json!(r.job) {
            return Err(corrupt());
        }
        require_pending(&mut tx, owner, &r.context).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT intención nativa", &e))?;
        Ok(())
    }
    /// Bounded startup page. Returned routing metadata is not closure evidence.
    pub async fn native_preparations(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<TransformationNativePreparation>, LarmError> {
        if limit == 0 || limit > 128 || after.is_some_and(|id| !is_uuid_v7(id)) {
            return Err(corrupt());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN intenciones pendientes", &e))?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT session_id FROM (SELECT id AS session_id FROM operation_journal WHERE kind=? AND phase IN ('PREPARING','REGISTERED') UNION SELECT json_extract(metadata,'$.native_owner.session_id') AS session_id FROM transformation_job WHERE json_type(metadata,'$.native_owner')='object' AND coalesce(json_type(metadata,'$.attempt_closed'),'null')='null') WHERE session_id>? ORDER BY session_id LIMIT ?")
            .bind(KIND).bind(after.unwrap_or("")).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(|e|db_error("listando intenciones",&e))?;
        let mut result = Vec::new();
        for id in ids {
            if let Some(stored) = load(&mut tx, &id).await? {
                let r = read(&mut tx, &stored.intent.owner.fence.job_id).await?;
                result.push(TransformationNativePreparation {
                    owner: stored.intent.owner,
                    context: r.context,
                    registered: stored.phase == "REGISTERED",
                });
            } else {
                let jobs:Vec<String>=sqlx::query_scalar("SELECT id FROM transformation_job WHERE json_extract(metadata,'$.native_owner.session_id')=?")
                    .bind(&id).fetch_all(&mut *tx).await.map_err(|e|db_error("buscando propietario registrado anterior",&e))?;
                if jobs.len() != 1 {
                    return Err(corrupt());
                }
                let r = read(&mut tx, &jobs[0]).await?;
                result.push(TransformationNativePreparation {
                    owner: r.journal.native_owner.ok_or_else(corrupt)?,
                    context: r.context,
                    registered: true,
                });
            }
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT intenciones pendientes", &e))?;
        Ok(result)
    }

    /// Housekeeping ONLY for owners whose Core attempt already closed durably.
    /// This metadata never authorizes a resource release. A live helper still
    /// must answer fresh native closure challenges before it can be retired.
    pub async fn native_retirements(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<TransformationNativeOwner>, LarmError> {
        if limit == 0 || limit > 128 || after.is_some_and(|id| !is_uuid_v7(id)) {
            return Err(corrupt());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN testigos cerrados", &e))?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT session_id FROM (SELECT id AS session_id FROM operation_journal WHERE kind=? AND phase='CLOSED' UNION SELECT json_extract(metadata,'$.native_owner.session_id') AS session_id FROM transformation_job WHERE json_type(metadata,'$.native_owner')='object' AND json_type(metadata,'$.attempt_closed')='object' UNION SELECT json_extract(h.value,'$.native_owner.session_id') AS session_id FROM transformation_job t,json_each(t.metadata,'$.attempt_history') h WHERE json_type(h.value,'$.native_owner')='object' AND json_type(h.value,'$.attempt_closed')='object') WHERE session_id>? ORDER BY session_id LIMIT ?")
            .bind(KIND).bind(after.unwrap_or("")).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(|e|db_error("listando testigos cerrados",&e))?;
        let mut result = Vec::new();
        for id in ids {
            let (owner, ctx, r) = if let Some(stored) = load(&mut tx, &id).await? {
                if stored.phase != "CLOSED" {
                    return Err(corrupt());
                }
                let r = read(&mut tx, &stored.intent.owner.fence.job_id).await?;
                let ctx = MutationContext {
                    actor_id: stored.intent.actor_id,
                    correlation_id: stored.intent.correlation_id,
                };
                (stored.intent.owner, ctx, r)
            } else {
                let jobs:Vec<String>=sqlx::query_scalar("SELECT id FROM transformation_job t WHERE json_extract(metadata,'$.native_owner.session_id')=? OR EXISTS(SELECT 1 FROM json_each(t.metadata,'$.attempt_history') h WHERE json_extract(h.value,'$.native_owner.session_id')=?)")
                    .bind(&id).bind(&id).fetch_all(&mut *tx).await.map_err(|e|db_error("buscando testigo anterior",&e))?;
                if jobs.len() != 1 {
                    return Err(corrupt());
                }
                let r = read(&mut tx, &jobs[0]).await?;
                if let Some(owner) = r
                    .journal
                    .native_owner
                    .as_ref()
                    .filter(|o| o.session_id == id)
                {
                    (owner.clone(), r.context.clone(), r)
                } else {
                    let h = meta(&r.job, "attempt_history")
                        .as_array()
                        .ok_or_else(corrupt)?
                        .iter()
                        .find(|h| h["native_owner"]["session_id"] == json!(id))
                        .ok_or_else(corrupt)?;
                    let owner: TransformationNativeOwner = json(&h["native_owner"])?;
                    let ctx = MutationContext {
                        actor_id: r.context.actor_id.clone(),
                        correlation_id: h["correlation_id"].as_str().ok_or_else(corrupt)?.into(),
                    };
                    (owner, ctx, r)
                }
            };
            owner.validate()?;
            if !known_closed(&r, &owner, &ctx)? {
                return Err(corrupt());
            }
            result.push(owner);
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT testigos cerrados", &e))?;
        Ok(result)
    }
}
