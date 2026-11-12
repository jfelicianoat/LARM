//! Recetas internas en journal: CAS, aprobación de un uso y evento/outbox juntos.
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::approval::ApprovalRecord;
use larm_domain::conformance::hash;
use larm_domain::conformance_profiles::{invalid, ConformanceProfile, KIND};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{ConformanceProfileRepository, MutationContext};
use sqlx::{SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlConformanceProfiles {
    pub pool: SqlitePool,
}
pub(crate) async fn load(
    conn: &mut SqliteConnection,
    installation: &str,
) -> Result<Option<ConformanceProfile>, LarmError> {
    let rows:Vec<(String,String,String,String)>=sqlx::query_as("SELECT id,payload,plan_hash,resource_ids FROM operation_journal WHERE kind=? AND phase='READY' AND json_extract(payload,'$.installation_id')=?")
        .bind(KIND).bind(installation).fetch_all(&mut *conn).await.map_err(|e|db_error("leyendo perfil de conformidad",&e))?;
    if rows.len() > 1 {
        return Err(invalid("conformance_profile_ambiguous"));
    }
    let Some((id, payload, digest, resources)) = rows.into_iter().next() else {
        return Ok(None);
    };
    let p: ConformanceProfile =
        serde_json::from_str(&payload).map_err(|_| invalid("conformance_profile_corrupt"))?;
    p.validate()?;
    let refs: Vec<String> =
        serde_json::from_str(&resources).map_err(|_| invalid("conformance_profile_corrupt"))?;
    if p.id != id
        || p.installation_id != installation
        || refs != [installation]
        || hash(&p)? != digest
    {
        return Err(invalid("conformance_profile_corrupt"));
    }
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='approval'",
    )
    .bind(&p.approval.id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo concesión de perfil", &e))?;
    let Some((phase, raw)) = row else {
        return Err(invalid("conformance_profile_approval_missing"));
    };
    let mut consumed: ApprovalRecord =
        serde_json::from_str(&raw).map_err(|_| invalid("conformance_profile_approval_corrupt"))?;
    if phase != "CONSUMED" || consumed.consumed_at.take().is_none() || consumed != p.approval {
        return Err(invalid("conformance_profile_approval_corrupt"));
    }
    Ok(Some(p))
}
#[async_trait]
impl ConformanceProfileRepository for SqlConformanceProfiles {
    async fn conformance_profile(
        &self,
        installation: &str,
    ) -> Result<Option<ConformanceProfile>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool", &e))?;
        load(&mut conn, installation).await
    }
    async fn store_conformance_profile(
        &self,
        p: &ConformanceProfile,
        expected: Option<&str>,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        p.validate()?;
        if ctx.actor_id != p.actor_id || !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id) {
            return Err(invalid("conformance_profile_actor_mismatch"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando perfiles", &e))?;
        let current = load(&mut tx, &p.installation_id).await?;
        if current.as_ref() == Some(p) {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(());
        }
        if current.as_ref().map(|v| v.id.as_str()) != expected {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El perfil cambió",
                "conformance_profile_stale",
            ));
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operation_journal WHERE kind='adapter.probe' AND phase IN ('QUEUED','RUNNING') AND json_extract(payload,'$.target')=?)").bind(&p.installation_id).fetch_one(&mut *tx).await.map_err(|e|db_error("sondeo activo",&e))?;
        if active {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Hay un sondeo activo",
                "adapter_probe_active",
            ));
        }
        let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
            .bind(&p.installation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("leyendo instalación", &e))?
            .ok_or_else(|| invalid("conformance_profile_subject_missing"))?;
        p.check_installation(&crate::adapters::to_installation(&row)?)?;
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='approval'",
        )
        .bind(&p.approval.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("aprobación de perfil", &e))?;
        if row.as_ref().is_none_or(|(phase, raw)| {
            phase != "APPROVED"
                || serde_json::from_str::<ApprovalRecord>(raw).ok().as_ref() != Some(&p.approval)
        }) {
            return Err(LarmError::new(
                ErrorCode::ApprovalRequired,
                "La aprobación no está disponible",
                "conformance_profile_approval_unavailable",
            ));
        }
        p.approval.check(
            &larm_domain::approval::ApprovalUse {
                actor_id: &p.actor_id,
                operation_digest: &p.approval.operation_digest,
                resource_ids: &p.approval.resource_ids.iter().cloned().collect::<Vec<_>>(),
                code_hash: p.approval.code_hash.as_deref(),
            },
            chrono::Utc::now(),
        )?;
        let at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed=sqlx::query("UPDATE operation_journal SET phase='CONSUMED',payload=json_set(payload,'$.consumed_at',?),updated_at=? WHERE id=? AND kind='approval' AND phase='APPROVED'").bind(&at).bind(&at).bind(&p.approval.id).execute(&mut *tx).await.map_err(|e|db_error("consumiendo aprobación de perfil",&e))?.rows_affected();
        if changed != 1 {
            return Err(invalid("conformance_profile_approval_raced"));
        }
        if let Some(old) = current {
            let changed=sqlx::query("UPDATE operation_journal SET phase='SUPERSEDED',updated_at=? WHERE id=? AND kind=? AND phase='READY'").bind(&at).bind(old.id).bind(KIND).execute(&mut *tx).await.map_err(|e|db_error("sustituyendo perfil",&e))?.rows_affected();
            if changed != 1 {
                return Err(invalid("conformance_profile_stale"));
            }
        }
        let changed=sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,?,?,'READY',?,?,?)").bind(&p.id).bind(KIND).bind(serde_json::to_string(&vec![&p.installation_id]).map_err(|_|invalid("conformance_profile_invalid"))?).bind(hash(p)?).bind(serde_json::to_string(p).map_err(|_|invalid("conformance_profile_invalid"))?).bind(&at).execute(&mut *tx).await.map_err(|e|db_error("guardando perfil",&e))?.rows_affected();
        if changed != 1 {
            return Err(invalid("conformance_profile_not_inserted"));
        }
        append(&mut tx,&NewEvent{event_type:"adapter.updated",resource_type:"conformance_profile",resource_id:&p.id,resource_version:1,data:serde_json::json!({"action":"conformance_profile_registered","installation_id":p.installation_id,"profile_id":p.id})},ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }
}
