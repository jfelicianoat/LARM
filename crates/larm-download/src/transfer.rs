//! Durable staging for one already-authorized artifact stream (P051).
//!
//! This module deliberately does not choose a source, authorize a license, or
//! fetch network data. The caller must do those things and validate HTTP range
//! semantics before passing the response body here. It owns only the partial
//! file and records a checkpoint after the bytes have reached stable storage.

use async_trait::async_trait;
use larm_adapter_sdk::source::{
    SourceDownloadChunk, SourceDownloadChunkRequest, SourceDownloadResumeValidator, SourceReply,
    SourceRequest,
};
use larm_domain::{
    bridge::is_uuid_v7,
    error::LarmError,
    model::Trust,
    units::{DecimalU64, DECIMAL_U64_MAX},
};
use larm_storage::cas::sha256_file;
use std::{future::Future, io, path::PathBuf};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

const BUFFER_SIZE: usize = 1024 * 1024;

/// Maximum accepted byte count and optional manifest-pinned exact size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferLimits {
    pub expected_size: Option<u64>,
    /// A known exact size, an evidenced upper bound, or an explicit hard
    /// budget. Unknown-size transfers must still provide a hard limit.
    pub max_bytes: u64,
}

impl TransferLimits {
    fn validate(self) -> Result<(), TransferError> {
        if self.expected_size.is_some_and(|size| size > self.max_bytes) {
            return Err(TransferError::InvalidLimits);
        }
        Ok(())
    }
}

/// Immutable inputs for one artifact transfer attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferRequest<'a> {
    pub job_id: &'a str,
    pub artifact_id: &'a str,
    /// Offset committed by the durable job repository.
    pub resume_offset: u64,
    pub limits: TransferLimits,
    pub expected_sha256: Option<&'a str>,
}

/// A checkpoint is called only after flushing and syncing the partial file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferControl {
    Continue,
    Pause,
    Cancel,
}

#[async_trait]
pub trait TransferProgressSink: Send + Sync {
    /// Persist this durable offset and return the current pause/cancel signal.
    async fn checkpoint(
        &self,
        job_id: &str,
        artifact_id: &str,
        durable_offset: u64,
        resume_validator: Option<&SourceDownloadResumeValidator>,
    ) -> io::Result<TransferControl>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedTransferCheckpoint {
    pub durable_offset: u64,
    pub resume_validator: Option<SourceDownloadResumeValidator>,
    pub source_id: Option<String>,
    pub revision_key: Option<String>,
}

/// Puente entre staging P051 y el checkpoint SQLite del job.
#[derive(Clone)]
pub struct SqliteTransferProgress {
    repository: larm_db::download_progress::SqlDownloadProgress,
}

impl SqliteTransferProgress {
    #[must_use]
    pub fn new(repository: larm_db::download_progress::SqlDownloadProgress) -> Self {
        Self { repository }
    }

    /// Carga un offset de staging y el validador que se guardó atómicamente.
    pub async fn load_checkpoint(
        &self,
        job_id: &str,
        artifact_id: &str,
    ) -> io::Result<PersistedTransferCheckpoint> {
        let checkpoint = self
            .repository
            .load_checkpoint(job_id, artifact_id)
            .await
            .map_err(progress_io_error)?;
        let resume_validator = checkpoint
            .resume_validator
            .map(serde_json::from_value::<SourceDownloadResumeValidator>)
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid resume validator"))?;
        if let Some(validator) = &resume_validator {
            validator.validate().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid resume validator")
            })?;
        }
        Ok(PersistedTransferCheckpoint {
            durable_offset: checkpoint.durable_offset.get(),
            resume_validator,
            source_id: checkpoint.source_id,
            revision_key: checkpoint.revision_key,
        })
    }

    pub async fn set_source_identity(
        &self,
        job_id: &str,
        artifact_id: &str,
        source_id: &str,
        revision_key: &str,
    ) -> io::Result<()> {
        self.repository
            .set_source_identity(job_id, artifact_id, source_id, revision_key)
            .await
            .map_err(progress_io_error)
    }
}

fn progress_io_error(error: larm_domain::error::LarmError) -> io::Error {
    io::Error::other(error.details.reason)
}

#[async_trait]
impl TransferProgressSink for SqliteTransferProgress {
    async fn checkpoint(
        &self,
        job_id: &str,
        artifact_id: &str,
        durable_offset: u64,
        resume_validator: Option<&SourceDownloadResumeValidator>,
    ) -> io::Result<TransferControl> {
        if let Some(validator) = resume_validator {
            validator.validate().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid resume validator")
            })?;
        }
        let resume_validator = resume_validator
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid resume validator"))?;
        self.repository
            .checkpoint(job_id, artifact_id, durable_offset, resume_validator)
            .await
            .map(|control| match control {
                larm_db::download_progress::DownloadControl::Continue => TransferControl::Continue,
                larm_db::download_progress::DownloadControl::Pause => TransferControl::Pause,
                larm_db::download_progress::DownloadControl::Cancel => TransferControl::Cancel,
            })
            .map_err(progress_io_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    Paused { durable_bytes: u64 },
    Cancelled,
    Completed(VerifiedArtifact),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    /// Upstream-pinned digests are VERIFIED; a locally calculated digest is
    /// DECLARED and must not be presented as upstream verification.
    pub trust: Trust,
}

#[derive(Debug)]
pub enum TransferError {
    InvalidIdentity,
    InvalidStagingPath,
    InvalidLimits,
    InvalidExpectedHash,
    ResumeFileShorterThanCheckpoint,
    SizeLimitExceeded,
    ExactSizeMismatch,
    HashMismatch,
    InvalidSourceBlock,
    Source(LarmError),
    SourceValidatorUnstable,
    SourceTotalChanged,
    Checkpoint(io::Error),
    Io(io::Error),
    HashTask(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIdentity => f.write_str("invalid download identity"),
            Self::InvalidStagingPath => {
                f.write_str("staging path contains a non-regular or reparse entry")
            }
            Self::InvalidLimits => f.write_str("invalid download byte limits"),
            Self::InvalidExpectedHash => f.write_str("invalid expected SHA-256"),
            Self::ResumeFileShorterThanCheckpoint => {
                f.write_str("partial file is shorter than its durable checkpoint")
            }
            Self::SizeLimitExceeded => f.write_str("download exceeded its byte limit"),
            Self::ExactSizeMismatch => f.write_str("download size differs from the pinned size"),
            Self::HashMismatch => f.write_str("download hash differs from the pinned SHA-256"),
            Self::InvalidSourceBlock => f.write_str("source adapter returned an invalid chunk"),
            Self::Source(_) => f.write_str("source adapter chunk request failed"),
            Self::SourceValidatorUnstable => {
                f.write_str("source artifact changed repeatedly while resuming")
            }
            Self::SourceTotalChanged => {
                f.write_str("source artifact total changed while transferring")
            }
            Self::Checkpoint(error) => write!(f, "could not persist download checkpoint: {error}"),
            Self::Io(error) => write!(f, "download staging I/O failed: {error}"),
            Self::HashTask(error) => write!(f, "download hash task failed: {error}"),
        }
    }
}

impl std::error::Error for TransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Checkpoint(error) | Self::Io(error) => Some(error),
            Self::Source(error) => Some(error),
            _ => None,
        }
    }
}

/// Owns the `staging/{job_id}/{artifact_id}.partial` files beneath a trusted
/// storage root. The daemon must resolve and validate that root from its
/// registered storage location before constructing this store.
#[derive(Debug, Clone)]
pub struct StagingStore {
    root: PathBuf,
}

impl StagingStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Retira sólo el parcial identificado; nunca sigue enlaces ni recorre
    /// carpetas de otros jobs. Se llama después de drenar al escritor.
    pub async fn discard_partial(
        &self,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<(), TransferError> {
        let path = self
            .partial_path(job_id, artifact_id)
            .ok_or(TransferError::InvalidIdentity)?;
        let mut directory = self.root.clone();
        for component in [None, Some("staging"), Some(job_id)] {
            if let Some(component) = component {
                directory.push(component);
            }
            match fs::symlink_metadata(&directory).await {
                Ok(metadata) if metadata.is_dir() && !is_reparse(&metadata) => {}
                Ok(_) => return Err(TransferError::InvalidStagingPath),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(TransferError::Io(e)),
            }
        }
        match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => {
                fs::remove_file(path).await.map_err(TransferError::Io)
            }
            Ok(_) => Err(TransferError::InvalidStagingPath),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(TransferError::Io(e)),
        }
    }

    #[must_use]
    pub fn partial_path(&self, job_id: &str, artifact_id: &str) -> Option<PathBuf> {
        if !is_uuid_v7(job_id) || !is_uuid_v7(artifact_id) {
            return None;
        }
        Some(
            self.root
                .join("staging")
                .join(job_id)
                .join(format!("{artifact_id}.partial")),
        )
    }

    /// Write one response body to its private partial file. `resume_offset`
    /// must come from the persisted checkpoint; any unsynced tail is removed
    /// before continuing. `input` must already represent a response accepted
    /// by `policy::decide_http_response`.
    pub async fn write<R>(
        &self,
        request: TransferRequest<'_>,
        mut input: R,
        progress: &dyn TransferProgressSink,
    ) -> Result<TransferOutcome, TransferError>
    where
        R: AsyncRead + Unpin + Send,
    {
        let TransferRequest {
            job_id,
            artifact_id,
            resume_offset,
            limits,
            expected_sha256,
        } = request;
        limits.validate()?;
        if expected_sha256.is_some_and(|hash| !is_sha256(hash)) {
            return Err(TransferError::InvalidExpectedHash);
        }
        if !is_uuid_v7(job_id) || !is_uuid_v7(artifact_id) {
            return Err(TransferError::InvalidIdentity);
        }
        let root_metadata = fs::symlink_metadata(&self.root)
            .await
            .map_err(TransferError::Io)?;
        if !root_metadata.is_dir() || is_reparse(&root_metadata) {
            return Err(TransferError::InvalidStagingPath);
        }
        let stage_root = ensure_directory(&self.root, "staging").await?;
        let job_directory = ensure_directory(&stage_root, job_id).await?;
        let path = job_directory.join(format!("{artifact_id}.partial"));
        let (existing_len, exists) = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => (metadata.len(), true),
            Ok(_) => return Err(TransferError::InvalidStagingPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, false),
            Err(error) => return Err(TransferError::Io(error)),
        };
        if existing_len < resume_offset {
            return Err(TransferError::ResumeFileShorterThanCheckpoint);
        }

        let mut options = OpenOptions::new();
        options.write(true);
        if !exists {
            options.create_new(true);
        } else if resume_offset == 0 {
            options.truncate(true);
        }
        let mut file = options.open(&path).await.map_err(TransferError::Io)?;
        // A crash can leave synced bytes that were not committed to SQLite.
        // Resume from the DB offset, never from that unacknowledged tail.
        if resume_offset == 0 || existing_len > resume_offset {
            file.set_len(resume_offset)
                .await
                .map_err(TransferError::Io)?;
        }
        file.seek(std::io::SeekFrom::End(0))
            .await
            .map_err(TransferError::Io)?;

        let mut durable_offset = resume_offset;
        let mut buffer = vec![0; BUFFER_SIZE];
        let mut eof = false;
        loop {
            let mut count = 0;
            while count < buffer.len() {
                let read = input
                    .read(&mut buffer[count..])
                    .await
                    .map_err(TransferError::Io)?;
                if read == 0 {
                    eof = true;
                    break;
                }
                count += read;
            }
            if count == 0 {
                break;
            }
            let next_offset = durable_offset
                .checked_add(count as u64)
                .ok_or(TransferError::SizeLimitExceeded)?;
            if next_offset > limits.max_bytes {
                return Err(TransferError::SizeLimitExceeded);
            }
            file.write_all(&buffer[..count])
                .await
                .map_err(TransferError::Io)?;
            file.flush().await.map_err(TransferError::Io)?;
            file.sync_data().await.map_err(TransferError::Io)?;
            durable_offset = next_offset;

            let control = progress
                .checkpoint(job_id, artifact_id, durable_offset, None)
                .await
                .map_err(TransferError::Checkpoint)?;
            match control {
                TransferControl::Continue => {}
                TransferControl::Pause => {
                    return Ok(TransferOutcome::Paused {
                        durable_bytes: durable_offset,
                    });
                }
                TransferControl::Cancel => {
                    drop(file);
                    fs::remove_file(&path).await.map_err(TransferError::Io)?;
                    return Ok(TransferOutcome::Cancelled);
                }
            }
            if eof {
                break;
            }
        }
        file.flush().await.map_err(TransferError::Io)?;
        file.sync_all().await.map_err(TransferError::Io)?;
        drop(file);

        if limits
            .expected_size
            .is_some_and(|expected| expected != durable_offset)
        {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::ExactSizeMismatch);
        }
        let hash_path = path.clone();
        let (sha256, size_bytes) = tokio::task::spawn_blocking(move || sha256_file(&hash_path))
            .await
            .map_err(|error| TransferError::HashTask(error.to_string()))?
            .map_err(TransferError::Io)?;
        if size_bytes > limits.max_bytes {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::SizeLimitExceeded);
        }
        if expected_sha256.is_some_and(|expected| expected != sha256) {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::HashMismatch);
        }
        let trust = if expected_sha256.is_some() {
            Trust::Verified
        } else {
            Trust::Declared
        };
        Ok(TransferOutcome::Completed(VerifiedArtifact {
            path,
            size_bytes,
            sha256,
            trust,
        }))
    }

    /// Fetch bounded blocks through an authorized source adapter and durably
    /// stage them. The callback owns RPC/permissions; this method owns only
    /// validation, checkpointing and local bytes.
    pub async fn write_source_chunks<F, Fut>(
        &self,
        request: TransferRequest<'_>,
        source_request: SourceDownloadChunkRequest,
        fetch: F,
        progress: &dyn TransferProgressSink,
    ) -> Result<TransferOutcome, TransferError>
    where
        F: FnMut(SourceDownloadChunkRequest) -> Fut,
        Fut: Future<Output = Result<SourceDownloadChunk, LarmError>>,
    {
        self.write_source_chunks_with_fault(request, source_request, fetch, progress, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn write_source_chunks_with_storage_full_after<F, Fut>(
        &self,
        request: TransferRequest<'_>,
        source_request: SourceDownloadChunkRequest,
        fetch: F,
        progress: &dyn TransferProgressSink,
        storage_full_after: u64,
    ) -> Result<TransferOutcome, TransferError>
    where
        F: FnMut(SourceDownloadChunkRequest) -> Fut,
        Fut: Future<Output = Result<SourceDownloadChunk, LarmError>>,
    {
        self.write_source_chunks_with_fault(
            request,
            source_request,
            fetch,
            progress,
            Some(storage_full_after),
        )
        .await
    }

    async fn write_source_chunks_with_fault<F, Fut>(
        &self,
        request: TransferRequest<'_>,
        mut source_request: SourceDownloadChunkRequest,
        mut fetch: F,
        progress: &dyn TransferProgressSink,
        mut storage_full_after: Option<u64>,
    ) -> Result<TransferOutcome, TransferError>
    where
        F: FnMut(SourceDownloadChunkRequest) -> Fut,
        Fut: Future<Output = Result<SourceDownloadChunk, LarmError>>,
    {
        let TransferRequest {
            job_id,
            artifact_id,
            resume_offset: original_offset,
            limits,
            expected_sha256,
        } = request;
        limits.validate()?;
        if expected_sha256.is_some_and(|hash| !is_sha256(hash)) {
            return Err(TransferError::InvalidExpectedHash);
        }
        if !is_uuid_v7(job_id) || !is_uuid_v7(artifact_id) {
            return Err(TransferError::InvalidIdentity);
        }
        source_request.validate().map_err(TransferError::Source)?;
        if source_request.job_id != job_id
            || source_request.offset.get() != original_offset
            || source_request.max_bytes.get()
                > larm_adapter_sdk::source::SOURCE_DOWNLOAD_CHUNK_MAX_BYTES
        {
            return Err(TransferError::InvalidIdentity);
        }

        // A durable partial without a validator cannot be resumed safely.
        // Start at zero before making any source request.
        let reset_unvalidated_resume = original_offset > 0 && source_request.validator.is_none();
        let mut durable_offset = if reset_unvalidated_resume {
            0
        } else {
            original_offset
        };
        let root_metadata = fs::symlink_metadata(&self.root)
            .await
            .map_err(TransferError::Io)?;
        if !root_metadata.is_dir() || is_reparse(&root_metadata) {
            return Err(TransferError::InvalidStagingPath);
        }
        let stage_root = ensure_directory(&self.root, "staging").await?;
        let job_directory = ensure_directory(&stage_root, job_id).await?;
        let path = job_directory.join(format!("{artifact_id}.partial"));
        let (existing_len, exists) = match fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => (metadata.len(), true),
            Ok(_) => return Err(TransferError::InvalidStagingPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, false),
            Err(error) => return Err(TransferError::Io(error)),
        };
        if existing_len < durable_offset {
            return Err(TransferError::ResumeFileShorterThanCheckpoint);
        }
        let mut options = OpenOptions::new();
        options.write(true);
        if !exists {
            options.create_new(true);
        } else if durable_offset == 0 {
            options.truncate(true);
        }
        let mut file = options.open(&path).await.map_err(TransferError::Io)?;
        if durable_offset == 0 || existing_len > durable_offset {
            file.set_len(durable_offset)
                .await
                .map_err(TransferError::Io)?;
        }
        file.seek(std::io::SeekFrom::End(0))
            .await
            .map_err(TransferError::Io)?;

        let mut current_validator = source_request.validator.clone();
        let mut observed_total = None;
        let mut validator_restarts = 0u8;
        let max_chunk_bytes = source_request.max_bytes.get();
        if reset_unvalidated_resume {
            match progress
                .checkpoint(job_id, artifact_id, 0, None)
                .await
                .map_err(TransferError::Checkpoint)?
            {
                TransferControl::Continue => {}
                TransferControl::Pause => return Ok(TransferOutcome::Paused { durable_bytes: 0 }),
                TransferControl::Cancel => {
                    drop(file);
                    fs::remove_file(&path).await.map_err(TransferError::Io)?;
                    return Ok(TransferOutcome::Cancelled);
                }
            }
        }

        loop {
            source_request.offset =
                DecimalU64::new(durable_offset).ok_or(TransferError::SizeLimitExceeded)?;
            source_request.validator = current_validator.clone();
            let remaining = limits.max_bytes.saturating_sub(durable_offset).max(1);
            let requested_bytes = max_chunk_bytes.min(remaining);
            source_request.max_bytes =
                DecimalU64::new(requested_bytes).ok_or(TransferError::InvalidLimits)?;
            let sent_request = source_request.clone();
            let chunk = match fetch(sent_request.clone()).await {
                Ok(chunk) => {
                    let expected_request = SourceRequest::DownloadChunk(sent_request);
                    SourceReply::DownloadChunk(chunk.clone())
                        .validate_for(&expected_request)
                        .map_err(|_| TransferError::InvalidSourceBlock)?;
                    Some(chunk)
                }
                // A source can refuse If-Range with a revision error instead
                // of returning bytes with a new validator. Keep the pinned
                // plan/revision/hash, discard old bytes and retry from zero.
                // The same error at zero is terminal; auth and other errors
                // never enter this restart path.
                Err(error)
                    if durable_offset > 0
                        && error.code == larm_domain::error::ErrorCode::StaleRevision =>
                {
                    None
                }
                Err(error) => return Err(TransferError::Source(error)),
            };

            let response_validator = chunk.as_ref().and_then(|chunk| chunk.validator.clone());
            if durable_offset == 0 {
                // At zero there are no old bytes to mix with a new revision.
                current_validator.clone_from(&response_validator);
            } else if chunk.is_none()
                || (current_validator != response_validator
                    && (current_validator.is_some() || response_validator.is_some()))
            {
                validator_restarts += 1;
                if validator_restarts > 5 {
                    return Err(TransferError::SourceValidatorUnstable);
                }
                file.set_len(0).await.map_err(TransferError::Io)?;
                file.seek(std::io::SeekFrom::Start(0))
                    .await
                    .map_err(TransferError::Io)?;
                file.sync_data().await.map_err(TransferError::Io)?;
                durable_offset = 0;
                observed_total = None;
                current_validator = response_validator;
                source_request.offset = DecimalU64::new(0).ok_or(TransferError::InvalidLimits)?;
                source_request.validator = current_validator.clone();
                match progress
                    .checkpoint(job_id, artifact_id, 0, current_validator.as_ref())
                    .await
                    .map_err(TransferError::Checkpoint)?
                {
                    TransferControl::Continue => continue,
                    TransferControl::Pause => {
                        return Ok(TransferOutcome::Paused { durable_bytes: 0 })
                    }
                    TransferControl::Cancel => {
                        drop(file);
                        fs::remove_file(&path).await.map_err(TransferError::Io)?;
                        return Ok(TransferOutcome::Cancelled);
                    }
                }
            }
            let chunk = chunk.ok_or(TransferError::InvalidSourceBlock)?;

            if let Some(total) = chunk.total_bytes.map(DecimalU64::get) {
                if total > limits.max_bytes {
                    drop(file);
                    let _ = fs::remove_file(&path).await;
                    return Err(TransferError::SizeLimitExceeded);
                }
                if limits
                    .expected_size
                    .is_some_and(|expected| expected != total)
                {
                    drop(file);
                    let _ = fs::remove_file(&path).await;
                    return Err(TransferError::ExactSizeMismatch);
                }
                if observed_total.is_some_and(|previous| previous != total) {
                    drop(file);
                    let _ = fs::remove_file(&path).await;
                    return Err(TransferError::SourceTotalChanged);
                }
                observed_total = Some(total);
            }

            let bytes = chunk
                .decoded_bytes()
                .map_err(|_| TransferError::InvalidSourceBlock)?;
            let next_offset = durable_offset
                .checked_add(bytes.len() as u64)
                .filter(|value| *value <= DECIMAL_U64_MAX)
                .ok_or(TransferError::SizeLimitExceeded)?;
            if next_offset > limits.max_bytes {
                drop(file);
                let _ = fs::remove_file(&path).await;
                return Err(TransferError::SizeLimitExceeded);
            }
            if !bytes.is_empty() {
                write_staging_chunk(&mut file, &bytes, &mut storage_full_after)
                    .await
                    .map_err(TransferError::Io)?;
                file.flush().await.map_err(TransferError::Io)?;
                file.sync_data().await.map_err(TransferError::Io)?;
                durable_offset = next_offset;
            }
            match progress
                .checkpoint(
                    job_id,
                    artifact_id,
                    durable_offset,
                    current_validator.as_ref(),
                )
                .await
                .map_err(TransferError::Checkpoint)?
            {
                TransferControl::Continue => {}
                TransferControl::Pause => {
                    return Ok(TransferOutcome::Paused {
                        durable_bytes: durable_offset,
                    })
                }
                TransferControl::Cancel => {
                    drop(file);
                    fs::remove_file(&path).await.map_err(TransferError::Io)?;
                    return Ok(TransferOutcome::Cancelled);
                }
            }
            if chunk.eof {
                break;
            }
        }

        file.flush().await.map_err(TransferError::Io)?;
        file.sync_all().await.map_err(TransferError::Io)?;
        drop(file);
        if limits
            .expected_size
            .is_some_and(|expected| expected != durable_offset)
            || observed_total.is_some_and(|total| total != durable_offset)
        {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::ExactSizeMismatch);
        }
        let hash_path = path.clone();
        let (sha256, size_bytes) = tokio::task::spawn_blocking(move || sha256_file(&hash_path))
            .await
            .map_err(|error| TransferError::HashTask(error.to_string()))?
            .map_err(TransferError::Io)?;
        if size_bytes > limits.max_bytes {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::SizeLimitExceeded);
        }
        if expected_sha256.is_some_and(|expected| expected != sha256) {
            let _ = fs::remove_file(&path).await;
            return Err(TransferError::HashMismatch);
        }
        let trust = if expected_sha256.is_some() {
            Trust::Verified
        } else {
            Trust::Declared
        };
        Ok(TransferOutcome::Completed(VerifiedArtifact {
            path,
            size_bytes,
            sha256,
            trust,
        }))
    }
}

async fn write_staging_chunk(
    file: &mut tokio::fs::File,
    bytes: &[u8],
    storage_full_after: &mut Option<u64>,
) -> io::Result<()> {
    if let Some(remaining) = storage_full_after {
        if *remaining < bytes.len() as u64 {
            let writable = *remaining as usize;
            file.write_all(&bytes[..writable]).await?;
            *storage_full_after = None;
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected staging volume full",
            ));
        }
        *remaining -= bytes.len() as u64;
    }
    file.write_all(bytes).await
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn ensure_directory(parent: &std::path::Path, name: &str) -> Result<PathBuf, TransferError> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path).await {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err(TransferError::InvalidStagingPath);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(TransferError::Io(error)),
            }
            let metadata = fs::symlink_metadata(&path)
                .await
                .map_err(TransferError::Io)?;
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err(TransferError::InvalidStagingPath);
            }
        }
        Err(error) => return Err(TransferError::Io(error)),
    }
    Ok(path)
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use sha2::Digest;
    use std::sync::Mutex;
    use tempfile::tempdir;

    struct Checkpoints {
        control: Mutex<TransferControl>,
        offsets: Mutex<Vec<u64>>,
        validators: Mutex<Vec<Option<SourceDownloadResumeValidator>>>,
    }

    impl Checkpoints {
        fn continuing() -> Self {
            Self {
                control: Mutex::new(TransferControl::Continue),
                offsets: Mutex::new(Vec::new()),
                validators: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TransferProgressSink for Checkpoints {
        async fn checkpoint(
            &self,
            _job_id: &str,
            _artifact_id: &str,
            durable_offset: u64,
            resume_validator: Option<&SourceDownloadResumeValidator>,
        ) -> io::Result<TransferControl> {
            self.offsets
                .lock()
                .map_err(|_| io::Error::other("checkpoint list poisoned"))?
                .push(durable_offset);
            self.validators
                .lock()
                .map_err(|_| io::Error::other("checkpoint validator list poisoned"))?
                .push(resume_validator.cloned());
            self.control
                .lock()
                .map(|value| *value)
                .map_err(|_| io::Error::other("transfer control poisoned"))
        }
    }

    struct FailingCheckpoint;

    #[async_trait]
    impl TransferProgressSink for FailingCheckpoint {
        async fn checkpoint(
            &self,
            _job_id: &str,
            _artifact_id: &str,
            _durable_offset: u64,
            _resume_validator: Option<&SourceDownloadResumeValidator>,
        ) -> io::Result<TransferControl> {
            Err(io::Error::other("injected checkpoint failure"))
        }
    }

    const JOB: &str = "0190f5c0-7a00-7000-8000-000000000001";
    const ARTIFACT: &str = "0190f5c0-7a00-7000-8000-000000000002";

    fn source_chunk_request(
        offset: u64,
        validator: Option<SourceDownloadResumeValidator>,
    ) -> SourceDownloadChunkRequest {
        SourceDownloadChunkRequest {
            source_id: "0190f5c0-7a00-7000-8000-000000000003".into(),
            job_id: JOB.into(),
            plan_id: "0190f5c0-7a00-7000-8000-000000000004".into(),
            plan_hash: "a".repeat(64),
            approval_id: None,
            revision: larm_adapter_sdk::source::RevisionRef {
                source_id: "0190f5c0-7a00-7000-8000-000000000003".into(),
                upstream_namespace: "fixture".into(),
                upstream_model_id: "model".into(),
                revision_key: "commit:fixture".into(),
            },
            relative_path: "weights.gguf".into(),
            offset: DecimalU64::new(offset).unwrap(),
            max_bytes: DecimalU64::new(4).unwrap(),
            validator,
        }
    }

    fn source_chunk(
        request: &SourceDownloadChunkRequest,
        data_base64: &str,
        eof: bool,
        validator: &str,
    ) -> SourceDownloadChunk {
        SourceDownloadChunk {
            offset: request.offset,
            data_base64: data_base64.into(),
            eof,
            total_bytes: Some(DecimalU64::new(10).unwrap()),
            validator: Some(SourceDownloadResumeValidator::StrongEtag {
                value: validator.into(),
            }),
        }
    }

    #[tokio::test]
    async fn writes_syncs_and_verifies_a_pinned_artifact() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        let bytes = b"larm artifact";
        let expected_hash = hex::encode(sha2::Sha256::digest(bytes));

        let outcome = store
            .write(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                &bytes[..],
                &progress,
            )
            .await
            .expect("transfer");

        let TransferOutcome::Completed(artifact) = outcome else {
            panic!("expected completed transfer");
        };
        assert_eq!(artifact.size_bytes, bytes.len() as u64);
        assert_eq!(artifact.sha256, expected_hash);
        assert_eq!(artifact.trust, Trust::Verified);
        assert_eq!(std::fs::read(artifact.path).expect("staged bytes"), bytes);
        assert_eq!(
            *progress.offsets.lock().expect("offsets"),
            vec![bytes.len() as u64]
        );
    }

    #[tokio::test]
    async fn eof_at_full_checkpoint_verifies_file_size_and_hash_before_completion() {
        let expected = b"abcdefghij";
        let sha = hex::encode(sha2::Sha256::digest(expected));
        for (contents, size, success) in [
            (expected.as_slice(), 10, true),
            (b"abcdefghik".as_slice(), 10, false),
            (expected.as_slice(), 9, false),
        ] {
            let directory = tempdir().unwrap();
            let store = StagingStore::new(directory.path().to_path_buf());
            let path = store.partial_path(JOB, ARTIFACT).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            let progress = Checkpoints::continuing();
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = calls.clone();
            let validator = Some(SourceDownloadResumeValidator::StrongEtag {
                value: "\"v1\"".into(),
            });
            let fetch = move |request: SourceDownloadChunkRequest| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    assert_eq!(request.offset.get(), 10);
                    Ok(source_chunk(&request, "", true, "\"v1\""))
                }
            };
            let result = store
                .write_source_chunks(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 10,
                        limits: TransferLimits {
                            expected_size: Some(size),
                            max_bytes: 10,
                        },
                        expected_sha256: Some(&sha),
                    },
                    source_chunk_request(10, validator),
                    fetch,
                    &progress,
                )
                .await;
            assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 1);
            if success {
                let TransferOutcome::Completed(artifact) = result.unwrap() else {
                    panic!("not completed")
                };
                assert_eq!(artifact.trust, Trust::Verified);
                assert_eq!(std::fs::read(artifact.path).unwrap(), expected);
            } else {
                assert!(matches!(
                    result,
                    Err(TransferError::HashMismatch | TransferError::ExactSizeMismatch)
                ));
                assert!(
                    !path.exists(),
                    "el EOF del adaptador no acredita un parcial inválido"
                );
            }
        }
    }

    #[tokio::test]
    async fn source_refused_resume_resets_durable_bytes_and_restores_chunk_limit() {
        use larm_domain::error::ErrorCode;
        let directory = tempdir().unwrap();
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let expected = b"abcdefghij";
        let expected_hash = hex::encode(sha2::Sha256::digest(expected));
        let fetch = move |request: SourceDownloadChunkRequest| {
            let calls = calls.clone();
            async move {
                let mut calls = calls.lock().unwrap();
                let index = calls.len();
                calls.push((request.offset.get(), request.max_bytes.get()));
                let (body, eof, validator) = match index {
                    0 | 1 => ("T0xEIQ==", false, "\"old\""),
                    2 => {
                        return Err(LarmError::new(
                            ErrorCode::StaleRevision,
                            "If-Range no válido",
                            "source_resume_validator_changed",
                        ))
                    }
                    3 => {
                        assert_eq!(request.offset.get(), 0);
                        assert!(request.validator.is_none());
                        ("YWJjZA==", false, "\"new\"")
                    }
                    4 => ("ZWZnaA==", false, "\"new\""),
                    5 => ("aWo=", true, "\"new\""),
                    _ => panic!("unbounded restart"),
                };
                Ok(source_chunk(&request, body, eof, validator))
            }
        };
        let outcome = store
            .write_source_chunks(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(10),
                        max_bytes: 10,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                source_chunk_request(0, None),
                fetch,
                &progress,
            )
            .await
            .unwrap();
        let TransferOutcome::Completed(artifact) = outcome else {
            panic!("not completed")
        };
        assert_eq!(std::fs::read(artifact.path).unwrap(), expected);
        assert_eq!(
            *observed.lock().unwrap(),
            vec![(0, 4), (4, 4), (8, 2), (0, 4), (4, 4), (8, 2)]
        );
        assert_eq!(*progress.offsets.lock().unwrap(), vec![4, 8, 0, 4, 8, 10]);
        assert!(progress.validators.lock().unwrap()[2].is_none());
    }

    #[tokio::test]
    async fn revision_error_at_zero_is_terminal_without_retry() {
        let directory = tempdir().unwrap();
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let fetch = move |_: SourceDownloadChunkRequest| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Err(LarmError::new(
                    larm_domain::error::ErrorCode::StaleRevision,
                    "Revisión no disponible",
                    "source_revision_changed",
                ))
            }
        };
        let result = store
            .write_source_chunks(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(10),
                        max_bytes: 10,
                    },
                    expected_sha256: None,
                },
                source_chunk_request(0, None),
                fetch,
                &progress,
            )
            .await;
        assert!(
            matches!(result, Err(TransferError::Source(error)) if error.code == larm_domain::error::ErrorCode::StaleRevision)
        );
        assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(progress.offsets.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn source_chunks_are_staged_checkpointed_and_restart_on_validator_change() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        let bytes = b"abcdefghij";
        let expected_hash = hex::encode(sha2::Sha256::digest(bytes));
        let fetch = |source_request: SourceDownloadChunkRequest| async move {
            let (body, eof, validator) = match (
                source_request.offset.get(),
                source_request.validator.as_ref(),
            ) {
                (0, None) => ("YWJjZA==", false, "\"v1\""),
                (4, Some(SourceDownloadResumeValidator::StrongEtag { value }))
                    if value == "\"v1\"" =>
                {
                    ("eHh4eA==", false, "\"v2\"")
                }
                (0, Some(SourceDownloadResumeValidator::StrongEtag { value }))
                    if value == "\"v2\"" =>
                {
                    ("YWJjZA==", false, "\"v2\"")
                }
                (4, Some(SourceDownloadResumeValidator::StrongEtag { value }))
                    if value == "\"v2\"" =>
                {
                    ("ZWZnaA==", false, "\"v2\"")
                }
                (8, Some(SourceDownloadResumeValidator::StrongEtag { value }))
                    if value == "\"v2\"" =>
                {
                    ("aWo=", true, "\"v2\"")
                }
                _ => panic!("unexpected source offset/validator"),
            };
            Ok(source_chunk(&source_request, body, eof, validator))
        };

        let outcome = store
            .write_source_chunks(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                source_chunk_request(0, None),
                fetch,
                &progress,
            )
            .await
            .expect("source transfer");

        let TransferOutcome::Completed(artifact) = outcome else {
            panic!("expected completed source transfer");
        };
        assert_eq!(artifact.trust, Trust::Verified);
        assert_eq!(std::fs::read(artifact.path).expect("staged bytes"), bytes);
        let offsets = progress.offsets.lock().expect("offsets").clone();
        assert!(
            offsets.contains(&0),
            "validator change checkpoints reset to zero"
        );
        assert_eq!(offsets.last(), Some(&10));
        let validators = progress.validators.lock().expect("validators");
        assert!(validators.iter().any(|value| {
            value
                == &Some(SourceDownloadResumeValidator::StrongEtag {
                    value: "\"v2\"".into(),
                })
        }));
    }

    #[tokio::test]
    async fn storage_full_keeps_checkpoint_and_resume_discards_partial_tail() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        let bytes = b"abcdefghij";
        let expected_hash = hex::encode(sha2::Sha256::digest(bytes));
        let validator = SourceDownloadResumeValidator::StrongEtag {
            value: "\"v1\"".into(),
        };

        let failed = store
            .write_source_chunks_with_fault(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                source_chunk_request(0, None),
                |request| async move {
                    let body = match request.offset.get() {
                        0 => "YWJjZA==",
                        4 => "ZWZnaA==",
                        _ => panic!("unexpected source offset"),
                    };
                    Ok(source_chunk(&request, body, false, "\"v1\""))
                },
                &progress,
                Some(5),
            )
            .await;

        assert!(matches!(
            failed,
            Err(TransferError::Io(ref error))
                if error.kind() == io::ErrorKind::StorageFull
        ));
        let partial_path = store
            .partial_path(JOB, ARTIFACT)
            .expect("valid partial path");
        assert_eq!(
            std::fs::read(&partial_path).expect("partial bytes"),
            b"abcde"
        );
        assert_eq!(
            *progress.offsets.lock().expect("checkpoint offsets"),
            vec![4],
            "a failed partial write must not advance the durable checkpoint"
        );

        let resumed = store
            .write_source_chunks(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 4,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                source_chunk_request(4, Some(validator)),
                |request| async move {
                    let (body, eof) = match request.offset.get() {
                        4 => ("ZWZnaA==", false),
                        8 => ("aWo=", true),
                        _ => panic!("unexpected resume offset"),
                    };
                    Ok(source_chunk(&request, body, eof, "\"v1\""))
                },
                &progress,
            )
            .await
            .expect("resume after storage full");

        let TransferOutcome::Completed(artifact) = resumed else {
            panic!("expected completed resumed transfer");
        };
        assert_eq!(artifact.size_bytes, bytes.len() as u64);
        assert_eq!(artifact.sha256, expected_hash);
        assert_eq!(std::fs::read(artifact.path).expect("verified bytes"), bytes);
        assert_eq!(
            *progress.offsets.lock().expect("checkpoint offsets"),
            vec![4, 8, 10]
        );
    }

    #[tokio::test]
    async fn sqlite_checkpoint_reanuda_staging_con_el_mismo_validador() {
        let (_db_directory, db) = larm_db::testing::temp_migrated()
            .await
            .expect("temporary database");
        seed_download_job(&db.pool).await;
        let repository = larm_db::download_progress::SqlDownloadProgress {
            pool: db.pool.clone(),
        };
        let progress = SqliteTransferProgress::new(repository);
        let staging_directory = tempdir().expect("staging directory");
        let store = StagingStore::new(staging_directory.path().to_path_buf());
        let bytes = b"helloworld";
        let expected_hash = hex::encode(sha2::Sha256::digest(bytes));
        let validator = SourceDownloadResumeValidator::StrongEtag {
            value: "\"version-1\"".into(),
        };
        sqlx::query("UPDATE download_job SET state='PAUSING' WHERE id=?")
            .bind(JOB)
            .execute(&db.pool)
            .await
            .expect("request pause");

        let first =
            store
                .write_source_chunks(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 0,
                        limits: TransferLimits {
                            expected_size: Some(bytes.len() as u64),
                            max_bytes: bytes.len() as u64,
                        },
                        expected_sha256: Some(&expected_hash),
                    },
                    source_chunk_request(0, None),
                    |request| async move {
                        Ok(source_chunk(&request, "aGVsbA==", false, "\"version-1\""))
                    },
                    &progress,
                )
                .await
                .expect("first paused transfer");
        assert_eq!(first, TransferOutcome::Paused { durable_bytes: 4 });
        let saved = progress
            .load_checkpoint(JOB, ARTIFACT)
            .await
            .expect("read durable checkpoint");
        assert_eq!(saved.durable_offset, 4);
        assert_eq!(saved.resume_validator, Some(validator.clone()));

        sqlx::query("UPDATE download_job SET state='PAUSED' WHERE id=?")
            .bind(JOB)
            .execute(&db.pool)
            .await
            .expect("paused state");
        sqlx::query("UPDATE download_job SET state='QUEUED' WHERE id=?")
            .bind(JOB)
            .execute(&db.pool)
            .await
            .expect("requeued state");
        sqlx::query("UPDATE download_job SET state='RUNNING' WHERE id=?")
            .bind(JOB)
            .execute(&db.pool)
            .await
            .expect("resumed state");

        let resumed = store
            .write_source_chunks(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: saved.durable_offset,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                source_chunk_request(saved.durable_offset, saved.resume_validator),
                |request| async move {
                    let (body, eof) = match request.offset.get() {
                        4 => ("b3dvcg==", false),
                        8 => ("bGQ=", true),
                        _ => panic!("unexpected resume offset"),
                    };
                    Ok(source_chunk(&request, body, eof, "\"version-1\""))
                },
                &progress,
            )
            .await
            .expect("resumed transfer");
        let TransferOutcome::Completed(artifact) = resumed else {
            panic!("expected verified resumed transfer");
        };
        assert_eq!(artifact.sha256, expected_hash);
        assert_eq!(artifact.size_bytes, bytes.len() as u64);
        assert_eq!(std::fs::read(&artifact.path).expect("staged bytes"), bytes);
        assert_eq!(
            progress
                .load_checkpoint(JOB, ARTIFACT)
                .await
                .expect("final durable checkpoint")
                .durable_offset,
            bytes.len() as u64
        );
        drop(staging_directory);
    }

    async fn seed_download_job(pool: &sqlx::SqlitePool) {
        const VARIANT: &str = "0190f5c0-7a00-7000-8000-000000000005";
        const LOCATION: &str = "0190f5c0-7a00-7000-8000-000000000006";
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
            .execute(pool).await.expect("seed adapter installation");
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','[]')")
            .execute(pool).await.expect("seed source adapter");
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(pool).await.expect("seed canonical model");
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','Source','msa','https://example.invalid',0,1,'NONE','{}')")
            .execute(pool).await.expect("seed source");
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm','ms','ns','model')")
            .execute(pool).await.expect("seed distribution");
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1','hash','CURRENT')")
            .execute(pool).await.expect("seed revision");
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).execute(pool).await.expect("seed variant");
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','Destination','D:\\models','D:\\models','NTFS','LOCAL',1000000,1000000,0,0,0,'ONLINE','volume-test','SSD')")
            .bind(LOCATION).execute(pool).await.expect("seed location");
        let metadata = serde_json::json!({"artifacts":[{"artifact_id":ARTIFACT}]});
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,1,?,'NORMAL',?,?,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','RUNNING',1,'{}',NULL)")
            .bind(JOB).bind("t").bind("t").bind(metadata.to_string()).bind(VARIANT).bind(LOCATION).execute(pool).await.expect("seed download job");
    }

    #[tokio::test]
    async fn resumes_from_the_committed_offset_and_discards_an_unsynced_tail() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let path = store.partial_path(JOB, ARTIFACT).expect("safe ids");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("staging dir");
        std::fs::write(&path, b"abcstale").expect("stale partial");
        let progress = Checkpoints::continuing();
        let bytes = b"abcdef";
        let expected_hash = hex::encode(sha2::Sha256::digest(bytes));

        let outcome = store
            .write(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 3,
                    limits: TransferLimits {
                        expected_size: Some(bytes.len() as u64),
                        max_bytes: bytes.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                &b"def"[..],
                &progress,
            )
            .await
            .expect("resumed transfer");

        let TransferOutcome::Completed(artifact) = outcome else {
            panic!("expected completed transfer");
        };
        assert_eq!(std::fs::read(artifact.path).expect("staged bytes"), bytes);
        assert_eq!(*progress.offsets.lock().expect("offsets"), vec![6]);
    }

    #[tokio::test]
    async fn failed_checkpoint_is_recovered_from_the_repository_offset() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let path = store.partial_path(JOB, ARTIFACT).expect("safe ids");
        assert!(matches!(
            store
                .write(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 0,
                        limits: TransferLimits {
                            expected_size: None,
                            max_bytes: 64,
                        },
                        expected_sha256: None,
                    },
                    &b"uncommitted offset"[..],
                    &FailingCheckpoint,
                )
                .await,
            Err(TransferError::Checkpoint(_))
        ));
        assert_eq!(
            std::fs::read(&path).expect("synced but uncommitted bytes"),
            b"uncommitted offset"
        );

        let replacement = b"restart from zero";
        let expected_hash = hex::encode(sha2::Sha256::digest(replacement));
        let outcome = store
            .write(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: Some(replacement.len() as u64),
                        max_bytes: replacement.len() as u64,
                    },
                    expected_sha256: Some(&expected_hash),
                },
                &replacement[..],
                &Checkpoints::continuing(),
            )
            .await
            .expect("restart after failed checkpoint");
        let TransferOutcome::Completed(artifact) = outcome else {
            panic!("expected completed transfer");
        };
        assert_eq!(
            std::fs::read(artifact.path).expect("replacement bytes"),
            replacement
        );
    }

    #[tokio::test]
    async fn pause_preserves_synced_bytes_and_cancel_removes_only_its_partial() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let paused = Checkpoints {
            control: Mutex::new(TransferControl::Pause),
            offsets: Mutex::new(Vec::new()),
            validators: Mutex::new(Vec::new()),
        };
        let outcome = store
            .write(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: None,
                        max_bytes: 32,
                    },
                    expected_sha256: None,
                },
                &b"partial"[..],
                &paused,
            )
            .await
            .expect("pause");
        assert_eq!(outcome, TransferOutcome::Paused { durable_bytes: 7 });
        let path = store.partial_path(JOB, ARTIFACT).expect("safe ids");
        assert_eq!(std::fs::read(&path).expect("partial bytes"), b"partial");

        let cancelled = Checkpoints {
            control: Mutex::new(TransferControl::Cancel),
            offsets: Mutex::new(Vec::new()),
            validators: Mutex::new(Vec::new()),
        };
        assert_eq!(
            store
                .write(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 7,
                        limits: TransferLimits {
                            expected_size: None,
                            max_bytes: 32,
                        },
                        expected_sha256: None,
                    },
                    &b"more"[..],
                    &cancelled,
                )
                .await
                .expect("cancel"),
            TransferOutcome::Cancelled
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn enforces_byte_budget_and_cleans_bad_integrity_results() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let progress = Checkpoints::continuing();
        assert!(matches!(
            store
                .write(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 0,
                        limits: TransferLimits {
                            expected_size: None,
                            max_bytes: 3,
                        },
                        expected_sha256: None,
                    },
                    &b"four"[..],
                    &progress,
                )
                .await,
            Err(TransferError::SizeLimitExceeded)
        ));
        let path = store.partial_path(JOB, ARTIFACT).expect("safe ids");
        assert_eq!(std::fs::metadata(&path).expect("empty partial").len(), 0);

        assert!(matches!(
            store
                .write(
                    TransferRequest {
                        job_id: JOB,
                        artifact_id: ARTIFACT,
                        resume_offset: 0,
                        limits: TransferLimits {
                            expected_size: Some(4),
                            max_bytes: 4,
                        },
                        expected_sha256: Some(&"0".repeat(64)),
                    },
                    &b"data"[..],
                    &progress,
                )
                .await,
            Err(TransferError::HashMismatch)
        ));
        assert!(!path.exists());
    }

    #[test]
    fn staging_paths_reject_non_uuid_identities() {
        let store = StagingStore::new(PathBuf::from("root"));
        assert!(store.partial_path("../escape", ARTIFACT).is_none());
        assert!(store.partial_path(JOB, "artifact").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_symlinked_partial_files_without_touching_the_target() {
        let directory = tempdir().expect("tempdir");
        let store = StagingStore::new(directory.path().to_path_buf());
        let path = store.partial_path(JOB, ARTIFACT).expect("safe ids");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("staging dir");
        let target = directory.path().join("outside.bin");
        std::fs::write(&target, b"keep me").expect("target");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");

        let result = store
            .write(
                TransferRequest {
                    job_id: JOB,
                    artifact_id: ARTIFACT,
                    resume_offset: 0,
                    limits: TransferLimits {
                        expected_size: None,
                        max_bytes: 64,
                    },
                    expected_sha256: None,
                },
                &b"overwrite attempt"[..],
                &Checkpoints::continuing(),
            )
            .await;

        assert!(matches!(result, Err(TransferError::InvalidStagingPath)));
        assert_eq!(std::fs::read(target).expect("untouched target"), b"keep me");
    }
}
