//! Durable public-engine delivery hints. Jobs/plans/native witnesses remain
//! authoritative; a READY row is neither a claim nor proof of process closure.
use super::*;
use serde::{Deserialize, Serialize};
const KIND: &str = "transformation.dispatch";
#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Delivery {
    schema_version: u8,
    job_id: String,
    plan_hash: String,
    attempt: u32,
    actor_id: String,
    correlation_id: String,
}
pub(crate) async fn enqueue(
    conn: &mut SqliteConnection,
    job: &TransformationJob,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let d = Delivery {
        schema_version: 1,
        job_id: job.id.clone(),
        plan_hash: job.plan_hash.clone(),
        attempt: job.attempt.max(1),
        actor_id: ctx.actor_id.clone(),
        correlation_id: ctx.correlation_id.clone(),
    };
    let id = uuid::Uuid::now_v7().to_string();
    let resources = json!([job.id]).to_string();
    let payload = serde_json::to_string(&d).map_err(|e| db_error("cola transformation", &e))?;
    sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,?,?,'READY',?,?,?)")
        .bind(&id).bind(KIND).bind(&resources).bind(&job.plan_hash).bind(&payload).bind(&job.updated_at).execute(&mut *conn).await.map_err(|e|db_error("encolando transformation",&e))?;
    let stored:Option<(String,String,String)>=sqlx::query_as("SELECT resource_ids,plan_hash,payload FROM operation_journal WHERE id=? AND kind=? AND phase='READY'")
        .bind(&id).bind(KIND).fetch_optional(&mut *conn).await.map_err(|e|db_error("confirmando cola transformation",&e))?;
    if stored != Some((resources, job.plan_hash.clone(), payload)) {
        return Err(invalid("transformation_dispatch_not_persisted"));
    }
    Ok(())
}
impl SqlTransformationJobs {
    pub async fn dispatch_candidates(&self, limit: u32) -> Result<Vec<String>, LarmError> {
        if !(1..=64).contains(&limit) {
            return Err(invalid("transformation_dispatch_limit_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cola transformation", &e))?;
        let rows:Vec<(String,String,String)>=sqlx::query_as("SELECT d.payload,d.resource_ids,d.plan_hash FROM operation_journal d JOIN transformation_job t ON t.id=json_extract(d.payload,'$.job_id') AND t.plan_hash=d.plan_hash WHERE d.kind=? AND d.phase='READY' AND t.state='QUEUED' AND NOT EXISTS(SELECT 1 FROM operation_journal o WHERE o.kind='transformation.owner' AND o.phase IN ('PREPARING','REGISTERED') AND json_extract(o.payload,'$.owner.fence.job_id')=t.id) ORDER BY d.updated_at,d.id LIMIT ?")
            .bind(KIND).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(|e|db_error("leyendo cola transformation",&e))?;
        let mut ids = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        for (payload, resources, digest) in rows {
            let d: Delivery = serde_json::from_str(&payload)
                .map_err(|_| invalid("transformation_dispatch_invalid"))?;
            let job = load(&mut tx, &d.job_id)
                .await?
                .ok_or_else(|| invalid("transformation_dispatch_job_missing"))?;
            let ctx = recovery_context(&job)?;
            if d.schema_version != 1
                || d.plan_hash != digest
                || job.plan_hash != digest
                || d.attempt != job.attempt.max(1)
                || ctx.actor_id != d.actor_id
                || ctx.correlation_id != d.correlation_id
                || serde_json::from_str::<Value>(&resources).ok() != Some(json!([d.job_id]))
                || !ids.insert(d.job_id.clone())
            {
                return Err(invalid("transformation_dispatch_invalid"));
            }
            result.push(d.job_id);
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cola transformation", &e))?;
        Ok(result)
    }
}
