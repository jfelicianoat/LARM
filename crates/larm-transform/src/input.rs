//! Physical input verification for P093. Catalogue descriptors and current
//! resolved roots come from the server, never from the execution HTTP body.
use std::{
    collections::BTreeMap,
    path::{Component, PathBuf},
};

use larm_adapter_host::input_files::PinnedReadFile;
use larm_domain::{
    artifacts::normalize_relative,
    bridge::is_uuid_v7,
    error::{ErrorCode, LarmError},
    transformations::{
        verify_observed_input_hashes, InputArtifact, TransformationInputGuard,
        TransformationInputIdentity, TransformationInputLocation,
    },
};
use sha2::{Digest, Sha256};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "Los archivos de entrada no coinciden con el plan",
        reason,
    )
}

struct FileInput {
    artifact: InputArtifact,
    owner: PinnedReadFile,
}

pub(crate) type InputCandidates = BTreeMap<String, Vec<(TransformationInputLocation, PathBuf)>>;

/// Keeps all files and their ancestor paths protected from initial measurement
/// through the final admission check. This is not adapter qualification, a disk
/// reservation, or permission to run a transformation.
pub struct VerifiedTransformationInput {
    identity: TransformationInputIdentity,
    artifacts: Vec<InputArtifact>,
    locations: Vec<TransformationInputLocation>,
    files: Vec<FileInput>,
}

impl VerifiedTransformationInput {
    pub async fn acquire(
        identity: TransformationInputIdentity,
        artifacts: Vec<InputArtifact>,
        locations: Vec<TransformationInputLocation>,
        resolved_roots: BTreeMap<String, PathBuf>,
    ) -> Result<Self, LarmError> {
        if locations.len() != artifacts.len() {
            return Err(invalid("transformation_input_locations_invalid"));
        }
        let mut candidates = InputCandidates::new();
        for location in locations {
            let root = resolved_roots
                .get(&location.storage_location_id)
                .ok_or_else(|| invalid("transformation_input_root_missing"))?
                .clone();
            if candidates
                .insert(location.artifact_id.clone(), vec![(location, root)])
                .is_some()
            {
                return Err(invalid("transformation_input_locations_invalid"));
            }
        }
        Self::acquire_candidates(identity, artifacts, candidates).await
    }

    pub(crate) async fn acquire_candidates(
        identity: TransformationInputIdentity,
        artifacts: Vec<InputArtifact>,
        candidates: InputCandidates,
    ) -> Result<Self, LarmError> {
        // Cancellation leaves this task owning every handle until its last
        // filesystem read finishes; dropping the awaiting future cannot unpin
        // a file while that blocking read is still active.
        tokio::task::spawn_blocking(move || Self::measure(identity, artifacts, candidates))
            .await
            .map_err(|_| invalid("transformation_input_measurement_failed"))?
    }

    fn measure(
        identity: TransformationInputIdentity,
        artifacts: Vec<InputArtifact>,
        candidates: InputCandidates,
    ) -> Result<Self, LarmError> {
        verify_observed_input_hashes(&artifacts, &artifacts)
            .map_err(|_| invalid("transformation_input_snapshot_invalid"))?;
        if ![
            &identity.variant_id,
            &identity.revision_id,
            &identity.bundle_id,
        ]
        .into_iter()
        .all(|id| is_uuid_v7(id))
            || identity.bundle_row_version < 1
            || ![
                &identity.variant_bundle_hash,
                &identity.bundle_manifest_hash,
            ]
            .into_iter()
            .all(|h| larm_domain::adapter_updates::is_sha256(h))
        {
            return Err(invalid("transformation_input_identity_invalid"));
        }
        if candidates.len() != artifacts.len() {
            return Err(invalid("transformation_input_locations_invalid"));
        }
        let mut files = Vec::with_capacity(artifacts.len());
        let mut locations = Vec::with_capacity(artifacts.len());
        let mut buffer = vec![0_u8; 1 << 20];
        // Earlier verified inputs stay pinned while later copies are selected.
        // At return every hash describes bytes owned simultaneously.
        for artifact in &artifacts {
            let choices = candidates
                .get(artifact.artifact_id.as_str())
                .ok_or_else(|| invalid("transformation_input_location_missing"))?;
            let mut selected = None;
            let mut last_error = invalid("transformation_input_copy_unavailable");
            for (location, root) in choices {
                match read_candidate(artifact, location, root, &mut buffer) {
                    Ok(file) => {
                        selected = Some((location.clone(), file));
                        break;
                    }
                    Err(error) => last_error = error,
                }
            }
            let (location, file) =
                selected.ok_or_else(|| last_error.with_resource(artifact.artifact_id.clone()))?;
            locations.push(location);
            files.push(file);
        }
        Ok(Self {
            identity,
            artifacts,
            locations,
            files,
        })
    }
}

fn read_candidate(
    artifact: &InputArtifact,
    location: &TransformationInputLocation,
    root: &std::path::Path,
    buffer: &mut [u8],
) -> Result<FileInput, LarmError> {
    if location.artifact_id != artifact.artifact_id
        || !is_uuid_v7(&location.storage_location_id)
        || !is_uuid_v7(&location.artifact_location_id)
        || location.volume_key.is_empty()
        || location.volume_key.starts_with("unknown:")
    {
        return Err(invalid("transformation_input_location_invalid"));
    }
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("transformation_input_root_invalid"));
    }
    let relative = normalize_relative(&location.relative_path)?;
    let owner = PinnedReadFile::open(&root.join(relative))?;
    let expected = artifact.size_bytes.get();
    if owner.size()? != expected {
        return Err(invalid("transformation_input_size_changed"));
    }
    let mut offset = 0;
    let mut hash = Sha256::new();
    while offset < expected {
        let count = usize::try_from((expected - offset).min(buffer.len() as u64))
            .map_err(|_| invalid("transformation_input_size_invalid"))?;
        let read = owner.read_at(offset, &mut buffer[..count])?;
        if read == 0 {
            return Err(invalid("transformation_input_truncated"));
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
    }
    if owner.read_at(offset, &mut buffer[..1])? != 0
        || hex::encode(hash.finalize()) != artifact.sha256
    {
        return Err(invalid("transformation_input_hash_changed"));
    }
    owner.recheck()?;
    Ok(FileInput {
        artifact: artifact.clone(),
        owner,
    })
}

#[async_trait::async_trait]
impl TransformationInputGuard for VerifiedTransformationInput {
    fn identity(&self) -> &TransformationInputIdentity {
        &self.identity
    }
    fn artifacts(&self) -> &[InputArtifact] {
        &self.artifacts
    }
    fn locations(&self) -> &[TransformationInputLocation] {
        &self.locations
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        for file in &self.files {
            if file.owner.size()? != file.artifact.size_bytes.get() {
                return Err(invalid("transformation_input_size_changed"));
            }
        }
        Ok(())
    }
}
