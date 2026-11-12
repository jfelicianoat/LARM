//! Fenced claims and failed-attempt closure. A terminal flag is never enough
//! to release resources: the caller must retain the actual process/IO owner.
use super::*;
use larm_domain::{
    bridge::{is_uuid_v7, timestamp},
    guard::{normalize, LockMode, Resource, ResourceKind},
    transformation_execution::{
        TransformationAttemptFence, TransformationAttemptGuard, TransformationClaim,
        TransformationClaimProposal, TransformationNativeOwner, TransformationOwnedClaim,
        TransformationReadyOwner,
    },
    transformations::{
        SealedTransformationPlan, TransformationInputIdentity, TransformationInputLocation,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

mod publication;
use larm_domain::transformation_publication::TransformationPublicationRecord;
pub use publication::{CommittedTransformationPublication, PreparedTransformationPublication};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Closed {
    fence: TransformationAttemptFence,
    closed_at: String,
    state: TransformationState,
    /// Prueba de cierre sin testigo: (boot del intento, boot actual). Un
    /// reinicio del sistema implica que ningún proceso ni handle de la
    /// sesión anterior sobrevive (evidencia del sistema operativo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_restart: Option<(u64, u64)>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: String,
    actor_id: String,
    correlation_id: String,
    state: TransformationState,
    attempt: u32,
    plan_hash: String,
    plan: SealedTransformationPlan,
    guard_generation: u64,
    planned_output_variant_id: String,
    input_identity: TransformationInputIdentity,
    input_locations: Vec<TransformationInputLocation>,
    #[serde(default)]
    attempt_execution: Option<TransformationAttemptFence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_owner: Option<TransformationNativeOwner>,
    #[serde(default)]
    attempt_closed: Option<Closed>,
    /// Cancelación en cola cerrada sin intento: sin fence que cerrar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancelled_queued: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    publication: Option<TransformationPublicationRecord>,
}
#[derive(Clone)]
struct Record {
    job: TransformationJob,
    journal: Journal,
    resources: Vec<(Resource, LockMode)>,
    context: MutationContext,
}
mod native_intent;
fn corrupt() -> LarmError {
    eprintln!(
        "DBG corrupt:
{}",
        std::backtrace::Backtrace::force_capture()
    );
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El journal del intento no es coherente",
        "transformation_attempt_journal_invalid",
    )
}
fn meta<'a>(job: &'a TransformationJob, key: &str) -> &'a Value {
    job.metadata.get(key).unwrap_or(&Value::Null)
}
fn stale() -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "El intento de transformación ya no está vigente",
        "transformation_attempt_fence_changed",
    )
}
fn json<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, LarmError> {
    serde_json::from_value(value.clone()).map_err(|_| corrupt())
}
fn phase(state: TransformationState) -> Result<String, LarmError> {
    match serde_json::to_value(state).map_err(|_| corrupt())? {
        Value::String(s) => Ok(s),
        _ => Err(corrupt()),
    }
}
fn changed(result: sqlx::sqlite::SqliteQueryResult) -> Result<(), LarmError> {
    if result.rows_affected() != 1 {
        return Err(stale());
    }
    Ok(())
}

fn resources(j: &Journal) -> Result<Vec<(Resource, LockMode)>, LarmError> {
    let mut resources = vec![
        (
            Resource::new(ResourceKind::Variant, &j.plan.request.input_variant_id),
            LockMode::Exclusive,
        ),
        (
            Resource::new(ResourceKind::Variant, &j.planned_output_variant_id),
            LockMode::Exclusive,
        ),
    ];
    let ids = j
        .plan
        .input_artifacts
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    let locations = j
        .input_locations
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    if ids != locations || locations.len() != j.input_locations.len() {
        return Err(corrupt());
    }
    for location in &j.input_locations {
        if !is_uuid_v7(&location.artifact_location_id)
            || !is_uuid_v7(&location.storage_location_id)
            || location.canonical_storage_path.is_empty()
            || location.volume_key.is_empty()
            || location.volume_key.starts_with("unknown:")
            || larm_domain::model::bundle_path(&location.relative_path).is_err()
        {
            return Err(corrupt());
        }
        resources.push((
            Resource::new(ResourceKind::Artifact, &location.artifact_id),
            LockMode::Exclusive,
        ));
        resources.push((
            Resource::new(ResourceKind::Volume, &location.volume_key),
            LockMode::Shared,
        ));
    }
    for volume in &j.plan.response.storage_estimate.per_volume {
        if volume.volume_key.is_empty() || volume.volume_key.starts_with("unknown:") {
            return Err(corrupt());
        }
        resources.push((
            Resource::new(ResourceKind::Volume, &volume.volume_key),
            LockMode::Shared,
        ));
    }
    Ok(normalize(&resources))
}
async fn check_resources(conn: &mut SqliteConnection, r: &Record) -> Result<(), LarmError> {
    let actual:Vec<(String,String,i64)>=sqlx::query_as("SELECT resource_id,mode,generation FROM resource_lock WHERE owner_id=? ORDER BY resource_id")
        .bind(&r.job.id).fetch_all(&mut *conn).await.map_err(|e|db_error("guards del intento",&e))?;
    let reserved:Vec<(String,i64,i64,Option<String>)>=sqlx::query_as("SELECT volume_key,total_bytes,remaining_bytes,expires_at FROM volume_reservation WHERE job_id=? ORDER BY volume_key")
        .bind(&r.job.id).fetch_all(conn).await.map_err(|e|db_error("reservas del intento",&e))?;
    if r.journal.attempt_closed.is_some() || r.journal.cancelled_queued.is_some() {
        if !actual.is_empty() || !reserved.is_empty() {
            return Err(corrupt());
        }
        return Ok(());
    }
    let mut expected = r
        .resources
        .iter()
        .map(|(res, mode)| {
            (
                res.key(),
                mode.as_str().to_owned(),
                r.journal.guard_generation as i64,
            )
        })
        .collect::<Vec<_>>();
    expected.sort();
    let volumes = &r.journal.plan.response.storage_estimate.per_volume;
    if actual != expected
        || reserved.len() != volumes.len()
        || volumes.is_empty()
        || volumes
            .iter()
            .map(|v| &v.volume_key)
            .collect::<BTreeSet<_>>()
            .len()
            != volumes.len()
    {
        return Err(corrupt());
    }
    for (key, total, remaining, expires) in reserved {
        let v = volumes
            .iter()
            .find(|v| v.volume_key == key)
            .ok_or_else(corrupt)?;
        let required = v
            .peak_extra_bytes
            .get()
            .checked_add(v.margin_bytes.get())
            .and_then(|n| i64::try_from(n).ok())
            .ok_or_else(corrupt)?;
        if total != required
            || remaining < 0
            || remaining > total
            || expires.is_some()
            || (r.job.state == TransformationState::Queued && remaining != total)
        {
            return Err(corrupt());
        }
    }
    Ok(())
}
async fn read(conn: &mut SqliteConnection, id: &str) -> Result<Record, LarmError> {
    let job = load(conn, id).await?.ok_or_else(|| {
        LarmError::new(
            ErrorCode::NotFound,
            "Trabajo desconocido",
            "transformation_job_missing",
        )
    })?;
    let context = recovery_context(&job)?;
    let row:Option<(String,String,String,String)>=sqlx::query_as("SELECT phase,plan_hash,resource_ids,payload FROM operation_journal WHERE id=? AND kind='transformation.job'")
        .bind(id).fetch_optional(&mut *conn).await.map_err(|e|db_error("journal del intento",&e))?;
    let (state, hash, resource_ids, payload) = row.ok_or_else(corrupt)?;
    let journal: Journal = serde_json::from_str(&payload).map_err(|_| corrupt())?;
    journal.plan.verify_seal().map_err(|_| corrupt())?;
    let identity = &journal.input_identity;
    if (job.output_variant_id.is_some()
        && (job.state != TransformationState::Completed
            || journal.publication.as_ref().map(|p| &p.variant.id)
                != job.output_variant_id.as_ref()))
        || (job.state == TransformationState::Completed && job.output_variant_id.is_none())
        || !matches!(
            (
                journal.schema_version.as_str(),
                journal.native_owner.is_some()
            ),
            ("1.0", false) | ("2.0", true)
        )
        || journal.state != job.state
        || state != phase(job.state)?
        || journal.attempt != job.attempt
        || journal.plan_hash != job.plan_hash
        || hash != job.plan_hash
        || journal.plan.response.plan_hash != job.plan_hash
        || journal.plan.execution_binding.is_none()
        || journal.actor_id != context.actor_id
        || journal.correlation_id != context.correlation_id
        || journal.guard_generation > i64::MAX as u64
        || job.metadata.get("guard_generation").and_then(Value::as_u64)
            != Some(journal.guard_generation)
        || !is_uuid_v7(&journal.planned_output_variant_id)
        || job
            .metadata
            .get("planned_output_variant_id")
            .and_then(Value::as_str)
            != Some(&journal.planned_output_variant_id)
        || json::<TransformationInputIdentity>(meta(&job, "input_identity"))? != *identity
        || json::<Vec<TransformationInputLocation>>(meta(&job, "input_locations"))?
            != journal.input_locations
        || identity.variant_id != job.input_variant_id
        || !is_uuid_v7(&identity.revision_id)
        || !is_uuid_v7(&identity.bundle_id)
        || identity.bundle_row_version < 1
        || !larm_domain::adapter_updates::is_sha256(&identity.variant_bundle_hash)
        || !larm_domain::adapter_updates::is_sha256(&identity.bundle_manifest_hash)
        || &serde_json::json!(journal.plan.request) != meta(&job, "request")
        || &serde_json::json!(journal.plan.response) != meta(&job, "plan_snapshot")
        || &serde_json::json!(journal.plan.input_artifacts) != meta(&job, "input_artifacts")
        || &serde_json::json!(journal.plan.execution_binding) != meta(&job, "execution_binding")
        || &serde_json::json!(journal.plan.output_policy) != meta(&job, "output_policy")
        || journal.error != job.error
        || &serde_json::json!(journal.publication) != meta(&job, "publication")
        || json::<Option<TransformationAttemptFence>>(
            &job.metadata
                .get("attempt_execution")
                .cloned()
                .unwrap_or(Value::Null),
        )? != journal.attempt_execution
        || json::<Option<Closed>>(
            &job.metadata
                .get("attempt_closed")
                .cloned()
                .unwrap_or(Value::Null),
        )? != journal.attempt_closed
    {
        return Err(corrupt());
    }
    if let Some(publication) = &journal.publication {
        publication::check_record(&job, &journal, publication)?;
    }
    let expected_locations = journal
        .input_locations
        .iter()
        .map(|l| &l.artifact_location_id)
        .collect::<Vec<_>>();
    if meta(&job, "input_location_ids") != &json!(expected_locations) {
        return Err(corrupt());
    }
    if let Some(fence) = &journal.attempt_execution {
        fence.validate()?;
        if fence.job_id != job.id
            || fence.plan_hash != job.plan_hash
            || fence.attempt != job.attempt
            || fence.generation != journal.guard_generation
        {
            return Err(corrupt());
        }
    }
    if json::<Option<TransformationNativeOwner>>(&meta(&job, "native_owner").clone())?
        != journal.native_owner
    {
        return Err(corrupt());
    }
    if let Some(owner) = &journal.native_owner {
        owner.validate()?;
        if Some(&owner.fence) != journal.attempt_execution.as_ref()
            || Some(&owner.binding) != journal.plan.execution_binding.as_ref()
            || owner.adapter_id != journal.plan.request.adapter_id
            || job.row_version <= owner.queued_row_version
        {
            return Err(corrupt());
        }
    }
    if let Some(closed) = &journal.attempt_closed {
        if Some(&closed.fence) != journal.attempt_execution.as_ref()
            || closed.state != job.state
            || !matches!(
                job.state,
                TransformationState::Failed
                    | TransformationState::FailedRecoverable
                    | TransformationState::Completed
                    | TransformationState::Cancelled
            )
            || chrono::DateTime::parse_from_rfc3339(&closed.closed_at).is_err()
        {
            return Err(corrupt());
        }
    }
    let resources = resources(&journal)?;
    if json::<Vec<(Resource, LockMode)>>(meta(&job, "resource_locks"))? != resources
        || serde_json::from_str::<Vec<String>>(&resource_ids).map_err(|_| corrupt())?
            != resources.iter().map(|(r, _)| r.key()).collect::<Vec<_>>()
    {
        return Err(corrupt());
    }
    let record = Record {
        job,
        journal,
        resources,
        context,
    };
    check_resources(conn, &record).await?;
    native_intent::check_record(conn, &record).await?;
    Ok(record)
}
fn check_claim(
    r: &Record,
    c: &TransformationClaim,
    owner: &dyn TransformationAttemptGuard,
) -> Result<(), LarmError> {
    c.fence.validate()?;
    if r.journal.attempt_execution.as_ref() != Some(&c.fence)
        || owner.fence() != &c.fence
        || Some(owner.binding()) != r.journal.plan.execution_binding.as_ref()
        || c.plan != r.journal.plan
        || c.context != r.context
        || c.job.id != r.job.id
        || c.job.attempt != c.fence.attempt
        || c.job.plan_hash != c.fence.plan_hash
        || c.input_identity != r.journal.input_identity
        || c.input_locations != r.journal.input_locations
        || owner.native_owner() != r.journal.native_owner.as_ref()
    {
        return Err(stale());
    }
    Ok(())
}
fn check_native_binding(r: &Record, owner: &TransformationNativeOwner) -> Result<(), LarmError> {
    owner.validate()?;
    if r.journal.attempt_execution.as_ref() != Some(&owner.fence)
        || r.journal.plan.execution_binding.as_ref() != Some(&owner.binding)
        || r.journal.plan.request.adapter_id != owner.adapter_id
        || r.job.row_version.checked_sub(1) != Some(owner.queued_row_version)
    {
        return Err(stale());
    }
    Ok(())
}
fn project_claim(mut r: Record, claim_id: &str) -> Result<Record, LarmError> {
    if r.job.state != TransformationState::Queued
        || r.job.output_variant_id.is_some()
        || r.journal.attempt_execution.is_some()
        || r.journal.attempt_closed.is_some()
    {
        return Err(invalid("transformation_job_not_queued"));
    }
    let history = meta(&r.job, "attempt_history")
        .as_array()
        .ok_or_else(corrupt)?;
    let generation = r.journal.guard_generation;
    if history.len() as u64 != generation
        || (generation == 0 && r.job.attempt != 0)
        || (generation > 0
            && !history.last().is_some_and(|p| {
                p["attempt"].as_u64().and_then(|n| n.checked_add(1))
                    == Some(u64::from(r.job.attempt))
                    && p["guard_generation"]
                        .as_u64()
                        .and_then(|n| n.checked_add(1))
                        == Some(generation)
            }))
    {
        return Err(corrupt());
    }
    if generation == 0 {
        r.job.attempt = 1;
    }
    let fence = TransformationAttemptFence {
        job_id: r.job.id.clone(),
        claim_id: claim_id.to_owned(),
        generation,
        attempt: r.job.attempt,
        plan_hash: r.job.plan_hash.clone(),
    };
    fence.validate()?;
    r.job
        .metadata
        .insert("attempt_execution".into(), json!(fence));
    r.job
        .transition(TransformationState::Running, timestamp(chrono::Utc::now()))
        .map_err(|_| corrupt())?;
    r.journal.attempt_execution = Some(fence);
    r.journal.state = r.job.state;
    r.journal.attempt = r.job.attempt;
    Ok(r)
}
fn claim(r: Record) -> Result<TransformationClaim, LarmError> {
    Ok(TransformationClaim {
        fence: r.journal.attempt_execution.ok_or_else(corrupt)?,
        plan: r.journal.plan,
        input_identity: r.journal.input_identity,
        input_locations: r.journal.input_locations,
        job: r.job,
        context: r.context,
    })
}
async fn persist(
    conn: &mut SqliteConnection,
    r: &Record,
    old_version: i64,
    old_state: TransformationState,
    action: &str,
) -> Result<(), LarmError> {
    let state = phase(r.job.state)?;
    changed(sqlx::query("UPDATE transformation_job SET state=?,updated_at=?,row_version=?,attempt=?,metadata=?,error=?,output_variant_id=? WHERE id=? AND row_version=? AND state=? AND plan_hash=? AND output_variant_id IS NULL")
        .bind(&state).bind(&r.job.updated_at).bind(r.job.row_version).bind(i64::from(r.job.attempt))
        .bind(json!(r.job.metadata).to_string()).bind(r.job.error.as_ref().map(Value::to_string))
        .bind(&r.job.output_variant_id)
        .bind(&r.job.id).bind(old_version).bind(phase(old_state)?).bind(&r.job.plan_hash)
        .execute(&mut *conn).await.map_err(|e|db_error("actualizando intento",&e))?)?;
    changed(sqlx::query("UPDATE operation_journal SET phase=?,payload=?,updated_at=? WHERE id=? AND kind='transformation.job' AND phase=? AND plan_hash=?")
        .bind(&state).bind(serde_json::to_string(&r.journal).map_err(|_|corrupt())?).bind(&r.job.updated_at)
        .bind(&r.job.id).bind(phase(old_state)?).bind(&r.job.plan_hash).execute(&mut *conn).await.map_err(|e|db_error("actualizando journal del intento",&e))?)?;
    crate::events::append(conn,&NewEvent{event_type:"transformation.state_changed",resource_type:"transformation_job",
        resource_id:&r.job.id,resource_version:r.job.row_version,data:json!({"action":action,"state":r.job.state,
            "input_variant_id":r.job.input_variant_id,"attempt":r.job.attempt,"plan_hash":r.job.plan_hash})},&r.context).await?;
    Ok(())
}
impl SqlTransformationJobs {
    /// No writes or execution authority. A prepared native owner must still be
    /// registered against this exact queued revision before any transform RPC.
    pub async fn propose_claim(&self, id: &str) -> Result<TransformationClaimProposal, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN propuesta de claim", &e))?;
        let r = read(&mut tx, id).await?;
        let version = r.job.row_version;
        let r = project_claim(r, &uuid::Uuid::now_v7().to_string())?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT propuesta de claim", &e))?;
        Ok(TransformationClaimProposal {
            queued_row_version: version,
            claim: claim(r)?,
        })
    }

    /// Called by the helper before handshake. It may prepare only the queued
    /// candidate; reusing an already registered session cannot start a worker.
    pub async fn check_pending_native_owner(
        &self,
        owner: &TransformationNativeOwner,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.proposed_native_claim(owner, ctx).await.map(|_| ())
    }
    pub async fn proposed_native_claim(
        &self,
        owner: &TransformationNativeOwner,
        ctx: &MutationContext,
    ) -> Result<TransformationClaim, LarmError> {
        owner.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN owner pendiente", &e))?;
        let r = read(&mut tx, &owner.fence.job_id).await?;
        native_intent::require_pending(&mut tx, owner, ctx).await?;
        if r.job.row_version != owner.queued_row_version
            || r.context != *ctx
            || r.journal.native_owner.is_some()
        {
            return Err(stale());
        }
        let r = project_claim(r, &owner.fence.claim_id)?;
        check_native_binding(&r, owner)?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT owner pendiente", &e))?;
        claim(r)
    }

    pub async fn claim_with_native_owner(
        &self,
        proposal: &TransformationClaimProposal,
        owner: &dyn TransformationReadyOwner,
    ) -> Result<TransformationOwnedClaim, LarmError> {
        self.claim_native(proposal, owner, Some(owner)).await
    }

    /// Startup may adopt an interrupted preparation only after its exact live
    /// witness proves closure. It grants no permission to execute or resume.
    pub async fn claim_closed_preparation(
        &self,
        proposal: &TransformationClaimProposal,
        owner: &dyn TransformationAttemptGuard,
    ) -> Result<TransformationOwnedClaim, LarmError> {
        self.claim_native(proposal, owner, None).await
    }

    async fn claim_native(
        &self,
        proposal: &TransformationClaimProposal,
        owner: &dyn TransformationAttemptGuard,
        ready: Option<&dyn TransformationReadyOwner>,
    ) -> Result<TransformationOwnedClaim, LarmError> {
        if let Some(ready) = ready {
            ready.require_ready().await?;
        } else {
            owner.require_closed().await?;
        }
        let native = owner.native_owner().ok_or_else(corrupt)?.clone();
        native.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN claim nativo", &e))?;
        lock(&mut tx).await?;
        let r = read(&mut tx, &proposal.claim.fence.job_id).await?;
        if r.job.row_version != proposal.queued_row_version
            || native.queued_row_version != proposal.queued_row_version
        {
            return Err(stale());
        }
        let old_version = r.job.row_version;
        let mut r = project_claim(r, &proposal.claim.fence.claim_id)?;
        let c = claim(Record {
            job: r.job.clone(),
            journal: r.journal.clone(),
            resources: r.resources.clone(),
            context: r.context.clone(),
        })?;
        let mut proposed_job = json!(proposal.claim.job);
        proposed_job["updated_at"] = json!(c.job.updated_at);
        if c.fence != proposal.claim.fence
            || c.plan != proposal.claim.plan
            || c.context != proposal.claim.context
            || c.input_identity != proposal.claim.input_identity
            || c.input_locations != proposal.claim.input_locations
            || json!(c.job) != proposed_job
        {
            return Err(stale());
        }
        check_native_binding(&r, &native)?;
        if owner.fence() != &native.fence || owner.binding() != &native.binding {
            return Err(stale());
        }
        native_intent::register(&mut tx, &native, &r.context).await?;
        r.job.metadata.insert("native_owner".into(), json!(native));
        r.journal.schema_version = "2.0".into();
        r.journal.native_owner = Some(native.clone());
        persist(
            &mut tx,
            &r,
            old_version,
            TransformationState::Queued,
            "claimed",
        )
        .await?;
        let verified = read(&mut tx, &r.job.id).await?;
        if json!(verified.job) != json!(r.job)
            || verified.journal.native_owner.as_ref() != Some(&native)
        {
            return Err(corrupt());
        }
        if let Some(ready) = ready {
            ready.require_ready().await?;
        } else {
            owner.require_closed().await?;
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT claim nativo", &e))?;
        Ok(TransformationOwnedClaim {
            claim: claim(r)?,
            owner: native,
        })
    }

    /// Reconstruct only from the validated job/journal pair. The returned
    /// descriptor still confers no physical closure authority.
    pub async fn owned_claim(&self, id: &str) -> Result<TransformationOwnedClaim, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN intento nativo", &e))?;
        let r = read(&mut tx, id).await?;
        let owner = r.journal.native_owner.clone().ok_or_else(stale)?;
        let claim = claim(r)?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT intento nativo", &e))?;
        Ok(TransformationOwnedClaim { claim, owner })
    }

    pub async fn check_registered_native_owner(
        &self,
        owner: &TransformationNativeOwner,
        ctx: &MutationContext,
    ) -> Result<TransformationClaim, LarmError> {
        let registered = self.owned_claim(&owner.fence.job_id).await?;
        if registered.owner != *owner
            || registered.claim.context != *ctx
            || !matches!(
                registered.claim.job.state,
                TransformationState::Running | TransformationState::Verifying
            )
            || registered
                .claim
                .job
                .metadata
                .get("attempt_closed")
                .is_some_and(|v| !v.is_null())
        {
            return Err(stale());
        }
        Ok(registered.claim)
    }

    /// Check immediately before worker IO. The caller retains physical input,
    /// process and workspace ownership across the RPC and its later commit.
    pub async fn check_active_claim(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
    ) -> Result<(), LarmError> {
        owner.recheck().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN comprobación de intento", &e))?;
        let r = read(&mut tx, &c.fence.job_id).await?;
        check_claim(&r, c, owner)?;
        if !matches!(
            r.job.state,
            TransformationState::Running | TransformationState::Verifying
        ) {
            return Err(invalid("transformation_attempt_not_running"));
        }
        owner.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT comprobación de intento", &e))?;
        Ok(())
    }
    pub async fn claim_next(&self) -> Result<Option<TransformationClaim>, LarmError> {
        self.claim_inner(None).await
    }
    pub async fn claim_job(&self, id: &str) -> Result<TransformationClaim, LarmError> {
        self.claim_inner(Some(id))
            .await?
            .ok_or_else(|| invalid("transformation_job_not_queued"))
    }
    async fn claim_inner(
        &self,
        id: Option<&str>,
    ) -> Result<Option<TransformationClaim>, LarmError> {
        if id.is_some_and(|id| !is_uuid_v7(id)) {
            return Err(corrupt());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN claim transformación", &e))?;
        lock(&mut tx).await?;
        let selected = if let Some(id) = id {
            Some(id.to_owned())
        } else {
            sqlx::query_scalar::<_,String>("SELECT id FROM transformation_job WHERE state='QUEUED' ORDER BY created_at,id LIMIT 1")
                .fetch_optional(&mut *tx).await.map_err(|e|db_error("seleccionando transformación",&e))?
        };
        let Some(id) = selected else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT cola vacía", &e))?;
            return Ok(None);
        };
        let r = read(&mut tx, &id).await?;
        native_intent::require_unprepared(&mut tx, &id).await?;
        let old_version = r.job.row_version;
        let r = project_claim(r, &uuid::Uuid::now_v7().to_string())?;
        persist(
            &mut tx,
            &r,
            old_version,
            TransformationState::Queued,
            "claimed",
        )
        .await?;
        let verified = read(&mut tx, &id).await?;
        if json!(verified.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT claim transformación", &e))?;
        Ok(Some(claim(r)?))
    }
    pub async fn mark_verifying(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
    ) -> Result<TransformationJob, LarmError> {
        owner.recheck().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN verificar transformación", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        check_claim(&r, c, owner)?;
        if r.job.state == TransformationState::Verifying {
            owner.recheck().await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de verificación", &e))?;
            return Ok(r.job);
        }
        if r.job.state != TransformationState::Running {
            return Err(invalid("transformation_attempt_not_running"));
        }
        let version = r.job.row_version;
        r.job
            .transition(
                TransformationState::Verifying,
                timestamp(chrono::Utc::now()),
            )
            .map_err(|_| corrupt())?;
        r.journal.state = r.job.state;
        persist(
            &mut tx,
            &r,
            version,
            TransformationState::Running,
            "verifying",
        )
        .await?;
        owner.recheck().await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT verificar transformación", &e))?;
        Ok(r.job)
    }
    /// A failed or interrupted attempt can release only its exact resources,
    /// after the retained physical owner proves all process/IO has drained.
    pub async fn finish_failed_attempt(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
        error: &LarmError,
        recoverable: bool,
    ) -> Result<TransformationJob, LarmError> {
        owner.require_closed().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cierre de transformación", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        check_claim(&r, c, owner)?;
        if r.journal.attempt_closed.is_some() {
            owner.require_closed().await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de cierre", &e))?;
            return Ok(r.job);
        }
        let old_state = r.job.state;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        if matches!(
            old_state,
            TransformationState::Failed | TransformationState::FailedRecoverable
        ) {
            r.job.row_version = r.job.row_version.checked_add(1).ok_or_else(corrupt)?;
            r.job.updated_at = at.clone();
        } else {
            if !matches!(
                old_state,
                TransformationState::Running | TransformationState::Verifying
            ) {
                return Err(invalid("transformation_attempt_cannot_fail"));
            }
            let normalized = redacted_error(error, &r.context)?;
            if recoverable {
                r.job
                    .mark_interrupted_recoverable(normalized, at.clone())
                    .map_err(|_| corrupt())?;
            } else {
                r.job.error = Some(normalized);
                r.job
                    .transition(TransformationState::Failed, &at)
                    .map_err(|_| corrupt())?;
            }
        }
        let closed = Closed {
            fence: c.fence.clone(),
            closed_at: at,
            state: r.job.state,
            system_restart: None,
        };
        r.job
            .metadata
            .insert("attempt_closed".into(), json!(closed));
        r.journal.attempt_closed = Some(closed);
        r.journal.error = r.job.error.clone();
        r.journal.state = r.job.state;
        r.job.validate().map_err(|_| corrupt())?;
        persist(&mut tx, &r, old_version, old_state, "attempt_closed").await?;
        let locks = sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=?")
            .bind(&r.job.id)
            .bind(c.fence.generation as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando guards del intento", &e))?;
        let reserves = sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
            .bind(&r.job.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberando reservas del intento", &e))?;
        if locks.rows_affected() != r.resources.len() as u64
            || reserves.rows_affected()
                != r.journal.plan.response.storage_estimate.per_volume.len() as u64
        {
            return Err(corrupt());
        }
        native_intent::close(&mut tx, &r).await?;
        owner.require_closed().await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cierre de transformación", &e))?;
        Ok(r.job)
    }

    /// Marca CANCELLING un trabajo propio no terminal (contrato 01: cualquier
    /// estado no terminal puede pedir cancelación). Transición, journal, evento
    /// y outbox se confirman en la misma transacción. Repetir sobre CANCELLING
    /// es idempotente; un trabajo ajeno o desconocido queda oculto (NOT_FOUND)
    /// y un terminal se rechaza con INVALID_STATE_TRANSITION.
    pub async fn request_cancellation(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<TransformationJob, LarmError> {
        if !is_uuid_v7(job_id)
            || !is_uuid_v7(&ctx.correlation_id)
            || principal_id.is_empty()
            || principal_id != ctx.actor_id
        {
            return Err(invalid("transformation_cancel_context_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cancelación de transformación", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, job_id).await?;
        if meta(&r.job, "actor_id").as_str() != Some(principal_id) {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "Trabajo desconocido",
                "transformation_job_missing",
            ));
        }
        if r.job.state == TransformationState::Cancelling {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de cancelación", &e))?;
            return Ok(r.job);
        }
        // Failed/FailedRecoverable sólo vuelven a cola con preflight nuevo
        // (contrato 01): no son cancelables, se rechazan con transición.
        if r.job.state.is_terminal()
            || matches!(
                r.job.state,
                TransformationState::Failed | TransformationState::FailedRecoverable
            )
        {
            return Err(invalid("transformation_job_not_cancellable"));
        }
        let old_state = r.job.state;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        r.job
            .transition(TransformationState::Cancelling, &at)
            .map_err(|_| corrupt())?;
        r.journal.state = r.job.state;
        r.job.validate().map_err(|_| corrupt())?;
        r.context = ctx.clone();
        persist(&mut tx, &r, old_version, old_state, "cancel_requested").await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cancelación de transformación", &e))?;
        Ok(r.job)
    }

    /// Cierra CANCELLED una cancelación todavía en cola (sin intento lanzado).
    /// Exige CANCELLING con `attempt_execution` ausente y CAS de revisión:
    /// si el consumidor reclamó el trabajo, la fila ya no coincide y se
    /// conserva CANCELLING para el cierre del intento. Libera locks y
    /// reservas de la admisión en la misma transacción.
    pub async fn release_queued_cancellation(
        &self,
        job_id: &str,
    ) -> Result<TransformationJob, LarmError> {
        if !is_uuid_v7(job_id) {
            return Err(invalid("transformation_cancel_context_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cierre de cancelación en cola", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, job_id).await?;
        if r.job.state == TransformationState::Cancelled {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de cancelación en cola", &e))?;
            return Ok(r.job);
        }
        if r.job.state != TransformationState::Cancelling || r.journal.attempt_execution.is_some() {
            return Err(stale());
        }
        let old_state = r.job.state;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        r.job
            .transition(TransformationState::Cancelled, &at)
            .map_err(|_| corrupt())?;
        r.journal.state = r.job.state;
        r.journal.cancelled_queued = Some(at.clone());
        r.job.validate().map_err(|_| corrupt())?;
        persist(&mut tx, &r, old_version, old_state, "cancelled_queued").await?;
        release_resources(&mut tx, &r, None).await?;
        native_intent::close_unclaimed(&mut tx, &r).await?;
        // Con el marcador cancelled_queued el registro re-leído exige los
        // recursos ya liberados; el contraste verifica que nada más mutó.
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cancelación en cola", &e))?;
        Ok(r.job)
    }

    /// Cierra el intento en curso como CANCELLED sólo con el testigo nativo
    /// cerrado y drenado. Replay idempotente si el cierre ya existió; exige
    /// CANCELLING y libera exactamente los recursos del intento en la misma
    /// transacción que el estado terminal.
    pub async fn finish_cancelled_attempt(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
    ) -> Result<TransformationJob, LarmError> {
        owner.require_closed().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cierre cancelado de transformación", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        check_claim(&r, c, owner)?;
        if r.journal.attempt_closed.is_some() {
            owner.require_closed().await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de cierre", &e))?;
            return Ok(r.job);
        }
        if r.job.state != TransformationState::Cancelling {
            return Err(invalid("transformation_attempt_cannot_cancel"));
        }
        let old_state = r.job.state;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        r.job
            .transition(TransformationState::Cancelled, &at)
            .map_err(|_| corrupt())?;
        let closed = Closed {
            fence: c.fence.clone(),
            closed_at: at,
            state: r.job.state,
            system_restart: None,
        };
        r.job
            .metadata
            .insert("attempt_closed".into(), json!(closed));
        r.journal.attempt_closed = Some(closed);
        r.journal.state = r.job.state;
        r.job.validate().map_err(|_| corrupt())?;
        persist(&mut tx, &r, old_version, old_state, "attempt_cancelled").await?;
        release_resources(&mut tx, &r, Some(c.fence.generation)).await?;
        native_intent::close(&mut tx, &r).await?;
        owner.require_closed().await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cierre cancelado de transformación", &e))?;
        Ok(r.job)
    }

    /// Cierra el intento de una intención REGISTERED cuyo testigo se perdió
    /// definitivamente por un reinicio del sistema: el boot registrado en la
    /// intención difiere del actual y el sistema operativo prueba que ningún
    /// proceso ni handle de esa sesión sobrevive. Misma transacción que el
    /// cierre de intento interrumpido (WORKER_INTERRUPTED recuperable,
    /// recursos liberados, intención cerrada) sin testigo que exija nada.
    pub async fn finish_interrupted_system_restart(
        &self,
        c: &TransformationClaim,
        previous_boot: u64,
        current_boot: u64,
    ) -> Result<TransformationJob, LarmError> {
        if previous_boot == 0 || previous_boot == current_boot {
            return Err(invalid("transformation_system_restart_proof_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN cierre sin testigo", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        if r.journal.attempt_closed.is_some() {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay de cierre", &e))?;
            return Ok(r.job);
        }
        if !matches!(
            r.job.state,
            TransformationState::FailedRecoverable | TransformationState::Cancelling
        ) {
            return Err(invalid("transformation_attempt_cannot_fail"));
        }
        let old_state = r.job.state;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        if r.job.state == TransformationState::FailedRecoverable {
            r.job.row_version = r.job.row_version.checked_add(1).ok_or_else(corrupt)?;
            r.job.updated_at = at.clone();
        } else {
            r.job
                .transition(TransformationState::Cancelled, &at)
                .map_err(|_| corrupt())?;
        }
        let closed = Closed {
            fence: c.fence.clone(),
            closed_at: at,
            state: r.job.state,
            system_restart: Some((previous_boot, current_boot)),
        };
        r.job
            .metadata
            .insert("attempt_closed".into(), json!(closed));
        r.journal.attempt_closed = Some(closed);
        r.journal.state = r.job.state;
        r.job.validate().map_err(|_| corrupt())?;
        persist(
            &mut tx,
            &r,
            old_version,
            old_state,
            "attempt_closed_system_restart",
        )
        .await?;
        release_resources(&mut tx, &r, Some(c.fence.generation)).await?;
        native_intent::close(&mut tx, &r).await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT cierre sin testigo", &e))?;
        Ok(r.job)
    }

    /// Retira la intención PREPARING de una sesión cuyo testigo se perdió
    /// definitivamente por un reinicio del sistema. El trabajo permanece
    /// QUEUED con su señal de despacho y sus recursos: el consumidor lo
    /// prepara de nuevo desde cero. Nunca muta estado de trabajo.
    pub async fn close_preparation_system_restart(
        &self,
        owner: &TransformationNativeOwner,
        current_boot: u64,
    ) -> Result<(), LarmError> {
        owner.validate()?;
        if owner.boot_session == 0 || owner.boot_session == current_boot {
            return Err(invalid("transformation_system_restart_proof_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN retiro de preparación sin testigo", &e))?;
        lock(&mut tx).await?;
        let r = read(&mut tx, &owner.fence.job_id).await?;
        if r.job.state != TransformationState::Queued {
            return Err(stale());
        }
        native_intent::close_unclaimed(&mut tx, &r).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT retiro de preparación sin testigo", &e))?;
        Ok(())
    }
}
/// Libera exactamente los recursos del intento: locks por generación (o todos
/// si el intento nunca se reclamó) y las reservas de la admisión.
async fn release_resources(
    conn: &mut SqliteConnection,
    r: &Record,
    generation: Option<u64>,
) -> Result<(), LarmError> {
    let locks = match generation {
        Some(generation) => {
            sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=?")
                .bind(&r.job.id)
                .bind(generation as i64)
                .execute(&mut *conn)
                .await
                .map_err(|e| db_error("liberando guards del intento", &e))?
        }
        None => sqlx::query("DELETE FROM resource_lock WHERE owner_id=?")
            .bind(&r.job.id)
            .execute(&mut *conn)
            .await
            .map_err(|e| db_error("liberando guards de la admisión", &e))?,
    };
    let reserves = sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
        .bind(&r.job.id)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("liberando reservas del intento", &e))?;
    if locks.rows_affected() != r.resources.len() as u64
        || reserves.rows_affected()
            != r.journal.plan.response.storage_estimate.per_volume.len() as u64
    {
        return Err(corrupt());
    }
    Ok(())
}
fn redacted_error(error: &LarmError, ctx: &MutationContext) -> Result<Value, LarmError> {
    fn redact(value: &mut Value) {
        match value {
            Value::String(s) => *s = larm_domain::redact::redact(s).chars().take(4096).collect(),
            Value::Array(a) => a.iter_mut().for_each(redact),
            Value::Object(o) => o.values_mut().for_each(redact),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(error).map_err(|_| corrupt())?;
    redact(&mut value);
    value["correlation_id"] = json!(ctx.correlation_id);
    Ok(value)
}
