//! Physical facts from a completed, sealed remote acquisition. Source declarations
//! stay immutable: an observed local digest is never an upstream checksum.
use crate::download_jobs::DownloadedArtifact;
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use larm_domain::download::DownloadPlanRecord;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::guard::{LockMode, Resource, ResourceKind};
use larm_domain::model::{
    bundle_hash, missing_roles, required_hash, validate_lifecycle_transition, BundleEntry,
    LifecycleEvidence, ModelArtifact, ModelBundle, ModelLifecycle,
};
use larm_domain::ports::MutationContext;
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "No se pudo conciliar el bundle descargado",
        reason,
    )
}

async fn transition(
    tx: &mut SqliteConnection,
    variant_id: &str,
    to: ModelLifecycle,
    evidence: LifecycleEvidence,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let (from, version, mut availability, update_available): (String, i64, String, bool) =
        sqlx::query_as("SELECT lifecycle,row_version,availability,update_available FROM model_variant WHERE id=?")
            .bind(variant_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_error("leyendo etapa", &e))?;
    let parsed =
        ModelLifecycle::parse(&from).ok_or_else(|| invalid("download_lifecycle_invalid"))?;
    validate_lifecycle_transition(parsed, to, &evidence)?;
    if parsed == ModelLifecycle::Corrupt && to == ModelLifecycle::Verified {
        availability = "UNKNOWN".into();
    }
    let status =
        larm_domain::model::ModelVariant::status_for(to.as_str(), &availability, update_available);
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let changed = sqlx::query("UPDATE model_variant SET lifecycle=?,status=?,availability=?,updated_at=?,row_version=row_version+1 WHERE id=? AND row_version=?")
        .bind(to.as_str()).bind(status).bind(&availability).bind(now).bind(variant_id).bind(version)
        .execute(&mut *tx).await.map_err(|e| db_error("cambiando etapa de descarga", &e))?;
    if changed.rows_affected() != 1 {
        return Err(invalid("download_variant_changed"));
    }
    emit(
        tx,
        "model_variant",
        variant_id,
        version + 1,
        serde_json::json!({"action":"download_lifecycle", "from":from,"to":to.as_str()}),
        ctx,
    )
    .await
}

async fn emit(
    tx: &mut SqliteConnection,
    kind: &str,
    id: &str,
    version: i64,
    data: serde_json::Value,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    append(
        tx,
        &NewEvent {
            event_type: "model.state_changed",
            resource_type: kind,
            resource_id: id,
            resource_version: version,
            data,
        },
        ctx,
    )
    .await?;
    Ok(())
}

async fn idle(
    tx: &mut SqliteConnection,
    variant_id: &str,
    job_id: Option<&str>,
) -> Result<(), LarmError> {
    let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runtime_instance i LEFT JOIN runtime_lease l ON l.instance_id=i.id AND l.state NOT IN ('EXPIRED','RELEASED','FAILED') WHERE i.variant_id=? AND (i.state<>'STOPPED' OR l.id IS NOT NULL)) OR EXISTS(SELECT 1 FROM download_job WHERE variant_id=? AND (? IS NULL OR id<>?) AND state NOT IN ('COMPLETED','FAILED','CANCELLED'))")
        .bind(variant_id).bind(variant_id).bind(job_id).bind(job_id)
        .fetch_one(&mut *tx).await.map_err(|e| db_error("revalidando uso de variante", &e))?;
    if busy {
        return Err(invalid("download_variant_in_use"));
    }
    Ok(())
}

fn resources(record: &DownloadPlanRecord) -> Vec<(Resource, LockMode)> {
    let mut resources = vec![(
        Resource::new(ResourceKind::Variant, &record.response.variant_id),
        LockMode::Exclusive,
    )];
    resources.extend(
        record
            .response
            .artifact_estimates
            .iter()
            .filter(|a| a.selected)
            .map(|a| {
                (
                    Resource::new(ResourceKind::Artifact, &a.artifact_id),
                    LockMode::Exclusive,
                )
            }),
    );
    if let Some(estimate) = &record.response.storage_estimate {
        resources.extend(estimate.per_volume.iter().map(|v| {
            (
                Resource::new(ResourceKind::Volume, &v.volume_key),
                LockMode::Shared,
            )
        }));
    }
    larm_domain::guard::normalize(&resources)
}

pub(crate) async fn release(
    tx: &mut SqliteConnection,
    job: &larm_domain::download::DownloadJob,
) -> Result<(), LarmError> {
    if job
        .metadata
        .get("acquisition")
        .is_some_and(|v| !v.is_null())
    {
        let owner = job
            .metadata
            .get("plan_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("download_job_plan_missing"))?;
        sqlx::query("DELETE FROM resource_lock WHERE owner_id=? AND generation=0")
            .bind(owner)
            .execute(tx)
            .await
            .map_err(|e| db_error("liberando locks de descarga", &e))?;
    }
    Ok(())
}

/// Called only after the confirmation, current preflight and admission checks,
/// inside the same writer transaction that creates the job and reservation.
pub(crate) async fn admit(
    tx: &mut SqliteConnection,
    record: &DownloadPlanRecord,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    if record.response.acquisition.is_none() {
        return Ok(());
    }
    let id = &record.response.variant_id;
    let owner = record
        .response
        .plan_id
        .as_deref()
        .ok_or_else(|| invalid("download_job_plan_missing"))?;
    crate::guard::acquire_in(tx, owner, 0, &resources(record), None).await?;
    idle(tx, id, None).await?;
    let state: String = sqlx::query_scalar("SELECT lifecycle FROM model_variant WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo etapa", &e))?;
    let evidence = LifecycleEvidence {
        selection_recorded: true,
        preflight_valid: true,
        job_admitted: true,
        ..LifecycleEvidence::default()
    };
    if state == "DISCOVERED" {
        transition(tx, id, ModelLifecycle::Selected, evidence, ctx).await?;
    }
    if matches!(state.as_str(), "DISCOVERED" | "SELECTED") {
        transition(tx, id, ModelLifecycle::DownloadPlanned, evidence, ctx).await?;
    }
    if matches!(
        state.as_str(),
        "DISCOVERED" | "SELECTED" | "DOWNLOAD_PLANNED"
    ) {
        transition(tx, id, ModelLifecycle::Downloading, evidence, ctx).await?;
    } else if !matches!(
        state.as_str(),
        "DOWNLOADING"
            | "DOWNLOADED"
            | "VERIFIED"
            | "PREPARED"
            | "TECHNICALLY_VALIDATED"
            | "AVAILABLE"
            | "CORRUPT"
    ) {
        return Err(invalid("download_lifecycle_not_admissible"));
    }
    Ok(())
}

/// Exact membership matters: all required declarations, only selected optional
/// ones. Unselected optional files may legitimately have unknown size/digest.
fn members(
    bundle: &ModelBundle,
    artifacts: Vec<ModelArtifact>,
) -> Result<Vec<ModelArtifact>, LarmError> {
    let required: BTreeSet<_> = bundle
        .required_artifact_ids
        .iter()
        .map(String::as_str)
        .collect();
    let optional: BTreeSet<_> = bundle
        .optional_artifact_ids
        .iter()
        .map(String::as_str)
        .collect();
    if required.is_empty()
        || required.len() != bundle.required_artifact_ids.len()
        || optional.len() != bundle.optional_artifact_ids.len()
        || !required.is_disjoint(&optional)
        || artifacts
            .iter()
            .filter(|a| a.required)
            .map(|a| a.id.as_str())
            .collect::<BTreeSet<_>>()
            != required
    {
        return Err(invalid("download_bundle_membership_mismatch"));
    }
    let chosen: Vec<_> = artifacts
        .into_iter()
        .filter(|a| required.contains(a.id.as_str()) || optional.contains(a.id.as_str()))
        .collect();
    if chosen.len() != required.len() + optional.len()
        || chosen.iter().any(|a| {
            a.variant_id != bundle.variant_id || a.required != required.contains(a.id.as_str())
        })
    {
        return Err(invalid("download_bundle_membership_mismatch"));
    }
    Ok(chosen)
}

fn entries(artifacts: &[ModelArtifact]) -> Option<Vec<BundleEntry>> {
    artifacts
        .iter()
        .map(|a| {
            Some(BundleEntry {
                relative_path: a.filename.clone(),
                sha256: a.sha256.clone()?,
                size_bytes: a.expected_size?.get(),
                required: a.required,
            })
        })
        .collect()
}

pub(crate) async fn check_fence(
    tx: &mut SqliteConnection,
    record: &DownloadPlanRecord,
) -> Result<(), LarmError> {
    let owner = record
        .response
        .plan_id
        .as_deref()
        .ok_or_else(|| invalid("download_job_plan_missing"))?;
    for (resource, mode) in resources(record) {
        let held:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource_lock WHERE resource_id=? AND owner_id=? AND generation=0 AND mode=?)")
            .bind(resource.key()).bind(owner).bind(mode.as_str()).fetch_one(&mut *tx).await.map_err(|e|db_error("revalidando locks de descarga",&e))?;
        if !held {
            return Err(invalid("download_resource_fence_lost"));
        }
    }
    Ok(())
}

pub(crate) async fn reconcile(
    tx: &mut SqliteConnection,
    record: &DownloadPlanRecord,
    downloaded: &[DownloadedArtifact],
    job_id: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    if record.response.acquisition.is_none() {
        return Ok(());
    }
    if record.response.plan_hash.as_deref() != Some(record.sealed_hash(true)?.as_str()) {
        return Err(invalid("download_plan_seal_mismatch"));
    }
    let variant_id = &record.response.variant_id;
    check_fence(tx, record).await?;
    idle(tx, variant_id, Some(job_id)).await?;
    let row = sqlx::query("SELECT * FROM model_variant WHERE id=?")
        .bind(variant_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo variante descargada", &e))?;
    let variant = crate::variants::to_variant(&row)?;
    let rows = sqlx::query("SELECT * FROM model_artifact WHERE variant_id=? ORDER BY filename,id")
        .bind(variant_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo artefactos físicos", &e))?;
    let all = rows
        .iter()
        .map(crate::variants::to_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    let selected: Vec<_> = record
        .response
        .artifact_estimates
        .iter()
        .filter(|a| a.selected)
        .collect();
    let mut bundle = ModelBundle {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        updated_at: String::new(),
        row_version: 1,
        metadata: serde_json::Map::new(),
        variant_id: variant_id.clone(),
        required_artifact_ids: selected
            .iter()
            .filter(|a| a.required)
            .map(|a| a.artifact_id.clone())
            .collect(),
        optional_artifact_ids: selected
            .iter()
            .filter(|a| !a.required)
            .map(|a| a.artifact_id.clone())
            .collect(),
        manifest_hash: String::new(),
        state: larm_domain::model::BundleState::Incomplete,
    };
    bundle.required_artifact_ids.sort();
    bundle.optional_artifact_ids.sort();
    let mut chosen = members(&bundle, all)?;
    let actual: BTreeMap<_, _> = downloaded
        .iter()
        .map(|a| (a.artifact_id.as_str(), a))
        .collect();
    if actual.len() != chosen.len() {
        return Err(invalid("download_bundle_membership_mismatch"));
    }
    for artifact in &mut chosen {
        let observed = actual
            .get(artifact.id.as_str())
            .ok_or_else(|| invalid("download_bundle_membership_mismatch"))?;
        artifact.sha256 = Some(observed.sha256.clone());
        artifact.expected_size = larm_domain::units::DecimalU64::new(observed.size_bytes);
    }
    let descriptors =
        entries(&chosen).ok_or_else(|| invalid("download_bundle_descriptors_missing"))?;
    bundle.manifest_hash = bundle_hash(&descriptors)?;
    let identity = required_hash(&descriptors)?;
    if variant
        .bundle_hash
        .as_ref()
        .is_some_and(|previous| previous != &identity)
    {
        return Err(invalid("download_bundle_identity_conflict"));
    }
    if missing_roles(variant.format.as_deref(), &variant.capabilities, &chosen)
        .is_none_or(|roles| roles.is_empty())
    {
        bundle.state = larm_domain::model::BundleState::Verified;
    }
    bundle.updated_at = bundle.created_at.clone();
    // Existing identity must never be silently remapped onto another Core UUID.
    let collision: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_variant WHERE id<>? AND revision_id=? AND format IS ? AND coalesce(quantization,'')=coalesce(?,'') AND architecture IS ? AND bundle_hash=?)")
        .bind(variant_id).bind(&variant.revision_id).bind(&variant.format).bind(&variant.quantization)
        .bind(&variant.architecture).bind(&identity).fetch_one(&mut *tx).await.map_err(|e|db_error("revalidando identidad física",&e))?;
    if collision {
        return Err(invalid("download_bundle_identity_collision"));
    }
    sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,?,?)")
        .bind(&bundle.id).bind(&bundle.created_at).bind(&bundle.updated_at).bind(variant_id)
        .bind(serde_json::to_string(&bundle.required_artifact_ids).map_err(|e|db_error("miembros requeridos",&e))?)
        .bind(serde_json::to_string(&bundle.optional_artifact_ids).map_err(|e|db_error("miembros opcionales",&e))?)
        .bind(&bundle.manifest_hash).bind(bundle.state.as_str()).execute(&mut *tx).await.map_err(|e|db_error("registrando bundle físico",&e))?;
    emit(tx,"model_bundle",&bundle.id,1,serde_json::json!({"action":"downloaded","state":bundle.state.as_str(),"variant_id":variant_id}),ctx).await?;
    if variant.bundle_hash.is_none() {
        sqlx::query("UPDATE model_variant SET bundle_hash=?,updated_at=?,row_version=row_version+1 WHERE id=?")
            .bind(&identity).bind(&bundle.updated_at).bind(variant_id).execute(&mut *tx).await.map_err(|e|db_error("registrando identidad física",&e))?;
        emit(
            tx,
            "model_variant",
            variant_id,
            variant.row_version + 1,
            serde_json::json!({"action":"bundle_reconciled","bundle_hash":identity}),
            ctx,
        )
        .await?;
    }
    let evidence = LifecycleEvidence {
        all_bytes_present: true,
        hashes_and_bundle_verified: true,
        reverification_passed: true,
        ..LifecycleEvidence::default()
    };
    if variant.lifecycle == "DOWNLOADING" {
        transition(tx, variant_id, ModelLifecycle::Downloaded, evidence, ctx).await?;
    }
    if bundle.state == larm_domain::model::BundleState::Verified
        && matches!(
            variant.lifecycle.as_str(),
            "DOWNLOADING" | "DOWNLOADED" | "CORRUPT"
        )
    {
        transition(tx, variant_id, ModelLifecycle::Verified, evidence, ctx).await?;
    }
    Ok(())
}

/// A physical projection for verification, never written over source facts.
/// The descriptor is frozen by an immutable acquisition, the plan seal, the
/// completed job result and the typed bundle hash/membership; no free metadata
/// or current file checksum can replace that expected descriptor.
pub(crate) async fn physical_artifacts(
    tx: &mut SqliteConnection,
    bundle: &ModelBundle,
    artifacts: Vec<ModelArtifact>,
) -> Result<Vec<ModelArtifact>, LarmError> {
    let chosen = members(bundle, artifacts)?;
    if entries(&chosen)
        .is_some_and(|list| bundle_hash(&list).is_ok_and(|hash| hash == bundle.manifest_hash))
    {
        return Ok(chosen);
    }
    let rows: Vec<(String,String,String)>=sqlx::query_as("SELECT j.id,j.plan_hash,o.payload FROM download_job j JOIN operation_journal o ON o.id=json_extract(j.metadata,'$.plan_id') AND o.plan_hash=j.plan_hash WHERE j.variant_id=? AND j.state='COMPLETED' AND o.kind='download.plan' AND o.phase='COMPLETED' ORDER BY j.updated_at DESC,j.id DESC")
        .bind(&bundle.variant_id).fetch_all(&mut *tx).await.map_err(|e|db_error("leyendo evidencia física",&e))?;
    for (job_id, seal, payload) in rows {
        let payload: serde_json::Value =
            serde_json::from_str(&payload).map_err(|e| db_error("evidencia física", &e))?;
        if payload
            .pointer("/execution/job_id")
            .and_then(serde_json::Value::as_str)
            != Some(job_id.as_str())
        {
            return Err(invalid("download_bundle_evidence_invalid"));
        }
        let record: DownloadPlanRecord = serde_json::from_value(payload["record"].clone())
            .map_err(|e| db_error("plan físico", &e))?;
        if record.response.variant_id != bundle.variant_id
            || record.response.plan_hash.as_deref() != Some(seal.as_str())
            || record.sealed_hash(true)? != seal
        {
            return Err(invalid("download_plan_seal_mismatch"));
        }
        let binding = record
            .response
            .acquisition
            .as_ref()
            .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
        let row = sqlx::query("SELECT * FROM model_acquisition_manifest WHERE id=?")
            .bind(&binding.manifest_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("snapshot físico histórico", &e))?
            .ok_or_else(|| invalid("download_bundle_evidence_missing"))?;
        let snapshot = crate::acquisition::decode(&row)?;
        snapshot.validate_download_record(&record)?;
        let selected: BTreeSet<_> = record
            .response
            .artifact_estimates
            .iter()
            .filter(|a| a.selected)
            .map(|a| a.artifact_id.as_str())
            .collect();
        if selected
            != chosen
                .iter()
                .map(|a| a.id.as_str())
                .collect::<BTreeSet<_>>()
        {
            continue;
        }
        let outputs = payload
            .pointer("/execution/result/artifacts")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
        if outputs.len() != chosen.len() {
            return Err(invalid("download_bundle_evidence_invalid"));
        }
        let mut projected = chosen.clone();
        let mut seen = BTreeSet::new();
        for output in outputs {
            let id = output["artifact_id"]
                .as_str()
                .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
            if !seen.insert(id) {
                return Err(invalid("download_bundle_evidence_invalid"));
            }
            let artifact = projected
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
            let declaration = snapshot
                .artifacts
                .iter()
                .find(|a| a.artifact_id == id)
                .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
            if declaration.relative_path != artifact.filename
                || declaration.required != artifact.required
            {
                return Err(invalid("download_bundle_evidence_invalid"));
            }
            let digest = output["sha256"]
                .as_str()
                .filter(|d| larm_domain::adapter_updates::is_sha256(d))
                .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
            let size = output["size_bytes"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .and_then(larm_domain::units::DecimalU64::new)
                .ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
            artifact.sha256 = Some(digest.to_string());
            artifact.expected_size = Some(size);
        }
        let list =
            entries(&projected).ok_or_else(|| invalid("download_bundle_evidence_invalid"))?;
        if bundle_hash(&list)? == bundle.manifest_hash {
            return Ok(projected);
        }
    }
    Err(invalid("download_bundle_evidence_missing"))
}
