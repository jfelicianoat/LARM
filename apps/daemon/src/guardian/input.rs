//! Read and retain the admitted model bytes before any tool handshake.
use larm_adapter_host::input_files::PinnedReadFile;
use larm_domain::{
    error::{ErrorCode, LarmError},
    transformation_execution::TransformationClaim,
    transformations::InputArtifact,
};
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(super) struct Pins(Vec<(PinnedReadFile, InputArtifact)>);
fn changed() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El input original de la transformación ha cambiado",
        "transformation_native_input_changed",
    )
}
impl Pins {
    pub(super) async fn capture(claim: &TransformationClaim) -> Result<Self, LarmError> {
        let mut files = Vec::new();
        for artifact in &claim.plan.input_artifacts {
            let location = claim
                .input_locations
                .iter()
                .find(|l| l.artifact_id == artifact.artifact_id)
                .ok_or_else(changed)?;
            larm_domain::model::bundle_path(&location.relative_path).map_err(|_| changed())?;
            let root = std::path::Path::new(&location.canonical_storage_path);
            if !root.is_absolute() {
                return Err(changed());
            }
            let file = PinnedReadFile::open(&root.join(&location.relative_path))?;
            files.push((file, artifact.clone()));
        }
        let pins = Self(files);
        pins.recheck().await?;
        Ok(pins)
    }
    pub(super) async fn recheck(&self) -> Result<(), LarmError> {
        let pins = self.clone();
        // Cancellation retains these handles until the real read completes.
        tokio::task::spawn_blocking(move || pins.check())
            .await
            .map_err(|_| changed())?
    }
    fn check(&self) -> Result<(), LarmError> {
        if self.0.is_empty() {
            return Err(changed());
        }
        let mut buffer = vec![0; 1 << 20];
        for (file, artifact) in &self.0 {
            let expected = artifact.size_bytes.get();
            if file.size()? != expected {
                return Err(changed());
            }
            let mut offset = 0_u64;
            let mut digest = Sha256::new();
            while offset < expected {
                let amount = usize::try_from((expected - offset).min(buffer.len() as u64))
                    .map_err(|_| changed())?;
                let read = file.read_at(offset, &mut buffer[..amount])?;
                if read == 0 {
                    return Err(changed());
                }
                digest.update(&buffer[..read]);
                offset += read as u64;
            }
            if file.read_at(expected, &mut buffer[..1])? != 0
                || file.size()? != expected
                || hex::encode(digest.finalize()) != artifact.sha256
            {
                return Err(changed());
            }
        }
        Ok(())
    }
}
