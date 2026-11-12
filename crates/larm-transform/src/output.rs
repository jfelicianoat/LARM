//! Independent output measurement after the qualified process and IO close.
//! This proof authorizes no catalogue mutation, CAS write or runtime status.
use crate::environment::{observation_time, stable_path};
use larm_adapter_host::{
    input_files::{PinnedDirectory, PinnedReadFile},
    transformation::{ArtifactManifest, TransformationOutputManifest},
};
use larm_domain::{
    error::{ErrorCode, LarmError},
    model::{bundle_hash, bundle_path, BundleEntry},
    ports::StorageProber,
    storage::parse_unc,
    transformation_execution::{
        TransformationAttemptFence, TransformationAttemptGuard, TransformationClaim,
    },
    transformations::{TransformationExecutionBinding, TransformationOutputPolicy},
};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "La salida de transformación no es íntegra",
        reason,
    )
}

/// The containing directory and prefix must come from the server's committed
/// qualified profile and worker receipt, never an HTTP path. The physical
/// volume is independently observed and addressed by GUID before opening.
pub struct TransformationOutputScope {
    pub root: PathBuf,
    pub bundle_prefix: String,
}

struct OutputFile {
    entry: BundleEntry,
    source_relative_path: String,
    role: String,
    owner: PinnedReadFile,
}

pub struct VerifiedTransformationOutput {
    fence: TransformationAttemptFence,
    binding: TransformationExecutionBinding,
    worker_job_id: String,
    target_format: String,
    policy: TransformationOutputPolicy,
    manifest_hash: String,
    root: PinnedDirectory,
    files: Vec<OutputFile>,
}

impl VerifiedTransformationOutput {
    pub async fn capture(
        claim: &TransformationClaim,
        reply: &TransformationOutputManifest,
        guard: &dyn TransformationAttemptGuard,
        scope: TransformationOutputScope,
        prober: Arc<dyn StorageProber>,
    ) -> Result<Self, LarmError> {
        // No file is opened while the writer or any of its IO is still live.
        guard.require_closed().await?;
        guard.recheck().await?;
        claim.fence.validate()?;
        claim
            .job
            .validate()
            .map_err(|_| invalid("transformation_output_claim_invalid"))?;
        claim
            .plan
            .verify_seal()
            .map_err(|_| invalid("transformation_output_plan_invalid"))?;
        if guard.fence() != &claim.fence
            || reply.fence() != &claim.fence
            || reply.binding() != guard.binding()
            || claim.plan.execution_binding.as_ref() != Some(guard.binding())
            || claim.plan.response.plan_hash != claim.fence.plan_hash
            || claim.job.id != claim.fence.job_id
            || claim.job.plan_hash != claim.fence.plan_hash
            || claim.job.attempt != claim.fence.attempt
        {
            return Err(invalid("transformation_output_attempt_mismatch"));
        }
        let policy = claim
            .plan
            .output_policy
            .clone()
            .ok_or_else(|| invalid("transformation_output_policy_missing"))?;
        validate_scope(&scope)?;
        let uncs = scope
            .root
            .to_str()
            .and_then(|p| parse_unc(p).ok())
            .map(|p| vec![p.root()])
            .unwrap_or_default();
        let observation = prober.probe(&uncs).await?;
        observation_time(&observation)?;
        let path = stable_path(&observation, &scope.root, &policy.worker_volume_key)?;
        let root = PinnedDirectory::open(&path)?;
        let manifest = reply.manifest().clone();
        let fence = reply.fence().clone();
        let binding = reply.binding().clone();
        let worker_job_id = reply.worker_job_id().to_owned();
        let format = claim.plan.request.target_format.clone();
        let output = tokio::task::spawn_blocking(move || {
            let files = measure(&path, &scope.bundle_prefix, &manifest, &policy)?;
            root.recheck()?;
            Ok(Self {
                fence,
                binding,
                worker_job_id,
                target_format: format,
                policy,
                manifest_hash: manifest.manifest_hash,
                root,
                files,
            })
        })
        .await
        .map_err(|_| invalid("transformation_output_measurement_failed"))??;
        guard.require_closed().await?;
        guard.recheck().await?;
        output.recheck()?;
        Ok(output)
    }

    pub fn fence(&self) -> &TransformationAttemptFence {
        &self.fence
    }
    pub fn binding(&self) -> &TransformationExecutionBinding {
        &self.binding
    }
    pub fn worker_job_id(&self) -> &str {
        &self.worker_job_id
    }
    pub fn target_format(&self) -> &str {
        &self.target_format
    }
    pub fn policy(&self) -> &TransformationOutputPolicy {
        &self.policy
    }
    pub fn worker_manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    pub fn entries(&self) -> Vec<BundleEntry> {
        self.files.iter().map(|f| f.entry.clone()).collect()
    }
    pub fn source_relative_path(&self, index: usize) -> Option<&str> {
        self.files
            .get(index)
            .map(|f| f.source_relative_path.as_str())
    }
    pub fn role(&self, index: usize) -> Option<&str> {
        self.files.get(index).map(|f| f.role.as_str())
    }

    /// A future CAS copier/registered runtime hook reads the retained file
    /// handle, never a fresh path which a producer might have replaced.
    pub fn read_at(&self, index: usize, offset: u64, bytes: &mut [u8]) -> Result<usize, LarmError> {
        self.recheck()?;
        let file = self
            .files
            .get(index)
            .ok_or_else(|| invalid("transformation_output_index_invalid"))?;
        if offset > file.entry.size_bytes {
            return Err(invalid("transformation_output_offset_invalid"));
        }
        let count = (file.entry.size_bytes - offset).min(bytes.len() as u64) as usize;
        file.owner.read_at(offset, &mut bytes[..count])
    }

    pub fn recheck(&self) -> Result<(), LarmError> {
        self.root.recheck()?;
        for file in &self.files {
            if file.owner.size()? != file.entry.size_bytes {
                return Err(invalid("transformation_output_size_changed"));
            }
        }
        Ok(())
    }
}

fn validate_scope(scope: &TransformationOutputScope) -> Result<(), LarmError> {
    if !scope.root.is_absolute()
        || scope
            .root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("transformation_output_root_invalid"));
    }
    bundle_path(&scope.bundle_prefix)?;
    Ok(())
}

fn measure(
    root: &Path,
    prefix: &str,
    manifest: &ArtifactManifest,
    policy: &TransformationOutputPolicy,
) -> Result<Vec<OutputFile>, LarmError> {
    policy
        .validate()
        .map_err(|_| invalid("transformation_output_policy_invalid"))?;
    manifest.validate()?;
    if manifest.artifacts.is_empty()
        || manifest.artifacts.len() > policy.max_artifacts as usize
        || !manifest.artifacts.iter().any(|a| a.required)
    {
        return Err(invalid("transformation_output_artifacts_invalid"));
    }
    let mut total = 0_u64;
    let mut descriptors = Vec::with_capacity(manifest.artifacts.len());
    let mut logical = Vec::with_capacity(manifest.artifacts.len());
    let prefix = format!("{prefix}/");
    // Validate the complete manifest, its sealed limits and scope before IO.
    for artifact in &manifest.artifacts {
        let size = artifact
            .size_bytes
            .ok_or_else(|| invalid("transformation_output_size_unknown"))?
            .get();
        let sha = artifact
            .sha256
            .as_ref()
            .ok_or_else(|| invalid("transformation_output_hash_missing"))?;
        total = total
            .checked_add(size)
            .filter(|n| *n <= policy.max_total_bytes.get())
            .ok_or_else(|| invalid("transformation_output_total_exceeded"))?;
        if size > policy.max_file_bytes.get() {
            return Err(invalid("transformation_output_file_exceeded"));
        }
        let name = artifact
            .relative_path
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid("transformation_output_outside_attempt"))?;
        bundle_path(name)?;
        let descriptor = BundleEntry {
            relative_path: artifact.relative_path.clone(),
            size_bytes: size,
            sha256: sha.clone(),
            required: artifact.required,
        };
        let mut entry = descriptor.clone();
        entry.relative_path = name.into();
        descriptors.push(descriptor);
        logical.push(entry);
    }
    if bundle_hash(&descriptors)? != manifest.manifest_hash
        || (manifest.revision_key.starts_with("content:")
            && manifest.revision_key != format!("content:{}", manifest.manifest_hash))
    {
        return Err(invalid("transformation_output_manifest_mismatch"));
    }
    // A stripped logical namespace must itself remain an unambiguous bundle.
    bundle_hash(&logical)?;
    let mut files = Vec::with_capacity(logical.len());
    let mut buffer = vec![0_u8; 1 << 20];
    for (artifact, entry) in manifest.artifacts.iter().zip(logical) {
        let owner = PinnedReadFile::open(&root.join(&artifact.relative_path))?;
        if owner.size()? != entry.size_bytes {
            return Err(invalid("transformation_output_size_changed"));
        }
        let mut offset = 0;
        let mut hash = Sha256::new();
        while offset < entry.size_bytes {
            let count = (entry.size_bytes - offset).min(buffer.len() as u64) as usize;
            let read = owner.read_at(offset, &mut buffer[..count])?;
            if read == 0 {
                return Err(invalid("transformation_output_truncated"));
            }
            hash.update(&buffer[..read]);
            offset += read as u64;
        }
        if owner.read_at(offset, &mut buffer[..1])? != 0
            || hex::encode(hash.finalize()) != entry.sha256
        {
            return Err(invalid("transformation_output_hash_changed"));
        }
        owner.recheck()?;
        files.push(OutputFile {
            entry,
            source_relative_path: artifact.relative_path.clone(),
            role: artifact.role.clone(),
            owner,
        });
    }
    Ok(files)
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use larm_adapter_host::transformation::SourceArtifact;
    use larm_domain::units::DecimalU64;

    fn d(n: u64) -> DecimalU64 {
        DecimalU64::new(n).unwrap()
    }
    struct Fixture {
        directory: tempfile::TempDir,
        root: PathBuf,
        prefix: String,
        manifest: ArtifactManifest,
        policy: TransformationOutputPolicy,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("salida 模型");
            let prefix = format!("{}-1", uuid::Uuid::now_v7());
            std::fs::create_dir_all(root.join(&prefix)).unwrap();
            let mut artifacts = Vec::new();
            for (name, data, required) in [
                ("weights.bin", vec![0x9a; (1 << 20) + 11], true),
                ("tokenizer.json", b"tokenizer".to_vec(), true),
                ("empty.bin", vec![], false),
            ] {
                let relative_path = format!("{prefix}/{name}");
                std::fs::write(root.join(&relative_path), &data).unwrap();
                artifacts.push(SourceArtifact {
                    relative_path,
                    size_bytes: Some(d(data.len() as u64)),
                    sha256: Some(hex::encode(Sha256::digest(&data))),
                    required,
                    role: "DATA".into(),
                    size_estimate: None,
                });
            }
            let mut f = Self {
                directory,
                root,
                prefix,
                manifest: ArtifactManifest {
                    revision_key: String::new(),
                    manifest_hash: String::new(),
                    artifacts,
                },
                policy: TransformationOutputPolicy {
                    worker_volume_key: "fixture".into(),
                    cas_volume_key: "fixture".into(),
                    max_total_bytes: d(2 << 20),
                    max_file_bytes: d(2 << 20),
                    max_artifacts: 3,
                    worker_control_bytes: d(0),
                    control_bytes: d(1 << 16),
                },
            };
            f.seal();
            f
        }
        fn seal(&mut self) {
            let entries = self
                .manifest
                .artifacts
                .iter()
                .map(|a| BundleEntry {
                    relative_path: a.relative_path.clone(),
                    sha256: a.sha256.clone().unwrap(),
                    size_bytes: a.size_bytes.unwrap().get(),
                    required: a.required,
                })
                .collect::<Vec<_>>();
            self.manifest.manifest_hash = bundle_hash(&entries).unwrap();
            self.manifest.revision_key = format!("content:{}", self.manifest.manifest_hash);
        }
        fn measure(&self) -> Result<Vec<OutputFile>, LarmError> {
            measure(&self.root, &self.prefix, &self.manifest, &self.policy)
        }
        // Component fixture only. The native test separately exercises the
        // qualified host reply, physical process closure and real volume IO.
        fn captured(&self) -> VerifiedTransformationOutput {
            let id = || uuid::Uuid::now_v7().to_string();
            VerifiedTransformationOutput {
                fence: TransformationAttemptFence {
                    job_id: id(),
                    claim_id: id(),
                    generation: 0,
                    attempt: 1,
                    plan_hash: "a".repeat(64),
                },
                binding: TransformationExecutionBinding {
                    adapter_installation_id: id(),
                    conformance_run_id: id(),
                    conformance_run_sha256: "b".repeat(64),
                    profile_sha256: "c".repeat(64),
                    worker_plan_id: id(),
                    worker_plan_hash: "d".repeat(64),
                },
                worker_job_id: id(),
                target_format: "GGUF".into(),
                policy: self.policy.clone(),
                manifest_hash: self.manifest.manifest_hash.clone(),
                root: PinnedDirectory::open(&self.root).unwrap(),
                files: self.measure().unwrap(),
            }
        }
    }

    #[test]
    fn every_real_file_is_hashed_and_retained_together_in_a_separate_logical_namespace() {
        let f = Fixture::new();
        let files = f.measure().unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].entry.relative_path, "weights.bin");
        assert_eq!(files[0].entry.size_bytes, (1 << 20) + 11);
        assert_eq!(files[2].entry.size_bytes, 0);
        for file in &files {
            assert!(std::fs::write(f.root.join(&file.source_relative_path), b"tampered").is_err());
            file.owner.recheck().unwrap();
        }
        assert!(std::fs::rename(&f.root, f.directory.path().join("moved")).is_err());
        drop(files);
        std::fs::write(
            f.root.join(&f.manifest.artifacts[0].relative_path),
            b"released",
        )
        .unwrap();
    }

    #[test]
    fn a_changed_or_missing_late_file_releases_earlier_pins_without_repairing_any_bytes() {
        for failure in ["hash", "short", "long", "missing"] {
            let f = Fixture::new();
            let path = f.root.join(&f.manifest.artifacts[1].relative_path);
            match failure {
                "hash" => std::fs::write(&path, b"replaced!").unwrap(),
                "short" => std::fs::write(&path, b"short").unwrap(),
                "long" => std::fs::write(&path, b"more than expected").unwrap(),
                "missing" => std::fs::remove_file(&path).unwrap(),
                _ => unreachable!(),
            }
            assert!(f.measure().is_err(), "{failure}");
            // The first output was already measured: rejection must release it.
            std::fs::write(
                f.root.join(&f.manifest.artifacts[0].relative_path),
                b"released",
            )
            .unwrap();
        }
    }

    #[test]
    fn unbounded_absent_ambiguous_or_tampered_descriptors_never_produce_output_ownership() {
        for failure in [
            "size",
            "hash",
            "digest",
            "revision",
            "duplicate",
            "optional",
            "empty",
            "file_limit",
            "total_limit",
            "count_limit",
        ] {
            let mut f = Fixture::new();
            match failure {
                "size" => f.manifest.artifacts[0].size_bytes = None,
                "hash" => f.manifest.artifacts[0].sha256 = None,
                "digest" => f.manifest.manifest_hash = "f".repeat(64),
                "revision" => f.manifest.revision_key = format!("content:{}", "e".repeat(64)),
                "duplicate" => {
                    f.manifest.artifacts[1].relative_path =
                        f.manifest.artifacts[0].relative_path.to_uppercase()
                }
                "optional" => {
                    for a in &mut f.manifest.artifacts {
                        a.required = false;
                    }
                    f.seal();
                }
                "empty" => f.manifest.artifacts.clear(),
                "file_limit" => f.policy.max_file_bytes = d(1 << 20),
                "total_limit" => {
                    f.policy.max_total_bytes = d(1 << 20);
                    f.policy.max_file_bytes = d(1 << 20);
                }
                "count_limit" => f.policy.max_artifacts = 2,
                _ => unreachable!(),
            }
            assert!(f.measure().is_err(), "{failure}");
        }
    }

    #[test]
    fn a_sibling_attempt_absolute_path_ads_or_parent_escape_is_not_an_output() {
        for suffix in [
            "../outside",
            "C:/outside",
            "/outside",
            "NUL",
            "attempt-other/weights.bin",
            "file:stream",
            "a/./b",
        ] {
            let mut f = Fixture::new();
            f.manifest.artifacts[0].relative_path = suffix.into();
            assert!(f.measure().is_err(), "{suffix}");
        }
        let mut f = Fixture::new();
        f.manifest.artifacts[0].relative_path = format!("{}-other/weights.bin", f.prefix);
        f.seal();
        assert!(f.measure().is_err());
        assert!(validate_scope(&TransformationOutputScope {
            root: PathBuf::from("relative"),
            bundle_prefix: f.prefix.clone()
        })
        .is_err());
        assert!(validate_scope(&TransformationOutputScope {
            root: f.root.clone(),
            bundle_prefix: "../escape".into()
        })
        .is_err());
    }

    #[test]
    fn an_existing_writer_prevents_a_false_immutable_output_proof() {
        let f = Fixture::new();
        let path = f.root.join(&f.manifest.artifacts[0].relative_path);
        let writer = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        assert!(f.measure().is_err());
        drop(writer);
        assert!(f.measure().is_ok());
    }

    #[test]
    fn a_junction_cannot_import_bytes_from_outside_the_attempt() {
        let mut f = Fixture::new();
        let outside = f.directory.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("empty.bin"), []).unwrap();
        let link = f.root.join(&f.prefix).join("redirect");
        let result = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        f.manifest.artifacts[2].relative_path = format!("{}/redirect/empty.bin", f.prefix);
        f.seal();
        assert!(f.measure().is_err());
        assert_eq!(
            std::fs::read(outside.join("empty.bin")).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[tokio::test]
    async fn absence_of_a_runtime_validator_remains_explicit_and_preserves_pinned_read_access() {
        use crate::output_validation::{
            TransformationOutputValidators, TransformationRuntimeValidation,
        };
        let f = Fixture::new();
        let checked = TransformationOutputValidators::default()
            .validate(f.captured())
            .await
            .unwrap();
        assert_eq!(
            checked.runtime(),
            &TransformationRuntimeValidation::NotRegistered
        );
        let mut bytes = [0; 4];
        assert_eq!(checked.output().read_at(0, 0, &mut bytes).unwrap(), 4);
        assert_eq!(bytes, [0x9a; 4]);
        assert_eq!(
            checked
                .output()
                .read_at(0, (1 << 20) + 11, &mut bytes)
                .unwrap(),
            0
        );
        assert!(checked.output().read_at(0, u64::MAX, &mut bytes).is_err());
        assert!(checked.output().read_at(99, 0, &mut bytes).is_err());
    }

    struct TestRuntimeHook {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_at: usize,
    }
    #[async_trait::async_trait]
    impl crate::output_validation::TransformationRuntimeValidator for TestRuntimeHook {
        async fn load_health_smoke(
            &self,
            output: &VerifiedTransformationOutput,
        ) -> Result<(), LarmError> {
            // Controlled hook, not an actual GGUF runtime acceptance. Exercise
            // a physical read and each possible load/health/smoke failure.
            let mut header = [0; 4];
            output.read_at(0, 0, &mut header)?;
            assert_eq!(header, [0x9a; 4]);
            for phase in 1..=3 {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if phase == self.fail_at {
                    return Err(invalid("test_runtime_phase_failed"));
                }
                tokio::task::yield_now().await;
                output.recheck()?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_registered_runtime_hook_cannot_be_bypassed_by_format_case_or_partial_success() {
        use crate::output_validation::{
            TransformationOutputValidators, TransformationRuntimeValidation,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        for fail_at in 0..=3 {
            let f = Fixture::new();
            let calls = Arc::new(AtomicUsize::new(0));
            let hook = Arc::new(TestRuntimeHook {
                calls: calls.clone(),
                fail_at,
            });
            let mut validators = TransformationOutputValidators::default();
            validators
                .register("gguf", "controlled-test-hook", "1.0.0", hook.clone())
                .unwrap();
            assert!(validators
                .register("GGUF", "replacement", "2", hook)
                .is_err());
            let result = validators.validate(f.captured()).await;
            if fail_at == 0 {
                let checked = result.unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 3);
                assert_eq!(
                    checked.runtime(),
                    &TransformationRuntimeValidation::Passed {
                        validator_id: "controlled-test-hook".into(),
                        validator_version: "1.0.0".into()
                    }
                );
            } else {
                assert!(result.is_err());
                assert_eq!(calls.load(Ordering::SeqCst), fail_at);
                // Failure yields no validation token and releases its pins.
                std::fs::write(
                    f.root.join(&f.manifest.artifacts[0].relative_path),
                    b"released",
                )
                .unwrap();
            }
        }
    }
}
