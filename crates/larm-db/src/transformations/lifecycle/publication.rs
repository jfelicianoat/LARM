//! A durable graph precedes filesystem writes; publication and job closure
//! share one transaction. Physical proofs are retained by the caller.
use super::*;
use larm_domain::{
    artifacts::{ArtifactLocation, ArtifactState, Ownership},
    conformance::hash,
    locations::{LocationState, StorageLocation},
    model::{
        bundle_hash, required_hash, ArtifactProvenance, ArtifactRole, ArtifactSizeEstimate,
        BundleState, CanonicalModel, ModelArtifact, ModelBundle, ModelDistribution, ModelLimits,
        ModelRevision, ModelVariant, Trust,
    },
    transformation_publication::{
        same_destination, MeasuredTransformationOutput, TransformationMeasuredOutputGuard,
        TransformationPublicationGuard,
    },
    transformations::TransformationInputGuard,
    units::DecimalU64,
};

/// Minted only after the complete graph and its digest are durably committed.
/// Reconstructed by the repository from the fenced journal for recovery.
pub struct PreparedTransformationPublication {
    record: TransformationPublicationRecord,
    sha256: String,
}
impl PreparedTransformationPublication {
    pub fn record(&self) -> &TransformationPublicationRecord {
        &self.record
    }
    pub fn record_sha256(&self) -> &str {
        &self.sha256
    }
}

pub struct CommittedTransformationPublication {
    fence: TransformationAttemptFence,
    sha256: String,
}
impl CommittedTransformationPublication {
    pub fn fence(&self) -> &TransformationAttemptFence {
        &self.fence
    }
    pub fn record_sha256(&self) -> &str {
        &self.sha256
    }
}

pub(super) fn check_record(
    job: &TransformationJob,
    journal: &Journal,
    p: &TransformationPublicationRecord,
) -> Result<(), LarmError> {
    let digest = p.digest()?;
    if journal.attempt_execution.as_ref() != Some(&p.measured.fence)
        || p.measured.fence.job_id != job.id
        || p.variant.id != journal.planned_output_variant_id
        || p.measured.binding
            != *journal
                .plan
                .execution_binding
                .as_ref()
                .ok_or_else(corrupt)?
        || Some(&p.measured.policy) != journal.plan.output_policy.as_ref()
        || p.measured.target_format != journal.plan.request.target_format
        || p.destination.id != journal.plan.request.storage_location_id
        || p.input_identity != journal.input_identity
        || p.input_artifacts != journal.plan.input_artifacts
        || meta(job, "publication_sha256").as_str() != Some(&digest)
        || p.provenance.iter().any(|origin| {
            origin.actor_id != journal.actor_id
                || origin.tool_id.as_ref() != Some(&journal.plan.request.adapter_id)
                || origin.tool_version.as_ref() != Some(&job.tool_version)
                || json!(origin.parameters) != job.parameters
        })
    {
        return Err(corrupt());
    }
    Ok(())
}

async fn qualification(conn: &mut SqliteConnection, r: &Record) -> Result<(), LarmError> {
    let binding = r
        .journal
        .plan
        .execution_binding
        .as_ref()
        .ok_or_else(corrupt)?;
    let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(&binding.adapter_installation_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("instalación de salida", &e))?;
    let installation = crate::adapters::to_installation(&row)?;
    let run = crate::conformance::require_consumer_current(conn, &installation).await?;
    let profile = crate::conformance_profiles::load(conn, &installation.id)
        .await?
        .ok_or_else(corrupt)?;
    profile.check_installation(&installation)?;
    if installation.family != "transformation"
        || installation.adapter_id != r.journal.plan.request.adapter_id
        || run.id != binding.conformance_run_id
        || run.digest()? != binding.conformance_run_sha256
        || hash(&profile)? != binding.profile_sha256
    {
        return Err(stale());
    }
    Ok(())
}
async fn destination(
    conn: &mut SqliteConnection,
    r: &Record,
) -> Result<StorageLocation, LarmError> {
    let row = sqlx::query("SELECT * FROM storage_location WHERE id=?")
        .bind(&r.journal.plan.request.storage_location_id)
        .fetch_one(conn)
        .await
        .map_err(|e| db_error("destino de salida", &e))?;
    let location = crate::locations::to_location(&row)?;
    if !matches!(
        location.state,
        LocationState::Online | LocationState::LowSpace | LocationState::Degraded
    ) || r
        .journal
        .plan
        .output_policy
        .as_ref()
        .is_none_or(|p| p.cas_volume_key != location.volume_key)
    {
        return Err(stale());
    }
    Ok(location)
}
async fn current(
    conn: &mut SqliteConnection,
    r: &Record,
    c: &TransformationClaim,
    owner: &dyn TransformationAttemptGuard,
    input: &dyn TransformationInputGuard,
) -> Result<StorageLocation, LarmError> {
    check_claim(r, c, owner)?;
    owner.require_closed().await?;
    owner.recheck().await?;
    input.recheck().await?;
    if input.identity() != &r.journal.input_identity
        || input.locations() != r.journal.input_locations
    {
        return Err(stale());
    }
    crate::transformation_plans::admission::catalogue(conn, &r.journal.plan, input).await?;
    qualification(conn, r).await?;
    destination(conn, r).await
}

async fn build(
    conn: &mut SqliteConnection,
    r: &Record,
    measured: MeasuredTransformationOutput,
    destination: StorageLocation,
) -> Result<TransformationPublicationRecord, LarmError> {
    let row = sqlx::query("SELECT * FROM model_revision WHERE id=?")
        .bind(&r.journal.input_identity.revision_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("revisión original", &e))?;
    let revision: ModelRevision = crate::models::to_revision(&row)?;
    let row = sqlx::query("SELECT * FROM model_distribution WHERE id=?")
        .bind(&revision.distribution_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("distribución original", &e))?;
    let distribution: ModelDistribution = crate::models::to_distribution(&row)?;
    let row = sqlx::query("SELECT * FROM canonical_model WHERE id=?")
        .bind(&distribution.canonical_model_id)
        .fetch_one(conn)
        .await
        .map_err(|e| db_error("modelo original", &e))?;
    let model: CanonicalModel = crate::models::to_model(&row)?;
    let at = timestamp(chrono::Utc::now());
    let id = || uuid::Uuid::now_v7().to_string();
    let entries = measured
        .artifacts
        .iter()
        .map(|a| a.entry.clone())
        .collect::<Vec<_>>();
    let lineage = json!({"schema_version":1,"input_variant_id":r.job.input_variant_id,"input_artifacts":r.journal.plan.input_artifacts,
        "fence":measured.fence,"binding":measured.binding,"worker_job_id":measured.worker_job_id,
        "tool_id":r.journal.plan.request.adapter_id,"tool_version":r.job.tool_version,"parameters":r.job.parameters,
        "worker_manifest_hash":measured.worker_manifest_hash,"runtime_validation":measured.runtime,"created_at":at});
    // El linaje de cuantización pasa a ser estructural: el request sellado
    // del plan (validado por el worker cualificado y comparado por
    // check_claim) transporta el parámetro tal cual al campo de la variante;
    // la validez del enum no se re-valida aquí — se transporta.
    let quantization = r
        .job
        .parameters
        .get("quantization")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut variant = ModelVariant {
        id: r.journal.planned_output_variant_id.clone(),
        created_at: at.clone(),
        updated_at: at.clone(),
        row_version: 1,
        metadata: serde_json::Map::from_iter([("transformation".into(), lineage.clone())]),
        revision_id: revision.id.clone(),
        format: Some(measured.target_format.clone()),
        quantization,
        architecture: None,
        bundle_hash: Some(required_hash(&entries)?),
        lifecycle: "VERIFIED".into(),
        availability: "UNKNOWN".into(),
        ownership: Ownership::LarmManaged,
        promotion: "NONE".into(),
        system_role: "GENERAL".into(),
        capabilities: vec![],
        model_limits: ModelLimits::default(),
        system1_state: None,
        update_available: false,
        status: String::new(),
    };
    variant.status = variant.derived_status().into();
    let mut artifacts = Vec::new();
    let mut locations = Vec::new();
    let mut provenance = Vec::new();
    for artifact in &measured.artifacts {
        let entry = &artifact.entry;
        let size = DecimalU64::new(entry.size_bytes).ok_or_else(corrupt)?;
        let artifact_id = id();
        let role = serde_json::from_value::<ArtifactRole>(json!(artifact.worker_role))
            .unwrap_or(ArtifactRole::Other);
        artifacts.push(ModelArtifact {
            id: artifact_id.clone(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: serde_json::Map::from_iter([
                ("role".into(), json!(role)),
                ("worker_role".into(), json!(artifact.worker_role)),
            ]),
            variant_id: variant.id.clone(),
            filename: entry.relative_path.clone(),
            format: None,
            quantization: None,
            expected_size: Some(size),
            actual_size: Some(size),
            size_estimate: ArtifactSizeEstimate {
                confidence: larm_domain::model::ArtifactSizeConfidence::Exact,
                bytes: Some(size),
                lower_bound_bytes: Some(size),
                upper_bound_bytes: Some(size),
                source: larm_domain::model::ArtifactSizeSource::Manifest,
                evidence_refs: vec![format!("transformation:{}", r.job.id)],
                reason: None,
            },
            sha256: Some(entry.sha256.clone()),
            source_url: None,
            required: entry.required,
        });
        locations.push(ArtifactLocation {
            id: id(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            artifact_id: artifact_id.clone(),
            storage_location_id: destination.id.clone(),
            relative_path: format!("blobs/sha256/{}/{}", &entry.sha256[..2], entry.sha256),
            file_size: Some(size),
            sha256: Some(entry.sha256.clone()),
            state: ArtifactState::Online,
            verified_at: Some(at.clone()),
            ownership: Ownership::LarmManaged,
        });
        provenance.push(ArtifactProvenance {
            id: id(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: serde_json::Map::from_iter([("transformation".into(), lineage.clone())]),
            artifact_id,
            source_id: Some(distribution.source_id.clone()),
            revision_key: Some(revision.revision_key.clone()),
            parents: r
                .journal
                .plan
                .input_artifacts
                .iter()
                .map(|a| a.artifact_id.clone())
                .collect(),
            tool_id: Some(r.journal.plan.request.adapter_id.clone()),
            tool_version: Some(r.job.tool_version.clone()),
            parameters: r.job.parameters.as_object().cloned().ok_or_else(corrupt)?,
            license_id: None,
            license_hash: None,
            trust: Trust::Declared,
            actor_id: r.context.actor_id.clone(),
            evidence_refs: vec![format!("transformation:{}", r.job.id)],
        });
    }
    let bundle = ModelBundle {
        id: id(),
        created_at: at.clone(),
        updated_at: at,
        row_version: 1,
        metadata: serde_json::Map::new(),
        variant_id: variant.id.clone(),
        required_artifact_ids: artifacts
            .iter()
            .filter(|a| a.required)
            .map(|a| a.id.clone())
            .collect(),
        optional_artifact_ids: artifacts
            .iter()
            .filter(|a| !a.required)
            .map(|a| a.id.clone())
            .collect(),
        manifest_hash: bundle_hash(&entries)?,
        state: BundleState::Verified,
    };
    let record = TransformationPublicationRecord {
        schema_version: 1,
        measured,
        input_identity: r.journal.input_identity.clone(),
        input_artifacts: r.journal.plan.input_artifacts.clone(),
        destination,
        model,
        distribution,
        revision,
        variant,
        artifacts,
        bundle,
        locations,
        provenance,
    };
    record.validate()?;
    Ok(record)
}

impl SqlTransformationJobs {
    pub async fn check_publication_space(
        &self,
        prepared: &PreparedTransformationPublication,
        snapshot: &larm_domain::transformations::TransformationSpaceSnapshot,
    ) -> Result<(), LarmError> {
        if !(0..5000).contains(&(chrono::Utc::now() - snapshot.observed_at).num_milliseconds()) {
            return Err(stale());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN espacio de publicación", &e))?;
        let r = read(&mut tx, &prepared.record.measured.fence.job_id).await?;
        if r.job.state != TransformationState::Verifying
            || r.journal.publication.as_ref() != Some(&prepared.record)
            || prepared.record.digest()? != prepared.sha256
            || snapshot.demands.len() != r.journal.plan.response.storage_estimate.per_volume.len()
        {
            return Err(stale());
        }
        let mut seen = BTreeSet::new();
        for demand in &snapshot.demands {
            let estimate = r
                .journal
                .plan
                .response
                .storage_estimate
                .per_volume
                .iter()
                .find(|v| v.volume_key == demand.volume_key)
                .ok_or_else(stale)?;
            let margin = larm_domain::estimate::margin(
                estimate.peak_extra_bytes.get(),
                snapshot.margin_policy,
            )
            .map_err(|_| corrupt())?;
            if !seen.insert(&demand.volume_key)
                || margin != estimate.margin_bytes.get()
                || estimate.peak_extra_bytes.get().checked_add(margin)
                    != Some(demand.required_bytes)
            {
                return Err(stale());
            }
            let others:i64=sqlx::query_scalar("SELECT coalesce(sum(remaining_bytes),0) FROM volume_reservation WHERE volume_key=? AND job_id<>?")
                .bind(&demand.volume_key).bind(&r.job.id).fetch_one(&mut *tx).await.map_err(|e|db_error("otras reservas de publicación",&e))?;
            if others < 0
                || demand
                    .os_free
                    .saturating_sub(demand.policy_minimum)
                    .saturating_sub(others as u64)
                    < demand.required_bytes
            {
                return Err(LarmError::new(
                    ErrorCode::StorageInsufficient,
                    "No hay espacio suficiente para incorporar la salida",
                    "transformation_publication_space_insufficient",
                ));
            }
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT espacio de publicación", &e))?;
        Ok(())
    }
    pub async fn prepared_publication(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
        input: &dyn TransformationInputGuard,
    ) -> Result<PreparedTransformationPublication, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN recuperar publicación", &e))?;
        let r = read(&mut tx, &c.fence.job_id).await?;
        let destination = current(&mut tx, &r, c, owner, input).await?;
        let record = r.journal.publication.as_ref().ok_or_else(corrupt)?;
        if r.job.state != TransformationState::Verifying
            || !same_destination(&destination, &record.destination)
        {
            return Err(stale());
        }
        let result = PreparedTransformationPublication {
            record: record.clone(),
            sha256: record.digest()?,
        };
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT recuperar publicación", &e))?;
        Ok(result)
    }

    pub async fn confirm_publication(
        &self,
        prepared: &PreparedTransformationPublication,
    ) -> Result<CommittedTransformationPublication, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN confirmar publicación", &e))?;
        let r = read(&mut tx, &prepared.record.measured.fence.job_id).await?;
        if r.job.state != TransformationState::Completed
            || r.journal.publication.as_ref() != Some(&prepared.record)
            || r.journal.attempt_closed.as_ref().map(|c| &c.fence)
                != Some(&prepared.record.measured.fence)
            || prepared.sha256 != prepared.record.digest()?
        {
            return Err(stale());
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT confirmar publicación", &e))?;
        Ok(CommittedTransformationPublication {
            fence: prepared.record.measured.fence.clone(),
            sha256: prepared.sha256.clone(),
        })
    }
    pub async fn prepare_publication(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
        input: &dyn TransformationInputGuard,
        output: &dyn TransformationMeasuredOutputGuard,
    ) -> Result<PreparedTransformationPublication, LarmError> {
        owner.require_closed().await?;
        output.recheck().await?;
        let measured = output.measured();
        measured.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN preparar salida", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        let destination = current(&mut tx, &r, c, owner, input).await?;
        if measured.fence != c.fence
            || Some(&measured.binding) != r.journal.plan.execution_binding.as_ref()
            || Some(&measured.policy) != r.journal.plan.output_policy.as_ref()
            || measured.target_format != r.journal.plan.request.target_format
            || r.job.state != TransformationState::Verifying
            || r.journal.attempt_closed.is_some()
        {
            return Err(stale());
        }
        if let Some(record) = &r.journal.publication {
            if record.measured != measured || !same_destination(&destination, &record.destination) {
                return Err(stale());
            }
            let result = PreparedTransformationPublication {
                record: record.clone(),
                sha256: record.digest()?,
            };
            output.recheck().await?;
            owner.recheck().await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay preparar salida", &e))?;
            return Ok(result);
        }
        let record = build(&mut tx, &r, measured, destination).await?;
        let sha256 = record.digest()?;
        let old_version = r.job.row_version;
        r.job.metadata.insert("publication".into(), json!(record));
        r.job
            .metadata
            .insert("publication_sha256".into(), json!(sha256));
        r.journal.publication = Some(record.clone());
        r.job.row_version = old_version.checked_add(1).ok_or_else(corrupt)?;
        r.job.updated_at = timestamp(chrono::Utc::now());
        // Bound durable graph, job/journal JSON, future root manifest and
        // receipts before any publisher can obtain a filesystem ticket.
        let bytes = serde_json::to_vec(&record).map_err(|_| corrupt())?.len() as u64;
        let retained = serde_json::to_vec(&r.journal).map_err(|_| corrupt())?.len() as u64
            + serde_json::to_vec(&r.job).map_err(|_| corrupt())?.len() as u64;
        let control = bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(retained))
            .and_then(|n| n.checked_add(65_536))
            .and_then(|n| n.checked_add(record.artifacts.len() as u64 * 4096))
            .ok_or_else(corrupt)?;
        if control > record.measured.policy.control_bytes.get() {
            return Err(invalid("transformation_output_control_budget_exceeded"));
        }
        persist(
            &mut tx,
            &r,
            old_version,
            TransformationState::Verifying,
            "publication_prepared",
        )
        .await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        output.recheck().await?;
        input.recheck().await?;
        owner.require_closed().await?;
        owner.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT preparar salida", &e))?;
        Ok(PreparedTransformationPublication { record, sha256 })
    }

    pub async fn complete_publication(
        &self,
        c: &TransformationClaim,
        owner: &dyn TransformationAttemptGuard,
        input: &dyn TransformationInputGuard,
        prepared: &PreparedTransformationPublication,
        physical: &dyn TransformationPublicationGuard,
    ) -> Result<TransformationJob, LarmError> {
        owner.require_closed().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN publicar salida", &e))?;
        lock(&mut tx).await?;
        let mut r = read(&mut tx, &c.fence.job_id).await?;
        check_claim(&r, c, owner)?;
        let record = r.journal.publication.as_ref().ok_or_else(corrupt)?.clone();
        if record != prepared.record
            || record.digest()? != prepared.sha256
            || physical.record_sha256() != prepared.sha256
            || physical.fence() != &c.fence
        {
            return Err(stale());
        }
        if r.job.state == TransformationState::Completed {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay publicar salida", &e))?;
            return Ok(r.job);
        }
        let destination = current(&mut tx, &r, c, owner, input).await?;
        if r.job.state != TransformationState::Verifying
            || r.journal.attempt_closed.is_some()
            || !same_destination(&destination, &record.destination)
        {
            return Err(stale());
        }
        physical.recheck().await?;
        crate::variants::insert_variant_in(
            &mut tx,
            &record.variant,
            &record.artifacts,
            Some(&record.bundle),
            &r.context,
        )
        .await?;
        crate::artifacts::insert_locations_in(&mut tx, &record.locations, &r.context).await?;
        for provenance in &record.provenance {
            crate::models::insert_provenance_in(&mut tx, provenance, &r.context).await?;
        }
        check_inserted(&mut tx, &record).await?;
        let old_version = r.job.row_version;
        let at = timestamp(chrono::Utc::now());
        r.job.output_variant_id = Some(record.variant.id.clone());
        r.job.error = None;
        r.job
            .transition(TransformationState::Completed, &at)
            .map_err(|_| corrupt())?;
        let closed = Closed {
            fence: c.fence.clone(),
            closed_at: at,
            state: TransformationState::Completed,
            system_restart: None,
        };
        r.job
            .metadata
            .insert("attempt_closed".into(), json!(closed));
        r.journal.attempt_closed = Some(closed);
        r.journal.state = r.job.state;
        r.journal.error = None;
        persist(
            &mut tx,
            &r,
            old_version,
            TransformationState::Verifying,
            "completed",
        )
        .await?;
        let locks = sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=?")
            .bind(&r.job.id)
            .bind(c.fence.generation as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberar guards de salida", &e))?;
        let reservations = sqlx::query("DELETE FROM volume_reservation WHERE job_id=?")
            .bind(&r.job.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("liberar reservas de salida", &e))?;
        if locks.rows_affected() != r.resources.len() as u64
            || reservations.rows_affected()
                != r.journal.plan.response.storage_estimate.per_volume.len() as u64
        {
            return Err(corrupt());
        }
        native_intent::close(&mut tx, &r).await?;
        if json!(read(&mut tx, &r.job.id).await?.job) != json!(r.job) {
            return Err(corrupt());
        }
        physical.recheck().await?;
        input.recheck().await?;
        owner.require_closed().await?;
        owner.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT publicar salida", &e))?;
        Ok(r.job)
    }
}

async fn check_inserted(
    conn: &mut SqliteConnection,
    p: &TransformationPublicationRecord,
) -> Result<(), LarmError> {
    let row = sqlx::query("SELECT * FROM model_variant WHERE id=?")
        .bind(&p.variant.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("confirmar variante", &e))?;
    if crate::variants::to_variant(&row)? != p.variant {
        return Err(corrupt());
    }
    let row = sqlx::query("SELECT * FROM model_bundle WHERE id=?")
        .bind(&p.bundle.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("confirmar bundle", &e))?;
    if crate::variants::to_bundle(&row)? != p.bundle {
        return Err(corrupt());
    }
    for ((artifact, location), provenance) in
        p.artifacts.iter().zip(&p.locations).zip(&p.provenance)
    {
        let row = sqlx::query("SELECT * FROM model_artifact WHERE id=?")
            .bind(&artifact.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_error("confirmar artefacto", &e))?;
        if crate::variants::to_artifact(&row)? != *artifact {
            return Err(corrupt());
        }
        let row = sqlx::query("SELECT * FROM artifact_location WHERE id=?")
            .bind(&location.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_error("confirmar ubicación", &e))?;
        if crate::artifacts::to_location(&row)? != *location {
            return Err(corrupt());
        }
        let row = sqlx::query("SELECT * FROM artifact_provenance WHERE id=?")
            .bind(&provenance.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_error("confirmar procedencia", &e))?;
        if crate::models::to_provenance(&row)? != *provenance {
            return Err(corrupt());
        }
    }
    Ok(())
}
