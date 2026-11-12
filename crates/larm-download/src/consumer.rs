//! Consumidor durable P051: source plan, bloques, staging y CAS.

use async_trait::async_trait;
use larm_adapter_sdk::source::{
    download_spec, validate_source_plan, DownloadPlanRequest as SourcePlanRequest, RevisionRef,
    SourceDownloadChunk, SourceDownloadChunkRequest,
};
use larm_db::download_jobs::{DownloadClaim, DownloadedArtifact, SqlDownloadJobs};
use larm_domain::acquisition::ModelAcquisitionManifest;
use larm_domain::download::{
    DownloadArtifactEstimate, DownloadJob, DownloadPlanRecord, DownloadPriority,
    DownloadSourceCandidate,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{ArtifactSizeConfidence, Trust};
use larm_domain::ports::DownloadPlanRepository;
use larm_domain::units::DecimalU64;
use larm_storage::cas::{finish_staged_promotion, promote_staged, recover_staged_promotion};
use std::path::PathBuf;

use crate::transfer::{
    SqliteTransferProgress, StagingStore, TransferLimits, TransferOutcome, TransferRequest,
    VerifiedArtifact,
};

#[async_trait]
pub trait DownloadSourceSession: Send + Sync {
    /// Resuelto y cualificado por el Core antes de abrir el worker. Nunca se
    /// obtiene del plugin ni se reconstruye a partir de sus UUID privados.
    fn acquisition(&self) -> Option<&ModelAcquisitionManifest> {
        None
    }
    async fn download_plan(
        &self,
        request: SourcePlanRequest,
    ) -> Result<larm_domain::artifacts::Plan, LarmError>;

    async fn download_chunk(
        &self,
        request: SourceDownloadChunkRequest,
    ) -> Result<SourceDownloadChunk, LarmError>;

    /// El consumidor no cierra ni completa el job hasta drenar el proceso y
    /// todos sus canales de IO.
    async fn close(&self) -> Result<(), LarmError>;
}

#[async_trait]
pub trait DownloadSourceResolver: Send + Sync {
    /// Resuelve fuente, cuenta/licencia, instalación, permisos y cualificación.
    /// La implementación debe fallar cerrado si no puede autorizar el job.
    async fn open(
        &self,
        claim: &DownloadClaim,
        record: &DownloadPlanRecord,
        candidate: &DownloadSourceCandidate,
    ) -> Result<Box<dyn DownloadSourceSession>, LarmError>;
}

#[async_trait]
pub trait DownloadStorageRootResolver: Send + Sync {
    async fn root(&self, location_id: &str) -> Result<PathBuf, LarmError>;

    async fn cleanup_root(&self, location_id: &str) -> Result<PathBuf, LarmError> {
        self.root(location_id).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadConsumerOutcome {
    Idle,
    Completed { job_id: String },
    Paused { job_id: String },
    Cancelled { job_id: String },
    Failed { job_id: String, reason: String },
}

pub struct DownloadConsumer {
    pub jobs: SqlDownloadJobs,
    pub plans: larm_db::download_plans::SqlDownloadPlans,
    pub progress: SqliteTransferProgress,
    pub sources: std::sync::Arc<dyn DownloadSourceResolver>,
    pub storage: std::sync::Arc<dyn DownloadStorageRootResolver>,
    pub bandwidth_limiter: std::sync::Arc<crate::policy::BandwidthLimiter>,
    #[cfg(test)]
    storage_full_after_bytes: Option<u64>,
}

fn invalid(reason: &str, code: ErrorCode, message: &str) -> LarmError {
    LarmError::new(code, message, reason)
}

fn metadata_string<'a>(job: &'a DownloadJob, key: &str) -> Result<&'a str, LarmError> {
    job.metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            invalid(
                "download_job_snapshot_invalid",
                ErrorCode::InvalidStateTransition,
                "El trabajo de descarga no conserva su plan aprobado",
            )
        })
}

fn approved_record(
    job: &DownloadJob,
    record: DownloadPlanRecord,
) -> Result<DownloadPlanRecord, LarmError> {
    let plan_id = metadata_string(job, "plan_id")?;
    let source_id = metadata_string(job, "source_id")?;
    let hard_budget: Option<DecimalU64> = serde_json::from_value(
        job.metadata
            .get("hard_budget_bytes")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|_| {
        invalid(
            "download_job_snapshot_invalid",
            ErrorCode::InvalidStateTransition,
            "La cota aprobada del trabajo no es válida",
        )
    })?;
    let artifacts: Vec<DownloadArtifactEstimate> =
        serde_json::from_value(job.metadata.get("artifacts").cloned().ok_or_else(|| {
            invalid(
                "download_job_snapshot_invalid",
                ErrorCode::InvalidStateTransition,
                "Falta la selección aprobada de artefactos",
            )
        })?)
        .map_err(|_| {
            invalid(
                "download_job_snapshot_invalid",
                ErrorCode::InvalidStateTransition,
                "La selección aprobada de artefactos no es válida",
            )
        })?;
    let acquisition: Option<larm_domain::acquisition::AcquisitionPlanBinding> =
        serde_json::from_value(
            job.metadata
                .get("acquisition")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|_| {
            invalid(
                "download_job_snapshot_invalid",
                ErrorCode::StaleRevision,
                "El vínculo de adquisición del trabajo no es válido",
            )
        })?;
    let mut request_optional_ids = record.request.optional_artifact_ids.clone();
    request_optional_ids.sort();
    let mut response_optional_ids = record.response.optional_artifact_ids.clone();
    response_optional_ids.sort();
    if record.response.plan_id.as_deref() != Some(plan_id)
        || record.response.plan_hash.as_deref() != Some(job.plan_hash.as_str())
        || record.response.preview_hash != job.plan_hash
        || record.response.variant_id != job.variant_id
        || record.response.selected_storage_location_id.as_deref()
            != Some(job.storage_location_id.as_str())
        || record.response.source_id.as_deref() != Some(source_id)
        || record.response.priority != job.priority
        || record.response.hard_budget_bytes != hard_budget
        || record.response.artifact_estimates != artifacts
        || record.response.acquisition != acquisition
        || record.request.variant_id != job.variant_id
        || record.request.storage_location_id.as_deref() != Some(job.storage_location_id.as_str())
        || record.request.priority != job.priority
        || record.request.hard_budget_bytes != hard_budget
        || record
            .request
            .source_id
            .as_deref()
            .is_some_and(|id| id != source_id)
        || request_optional_ids != response_optional_ids
        || artifacts.is_empty()
        || !artifacts.iter().any(|item| item.required && item.selected)
        || artifacts
            .iter()
            .any(|item| item.selected && item.relative_path.is_empty())
    {
        return Err(invalid(
            "download_job_plan_snapshot_mismatch",
            ErrorCode::StaleRevision,
            "El trabajo ya no coincide con el plan confirmado",
        ));
    }
    for artifact in &artifacts {
        artifact.size_estimate.validate()?;
    }
    if !record.response.source_candidates.is_empty() {
        let ordered = crate::planner::order_source_candidates(
            record.response.source_candidates.clone(),
            &record.response.artifact_estimates,
            None,
        )
        .map_err(|_| {
            invalid(
                "download_job_plan_snapshot_mismatch",
                ErrorCode::StaleRevision,
                "Las fuentes alternativas ya no coinciden con el plan confirmado",
            )
        })?;
        if ordered != record.response.source_candidates
            || ordered.iter().any(|candidate| {
                Some(candidate.source_id.as_str()) == record.response.source_id.as_deref()
            })
        {
            return Err(invalid(
                "download_job_plan_snapshot_mismatch",
                ErrorCode::StaleRevision,
                "Las fuentes alternativas ya no coinciden con el plan confirmado",
            ));
        }
    }
    Ok(record)
}

fn execution_candidates(
    record: &DownloadPlanRecord,
) -> Result<Vec<DownloadSourceCandidate>, LarmError> {
    // The selected variant's own source remains the primary. The sealed list
    // contains only exact mirrors, so old and unknown-size plans can still run
    // from their pinned source without inventing alternate sources.
    let source_id = record
        .response
        .source_id
        .clone()
        .filter(|id| larm_domain::bridge::is_uuid_v7(id))
        .ok_or_else(|| {
            invalid(
                "download_source_missing",
                ErrorCode::StaleRevision,
                "El plan no conserva una fuente de descarga válida",
            )
        })?;
    let mut candidates = vec![DownloadSourceCandidate {
        source_id,
        variant_id: record.response.variant_id.clone(),
        revision_id: record.revision_id.clone(),
        revision_key: record.revision_key.clone(),
        manifest_hash: record.manifest_hash.clone(),
        source_priority: 0,
        confidence: ArtifactSizeConfidence::Unknown,
        authenticated: false,
        https: false,
        artifacts: vec![],
    }];
    candidates.extend(record.response.source_candidates.iter().cloned());
    Ok(candidates)
}

fn source_plan_request(
    job: &DownloadJob,
    record: &DownloadPlanRecord,
    candidate: &DownloadSourceCandidate,
    acquisition: Option<&ModelAcquisitionManifest>,
) -> Result<SourcePlanRequest, LarmError> {
    source_request_for(
        record,
        candidate,
        &job.storage_location_id,
        job.priority,
        acquisition,
    )
}

fn source_request_for(
    record: &DownloadPlanRecord,
    candidate: &DownloadSourceCandidate,
    storage_location_id: &str,
    priority: DownloadPriority,
    acquisition: Option<&ModelAcquisitionManifest>,
) -> Result<SourcePlanRequest, LarmError> {
    let optional_artifact_ids = if candidate.artifacts.is_empty() {
        record.response.optional_artifact_ids.clone()
    } else {
        record
            .response
            .optional_artifact_ids
            .iter()
            .map(|target_id| {
                candidate
                    .artifacts
                    .iter()
                    .find(|mapping| mapping.target_artifact_id == *target_id)
                    .map(|mapping| mapping.source_artifact_id.clone())
                    .ok_or_else(|| {
                        invalid(
                            "download_source_optional_mapping_missing",
                            ErrorCode::StaleRevision,
                            "El mirror no contiene el mapeo del artefacto opcional aprobado",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let acquisition = match acquisition {
        Some(manifest) => {
            manifest.validate()?;
            if candidate.artifacts.is_empty() {
                if record.response.acquisition.is_none() {
                    return Err(invalid(
                        "download_legacy_acquisition_changed",
                        ErrorCode::StaleRevision,
                        "Un plan físico no se convierte implícitamente en una adquisición remota",
                    ));
                }
                manifest.validate_download_record(record)?;
            }
            if manifest.variant_id != candidate.variant_id
                || manifest.revision_id != candidate.revision_id
                || manifest.source_id != candidate.source_id
                || manifest.source_reference.revision_key != candidate.revision_key
                || manifest.upstream_manifest_hash != candidate.manifest_hash
            {
                return Err(invalid(
                    "download_source_acquisition_mismatch",
                    ErrorCode::StaleRevision,
                    "El manifiesto de adquisición no coincide con la fuente aprobada",
                ));
            }
            Some(larm_adapter_sdk::source::SourceAcquisitionBinding {
                revision: manifest.source_reference.clone(),
                manifest_hash: manifest.upstream_manifest_hash.clone(),
                artifacts: manifest
                    .artifacts
                    .iter()
                    .map(
                        |artifact| larm_adapter_sdk::source::SourceAcquisitionArtifactBinding {
                            artifact_id: artifact.artifact_id.clone(),
                            relative_path: artifact.relative_path.clone(),
                            required: artifact.required,
                        },
                    )
                    .collect(),
            })
        }
        None if record.response.acquisition.is_some() && candidate.artifacts.is_empty() => {
            return Err(invalid(
                "download_acquisition_snapshot_missing",
                ErrorCode::StaleRevision,
                "Falta el manifiesto remoto fijado por el plan confirmado",
            ));
        }
        None => None,
    };
    let request = SourcePlanRequest {
        acquisition,
        variant_id: candidate.variant_id.clone(),
        storage_location_id: storage_location_id.to_owned(),
        hard_budget_bytes: record.response.hard_budget_bytes,
        source_id: Some(candidate.source_id.clone()),
        priority,
        optional_artifact_ids: Some(optional_artifact_ids),
    };
    if let Some(binding) = &request.acquisition {
        binding.validate_for(&request)?;
    }
    Ok(request)
}

fn validate_source_snapshot(
    job: &DownloadJob,
    record: &DownloadPlanRecord,
    candidate: &DownloadSourceCandidate,
    plan: &larm_domain::artifacts::Plan,
    acquisition: Option<&ModelAcquisitionManifest>,
) -> Result<(RevisionRef, String, String), LarmError> {
    let source_request = source_plan_request(job, record, candidate, acquisition)?;
    validate_source_snapshot_request(record, candidate, plan, &source_request)
}

/// Solicitud de la fuente principal antes de aceptar el job. Conserva el
/// destino, presupuesto, prioridad y opcionales del preflight confirmado.
pub fn primary_source_plan_request(
    record: &DownloadPlanRecord,
    acquisition: Option<&ModelAcquisitionManifest>,
) -> Result<(DownloadSourceCandidate, SourcePlanRequest), LarmError> {
    let candidate = primary_source_candidate(record)?;
    let destination = record
        .response
        .selected_storage_location_id
        .as_deref()
        .filter(|id| larm_domain::bridge::is_uuid_v7(id))
        .ok_or_else(|| {
            invalid(
                "download_destination_missing",
                ErrorCode::StaleRevision,
                "El plan no conserva un destino válido",
            )
        })?;
    let request = source_request_for(
        record,
        &candidate,
        destination,
        record.response.priority,
        acquisition,
    )?;
    Ok((candidate, request))
}

/// Admisión de metadata, incluida comparación sin destino. No fabrica un
/// destino ni inicia un source.download_plan para comprobar permisos.
pub fn primary_source_candidate(
    record: &DownloadPlanRecord,
) -> Result<DownloadSourceCandidate, LarmError> {
    Ok(execution_candidates(record)?.remove(0))
}

fn validate_source_snapshot_request(
    record: &DownloadPlanRecord,
    candidate: &DownloadSourceCandidate,
    plan: &larm_domain::artifacts::Plan,
    source_request: &SourcePlanRequest,
) -> Result<(RevisionRef, String, String), LarmError> {
    if plan.approval_required {
        return Err(invalid(
            "download_source_approval_required",
            ErrorCode::ApprovalRequired,
            "La fuente exige una aprobación que no está incluida en este plan",
        ));
    }
    validate_source_plan(source_request, plan)?;
    let spec = download_spec(plan)?;
    spec.check_execution_requirements(plan.storage_estimate.confidence)?;
    if spec.revision.source_id != candidate.source_id
        || spec.revision.revision_key != candidate.revision_key
        || spec.variant_id != candidate.variant_id
    {
        return Err(invalid(
            "download_source_revision_changed",
            ErrorCode::StaleRevision,
            "La revisión de la fuente cambió desde la preparación del plan",
        ));
    }

    let selected = record
        .response
        .artifact_estimates
        .iter()
        .filter(|artifact| artifact.selected)
        .collect::<Vec<_>>();
    if selected.len() != spec.artifacts.len() {
        return Err(invalid(
            "download_source_manifest_changed",
            ErrorCode::StaleRevision,
            "El manifiesto de la fuente cambió desde la preparación del plan",
        ));
    }
    for upstream in &spec.artifacts {
        let Some(snapshot) = selected
            .iter()
            .find(|item| item.relative_path == upstream.relative_path)
        else {
            return Err(invalid(
                "download_source_manifest_changed",
                ErrorCode::StaleRevision,
                "El manifiesto de la fuente cambió desde la preparación del plan",
            ));
        };
        let mapping = candidate
            .artifacts
            .iter()
            .find(|mapping| mapping.target_artifact_id == snapshot.artifact_id);
        if (!candidate.artifacts.is_empty() && mapping.is_none())
            || mapping.is_some_and(|mapping| {
                mapping.relative_path != upstream.relative_path
                    || Some(mapping.sha256.as_str()) != upstream.sha256.as_deref()
                    || Some(mapping.size_bytes.get()) != source_artifact_exact_bytes(upstream)
                    || snapshot.sha256.as_deref() != Some(mapping.sha256.as_str())
                    || snapshot.size_estimate.bytes.map(DecimalU64::get)
                        != Some(mapping.size_bytes.get())
            })
            || snapshot
                .sha256
                .as_deref()
                .zip(upstream.sha256.as_deref())
                .is_some_and(|(expected, actual)| expected != actual)
            || (snapshot.size_estimate.confidence == ArtifactSizeConfidence::Exact
                && source_artifact_exact_bytes(upstream)
                    != snapshot.size_estimate.bytes.map(DecimalU64::get))
        {
            return Err(invalid(
                "download_source_artifact_changed",
                ErrorCode::StaleRevision,
                "Los datos del manifiesto no coinciden con el plan confirmado",
            ));
        }
    }
    Ok((spec.revision, plan.plan_id.clone(), plan.plan_hash.clone()))
}

fn source_artifact_exact_bytes(artifact: &larm_adapter_sdk::source::SourceArtifact) -> Option<u64> {
    artifact
        .size_estimate
        .as_ref()
        .filter(|estimate| estimate.confidence == ArtifactSizeConfidence::Exact)
        .and_then(|estimate| estimate.bytes)
        .or(artifact.size_bytes)
        .map(DecimalU64::get)
}

fn transfer_limits(
    estimate: &larm_domain::model::ArtifactSizeEstimate,
    hard_budget: Option<u64>,
    already_transferred: u64,
) -> Result<TransferLimits, LarmError> {
    let remaining_budget = hard_budget.map(|budget| budget.saturating_sub(already_transferred));
    if remaining_budget == Some(0) {
        return Err(invalid(
            "download_hard_budget_exhausted",
            ErrorCode::StorageInsufficient,
            "La cota confirmada ya se ha consumido",
        ));
    }
    let expected_size = (estimate.confidence == ArtifactSizeConfidence::Exact)
        .then(|| estimate.bytes.map(DecimalU64::get))
        .flatten();
    let observed_bound = estimate.upper_bound_bytes.map(DecimalU64::get);
    let size_bound = expected_size
        .or(observed_bound)
        .or(remaining_budget)
        .ok_or_else(|| {
            invalid(
                "download_size_has_no_execution_bound",
                ErrorCode::UnknownSize,
                "Se necesita una cota o presupuesto para limitar la transferencia",
            )
        })?;
    let requested_limit =
        remaining_budget.map_or(size_bound, |remaining| size_bound.min(remaining));
    if expected_size.is_some_and(|exact| exact > requested_limit) {
        return Err(invalid(
            "download_hard_budget_below_exact_size",
            ErrorCode::StorageInsufficient,
            "La cota es menor que el tamaño exacto confirmado",
        ));
    }
    Ok(TransferLimits {
        expected_size,
        max_bytes: requested_limit,
    })
}

fn transfer_error(error: crate::transfer::TransferError) -> LarmError {
    match error {
        crate::transfer::TransferError::Source(source) => source,
        crate::transfer::TransferError::HashMismatch
        | crate::transfer::TransferError::ExactSizeMismatch
        | crate::transfer::TransferError::InvalidSourceBlock
        | crate::transfer::TransferError::SourceTotalChanged
        | crate::transfer::TransferError::SourceValidatorUnstable => invalid(
            "download_artifact_integrity_failed",
            ErrorCode::IntegrityFailed,
            "El archivo recibido no coincide con el plan o cambió durante la descarga",
        ),
        crate::transfer::TransferError::SizeLimitExceeded => invalid(
            "download_hard_budget_exceeded",
            ErrorCode::StorageInsufficient,
            "La transferencia superó la cota aprobada",
        ),
        crate::transfer::TransferError::Io(error)
            if error.kind() == std::io::ErrorKind::StorageFull =>
        {
            LarmError::new(
                ErrorCode::StorageInsufficient,
                "No hay espacio suficiente para guardar la descarga",
                "download_storage_full",
            )
            .with_technical(error.to_string())
        }
        other => LarmError::new(
            ErrorCode::DownloadFailed,
            "No se pudo completar la transferencia",
            "download_transfer_failed",
        )
        .with_technical(other.to_string()),
    }
}

fn mirror_fallback_reason(error: &LarmError) -> Option<&'static str> {
    if error.code == ErrorCode::Timeout {
        return Some("timeout");
    }
    match (&error.code, error.details.reason.as_str()) {
        (ErrorCode::ResourceUnavailable, "source_io_error") => Some("io_error"),
        (ErrorCode::UpstreamUnavailable, "source_http_408") => Some("http_408"),
        (ErrorCode::UpstreamUnavailable, "source_http_429") => Some("http_429"),
        (ErrorCode::UpstreamUnavailable, "source_http_5xx") => Some("http_5xx"),
        _ => None,
    }
}

fn candidate_start_index(
    checkpoint: &crate::transfer::PersistedTransferCheckpoint,
    candidates: &[DownloadSourceCandidate],
) -> Result<usize, LarmError> {
    let Some(source_id) = checkpoint.source_id.as_deref() else {
        return Ok(0);
    };
    candidates
        .iter()
        .position(|candidate| {
            candidate.source_id == source_id
                && checkpoint.revision_key.as_deref() == Some(candidate.revision_key.as_str())
        })
        .ok_or_else(|| {
            invalid(
                "download_checkpoint_source_not_in_plan",
                ErrorCode::StaleRevision,
                "La procedencia guardada ya no pertenece al plan confirmado",
            )
        })
}

fn checkpoint_source(
    checkpoint: &crate::transfer::PersistedTransferCheckpoint,
    candidates: &[DownloadSourceCandidate],
) -> Result<(String, String), LarmError> {
    if let Some(index) = checkpoint.source_id.as_deref() {
        let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.source_id == index
                && checkpoint.revision_key.as_deref() == Some(candidate.revision_key.as_str())
        }) else {
            return Err(invalid(
                "download_checkpoint_source_not_in_plan",
                ErrorCode::StaleRevision,
                "La procedencia guardada ya no pertenece al plan confirmado",
            ));
        };
        return Ok((candidate.source_id.clone(), candidate.revision_key.clone()));
    }
    let candidate = candidates.first().ok_or_else(|| {
        invalid(
            "download_source_missing",
            ErrorCode::StaleRevision,
            "El plan no conserva una fuente de descarga válida",
        )
    })?;
    Ok((candidate.source_id.clone(), candidate.revision_key.clone()))
}

struct PreparedArtifact {
    estimate: DownloadArtifactEstimate,
    verified: Option<VerifiedArtifact>,
    recovered: Option<larm_storage::cas::PromotedArtifact>,
    source_id: String,
    revision_key: String,
}

struct ArtifactTransfer<'a> {
    claim: &'a DownloadClaim,
    record: &'a DownloadPlanRecord,
    candidate: &'a DownloadSourceCandidate,
    estimate: &'a DownloadArtifactEstimate,
    already_transferred: u64,
    staging: &'a StagingStore,
    source: &'a dyn DownloadSourceSession,
}

impl DownloadConsumer {
    /// La limpieza tiene un worker propio: una transferencia larga no puede
    /// retener indefinidamente la cancelación de un job QUEUED/PAUSED.
    pub async fn run_pending_cancellation(&self) -> Result<DownloadConsumerOutcome, LarmError> {
        if let Some(job) = self.jobs.claim_pending_cancellation().await? {
            self.settle_cancellation(&job.id).await?;
            return Ok(DownloadConsumerOutcome::Cancelled { job_id: job.id });
        }
        Ok(DownloadConsumerOutcome::Idle)
    }

    /// Consume una entrada de la cola. Los fallos de un job se guardan en su
    /// estado durable; los errores de SQLite sí se devuelven al supervisor.
    pub async fn run_once(&self) -> Result<DownloadConsumerOutcome, LarmError> {
        self.run_once_with_limit(i64::MAX).await
    }

    /// Consume respetando el máximo compartido por fuente.
    pub async fn run_once_with_source_limit(
        &self,
        per_source_limit: u32,
    ) -> Result<DownloadConsumerOutcome, LarmError> {
        self.run_once_with_limit(i64::from(per_source_limit)).await
    }

    async fn run_once_with_limit(
        &self,
        per_source_limit: i64,
    ) -> Result<DownloadConsumerOutcome, LarmError> {
        match self.run_pending_cancellation().await? {
            DownloadConsumerOutcome::Idle => {}
            outcome => return Ok(outcome),
        }
        let Some(claim) = self
            .jobs
            .claim_next_with_source_limit(per_source_limit)
            .await?
        else {
            return Ok(DownloadConsumerOutcome::Idle);
        };
        let job_id = claim.job.id.clone();
        match self.execute_claim(&claim).await {
            Ok(DownloadConsumerOutcome::Completed { .. }) => {
                Ok(DownloadConsumerOutcome::Completed { job_id })
            }
            Ok(DownloadConsumerOutcome::Paused { .. }) => {
                // Cancel puede haber sustituido PAUSING mientras cerraba IO.
                if let Err(error) = self.jobs.finish_pause(&job_id).await {
                    if let Some(outcome) = self.settle_stop_race(&job_id).await? {
                        return Ok(outcome);
                    }
                    return Err(error);
                }
                Ok(DownloadConsumerOutcome::Paused { job_id })
            }
            Ok(DownloadConsumerOutcome::Cancelled { .. }) => {
                self.settle_cancellation(&job_id).await?;
                Ok(DownloadConsumerOutcome::Cancelled { job_id })
            }
            Ok(DownloadConsumerOutcome::Idle | DownloadConsumerOutcome::Failed { .. }) => {
                Err(invalid(
                    "download_consumer_internal_outcome",
                    ErrorCode::Internal,
                    "El consumidor produjo un estado interno no válido",
                ))
            }
            Err(error) => {
                if error.details.reason == "download_source_close_failed" {
                    // El adapter host no confirmó que proceso y canales estén
                    // drenados. Mantener RUNNING deja la recuperación durable
                    // al siguiente arranque, cuando el supervisor ya cerró el
                    // árbol de procesos.
                    return Err(error);
                }
                if let Some(outcome) = self.settle_stop_race(&job_id).await? {
                    return Ok(outcome);
                }
                if let Err(failure) = self.jobs.fail_download(&job_id, &error).await {
                    if let Some(outcome) = self.settle_stop_race(&job_id).await? {
                        return Ok(outcome);
                    }
                    return Err(failure);
                }
                Ok(DownloadConsumerOutcome::Failed {
                    job_id,
                    reason: error.details.reason,
                })
            }
        }
    }

    async fn settle_stop_race(
        &self,
        job_id: &str,
    ) -> Result<Option<DownloadConsumerOutcome>, LarmError> {
        let state = self.jobs.current_state(job_id).await?;
        match state {
            Some(larm_domain::download::DownloadState::Pausing) => {
                if let Err(error) = self.jobs.finish_pause(job_id).await {
                    if self.jobs.current_state(job_id).await?
                        == Some(larm_domain::download::DownloadState::Cancelling)
                    {
                        self.settle_cancellation(job_id).await?;
                        return Ok(Some(DownloadConsumerOutcome::Cancelled {
                            job_id: job_id.into(),
                        }));
                    }
                    return Err(error);
                }
                Ok(Some(DownloadConsumerOutcome::Paused {
                    job_id: job_id.to_owned(),
                }))
            }
            Some(larm_domain::download::DownloadState::Cancelling) => {
                self.settle_cancellation(job_id).await?;
                Ok(Some(DownloadConsumerOutcome::Cancelled {
                    job_id: job_id.to_owned(),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Ya no queda un source worker escribiendo. Si falla la limpieza, la
    /// reserva y CANCELLING sobreviven y otro ciclo puede reintentar.
    async fn settle_cancellation(&self, job_id: &str) -> Result<(), LarmError> {
        let result = async {
            let job = self.jobs.cancellation_job(job_id).await?;
            let root = self.storage.cleanup_root(&job.storage_location_id).await?;
            let artifacts: Vec<DownloadArtifactEstimate> = serde_json::from_value(
                job.metadata.get("artifacts").cloned().ok_or_else(|| {
                    invalid(
                        "download_job_snapshot_invalid",
                        ErrorCode::InvalidStateTransition,
                        "Falta la selección de archivos del trabajo",
                    )
                })?,
            )
            .map_err(|_| {
                invalid(
                    "download_job_snapshot_invalid",
                    ErrorCode::InvalidStateTransition,
                    "La selección de archivos no es válida",
                )
            })?;
            let staging = StagingStore::new(root.clone());
            for artifact in artifacts.iter().filter(|artifact| artifact.selected) {
                let partial = staging
                    .partial_path(job_id, &artifact.artifact_id)
                    .ok_or_else(|| {
                        invalid(
                            "download_cleanup_identity_invalid",
                            ErrorCode::ValidationFailed,
                            "Identidad de temporal no válida",
                        )
                    })?;
                let receipt = partial.with_extension("promote.json");
                match tokio::fs::symlink_metadata(&receipt).await {
                    Ok(_) => {
                        let root = root.clone();
                        let id = job.id.clone();
                        let artifact_id = artifact.artifact_id.clone();
                        let hash = job.plan_hash.clone();
                        tokio::task::spawn_blocking(move || {
                            finish_staged_promotion(&root, &id, &artifact_id, &hash)
                        })
                        .await
                        .map_err(|_| {
                            invalid(
                                "download_cleanup_task_failed",
                                ErrorCode::ResourceUnavailable,
                                "No se pudo cerrar la limpieza",
                            )
                        })?
                        .map_err(|_| {
                            invalid(
                                "download_cleanup_failed",
                                ErrorCode::ResourceUnavailable,
                                "No se pudo limpiar el recibo de promoción",
                            )
                        })?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(invalid(
                            "download_cleanup_failed",
                            ErrorCode::ResourceUnavailable,
                            "No se pudo comprobar el temporal",
                        ))
                    }
                }
                staging
                    .discard_partial(job_id, &artifact.artifact_id)
                    .await
                    .map_err(transfer_error)?;
            }
            self.jobs.finish_cancel(job_id).await
        }
        .await;
        if result.is_err() {
            self.jobs.retry_cancellation_cleanup(job_id).await?;
        }
        result
    }

    async fn execute_claim(
        &self,
        claim: &DownloadClaim,
    ) -> Result<DownloadConsumerOutcome, LarmError> {
        let job = &claim.job;
        let plan_id = metadata_string(job, "plan_id")?;
        let record = self.plans.get_plan(plan_id).await?.ok_or_else(|| {
            invalid(
                "download_plan_snapshot_missing",
                ErrorCode::StaleRevision,
                "El plan confirmado ya no está disponible",
            )
        })?;
        let record = approved_record(job, record)?;
        self.revalidate_acquisition(claim, &record).await?;
        let root = self.storage.root(&job.storage_location_id).await?;
        let prepared = self.transfer_selected(claim, &record, &root).await?;
        match prepared {
            PreparedResult::Paused => Ok(DownloadConsumerOutcome::Paused {
                job_id: job.id.clone(),
            }),
            PreparedResult::Cancelled => Ok(DownloadConsumerOutcome::Cancelled {
                job_id: job.id.clone(),
            }),
            PreparedResult::Ready { artifacts } => {
                self.revalidate_acquisition(claim, &record).await?;
                self.jobs.begin_verification(&job.id).await?;
                let mut downloaded = Vec::with_capacity(artifacts.len());
                for artifact in artifacts {
                    let promoted = if let Some(recovered) = artifact.recovered {
                        recovered
                    } else {
                        let staged = artifact.verified.ok_or_else(|| {
                            invalid(
                                "download_verified_artifact_missing",
                                ErrorCode::Internal,
                                "Falta un artefacto verificado antes del CAS",
                            )
                        })?;
                        let promoted = tokio::task::spawn_blocking({
                            let root = root.clone();
                            let staged_path = staged.path.clone();
                            let job_id = job.id.clone();
                            let artifact_id = artifact.estimate.artifact_id.clone();
                            let plan_hash = job.plan_hash.clone();
                            let sha256 = staged.sha256.clone();
                            let size_bytes = staged.size_bytes;
                            move || {
                                promote_staged(
                                    &root,
                                    &staged_path,
                                    &job_id,
                                    &artifact_id,
                                    &plan_hash,
                                    &sha256,
                                    size_bytes,
                                )
                            }
                        })
                        .await
                        .map_err(|error| {
                            LarmError::new(
                                ErrorCode::Internal,
                                "No se pudo finalizar la promoción al CAS",
                                "download_cas_task_failed",
                            )
                            .with_technical(error.to_string())
                        })?
                        .map_err(|error| {
                            LarmError::new(
                                ErrorCode::ResourceUnavailable,
                                "No se pudo ingresar el archivo verificado al CAS",
                                "download_cas_promotion_failed",
                            )
                            .with_technical(error.to_string())
                        })?;
                        // Preserve the locally verified trust classification.
                        downloaded.push(DownloadedArtifact {
                            artifact_id: artifact.estimate.artifact_id.clone(),
                            relative_path: promoted.relative_path,
                            size_bytes: promoted.size_bytes,
                            sha256: promoted.sha256,
                            trust: staged.trust,
                            source_id: artifact.source_id,
                            revision_key: artifact.revision_key,
                        });
                        continue;
                    };
                    let trust = if artifact.estimate.sha256.is_some() {
                        Trust::Verified
                    } else {
                        Trust::Declared
                    };
                    downloaded.push(DownloadedArtifact {
                        artifact_id: artifact.estimate.artifact_id.clone(),
                        relative_path: promoted.relative_path,
                        size_bytes: promoted.size_bytes,
                        sha256: promoted.sha256,
                        trust,
                        source_id: artifact.source_id,
                        revision_key: artifact.revision_key,
                    });
                }
                self.jobs
                    .complete_download_qualified(
                        &job.id,
                        &downloaded,
                        self.plans.qualification.as_ref(),
                    )
                    .await?;
                for artifact in &downloaded {
                    let _ = finish_staged_promotion(
                        &root,
                        &job.id,
                        &artifact.artifact_id,
                        &job.plan_hash,
                    );
                }
                Ok(DownloadConsumerOutcome::Completed {
                    job_id: job.id.clone(),
                })
            }
        }
    }

    async fn revalidate_acquisition(
        &self,
        claim: &DownloadClaim,
        record: &DownloadPlanRecord,
    ) -> Result<(), LarmError> {
        use larm_domain::ports::AcquisitionManifestCatalog;
        if let Some(binding) = &record.response.acquisition {
            self.jobs
                .check_acquisition_fence(&claim.job.id, record)
                .await?;
            let catalog = larm_db::acquisition::SqlAcquisitionManifests {
                pool: self.plans.pool.clone(),
                qualification: self.plans.qualification.clone(),
            };
            let context = larm_domain::ports::MutationContext {
                actor_id: claim.actor_id.clone(),
                correlation_id: claim.correlation_id.clone(),
            };
            catalog
                .bound_current(&record.response.variant_id, binding, &context)
                .await?
                .validate_download_record(record)?;
        }
        Ok(())
    }

    async fn transfer_selected(
        &self,
        claim: &DownloadClaim,
        record: &DownloadPlanRecord,
        root: &std::path::Path,
    ) -> Result<PreparedResult, LarmError> {
        let job = &claim.job;
        let candidates = execution_candidates(record)?;
        let staging = StagingStore::new(root.to_path_buf());
        let mut hard_budget_used = 0_u64;
        let hard_budget = record.response.hard_budget_bytes.map(DecimalU64::get);
        let mut prepared = Vec::new();

        for estimate in record
            .response
            .artifact_estimates
            .iter()
            .filter(|artifact| artifact.selected)
        {
            if let Some(recovered) =
                recover_staged_promotion(root, &job.id, &estimate.artifact_id, &job.plan_hash)
                    .map_err(|error| {
                        LarmError::new(
                            ErrorCode::IntegrityFailed,
                            "No se pudo recuperar la promoción del CAS",
                            "download_cas_recovery_failed",
                        )
                        .with_technical(error.to_string())
                    })?
            {
                verify_recovered(&recovered, estimate, hard_budget, hard_budget_used)?;
                let checkpoint = self
                    .progress
                    .load_checkpoint(&job.id, &estimate.artifact_id)
                    .await
                    .map_err(|error| {
                        LarmError::new(
                            ErrorCode::ResourceUnavailable,
                            "No se pudo recuperar la procedencia del artefacto",
                            "download_checkpoint_load_failed",
                        )
                        .with_technical(error.to_string())
                    })?;
                let (source_id, revision_key) = checkpoint_source(&checkpoint, &candidates)?;
                hard_budget_used = hard_budget_used
                    .checked_add(recovered.size_bytes)
                    .filter(|total| *total <= larm_domain::units::DECIMAL_U64_MAX)
                    .ok_or_else(|| {
                        invalid(
                            "download_total_size_overflow",
                            ErrorCode::StorageInsufficient,
                            "La suma de bytes descargados desborda",
                        )
                    })?;
                prepared.push(PreparedArtifact {
                    estimate: estimate.clone(),
                    verified: None,
                    recovered: Some(recovered),
                    source_id,
                    revision_key,
                });
                continue;
            }
            let saved_checkpoint = self
                .progress
                .load_checkpoint(&job.id, &estimate.artifact_id)
                .await
                .map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo recuperar el punto seguro de la descarga",
                        "download_checkpoint_load_failed",
                    )
                    .with_technical(error.to_string())
                })?;
            let first_index = candidate_start_index(&saved_checkpoint, &candidates)?;
            let mut completed = None;
            for index in first_index..candidates.len() {
                let candidate = &candidates[index];
                let attempt = match self.sources.open(claim, record, candidate).await {
                    Ok(session) => {
                        let result = self
                            .transfer_artifact(ArtifactTransfer {
                                claim,
                                record,
                                candidate,
                                estimate,
                                already_transferred: hard_budget_used,
                                staging: &staging,
                                source: session.as_ref(),
                            })
                            .await;
                        let close = session.close().await;
                        if let Err(error) = close {
                            return Err(LarmError::new(
                                ErrorCode::ResourceUnavailable,
                                "El worker de fuente no confirmó el cierre de su proceso",
                                "download_source_close_failed",
                            )
                            .with_technical(error.to_string()));
                        }
                        result
                    }
                    Err(error) => Err(error),
                };
                match attempt {
                    Ok(TransferOutcome::Paused { .. }) => return Ok(PreparedResult::Paused),
                    Ok(TransferOutcome::Cancelled) => return Ok(PreparedResult::Cancelled),
                    Ok(TransferOutcome::Completed(verified)) => {
                        hard_budget_used = hard_budget_used
                            .checked_add(verified.size_bytes)
                            .filter(|total| *total <= larm_domain::units::DECIMAL_U64_MAX)
                            .ok_or_else(|| {
                                invalid(
                                    "download_total_size_overflow",
                                    ErrorCode::StorageInsufficient,
                                    "La suma de bytes descargados desborda",
                                )
                            })?;
                        if hard_budget.is_some_and(|budget| hard_budget_used > budget) {
                            return Err(invalid(
                                "download_hard_budget_exceeded",
                                ErrorCode::StorageInsufficient,
                                "La transferencia superó la cota aprobada",
                            ));
                        }
                        completed = Some(PreparedArtifact {
                            estimate: estimate.clone(),
                            verified: Some(verified),
                            recovered: None,
                            source_id: candidate.source_id.clone(),
                            revision_key: candidate.revision_key.clone(),
                        });
                        break;
                    }
                    Err(error) => {
                        let Some(reason_code) = mirror_fallback_reason(&error) else {
                            return Err(error);
                        };
                        let Some(next) = candidates.get(index + 1) else {
                            return Err(error);
                        };
                        self.jobs
                            .record_mirror_failure(
                                &job.id,
                                &estimate.artifact_id,
                                &candidate.source_id,
                                &next.source_id,
                                &next.revision_key,
                                reason_code,
                            )
                            .await?;
                    }
                }
            }
            prepared.push(completed.ok_or_else(|| {
                invalid(
                    "download_mirror_candidates_exhausted",
                    ErrorCode::DownloadFailed,
                    "No se pudo completar el archivo desde las fuentes aprobadas",
                )
            })?);
        }
        Ok(PreparedResult::Ready {
            artifacts: prepared,
        })
    }

    async fn transfer_artifact(
        &self,
        transfer: ArtifactTransfer<'_>,
    ) -> Result<TransferOutcome, LarmError> {
        let ArtifactTransfer {
            claim,
            record,
            candidate,
            estimate,
            already_transferred,
            staging,
            source,
        } = transfer;
        let job = &claim.job;
        let request = source_plan_request(job, record, candidate, source.acquisition())?;
        let source_plan = source.download_plan(request).await?;
        let (revision, source_plan_id, source_plan_hash) =
            validate_source_snapshot(job, record, candidate, &source_plan, source.acquisition())?;
        if !download_spec(&source_plan)?
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == estimate.relative_path)
        {
            return Err(invalid(
                "download_source_artifact_missing",
                ErrorCode::StaleRevision,
                "La fuente ya no ofrece un artefacto seleccionado",
            ));
        }
        self.progress
            .set_source_identity(
                &job.id,
                &estimate.artifact_id,
                &candidate.source_id,
                &candidate.revision_key,
            )
            .await
            .map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo guardar la procedencia del artefacto",
                    "download_checkpoint_source_save_failed",
                )
                .with_technical(error.to_string())
            })?;
        let checkpoint = self
            .progress
            .load_checkpoint(&job.id, &estimate.artifact_id)
            .await
            .map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo recuperar el punto seguro de la descarga",
                    "download_checkpoint_load_failed",
                )
                .with_technical(error.to_string())
            })?;
        let hard_budget = record.response.hard_budget_bytes.map(DecimalU64::get);
        let limits = transfer_limits(&estimate.size_estimate, hard_budget, already_transferred)?;
        let source_request = SourceDownloadChunkRequest {
            source_id: candidate.source_id.clone(),
            job_id: job.id.clone(),
            plan_id: source_plan_id,
            plan_hash: source_plan_hash,
            approval_id: None,
            revision,
            relative_path: estimate.relative_path.clone(),
            offset: DecimalU64::new(checkpoint.durable_offset).ok_or_else(|| {
                invalid(
                    "download_checkpoint_offset_invalid",
                    ErrorCode::InvalidStateTransition,
                    "El punto de reanudación no es válido",
                )
            })?,
            max_bytes: DecimalU64::new(larm_adapter_sdk::source::SOURCE_DOWNLOAD_CHUNK_MAX_BYTES)
                .ok_or_else(|| {
                invalid(
                    "download_chunk_limit_invalid",
                    ErrorCode::Internal,
                    "El límite de bloque no es válido",
                )
            })?,
            validator: checkpoint.resume_validator,
        };
        let request = TransferRequest {
            job_id: &job.id,
            artifact_id: &estimate.artifact_id,
            resume_offset: checkpoint.durable_offset,
            limits,
            expected_sha256: estimate.sha256.as_deref(),
        };
        let limiter = self.bandwidth_limiter.clone();
        let fetch = |mut chunk_request: SourceDownloadChunkRequest| {
            let limiter = limiter.clone();
            async move {
                let permit = limiter.acquire_async(chunk_request.max_bytes.get()).await;
                chunk_request.max_bytes = DecimalU64::new(permit.bytes).ok_or_else(|| {
                    invalid(
                        "download_bandwidth_permit_invalid",
                        ErrorCode::Internal,
                        "El permiso de caudal no es válido",
                    )
                })?;
                source.download_chunk(chunk_request).await
            }
        };
        #[cfg(test)]
        let transfer = if let Some(storage_full_after_bytes) = self.storage_full_after_bytes {
            staging
                .write_source_chunks_with_storage_full_after(
                    request,
                    source_request,
                    fetch,
                    &self.progress,
                    storage_full_after_bytes,
                )
                .await
        } else {
            staging
                .write_source_chunks(request, source_request, fetch, &self.progress)
                .await
        };
        #[cfg(not(test))]
        let transfer = staging
            .write_source_chunks(request, source_request, fetch, &self.progress)
            .await;
        transfer.map_err(transfer_error)
    }
}

enum PreparedResult {
    Paused,
    Cancelled,
    Ready { artifacts: Vec<PreparedArtifact> },
}

fn verify_recovered(
    artifact: &larm_storage::cas::PromotedArtifact,
    estimate: &DownloadArtifactEstimate,
    hard_budget: Option<u64>,
    already_transferred: u64,
) -> Result<(), LarmError> {
    let limits = transfer_limits(&estimate.size_estimate, hard_budget, already_transferred)?;
    if estimate
        .sha256
        .as_deref()
        .is_some_and(|sha| sha != artifact.sha256)
        || (estimate.size_estimate.confidence == ArtifactSizeConfidence::Exact
            && estimate.size_estimate.bytes.map(DecimalU64::get) != Some(artifact.size_bytes))
        || artifact.size_bytes > limits.max_bytes
        || estimate
            .size_estimate
            .lower_bound_bytes
            .is_some_and(|lower| artifact.size_bytes < lower.get())
        || estimate
            .size_estimate
            .upper_bound_bytes
            .is_some_and(|upper| artifact.size_bytes > upper.get())
        || hard_budget.is_some_and(|budget| {
            already_transferred
                .checked_add(artifact.size_bytes)
                .is_none_or(|total| total > budget)
        })
    {
        return Err(invalid(
            "download_recovered_artifact_mismatch",
            ErrorCode::IntegrityFailed,
            "El archivo recuperado del CAS no coincide con la selección confirmada",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    mod h08 {
        include!("consumer_h08.rs");
    }

    use super::*;
    use async_trait::async_trait;
    use larm_adapter_sdk::source::{
        AuthResult, DownloadPriority, RevisionRef, SourceArtifact, SourceDownloadResumeValidator,
        SourceDownloadSpec,
    };
    use larm_domain::download::{
        DownloadDestinationOption, DownloadPlanRequest, DownloadPlanResponse,
    };
    use larm_domain::estimate::{Confidence, StorageEstimate};
    use larm_domain::model::{ArtifactSizeEstimate, ArtifactSizeSource};
    use serde_json::json;

    const VARIANT: &str = "0190f5c0-7a00-7000-8000-000000000031";
    const LOCATION: &str = "0190f5c0-7a00-7000-8000-000000000032";
    const PLAN: &str = "0190f5c0-7a00-7000-8000-000000000033";
    const SOURCE: &str = "0190f5c0-7a00-7000-8000-000000000034";
    const ARTIFACT: &str = "0190f5c0-7a00-7000-8000-000000000035";
    const MIRROR_SOURCE: &str = "0190f5c0-7a00-7000-8000-000000000046";
    const MIRROR_VARIANT: &str = "0190f5c0-7a00-7000-8000-000000000047";
    const MIRROR_REVISION: &str = "0190f5c0-7a00-7000-8000-000000000048";
    const MIRROR_ARTIFACT: &str = "0190f5c0-7a00-7000-8000-000000000049";

    fn exact_size(bytes: u64) -> ArtifactSizeEstimate {
        let bytes = DecimalU64::new(bytes).unwrap();
        ArtifactSizeEstimate {
            confidence: ArtifactSizeConfidence::Exact,
            bytes: Some(bytes),
            lower_bound_bytes: Some(bytes),
            upper_bound_bytes: Some(bytes),
            source: ArtifactSizeSource::Manifest,
            evidence_refs: vec!["fixture:manifest".into()],
            reason: None,
        }
    }

    #[test]
    fn fallback_solo_admite_fallos_transitorios_de_transporte() {
        let error = |code, reason| LarmError::new(code, "fixture", reason);
        assert_eq!(
            mirror_fallback_reason(&error(ErrorCode::Timeout, "request_timeout")),
            Some("timeout")
        );
        assert_eq!(
            mirror_fallback_reason(&error(ErrorCode::ResourceUnavailable, "source_io_error")),
            Some("io_error")
        );
        assert_eq!(
            mirror_fallback_reason(&error(ErrorCode::UpstreamUnavailable, "source_http_408")),
            Some("http_408")
        );
        assert_eq!(
            mirror_fallback_reason(&error(ErrorCode::UpstreamUnavailable, "source_http_429")),
            Some("http_429")
        );
        assert_eq!(
            mirror_fallback_reason(&error(ErrorCode::UpstreamUnavailable, "source_http_5xx")),
            Some("http_5xx")
        );
        assert!(mirror_fallback_reason(&error(ErrorCode::Forbidden, "source_http_5xx")).is_none());
        assert!(
            mirror_fallback_reason(&error(ErrorCode::ProtocolError, "source_io_error")).is_none()
        );
        assert!(
            mirror_fallback_reason(&error(ErrorCode::IntegrityFailed, "source_http_5xx")).is_none()
        );
    }

    #[test]
    fn disco_lleno_se_informa_como_espacio_insuficiente() {
        let error = transfer_error(crate::transfer::TransferError::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "injected disk full",
        )));

        assert_eq!(error.code, ErrorCode::StorageInsufficient);
        assert_eq!(error.details.reason, "download_storage_full");
        assert_eq!(
            error.message,
            "No hay espacio suficiente para guardar la descarga"
        );
        assert!(error.technical_detail().contains("injected disk full"));
        assert!(mirror_fallback_reason(&error).is_none());
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("injected disk full"));
    }

    fn unknown_size() -> ArtifactSizeEstimate {
        ArtifactSizeEstimate::unknown("La fuente no publica el tamaño", "fixture:unknown")
    }

    fn approved_artifact() -> DownloadArtifactEstimate {
        DownloadArtifactEstimate {
            artifact_id: ARTIFACT.into(),
            relative_path: "weights.gguf".into(),
            required: true,
            selected: true,
            size_estimate: exact_size(10),
            sha256: Some("b".repeat(64)),
        }
    }

    fn record_and_job() -> (DownloadPlanRecord, DownloadJob) {
        let artifact = approved_artifact();
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let record = DownloadPlanRecord {
            request: DownloadPlanRequest {
                variant_id: VARIANT.into(),
                storage_location_id: Some(LOCATION.into()),
                hard_budget_bytes: None,
                source_id: Some(SOURCE.into()),
                priority: DownloadPriority::Normal,
                optional_artifact_ids: vec![],
            },
            response: DownloadPlanResponse {
                acquisition: None,
                preview_id: "0190f5c0-7a00-7000-8000-000000000036".into(),
                preview_hash: "a".repeat(64),
                variant_id: VARIANT.into(),
                optional_artifact_ids: vec![],
                artifact_estimates: vec![artifact.clone()],
                destination_options: vec![DownloadDestinationOption {
                    storage_location_id: LOCATION.into(),
                    selected: true,
                    eligible: true,
                    reason: "fits".into(),
                    storage_estimate: None,
                }],
                selected_storage_location_id: Some(LOCATION.into()),
                plan_id: Some(PLAN.into()),
                plan_hash: Some("a".repeat(64)),
                storage_estimate: None,
                source_id: Some(SOURCE.into()),
                source_candidates: vec![],
                hard_budget_bytes: None,
                priority: DownloadPriority::Normal,
                expires_at: larm_domain::bridge::timestamp(
                    chrono::Utc::now() + chrono::Duration::minutes(3),
                ),
                operator_confirmation_required: true,
            },
            revision_id: "0190f5c0-7a00-7000-8000-000000000037".into(),
            revision_key: "commit:fixture".into(),
            manifest_hash: "c".repeat(64),
            storage_observed_at: now.clone(),
        };
        let job = DownloadJob {
            id: "0190f5c0-7a00-7000-8000-000000000038".into(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 2,
            metadata: json!({
                "plan_id": PLAN,
                "source_id": SOURCE,
                "hard_budget_bytes": null,
                "artifacts": [artifact],
                "actor_id": "0190f5c0-7a00-7000-8000-000000000039",
                "correlation_id": "0190f5c0-7a00-7000-8000-000000000040"
            })
            .as_object()
            .unwrap()
            .clone(),
            variant_id: VARIANT.into(),
            storage_location_id: LOCATION.into(),
            plan_hash: "a".repeat(64),
            state: larm_domain::download::DownloadState::Running,
            attempt: 1,
            progress: Default::default(),
            error: None,
            priority: DownloadPriority::Normal,
        };
        (record, job)
    }

    fn source_plan(revision_key: &str) -> larm_domain::artifacts::Plan {
        source_plan_with_hash(revision_key, &"b".repeat(64))
    }

    fn remote_record_and_job() -> (DownloadPlanRecord, DownloadJob, ModelAcquisitionManifest) {
        let (mut record, mut job) = record_and_job();
        let artifact = &record.response.artifact_estimates[0];
        let snapshot = ModelAcquisitionManifest {
            id: uuid::Uuid::now_v7().to_string(),
            variant_id: record.response.variant_id.clone(),
            revision_id: record.revision_id.clone(),
            source_id: SOURCE.into(),
            source_reference: larm_domain::acquisition::SourceCatalogReference {
                source_id: SOURCE.into(),
                upstream_namespace: "fixture".into(),
                upstream_model_id: "model".into(),
                revision_key: record.revision_key.clone(),
            },
            upstream_manifest_hash: record.manifest_hash.clone(),
            snapshot_hash: String::new(),
            observed_at: record.storage_observed_at.clone(),
            required_artifact_ids: vec![artifact.artifact_id.clone()],
            optional_artifact_ids: vec![],
            artifacts: vec![larm_domain::acquisition::ModelAcquisitionArtifact {
                artifact_id: artifact.artifact_id.clone(),
                relative_path: artifact.relative_path.clone(),
                size_bytes: artifact.size_estimate.bytes,
                sha256: artifact.sha256.clone(),
                required: true,
                role: "weights".into(),
                size_estimate: artifact.size_estimate.clone(),
            }],
            adapter_installation_id: uuid::Uuid::now_v7().to_string(),
            adapter_installation_revision: "1".into(),
            adapter_installation_sha256: "d".repeat(64),
            conformance_run_id: uuid::Uuid::now_v7().to_string(),
            conformance_run_sha256: "e".repeat(64),
        }
        .seal()
        .unwrap();
        record.response.acquisition = Some(snapshot.binding());
        job.metadata
            .insert("acquisition".into(), json!(snapshot.binding()));
        (record, job, snapshot)
    }

    #[test]
    fn remote_source_request_uses_public_reference_and_core_ids_and_rejects_legacy_or_private_replies(
    ) {
        let (record, job, snapshot) = remote_record_and_job();
        let primary = primary_source_candidate(&record).unwrap();
        assert_eq!(
            source_plan_request(&job, &record, &primary, None)
                .unwrap_err()
                .details
                .reason,
            "download_acquisition_snapshot_missing"
        );
        let request = source_plan_request(&job, &record, &primary, Some(&snapshot)).unwrap();
        let binding = request.acquisition.as_ref().unwrap();
        assert_eq!(request.variant_id, VARIANT);
        assert_eq!(binding.revision, snapshot.source_reference);
        assert_eq!(binding.artifacts[0].artifact_id, ARTIFACT);
        assert_eq!(binding.manifest_hash, snapshot.upstream_manifest_hash);
        assert_ne!(binding.manifest_hash, snapshot.snapshot_hash);
        let mut response = source_plan(&record.revision_key);
        assert!(
            validate_source_snapshot(&job, &record, &primary, &response, Some(&snapshot)).is_err()
        );
        let spec = download_spec(&response).unwrap();
        response.steps.clear();
        let response = spec.bind(&request, response, SOURCE).unwrap();
        validate_source_snapshot(&job, &record, &primary, &response, Some(&snapshot)).unwrap();
        let mut changed = response.clone();
        changed.steps[0]
            .request
            .get_mut(larm_adapter_sdk::source::SOURCE_ACQUISITION_SPEC)
            .unwrap()["manifest_hash"] = json!(snapshot.snapshot_hash);
        changed.plan_hash = larm_adapter_sdk::source::plan_hash(&changed).unwrap();
        assert!(
            validate_source_snapshot(&job, &record, &primary, &changed, Some(&snapshot)).is_err()
        );
        let mut private = response;
        private.steps[0]
            .request
            .get_mut(larm_adapter_sdk::source::SOURCE_DOWNLOAD_SPEC)
            .unwrap()["variant_id"] = json!(uuid::Uuid::now_v7().to_string());
        private.plan_hash = larm_adapter_sdk::source::plan_hash(&private).unwrap();
        assert!(
            validate_source_snapshot(&job, &record, &primary, &private, Some(&snapshot)).is_err()
        );
    }

    #[test]
    fn remote_job_cannot_drop_or_replace_the_acquisition_binding_on_recovery() {
        let (record, mut job, snapshot) = remote_record_and_job();
        approved_record(&job, record.clone()).unwrap();
        job.metadata.remove("acquisition");
        assert!(approved_record(&job, record.clone()).is_err());
        let mut binding = snapshot.binding();
        binding.manifest_id = uuid::Uuid::now_v7().to_string();
        job.metadata.insert("acquisition".into(), json!(binding));
        assert!(approved_record(&job, record).is_err());
    }

    fn source_plan_with_hash(revision_key: &str, sha256: &str) -> larm_domain::artifacts::Plan {
        source_plan_for(SOURCE, VARIANT, revision_key, sha256)
    }

    fn source_plan_for(
        source_id: &str,
        variant_id: &str,
        revision_key: &str,
        sha256: &str,
    ) -> larm_domain::artifacts::Plan {
        let request = SourcePlanRequest {
            acquisition: None,
            variant_id: variant_id.into(),
            storage_location_id: LOCATION.into(),
            hard_budget_bytes: None,
            source_id: Some(source_id.into()),
            priority: DownloadPriority::Normal,
            optional_artifact_ids: None,
        };
        let spec = SourceDownloadSpec {
            variant_id: variant_id.into(),
            storage_location_id: LOCATION.into(),
            revision: RevisionRef {
                source_id: source_id.into(),
                upstream_namespace: "fixture".into(),
                upstream_model_id: "model".into(),
                revision_key: revision_key.into(),
            },
            artifacts: vec![SourceArtifact {
                relative_path: "weights.gguf".into(),
                size_bytes: DecimalU64::new(10),
                size_estimate: Some(exact_size(10)),
                sha256: Some(sha256.to_owned()),
                required: true,
                role: "weights".into(),
            }],
            auth: AuthResult {
                authenticated: false,
                auth_required: false,
                license_required: false,
                reason: "fixture_public".into(),
            },
            resumable: true,
            priority: DownloadPriority::Normal,
            hard_budget_bytes: None,
        };
        spec.bind(
            &request,
            larm_domain::artifacts::Plan {
                plan_id: "0190f5c0-7a00-7000-8000-000000000041".into(),
                plan_hash: "0".repeat(64),
                operation: "source.download".into(),
                resource_ids: vec![variant_id.into(), LOCATION.into()],
                storage_estimate: StorageEstimate {
                    download_bytes: DecimalU64::new(10),
                    final_artifact_bytes: DecimalU64::new(10),
                    installed_bytes: None,
                    derived_cache_bytes: None,
                    temporary_peak_bytes: None,
                    existing_bytes: None,
                    peak_extra_bytes: None,
                    required_free_bytes: None,
                    upper_bound_bytes: DecimalU64::new(10),
                    confidence: Confidence::Exact,
                    reason: "fixture".into(),
                    evidence_refs: vec!["fixture:manifest".into()],
                    per_volume: vec![],
                },
                steps: vec![],
                expires_at: larm_domain::bridge::timestamp(
                    chrono::Utc::now() + chrono::Duration::minutes(5),
                ),
                approval_required: false,
            },
            source_id,
        )
        .unwrap()
    }

    #[test]
    fn limites_de_transferencia_distinguen_exactos_estimados_y_desconocidos() {
        let exact = transfer_limits(&exact_size(100), None, 0).unwrap();
        assert_eq!(exact.expected_size, Some(100));
        assert_eq!(exact.max_bytes, 100);
        assert!(transfer_limits(&exact_size(100), Some(99), 0).is_err());

        let mut estimated = exact_size(40);
        estimated.confidence = ArtifactSizeConfidence::Estimated;
        estimated.lower_bound_bytes = None;
        estimated.upper_bound_bytes = DecimalU64::new(60);
        estimated.source = ArtifactSizeSource::SourceMetadata;
        assert_eq!(
            transfer_limits(&estimated, Some(50), 20).unwrap().max_bytes,
            30
        );

        assert_eq!(
            transfer_limits(&unknown_size(), Some(100), 25)
                .unwrap()
                .max_bytes,
            75
        );
        assert_eq!(
            transfer_limits(&unknown_size(), None, 0)
                .unwrap_err()
                .details
                .reason,
            "download_size_has_no_execution_bound"
        );
    }

    #[test]
    fn plan_de_fuente_debe_respetar_revision_y_manifest_aprobados() {
        let (mut record, job) = record_and_job();
        let mut unselected_optional = approved_artifact();
        unselected_optional.artifact_id = "0190f5c0-7a00-7000-8000-000000000042".into();
        unselected_optional.relative_path = "README.md".into();
        unselected_optional.required = false;
        unselected_optional.selected = false;
        record.response.artifact_estimates.push(unselected_optional);
        let primary = execution_candidates(&record).unwrap().remove(0);
        let plan = source_plan(&record.revision_key);
        assert!(validate_source_snapshot(&job, &record, &primary, &plan, None).is_ok());

        let optional_id = "0190f5c0-7a00-7000-8000-000000000043".to_owned();
        let mut selected_record = record.clone();
        selected_record.request.optional_artifact_ids = vec![optional_id.clone()];
        selected_record.response.optional_artifact_ids = vec![optional_id.clone()];
        assert_eq!(
            source_plan_request(&job, &selected_record, &primary, None)
                .unwrap()
                .optional_artifact_ids,
            Some(vec![optional_id])
        );

        let changed = source_plan("commit:changed");
        assert_eq!(
            validate_source_snapshot(&job, &record, &primary, &changed, None)
                .unwrap_err()
                .details
                .reason,
            "download_source_revision_changed"
        );

        let mismatched_job = DownloadJob {
            plan_hash: "d".repeat(64),
            ..job
        };
        assert_eq!(
            approved_record(&mismatched_job, record)
                .unwrap_err()
                .details
                .reason,
            "download_job_plan_snapshot_mismatch"
        );
    }

    #[test]
    fn manifiesto_source_respeta_la_seleccion_p052_de_opcionales() {
        let (mut record, mut job) = record_and_job();
        let optional_id = "0190f5c0-7a00-7000-8000-000000000043".to_owned();
        let second_optional_id = "0190f5c0-7a00-7000-8000-000000000045".to_owned();
        record.request.optional_artifact_ids =
            vec![second_optional_id.clone(), optional_id.clone()];
        record.response.optional_artifact_ids =
            vec![optional_id.clone(), second_optional_id.clone()];
        let mut optional_estimate = approved_artifact();
        optional_estimate.artifact_id = optional_id.clone();
        optional_estimate.relative_path = "tokenizer.json".into();
        optional_estimate.required = false;
        optional_estimate.selected = true;
        optional_estimate.size_estimate = exact_size(5);
        optional_estimate.sha256 = Some("d".repeat(64));
        record.response.artifact_estimates.push(optional_estimate);
        let mut second_optional_estimate = approved_artifact();
        second_optional_estimate.artifact_id = second_optional_id.clone();
        second_optional_estimate.relative_path = "tokenizer_config.json".into();
        second_optional_estimate.required = false;
        second_optional_estimate.selected = true;
        second_optional_estimate.size_estimate = exact_size(4);
        second_optional_estimate.sha256 = Some("e".repeat(64));
        record
            .response
            .artifact_estimates
            .push(second_optional_estimate);
        let mut discarded_optional = approved_artifact();
        discarded_optional.artifact_id = "0190f5c0-7a00-7000-8000-000000000044".into();
        discarded_optional.relative_path = "README.md".into();
        discarded_optional.required = false;
        discarded_optional.selected = false;
        record.response.artifact_estimates.push(discarded_optional);
        let primary = execution_candidates(&record).unwrap().remove(0);
        job.metadata.insert(
            "artifacts".into(),
            serde_json::to_value(&record.response.artifact_estimates).unwrap(),
        );
        assert!(approved_record(&job, record.clone()).is_ok());

        let mut plan = source_plan(&record.revision_key);
        let mut spec = download_spec(&plan).unwrap();
        spec.artifacts.push(SourceArtifact {
            relative_path: "tokenizer.json".into(),
            size_bytes: DecimalU64::new(5),
            size_estimate: Some(exact_size(5)),
            sha256: Some("d".repeat(64)),
            required: false,
            role: "tokenizer".into(),
        });
        spec.artifacts.push(SourceArtifact {
            relative_path: "tokenizer_config.json".into(),
            size_bytes: DecimalU64::new(4),
            size_estimate: Some(exact_size(4)),
            sha256: Some("e".repeat(64)),
            required: false,
            role: "tokenizer_config".into(),
        });
        plan.steps.clear();
        plan.storage_estimate.download_bytes = DecimalU64::new(19);
        plan.storage_estimate.final_artifact_bytes = DecimalU64::new(19);
        plan.storage_estimate.upper_bound_bytes = DecimalU64::new(19);
        let request = source_plan_request(&job, &record, &primary, None).unwrap();
        assert_eq!(
            request.optional_artifact_ids,
            Some(vec![optional_id, second_optional_id])
        );
        let plan = spec.bind(&request, plan, "fixture.source").unwrap();
        assert!(validate_source_snapshot(&job, &record, &primary, &plan, None).is_ok());

        let required_only = source_plan(&record.revision_key);
        assert_eq!(
            validate_source_snapshot(&job, &record, &primary, &required_only, None)
                .unwrap_err()
                .details
                .reason,
            "download_source_manifest_changed"
        );
    }

    #[derive(Clone)]
    enum FixturePrimaryBehavior {
        PartialThenTimeout,
        BlockAfterPartial(std::sync::Arc<tokio::sync::Notify>),
        ResumeAfterPartial,
        FullAfterPartial,
    }

    struct FixtureSourceSession {
        plan: larm_domain::artifacts::Plan,
        bytes: Vec<u8>,
        source_id: String,
        primary_behavior: Option<FixturePrimaryBehavior>,
        closed_sessions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        requested_offsets: std::sync::Arc<std::sync::Mutex<Vec<(String, u64)>>>,
    }

    #[async_trait]
    impl DownloadSourceSession for FixtureSourceSession {
        async fn download_plan(
            &self,
            _request: SourcePlanRequest,
        ) -> Result<larm_domain::artifacts::Plan, LarmError> {
            Ok(self.plan.clone())
        }

        async fn download_chunk(
            &self,
            request: SourceDownloadChunkRequest,
        ) -> Result<SourceDownloadChunk, LarmError> {
            assert_eq!(request.relative_path, "weights.gguf");
            self.requested_offsets
                .lock()
                .unwrap()
                .push((self.source_id.clone(), request.offset.get()));
            if self.source_id == SOURCE {
                match (&self.primary_behavior, request.offset.get()) {
                    (
                        Some(
                            FixturePrimaryBehavior::PartialThenTimeout
                            | FixturePrimaryBehavior::BlockAfterPartial(_),
                        ),
                        0,
                    ) => {
                        return Ok(SourceDownloadChunk {
                            offset: DecimalU64::new(0).unwrap(),
                            data_base64: "MDEyMw==".into(),
                            eof: false,
                            total_bytes: Some(DecimalU64::new(self.bytes.len() as u64).unwrap()),
                            validator: Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            }),
                        });
                    }
                    (Some(FixturePrimaryBehavior::PartialThenTimeout), 4) => {
                        return Err(LarmError::new(
                            ErrorCode::Timeout,
                            "La fuente principal agotó el tiempo de espera tras un bloque parcial",
                            "request_timeout",
                        ));
                    }
                    (Some(FixturePrimaryBehavior::BlockAfterPartial(partial_seen)), 4) => {
                        partial_seen.notify_one();
                        return std::future::pending().await;
                    }
                    (Some(FixturePrimaryBehavior::ResumeAfterPartial), 4) => {
                        assert_eq!(
                            request.validator,
                            Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            })
                        );
                        return Ok(SourceDownloadChunk {
                            offset: DecimalU64::new(4).unwrap(),
                            data_base64: "NDU2Nzg5".into(),
                            eof: true,
                            total_bytes: Some(DecimalU64::new(self.bytes.len() as u64).unwrap()),
                            validator: Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            }),
                        });
                    }
                    (Some(FixturePrimaryBehavior::FullAfterPartial), 0) => {
                        return Ok(SourceDownloadChunk {
                            offset: DecimalU64::new(0).unwrap(),
                            data_base64: "MDEyMw==".into(),
                            eof: false,
                            total_bytes: Some(DecimalU64::new(self.bytes.len() as u64).unwrap()),
                            validator: Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            }),
                        });
                    }
                    (Some(FixturePrimaryBehavior::FullAfterPartial), 4) => {
                        assert_eq!(
                            request.validator,
                            Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            })
                        );
                        return Ok(SourceDownloadChunk {
                            offset: DecimalU64::new(4).unwrap(),
                            data_base64: "NDU2Nzg5".into(),
                            eof: true,
                            total_bytes: Some(DecimalU64::new(self.bytes.len() as u64).unwrap()),
                            validator: Some(SourceDownloadResumeValidator::StrongEtag {
                                value: "\"fixture-v1\"".into(),
                            }),
                        });
                    }
                    (Some(FixturePrimaryBehavior::ResumeAfterPartial), offset) => {
                        return Err(LarmError::new(
                            ErrorCode::ProtocolError,
                            format!("Recovery resumed at unexpected offset {offset}"),
                            "fixture_unexpected_primary_offset",
                        ));
                    }
                    (Some(_), offset) => {
                        return Err(LarmError::new(
                            ErrorCode::ProtocolError,
                            format!("Primary source received unexpected offset {offset}"),
                            "fixture_unexpected_primary_offset",
                        ));
                    }
                    (None, _) => {}
                }
            }
            Ok(SourceDownloadChunk {
                offset: DecimalU64::new(0).unwrap(),
                // Base64 canónico de `0123456789`.
                data_base64: "MDEyMzQ1Njc4OQ==".into(),
                eof: true,
                total_bytes: Some(DecimalU64::new(self.bytes.len() as u64).unwrap()),
                validator: None,
            })
        }

        async fn close(&self) -> Result<(), LarmError> {
            self.closed_sessions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    struct FixtureSourceResolver {
        primary_plan: larm_domain::artifacts::Plan,
        mirror_plan: larm_domain::artifacts::Plan,
        bytes: Vec<u8>,
        primary_behavior: Option<FixturePrimaryBehavior>,
        closed_sessions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        requested_offsets: std::sync::Arc<std::sync::Mutex<Vec<(String, u64)>>>,
    }

    #[async_trait]
    impl DownloadSourceResolver for FixtureSourceResolver {
        async fn open(
            &self,
            _claim: &DownloadClaim,
            _record: &DownloadPlanRecord,
            candidate: &DownloadSourceCandidate,
        ) -> Result<Box<dyn DownloadSourceSession>, LarmError> {
            Ok(Box::new(FixtureSourceSession {
                plan: if candidate.source_id == SOURCE {
                    self.primary_plan.clone()
                } else {
                    self.mirror_plan.clone()
                },
                bytes: self.bytes.clone(),
                source_id: candidate.source_id.clone(),
                primary_behavior: self.primary_behavior.clone(),
                closed_sessions: self.closed_sessions.clone(),
                requested_offsets: self.requested_offsets.clone(),
            }))
        }
    }

    struct FixtureStorageRoot(PathBuf);

    #[async_trait]
    impl DownloadStorageRootResolver for FixtureStorageRoot {
        async fn root(&self, _location_id: &str) -> Result<PathBuf, LarmError> {
            Ok(self.0.clone())
        }
    }

    struct SeededDownloadFixture {
        _db_dir: tempfile::TempDir,
        db_path: PathBuf,
        db: larm_db::pool::Db,
        _root_dir: tempfile::TempDir,
        root: PathBuf,
        bytes: Vec<u8>,
        sha256: String,
        record: DownloadPlanRecord,
        job: DownloadJob,
    }

    fn configured_download_records() -> (Vec<u8>, String, DownloadPlanRecord, DownloadJob) {
        use sha2::{Digest, Sha256};

        let bytes = b"0123456789".to_vec();
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let (mut record, mut job) = record_and_job();
        record.response.artifact_estimates[0].sha256 = Some(sha256.clone());
        record.response.source_candidates = vec![DownloadSourceCandidate {
            source_id: MIRROR_SOURCE.into(),
            variant_id: MIRROR_VARIANT.into(),
            revision_id: MIRROR_REVISION.into(),
            revision_key: "mirror-commit".into(),
            manifest_hash: record.manifest_hash.clone(),
            source_priority: 5,
            confidence: ArtifactSizeConfidence::Exact,
            authenticated: false,
            https: true,
            artifacts: vec![larm_domain::download::DownloadMirrorArtifact {
                target_artifact_id: ARTIFACT.into(),
                source_artifact_id: MIRROR_ARTIFACT.into(),
                relative_path: "weights.gguf".into(),
                sha256: sha256.clone(),
                size_bytes: DecimalU64::new(10).unwrap(),
            }],
        }];
        job.metadata.insert(
            "artifacts".into(),
            serde_json::to_value(&record.response.artifact_estimates).unwrap(),
        );
        (bytes, sha256, record, job)
    }

    async fn seeded_download_fixture() -> SeededDownloadFixture {
        let (_db_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let db_path = _db_dir.path().join("larm.sqlite");
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .to_path_buf();
        let test_temp = workspace.join("target/test-tmp");
        std::fs::create_dir_all(&test_temp).unwrap();
        let root_dir = tempfile::Builder::new()
            .prefix("download-consumer-root-")
            .tempdir_in(test_temp)
            .unwrap();
        let root = root_dir.path().to_path_buf();
        let (bytes, sha256, record, mut job) = configured_download_records();
        job.state = larm_domain::download::DownloadState::Queued;
        job.attempt = 0;
        job.row_version = 1;
        let pool = &db.pool;
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','fixture.source','1.0','source','ph','pub','VERIFIED','{}','VALIDATED')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,'t','t',1,'{}','Source','msa','https://example.invalid',0,1,'NONE','{}')")
            .bind(SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm',?,'fixture','model')")
            .bind(SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,'t','t',1,'{}','md','commit:fixture',?,'CURRENT')")
            .bind(&record.revision_id).bind(&record.manifest_hash).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).bind(&record.revision_id).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,'t','t',1,'{}','Mirror','msa','https://mirror.example.invalid',5,1,'NONE','{}')")
            .bind(MIRROR_SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md-mirror','t','t',1,'{}','cm',?,'fixture','model')")
            .bind(MIRROR_SOURCE).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,'t','t',1,'{}','md-mirror','mirror-commit',?,'CURRENT')")
            .bind(MIRROR_REVISION).bind(&record.manifest_hash).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(MIRROR_VARIANT).bind(MIRROR_REVISION).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,expected_size,actual_size,sha256,source_url,required,size_estimate) VALUES(?,'t','t',1,'{}',?,'weights.gguf','GGUF',NULL,10,NULL,?,NULL,1,?)")
            .bind(MIRROR_ARTIFACT).bind(MIRROR_VARIANT).bind(&sha256).bind(serde_json::to_string(&exact_size(10)).unwrap())
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','Destination',?,?, 'NTFS','LOCAL',1000000,1000000,0,0,0,'ONLINE','volume-test','SSD')")
            .bind(LOCATION).bind(root.to_string_lossy().as_ref()).bind(root.to_string_lossy().as_ref()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,expected_size,actual_size,sha256,source_url,required,size_estimate) VALUES(?,'t','t',1,'{}',?,'weights.gguf','GGUF',NULL,10,NULL,?,NULL,1,?)")
            .bind(ARTIFACT).bind(VARIANT).bind(&sha256).bind(serde_json::to_string(&exact_size(10)).unwrap())
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED',?,?, 't')")
            .bind(PLAN).bind(&job.plan_hash).bind(serde_json::json!({"record":&record,"execution":{"job_id":&job.id,"phase":"QUEUED"}}).to_string())
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,?,?,?,?,?,?,'QUEUED',0,'{}',NULL)")
            .bind(&job.id).bind(&job.created_at).bind(&job.updated_at).bind(job.row_version)
            .bind(serde_json::to_string(&job.metadata).unwrap()).bind("NORMAL").bind(VARIANT).bind(LOCATION).bind(&job.plan_hash)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES('fixture-reservation','volume-test',?,100,100,NULL)")
            .bind(&job.id).execute(pool).await.unwrap();

        SeededDownloadFixture {
            _db_dir,
            db_path,
            db,
            _root_dir: root_dir,
            root,
            bytes,
            sha256,
            record,
            job,
        }
    }

    struct ConsumerFixtureRefs<'a> {
        db: &'a larm_db::pool::Db,
        root: &'a std::path::Path,
        record: &'a DownloadPlanRecord,
        sha256: &'a str,
        bytes: &'a [u8],
    }

    impl<'a> From<&'a SeededDownloadFixture> for ConsumerFixtureRefs<'a> {
        fn from(fixture: &'a SeededDownloadFixture) -> Self {
            Self {
                db: &fixture.db,
                root: &fixture.root,
                record: &fixture.record,
                sha256: &fixture.sha256,
                bytes: &fixture.bytes,
            }
        }
    }

    fn consumer_for_fixture(
        fixture: ConsumerFixtureRefs<'_>,
        primary_behavior: Option<FixturePrimaryBehavior>,
        closed_sessions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        requested_offsets: std::sync::Arc<std::sync::Mutex<Vec<(String, u64)>>>,
    ) -> DownloadConsumer {
        DownloadConsumer {
            jobs: SqlDownloadJobs {
                pool: fixture.db.pool.clone(),
            },
            plans: larm_db::download_plans::SqlDownloadPlans {
                pool: fixture.db.pool.clone(),
                qualification: None,
            },
            progress: SqliteTransferProgress::new(
                larm_db::download_progress::SqlDownloadProgress {
                    pool: fixture.db.pool.clone(),
                },
            ),
            sources: std::sync::Arc::new(FixtureSourceResolver {
                primary_plan: source_plan_for(
                    SOURCE,
                    VARIANT,
                    &fixture.record.revision_key,
                    fixture.sha256,
                ),
                mirror_plan: source_plan_for(
                    MIRROR_SOURCE,
                    MIRROR_VARIANT,
                    "mirror-commit",
                    fixture.sha256,
                ),
                bytes: fixture.bytes.to_vec(),
                primary_behavior,
                closed_sessions,
                requested_offsets,
            }),
            storage: std::sync::Arc::new(FixtureStorageRoot(fixture.root.to_path_buf())),
            bandwidth_limiter: std::sync::Arc::new(crate::policy::BandwidthLimiter::new(None)),
            storage_full_after_bytes: None,
        }
    }

    #[tokio::test]
    async fn consumidor_reintenta_en_mirror_y_guarda_procedencia_tras_verificar() {
        let fixture = seeded_download_fixture().await;
        let root = fixture.root.clone();
        let bytes = fixture.bytes.clone();
        let sha256 = fixture.sha256.clone();
        let job = fixture.job.clone();
        let pool = &fixture.db.pool;
        let closed_sessions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requested_offsets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let consumer = consumer_for_fixture(
            (&fixture).into(),
            Some(FixturePrimaryBehavior::PartialThenTimeout),
            closed_sessions.clone(),
            requested_offsets.clone(),
        );
        let outcome = consumer.run_once().await.unwrap();
        let error = if !matches!(&outcome, DownloadConsumerOutcome::Completed { .. }) {
            let error: Option<String> =
                sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
                    .bind(&job.id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            error
        } else {
            None
        };
        assert!(
            matches!(&outcome, DownloadConsumerOutcome::Completed { .. }),
            "expected completed download, got {outcome:?}; error={error:?}"
        );
        assert_eq!(
            closed_sessions.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "both source worker sessions must be closed"
        );
        assert_eq!(
            *requested_offsets.lock().unwrap(),
            vec![
                (SOURCE.into(), 0),
                (SOURCE.into(), 4),
                (MIRROR_SOURCE.into(), 0),
            ],
            "the mirror must restart at offset zero after the partial primary transfer"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(state, "COMPLETED");
        let provenance: (String, String) = sqlx::query_as(
            "SELECT source_id,revision_key FROM artifact_provenance WHERE artifact_id=?",
        )
        .bind(ARTIFACT)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(provenance, (MIRROR_SOURCE.into(), "mirror-commit".into()));
        let mirror_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.mirror.failed'",
        )
        .bind(&job.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(mirror_events, 1);
        let location: String =
            sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
                .bind(ARTIFACT)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(
            location,
            format!("blobs/sha256/{}/{}", &sha256[..2], sha256)
        );
        assert!(root.join(&location).is_file());
        assert_eq!(tokio::fs::read(root.join(&location)).await.unwrap(), bytes);
        assert!(!root
            .join("staging")
            .join(&job.id)
            .join(format!("{ARTIFACT}.partial"))
            .exists());
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(&job.id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
    }

    #[tokio::test]
    async fn consumidor_persiste_storage_full_y_libera_reserva_sin_fallback(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = seeded_download_fixture().await;
        let job = fixture.job.clone();
        let closed_sessions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requested_offsets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut consumer = consumer_for_fixture(
            (&fixture).into(),
            Some(FixturePrimaryBehavior::FullAfterPartial),
            closed_sessions.clone(),
            requested_offsets.clone(),
        );
        consumer.storage_full_after_bytes = Some(5);

        let outcome = consumer.run_once().await?;
        assert_eq!(
            outcome,
            DownloadConsumerOutcome::Failed {
                job_id: job.id.clone(),
                reason: "download_storage_full".into(),
            }
        );
        assert_eq!(
            closed_sessions.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the source worker session must close before the job is finalized"
        );
        let observed_offsets = requested_offsets
            .lock()
            .map_err(|_| std::io::Error::other("requested offsets poisoned"))?
            .clone();
        assert_eq!(
            observed_offsets,
            vec![(SOURCE.into(), 0), (SOURCE.into(), 4)],
            "local storage exhaustion must not trigger mirror fallback"
        );

        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(&fixture.db.pool)
            .await?;
        assert_eq!(state, "FAILED");
        let error_text: String = sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(&fixture.db.pool)
            .await?;
        let error: serde_json::Value = serde_json::from_str(&error_text)?;
        assert_eq!(error["code"], "STORAGE_INSUFFICIENT");
        assert_eq!(error["details"]["reason"], "download_storage_full");
        assert_eq!(
            error["message"],
            "No hay espacio suficiente para guardar la descarga"
        );
        assert!(error.get("technical").is_none());

        let progress_text: String =
            sqlx::query_scalar("SELECT progress FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await?;
        let progress: serde_json::Value = serde_json::from_str(&progress_text)?;
        assert_eq!(progress["artifacts"][ARTIFACT]["durable_offset"], "4");
        let partial_path = fixture
            .root
            .join("staging")
            .join(&job.id)
            .join(format!("{ARTIFACT}.partial"));
        assert_eq!(
            tokio::fs::read(partial_path).await?,
            b"01234",
            "the durable checkpoint stays before the injected partial write"
        );
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await?;
        assert_eq!(reservations, 0);
        Ok(())
    }

    #[tokio::test]
    async fn consumidor_recupera_checkpoint_y_reanuda_misma_fuente_tras_reapertura_sqlite() {
        let mut fixture = seeded_download_fixture().await;
        let job = fixture.job.clone();
        let closed_sessions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requested_offsets =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, u64)>::new()));
        let partial_seen = std::sync::Arc::new(tokio::sync::Notify::new());
        let interrupted_consumer = consumer_for_fixture(
            (&fixture).into(),
            Some(FixturePrimaryBehavior::BlockAfterPartial(
                partial_seen.clone(),
            )),
            closed_sessions.clone(),
            requested_offsets.clone(),
        );
        let interrupted = tokio::spawn(async move { interrupted_consumer.run_once().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), partial_seen.notified())
            .await
            .unwrap();
        interrupted.abort();
        assert!(interrupted.await.unwrap_err().is_cancelled());

        let raw_progress: String =
            sqlx::query_scalar("SELECT progress FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        let progress: serde_json::Value = serde_json::from_str(&raw_progress).unwrap();
        let checkpoint = &progress["artifacts"][ARTIFACT];
        assert_eq!(checkpoint["durable_offset"], "4");
        assert_eq!(checkpoint["source_id"], SOURCE);
        assert_eq!(checkpoint["revision_key"], fixture.record.revision_key);
        let partial_path = fixture
            .root
            .join("staging")
            .join(&job.id)
            .join(format!("{ARTIFACT}.partial"));
        assert_eq!(tokio::fs::read(&partial_path).await.unwrap(), b"0123");
        let mut staged = std::fs::OpenOptions::new()
            .append(true)
            .open(&partial_path)
            .unwrap();
        std::io::Write::write_all(&mut staged, b"unacknowledged-tail").unwrap();
        staged.sync_all().unwrap();
        drop(staged);
        assert_eq!(
            tokio::fs::read(&partial_path).await.unwrap(),
            b"0123unacknowledged-tail"
        );

        fixture.db.pool.close().await;
        fixture.db = larm_db::pool::Db::open(&fixture.db_path).await.unwrap();
        larm_db::migrate::migrate(&fixture.db.pool).await.unwrap();
        let persisted_progress: String =
            sqlx::query_scalar("SELECT progress FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        let persisted_progress: serde_json::Value =
            serde_json::from_str(&persisted_progress).unwrap();
        assert_eq!(
            persisted_progress["artifacts"][ARTIFACT]["durable_offset"], "4",
            "the SQLite checkpoint must survive closing and reopening the database pool"
        );

        let jobs = SqlDownloadJobs {
            pool: fixture.db.pool.clone(),
        };
        assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
        let resumed_consumer = consumer_for_fixture(
            (&fixture).into(),
            Some(FixturePrimaryBehavior::ResumeAfterPartial),
            closed_sessions.clone(),
            requested_offsets.clone(),
        );
        let outcome = resumed_consumer.run_once().await.unwrap();
        let resumed_error: Option<String> =
            sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert!(
            matches!(outcome, DownloadConsumerOutcome::Completed { .. }),
            "expected completed download after reopening SQLite, got {outcome:?}; error={resumed_error:?}"
        );
        assert_eq!(
            *requested_offsets.lock().unwrap(),
            vec![(SOURCE.into(), 0), (SOURCE.into(), 4), (SOURCE.into(), 4)],
            "restart should continue from the synced checkpoint on the same source"
        );
        assert_eq!(closed_sessions.load(std::sync::atomic::Ordering::SeqCst), 1);
        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(&fixture.db.pool)
            .await
            .unwrap();
        assert_eq!(state, "COMPLETED");
        let provenance: (String, String) = sqlx::query_as(
            "SELECT source_id,revision_key FROM artifact_provenance WHERE artifact_id=?",
        )
        .bind(ARTIFACT)
        .fetch_one(&fixture.db.pool)
        .await
        .unwrap();
        assert_eq!(provenance, (SOURCE.into(), fixture.record.revision_key));
        let location: String =
            sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
                .bind(ARTIFACT)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(
            tokio::fs::read(fixture.root.join(location)).await.unwrap(),
            fixture.bytes
        );
    }

    #[tokio::test]
    async fn consumidor_recupera_descarga_despues_de_caida_del_proceso() {
        const CHILD_MODE: &str = "LARM_DOWNLOAD_CRASH_RECOVERY_CHILD";
        if std::env::var(CHILD_MODE).as_deref() == Ok("interrupt") {
            let db_path = std::path::PathBuf::from(
                std::env::var_os("LARM_DOWNLOAD_CRASH_RECOVERY_DB").unwrap(),
            );
            let root = std::path::PathBuf::from(
                std::env::var_os("LARM_DOWNLOAD_CRASH_RECOVERY_ROOT").unwrap(),
            );
            let db = larm_db::pool::Db::open(&db_path).await.unwrap();
            larm_db::migrate::migrate(&db.pool).await.unwrap();
            let (bytes, sha256, record, _) = configured_download_records();
            let partial_seen = std::sync::Arc::new(tokio::sync::Notify::new());
            let consumer = consumer_for_fixture(
                ConsumerFixtureRefs {
                    db: &db,
                    root: &root,
                    record: &record,
                    sha256: &sha256,
                    bytes: &bytes,
                },
                Some(FixturePrimaryBehavior::BlockAfterPartial(
                    partial_seen.clone(),
                )),
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            );
            let _running = tokio::spawn(async move { consumer.run_once().await });
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(10), partial_seen.notified())
                    .await
                    .is_ok(),
                "child consumer did not reach the durable checkpoint"
            );

            // Bypass destructors and graceful shutdown to model an OS process crash.
            std::process::exit(86);
        }

        assert!(
            std::env::var(CHILD_MODE).is_err(),
            "unexpected crash-recovery child mode"
        );
        let mut fixture = seeded_download_fixture().await;
        fixture.db.pool.close().await;

        let child_output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "consumer::tests::consumidor_recupera_descarga_despues_de_caida_del_proceso",
                "--nocapture",
            ])
            .env(CHILD_MODE, "interrupt")
            .env("LARM_DOWNLOAD_CRASH_RECOVERY_DB", &fixture.db_path)
            .env("LARM_DOWNLOAD_CRASH_RECOVERY_ROOT", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(
            child_output.status.code(),
            Some(86),
            "expected the child process to terminate at its checkpoint; stdout={} stderr={}",
            String::from_utf8_lossy(&child_output.stdout),
            String::from_utf8_lossy(&child_output.stderr),
        );

        fixture.db = larm_db::pool::Db::open(&fixture.db_path).await.unwrap();
        larm_db::migrate::migrate(&fixture.db.pool).await.unwrap();
        let job = fixture.job.clone();
        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(&fixture.db.pool)
            .await
            .unwrap();
        assert_eq!(state, "RUNNING", "the child must die mid-transfer");
        let progress_text: String =
            sqlx::query_scalar("SELECT progress FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        let progress: serde_json::Value = serde_json::from_str(&progress_text).unwrap();
        assert_eq!(
            progress["artifacts"][ARTIFACT]["durable_offset"], "4",
            "the child process must leave a durable four-byte checkpoint"
        );
        assert_eq!(
            progress["artifacts"][ARTIFACT]["source_id"], SOURCE,
            "recovery must retain the source identity"
        );
        let partial_path = fixture
            .root
            .join("staging")
            .join(&job.id)
            .join(format!("{ARTIFACT}.partial"));
        assert_eq!(tokio::fs::read(&partial_path).await.unwrap(), b"0123");
        let mut staged = std::fs::OpenOptions::new()
            .append(true)
            .open(&partial_path)
            .unwrap();
        std::io::Write::write_all(&mut staged, b"unacknowledged-tail").unwrap();
        staged.sync_all().unwrap();
        drop(staged);

        let jobs = SqlDownloadJobs {
            pool: fixture.db.pool.clone(),
        };
        assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
        let requested_offsets =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, u64)>::new()));
        let closed_sessions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resumed_consumer = consumer_for_fixture(
            (&fixture).into(),
            Some(FixturePrimaryBehavior::ResumeAfterPartial),
            closed_sessions.clone(),
            requested_offsets.clone(),
        );
        let outcome = resumed_consumer.run_once().await.unwrap();
        let resumed_error: Option<String> =
            sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
                .bind(&job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert!(
            matches!(outcome, DownloadConsumerOutcome::Completed { .. }),
            "expected completion after a process crash and restart, got {outcome:?}; error={resumed_error:?}"
        );
        assert_eq!(
            *requested_offsets.lock().unwrap(),
            vec![(SOURCE.into(), 4)],
            "restarted consumer must truncate the unacknowledged tail and resume at the checkpoint"
        );
        assert_eq!(closed_sessions.load(std::sync::atomic::Ordering::SeqCst), 1);
        let location: String =
            sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
                .bind(ARTIFACT)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(
            tokio::fs::read(fixture.root.join(location)).await.unwrap(),
            fixture.bytes
        );
        let state: String = sqlx::query_scalar("SELECT state FROM download_job WHERE id=?")
            .bind(&job.id)
            .fetch_one(&fixture.db.pool)
            .await
            .unwrap();
        assert_eq!(state, "COMPLETED");
    }

    #[tokio::test]
    async fn cancelacion_sin_worker_limpia_todos_los_parciales_y_preserva_otro_job() {
        use larm_domain::download::{DownloadCommand, DownloadControlIdempotency, DownloadStop};
        let fixture = seeded_download_fixture().await;
        let id = &fixture.job.id;
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consumer =
            consumer_for_fixture((&fixture).into(), None, closed.clone(), Default::default());
        let second_artifact = uuid::Uuid::now_v7().to_string();
        let other_job = uuid::Uuid::now_v7().to_string();
        let mut artifacts = fixture.record.response.artifact_estimates.clone();
        let mut extra = artifacts[0].clone();
        extra.artifact_id = second_artifact.clone();
        extra.relative_path = "extra.bin".into();
        artifacts.push(extra);
        sqlx::query(
            "UPDATE download_job SET metadata=json_set(metadata,'$.artifacts',json(?)) WHERE id=?",
        )
        .bind(serde_json::to_string(&artifacts).unwrap())
        .bind(id)
        .execute(&fixture.db.pool)
        .await
        .unwrap();
        let staging = StagingStore::new(fixture.root.clone());
        for (job, artifact, bytes) in [
            (id.as_str(), ARTIFACT, b"own".as_slice()),
            (id.as_str(), second_artifact.as_str(), b"second".as_slice()),
            (other_job.as_str(), ARTIFACT, b"preserve".as_slice()),
        ] {
            let path = staging.partial_path(job, artifact).unwrap();
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(path, bytes).await.unwrap();
        }
        let ctx = larm_domain::ports::MutationContext {
            actor_id: fixture.job.metadata["actor_id"].as_str().unwrap().into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        consumer
            .jobs
            .request_control(
                id,
                DownloadCommand::Cancel,
                &DownloadStop {
                    reason: "operator_request".into(),
                    approval_id: None,
                },
                &ctx,
                &DownloadControlIdempotency {
                    key: "cancel-queued".into(),
                    body_hash: "a".repeat(64),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            consumer.run_once().await.unwrap(),
            DownloadConsumerOutcome::Cancelled { job_id: id.clone() }
        );
        assert!(!staging.partial_path(id, ARTIFACT).unwrap().exists());
        assert!(!staging.partial_path(id, &second_artifact).unwrap().exists());
        assert_eq!(
            tokio::fs::read(staging.partial_path(&other_job, ARTIFACT).unwrap())
                .await
                .unwrap(),
            b"preserve"
        );
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 0);
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
        assert_eq!(
            consumer.run_once().await.unwrap(),
            DownloadConsumerOutcome::Idle
        );
    }

    #[tokio::test]
    async fn fallo_de_limpieza_conserva_cancelacion_y_reserva_hasta_reintento() {
        use larm_domain::download::{DownloadCommand, DownloadControlIdempotency, DownloadStop};
        let fixture = seeded_download_fixture().await;
        let id = &fixture.job.id;
        let consumer = consumer_for_fixture(
            (&fixture).into(),
            None,
            Default::default(),
            Default::default(),
        );
        let directory = fixture.root.join("staging").join(id);
        tokio::fs::create_dir_all(directory.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&directory, b"not-a-directory")
            .await
            .unwrap();
        let ctx = larm_domain::ports::MutationContext {
            actor_id: fixture.job.metadata["actor_id"].as_str().unwrap().into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        consumer
            .jobs
            .request_control(
                id,
                DownloadCommand::Cancel,
                &DownloadStop {
                    reason: "operator_request".into(),
                    approval_id: None,
                },
                &ctx,
                &DownloadControlIdempotency {
                    key: "cancel-queued".into(),
                    body_hash: "a".repeat(64),
                },
            )
            .await
            .unwrap();
        assert!(consumer.run_once().await.is_err());
        assert_eq!(
            consumer.jobs.current_state(id).await.unwrap(),
            Some(larm_domain::download::DownloadState::Cancelling)
        );
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 1);
        assert_eq!(
            consumer.run_once().await.unwrap(),
            DownloadConsumerOutcome::Idle,
            "el backoff de limpieza permite atender la cola normal"
        );
        tokio::fs::remove_file(&directory).await.unwrap();
        // Adelantar el vencimiento de la espera de cinco segundos en la fixture.
        sqlx::query(
            "UPDATE download_job SET progress=json_remove(progress,'$.cancel_retry_at') WHERE id=?",
        )
        .bind(id)
        .execute(&fixture.db.pool)
        .await
        .unwrap();
        assert_eq!(
            consumer.run_once().await.unwrap(),
            DownloadConsumerOutcome::Cancelled { job_id: id.clone() }
        );
    }

    struct CancelDuringPauseClose {
        inner: std::sync::Arc<dyn DownloadSourceResolver>,
        jobs: SqlDownloadJobs,
    }

    struct StopOnChunkSession {
        inner: Box<dyn DownloadSourceSession>,
        jobs: SqlDownloadJobs,
        job_id: String,
        ctx: larm_domain::ports::MutationContext,
    }

    impl StopOnChunkSession {
        async fn command(
            &self,
            command: larm_domain::download::DownloadCommand,
            key: &str,
        ) -> Result<(), LarmError> {
            self.jobs
                .request_control(
                    &self.job_id,
                    command,
                    &larm_domain::download::DownloadStop {
                        reason: "operator_request".into(),
                        approval_id: None,
                    },
                    &self.ctx,
                    &larm_domain::download::DownloadControlIdempotency {
                        key: key.into(),
                        body_hash: "a".repeat(64),
                    },
                )
                .await?;
            Ok(())
        }
    }

    #[async_trait]
    impl DownloadSourceResolver for CancelDuringPauseClose {
        async fn open(
            &self,
            claim: &DownloadClaim,
            record: &DownloadPlanRecord,
            candidate: &DownloadSourceCandidate,
        ) -> Result<Box<dyn DownloadSourceSession>, LarmError> {
            Ok(Box::new(StopOnChunkSession {
                inner: self.inner.open(claim, record, candidate).await?,
                jobs: self.jobs.clone(),
                job_id: claim.job.id.clone(),
                ctx: larm_domain::ports::MutationContext {
                    actor_id: claim.actor_id.clone(),
                    correlation_id: claim.correlation_id.clone(),
                },
            }))
        }
    }

    #[async_trait]
    impl DownloadSourceSession for StopOnChunkSession {
        async fn download_plan(
            &self,
            request: SourcePlanRequest,
        ) -> Result<larm_domain::artifacts::Plan, LarmError> {
            self.inner.download_plan(request).await
        }
        async fn download_chunk(
            &self,
            request: SourceDownloadChunkRequest,
        ) -> Result<SourceDownloadChunk, LarmError> {
            let chunk = self.inner.download_chunk(request).await?;
            self.command(
                larm_domain::download::DownloadCommand::Pause,
                "pause-on-chunk",
            )
            .await?;
            Ok(chunk)
        }
        async fn close(&self) -> Result<(), LarmError> {
            self.inner.close().await?;
            self.command(
                larm_domain::download::DownloadCommand::Cancel,
                "cancel-while-closing",
            )
            .await
        }
    }

    #[tokio::test]
    async fn cancelacion_durante_cierre_de_pausa_limpia_y_no_deja_job_activo() {
        let fixture = seeded_download_fixture().await;
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut consumer =
            consumer_for_fixture((&fixture).into(), None, closed.clone(), Default::default());
        consumer.sources = std::sync::Arc::new(CancelDuringPauseClose {
            inner: consumer.sources.clone(),
            jobs: consumer.jobs.clone(),
        });
        assert_eq!(
            consumer.run_once().await.unwrap(),
            DownloadConsumerOutcome::Cancelled {
                job_id: fixture.job.id.clone()
            }
        );
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            consumer.jobs.current_state(&fixture.job.id).await.unwrap(),
            Some(larm_domain::download::DownloadState::Cancelled)
        );
        assert!(!StagingStore::new(fixture.root.clone())
            .partial_path(&fixture.job.id, ARTIFACT)
            .unwrap()
            .exists());
        let actions: Vec<String> = sqlx::query_scalar("SELECT json_extract(payload,'$.action') FROM event WHERE resource_id=? AND event_type='download.state_changed' ORDER BY event_id")
            .bind(&fixture.job.id).fetch_all(&fixture.db.pool).await.unwrap();
        assert!(actions.contains(&"pause_requested".into()));
        assert!(actions.contains(&"cancel_requested".into()));
        assert!(actions.contains(&"cancelled".into()));
        assert!(!actions.contains(&"paused".into()));
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
                .bind(&fixture.job.id)
                .fetch_one(&fixture.db.pool)
                .await
                .unwrap();
        assert_eq!(reservations, 0);
    }
}
