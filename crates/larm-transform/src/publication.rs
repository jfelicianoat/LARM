//! Recoverable managed-CAS publication from retained, measured output files.
//! SQLite must mint the preparation ticket before any owned filesystem write.
use crate::{
    environment::{observation_time, resolve_location},
    output_validation::CheckedTransformationOutput,
};
use larm_adapter_host::input_files::{PinnedDirectory, PinnedReadFile};
use larm_db::transformations::{
    CommittedTransformationPublication, PreparedTransformationPublication,
};
use larm_domain::{
    error::{ErrorCode, LarmError},
    ports::StorageProber,
    storage::parse_unc,
    transformation_execution::{TransformationAttemptFence, TransformationAttemptGuard},
    transformation_publication::{
        TransformationMeasuredOutputGuard, TransformationPublicationGuard,
        TransformationPublicationRecord,
    },
};
use larm_storage::{
    cas,
    rebuild::{
        Manifest, ManifestArtifact, ManifestDistribution, ManifestModel, ManifestRevision,
        ManifestVariant, MANIFEST_MAX_BYTES, MANIFEST_VERSION,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "No se pudo incorporar la salida al almacén",
        reason,
    )
}
fn io(error: std::io::Error) -> LarmError {
    invalid("transformation_publication_io_failed").with_technical(error.to_string())
}

pub struct PublishedTransformationOutput {
    prepared: Arc<PreparedTransformationPublication>,
    root: PathBuf,
    directories: Vec<PinnedDirectory>,
    blobs: Vec<PinnedReadFile>,
    manifest: PinnedReadFile,
    marker: PinnedReadFile,
    writer: File,
}

fn plain_file(path: &Path) -> Result<bool, LarmError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    return Err(invalid("transformation_publication_reparse"));
                }
            }
            if !m.is_file() {
                return Err(invalid("transformation_publication_not_file"));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(e)),
    }
}
fn open_owned(path: &Path, new: bool) -> Result<File, LarmError> {
    if !new {
        plain_file(path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0).custom_flags(0x00200000);
    }
    let file = options.open(path).map_err(io)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata().map_err(io)?.file_attributes() & 0x400 != 0 {
            return Err(invalid("transformation_publication_reparse"));
        }
    }
    Ok(file)
}
fn directory(
    parent: &Path,
    name: &str,
    pins: &mut Vec<PinnedDirectory>,
) -> Result<PathBuf, LarmError> {
    let path = parent.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io(e)),
    }
    pins.push(PinnedDirectory::open(&path)?);
    Ok(path)
}
fn verify(path: &Path, sha: &str, size: u64) -> Result<PinnedReadFile, LarmError> {
    let owner = PinnedReadFile::open(path)?;
    if owner.size()? != size {
        return Err(invalid("transformation_cas_size_changed"));
    }
    let mut offset = 0;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    while offset < size {
        let count = (size - offset).min(buffer.len() as u64) as usize;
        let read = owner.read_at(offset, &mut buffer[..count])?;
        if read == 0 {
            return Err(invalid("transformation_cas_truncated"));
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
    }
    if owner.read_at(offset, &mut buffer[..1])? != 0 || hex::encode(hash.finalize()) != sha {
        return Err(invalid("transformation_cas_hash_changed"));
    }
    owner.recheck()?;
    Ok(owner)
}
fn manifest(record: &TransformationPublicationRecord) -> Result<Manifest, LarmError> {
    Ok(Manifest {
        schema_version: MANIFEST_VERSION,
        variant_id: record.variant.id.clone(),
        written_at: record.variant.created_at.clone(),
        model: ManifestModel {
            name: record.model.name.clone(),
            upstream_identity: record.model.upstream_identity.clone(),
            evidence: json!({"transformation":record}),
        },
        distribution: ManifestDistribution {
            source_id: record.distribution.source_id.clone(),
            upstream_namespace: record.distribution.upstream_namespace.clone(),
            upstream_model_id: record.distribution.upstream_model_id.clone(),
            license_id: record.distribution.license_id.clone(),
        },
        revision: ManifestRevision {
            revision_key: record.revision.revision_key.clone(),
            manifest_hash: record.revision.manifest_hash.clone(),
            upstream_commit: record.revision.upstream_commit.clone(),
        },
        variant: ManifestVariant {
            format: record.variant.format.clone(),
            quantization: record.variant.quantization.clone(),
            architecture: record.variant.architecture.clone(),
            capabilities: record.variant.capabilities.clone(),
        },
        artifacts: record
            .artifacts
            .iter()
            .zip(&record.locations)
            .map(|(a, l)| {
                Ok(ManifestArtifact {
                    filename: a.filename.clone(),
                    role: serde_json::to_value(a.role())
                        .map_err(|_| invalid("transformation_role_invalid"))?
                        .as_str()
                        .ok_or_else(|| invalid("transformation_role_invalid"))?
                        .into(),
                    sha256: a
                        .sha256
                        .clone()
                        .ok_or_else(|| invalid("transformation_cas_hash_missing"))?,
                    size_bytes: a
                        .actual_size
                        .ok_or_else(|| invalid("transformation_cas_size_missing"))?
                        .get(),
                    required: a.required,
                    blob: l.relative_path.clone(),
                })
            })
            .collect::<Result<_, LarmError>>()?,
    })
}

impl PublishedTransformationOutput {
    pub async fn publish(
        prepared: Arc<PreparedTransformationPublication>,
        output: Arc<CheckedTransformationOutput>,
        owner: &dyn TransformationAttemptGuard,
        prober: Arc<dyn StorageProber>,
        space: &dyn larm_domain::transformations::TransformationSpaceObserver,
        jobs: &larm_db::transformations::SqlTransformationJobs,
    ) -> Result<Self, LarmError> {
        owner.require_closed().await?;
        owner.recheck().await?;
        output.recheck().await?;
        jobs.check_publication_space(&prepared, &space.observe().await?)
            .await?;
        let record = prepared.record();
        if record.digest()? != prepared.record_sha256()
            || record.measured != output.measured()
            || owner.fence() != &record.measured.fence
            || owner.binding() != &record.measured.binding
        {
            return Err(invalid("transformation_publication_ticket_mismatch"));
        }
        let uncs = record
            .destination
            .unc_path
            .as_deref()
            .and_then(|p| parse_unc(p).ok())
            .map(|p| vec![p.root()])
            .unwrap_or_default();
        let observation = prober.probe(&uncs).await?;
        observation_time(&observation)?;
        let volume = observation
            .volumes
            .iter()
            .find(|v| v.identity.volume_key.as_deref() == Some(&record.destination.volume_key))
            .ok_or_else(|| invalid("transformation_publication_volume_missing"))?;
        if volume.read_only
            || volume
                .filesystem
                .as_deref()
                .is_none_or(|fs| !fs.eq_ignore_ascii_case(&record.destination.filesystem))
            || (volume
                .filesystem
                .as_deref()
                .is_some_and(|fs| fs.eq_ignore_ascii_case("FAT32"))
                && record
                    .artifacts
                    .iter()
                    .any(|a| a.actual_size.is_none_or(|s| s.get() > u32::MAX as u64)))
        {
            return Err(invalid("transformation_publication_volume_ineligible"));
        }
        let root = resolve_location(&record.destination, &observation)?;
        let root_pin = PinnedDirectory::open(&root)?;
        root_pin.check_writable()?;
        let publication =
            tokio::task::spawn_blocking(move || Self::write(prepared, output, root, root_pin))
                .await
                .map_err(|_| invalid("transformation_publication_task_failed"))??;
        owner.require_closed().await?;
        owner.recheck().await?;
        publication.recheck().await?;
        Ok(publication)
    }

    fn write(
        prepared: Arc<PreparedTransformationPublication>,
        output: Arc<CheckedTransformationOutput>,
        root: PathBuf,
        root_pin: PinnedDirectory,
    ) -> Result<Self, LarmError> {
        let record = prepared.record();
        let marker_bytes=serde_json::to_vec(&json!({"schema_version":1,"fence":record.measured.fence,"record_sha256":prepared.record_sha256()})).map_err(|_|invalid("transformation_marker_invalid"))?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest(record)?)
            .map_err(|_| invalid("transformation_manifest_invalid"))?;
        let control = (manifest_bytes.len() as u64)
            .checked_mul(2)
            .and_then(|n| n.checked_add(marker_bytes.len() as u64))
            .and_then(|n| n.checked_add(record.artifacts.len() as u64 * 4096))
            .ok_or_else(|| invalid("transformation_control_overflow"))?;
        if manifest_bytes.len() as u64 > MANIFEST_MAX_BYTES
            || control > record.measured.policy.control_bytes.get()
        {
            return Err(invalid("transformation_output_control_budget_exceeded"));
        }
        let mut directories = vec![root_pin];
        let staging = directory(&root, "staging", &mut directories)?;
        let owned = directory(&staging, &record.measured.fence.claim_id, &mut directories)?;
        // Actual exclusive Windows file ownership prevents two publishers from
        // deleting or replacing one another's partial copy during recovery.
        let writer = open_owned(&owned.join("publish.lock"), false)?;
        let marker_path = owned.join("publication.json");
        if !plain_file(&marker_path)? {
            let mut file = open_owned(&marker_path, true)?;
            file.write_all(&marker_bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        let marker = verify(
            &marker_path,
            &hex::encode(Sha256::digest(&marker_bytes)),
            marker_bytes.len() as u64,
        )?;
        let blob_root = directory(&root, "blobs", &mut directories)?;
        let sha_root = directory(&blob_root, "sha256", &mut directories)?;
        let mut blobs = Vec::new();
        for (index, artifact) in record.artifacts.iter().enumerate() {
            for pin in &directories {
                pin.recheck()?;
            }
            output.output().recheck()?;
            let sha = artifact
                .sha256
                .as_deref()
                .ok_or_else(|| invalid("transformation_cas_hash_missing"))?;
            let size = artifact
                .actual_size
                .ok_or_else(|| invalid("transformation_cas_size_missing"))?
                .get();
            let relative = cas::blob_relative(sha).map_err(io)?;
            directory(&sha_root, &sha[..2], &mut directories)?;
            let blob = root.join(&relative);
            if let Some(promoted) = cas::recover_staged_promotion(
                &root,
                &record.measured.fence.claim_id,
                &artifact.id,
                &record.measured.fence.plan_hash,
            )
            .map_err(io)?
            {
                if promoted.relative_path != relative
                    || promoted.sha256 != sha
                    || promoted.size_bytes != size
                {
                    return Err(invalid("transformation_cas_receipt_mismatch"));
                }
                blobs.push(verify(&blob, sha, size)?);
                continue;
            }
            if plain_file(&blob)? {
                blobs.push(verify(&blob, sha, size)?);
                continue;
            }
            let partial = owned.join(format!("{}.partial", artifact.id));
            if plain_file(&partial)? {
                // The immutable owner marker and exclusive publish lock bind
                // this exact partial to this claim, never to an older attempt.
                let pin = PinnedReadFile::open(&partial)?;
                pin.recheck()?;
                drop(pin);
                std::fs::remove_file(&partial).map_err(io)?;
            }
            let mut file = open_owned(&partial, true)?;
            let mut buffer = vec![0; 1 << 20];
            let mut offset = 0;
            while offset < size {
                let count = (size - offset).min(buffer.len() as u64) as usize;
                let read = output
                    .output()
                    .read_at(index, offset, &mut buffer[..count])?;
                if read == 0 {
                    return Err(invalid("transformation_output_truncated_during_copy"));
                }
                file.write_all(&buffer[..read]).map_err(io)?;
                offset += read as u64;
            }
            file.sync_all().map_err(io)?;
            drop(file);
            let result = cas::promote_staged(
                &root,
                &partial,
                &record.measured.fence.claim_id,
                &artifact.id,
                &record.measured.fence.plan_hash,
                sha,
                size,
            )
            .map_err(io)?;
            if result.relative_path != relative {
                return Err(invalid("transformation_cas_path_mismatch"));
            }
            blobs.push(verify(&blob, sha, size)?);
        }
        let manifests = directory(&root, "manifests", &mut directories)?;
        let manifest_path = manifests.join(format!("{}.json", record.variant.id));
        if !plain_file(&manifest_path)? {
            let temp = manifests.join(format!(
                "{}.{}.partial",
                record.variant.id, record.measured.fence.claim_id
            ));
            if plain_file(&temp)? {
                let pin = PinnedReadFile::open(&temp)?;
                pin.recheck()?;
                drop(pin);
                std::fs::remove_file(&temp).map_err(io)?;
            }
            let mut file = open_owned(&temp, true)?;
            file.write_all(&manifest_bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
            drop(file);
            // Persist without replacement, including a competing target that
            // appears after the existence check. The retained parent pins and
            // subsequent hash measurement also cover a recovered temporary.
            let file = File::open(&temp).map_err(io)?;
            let named = tempfile::NamedTempFile::from_parts(
                file,
                tempfile::TempPath::try_from_path(temp).map_err(io)?,
            );
            named
                .persist_noclobber(&manifest_path)
                .map_err(|e| io(e.error))?;
        }
        let manifest = verify(
            &manifest_path,
            &hex::encode(Sha256::digest(&manifest_bytes)),
            manifest_bytes.len() as u64,
        )?;
        Ok(Self {
            prepared,
            root,
            directories,
            blobs,
            manifest,
            marker,
            writer,
        })
    }

    /// Receipts can be removed only with repository-minted evidence of the
    /// atomic catalogue/job commit. Never delete a promoted shared blob.
    pub fn finish(self, committed: &CommittedTransformationPublication) -> Result<(), LarmError> {
        if committed.fence() != &self.prepared.record().measured.fence
            || committed.record_sha256() != self.prepared.record_sha256()
        {
            return Err(invalid("transformation_publication_commit_mismatch"));
        }
        for file in &self.blobs {
            file.recheck()?;
        }
        self.manifest.recheck()?;
        self.marker.recheck()?;
        let record = self.prepared.record();
        for artifact in &record.artifacts {
            cas::finish_staged_promotion(
                &self.root,
                &record.measured.fence.claim_id,
                &artifact.id,
                &record.measured.fence.plan_hash,
            )
            .map_err(io)?;
        }
        let marker_path = self
            .root
            .join("staging")
            .join(&record.measured.fence.claim_id)
            .join("publication.json");
        drop(self.marker);
        std::fs::remove_file(marker_path).map_err(io)?;
        Ok(())
    }
}
#[async_trait::async_trait]
impl TransformationPublicationGuard for PublishedTransformationOutput {
    fn fence(&self) -> &TransformationAttemptFence {
        &self.prepared.record().measured.fence
    }
    fn record_sha256(&self) -> &str {
        self.prepared.record_sha256()
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        for directory in &self.directories {
            directory.recheck()?;
        }
        for blob in &self.blobs {
            blob.recheck()?;
        }
        self.manifest.recheck()?;
        self.marker.recheck()?;
        self.writer.metadata().map_err(io)?;
        Ok(())
    }
}
