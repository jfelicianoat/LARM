//! Private durable output publication. Neither a serialized record nor a
//! worker success flag supplies the physical guards required at commit.
use crate::{
    artifacts::{ArtifactLocation, ArtifactState, Ownership, VOLUME_RELATIVE_KEY},
    bridge::is_uuid_v7,
    conformance::hash,
    error::{ErrorCode, LarmError},
    locations::StorageLocation,
    model::{
        bundle_hash, required_hash, ArtifactProvenance, BundleEntry, BundleState, CanonicalModel,
        ModelArtifact, ModelBundle, ModelDistribution, ModelRevision, ModelVariant,
    },
    transformation_execution::TransformationAttemptFence,
    transformations::{
        InputArtifact, TransformationExecutionBinding, TransformationInputIdentity,
        TransformationOutputPolicy,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub fn invalid() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El registro de salida no es coherente",
        "transformation_publication_invalid",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum TransformationRuntimeValidation {
    NotRegistered,
    Passed {
        validator_id: String,
        validator_version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredTransformationArtifact {
    pub entry: BundleEntry,
    pub worker_relative_path: String,
    pub worker_role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredTransformationOutput {
    pub fence: TransformationAttemptFence,
    pub binding: TransformationExecutionBinding,
    pub worker_job_id: String,
    pub worker_manifest_hash: String,
    pub target_format: String,
    pub policy: TransformationOutputPolicy,
    pub artifacts: Vec<MeasuredTransformationArtifact>,
    pub runtime: TransformationRuntimeValidation,
}
impl MeasuredTransformationOutput {
    pub fn validate(&self) -> Result<(), LarmError> {
        self.fence.validate()?;
        self.binding.validate().map_err(|_| invalid())?;
        self.policy.validate().map_err(|_| invalid())?;
        if !is_uuid_v7(&self.worker_job_id)
            || self.target_format.trim().is_empty()
            || self.artifacts.is_empty()
            || self.artifacts.len() > self.policy.max_artifacts as usize
            || !self.artifacts.iter().any(|a| a.entry.required)
        {
            return Err(invalid());
        }
        let mut total = 0_u64;
        let mut worker = Vec::new();
        let mut prefixes = BTreeSet::new();
        for artifact in &self.artifacts {
            crate::model::bundle_path(&artifact.worker_relative_path)?;
            let suffix = format!("/{}", artifact.entry.relative_path);
            let prefix = artifact
                .worker_relative_path
                .strip_suffix(&suffix)
                .filter(|p| !p.is_empty())
                .ok_or_else(invalid)?;
            prefixes.insert(prefix);
            if artifact.worker_role.trim().is_empty()
                || artifact.entry.size_bytes > self.policy.max_file_bytes.get()
            {
                return Err(invalid());
            }
            total = total
                .checked_add(artifact.entry.size_bytes)
                .filter(|n| *n <= self.policy.max_total_bytes.get())
                .ok_or_else(invalid)?;
            let mut entry = artifact.entry.clone();
            entry.relative_path = artifact.worker_relative_path.clone();
            worker.push(entry);
        }
        bundle_hash(
            &self
                .artifacts
                .iter()
                .map(|a| a.entry.clone())
                .collect::<Vec<_>>(),
        )?;
        if prefixes.len() != 1 || bundle_hash(&worker)? != self.worker_manifest_hash {
            return Err(invalid());
        }
        if let TransformationRuntimeValidation::Passed {
            validator_id,
            validator_version,
        } = &self.runtime
        {
            if [validator_id, validator_version]
                .iter()
                .any(|s| s.trim().is_empty())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait TransformationMeasuredOutputGuard: Send + Sync {
    fn measured(&self) -> MeasuredTransformationOutput;
    async fn recheck(&self) -> Result<(), LarmError>;
}

/// Complete graph persisted before any owned staging/CAS mutation. Original
/// model/revision/distribution are snapshots for reconstruction, never inserts
/// or updates of upstream declarations during publication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationPublicationRecord {
    pub schema_version: u32,
    pub measured: MeasuredTransformationOutput,
    pub input_identity: TransformationInputIdentity,
    pub input_artifacts: Vec<InputArtifact>,
    pub destination: StorageLocation,
    pub model: CanonicalModel,
    pub distribution: ModelDistribution,
    pub revision: ModelRevision,
    pub variant: ModelVariant,
    pub artifacts: Vec<ModelArtifact>,
    pub bundle: ModelBundle,
    pub locations: Vec<ArtifactLocation>,
    pub provenance: Vec<ArtifactProvenance>,
}
impl TransformationPublicationRecord {
    pub fn digest(&self) -> Result<String, LarmError> {
        self.validate()?;
        hash(self)
    }
    pub fn validate(&self) -> Result<(), LarmError> {
        self.measured.validate()?;
        self.model.validate()?;
        self.distribution.validate()?;
        self.revision.validate()?;
        self.variant.validate()?;
        let n = self.measured.artifacts.len();
        let mut ids = BTreeSet::new();
        if self.schema_version != 1
            || self.artifacts.len() != n
            || self.locations.len() != n
            || self.provenance.len() != n
            || self.revision.id != self.input_identity.revision_id
            || self.variant.revision_id != self.revision.id
            || self.revision.distribution_id != self.distribution.id
            || self.distribution.canonical_model_id != self.model.id
            || self.destination.volume_key != self.measured.policy.cas_volume_key
            || self.bundle.variant_id != self.variant.id
            || self.bundle.state != BundleState::Verified
            || self.variant.lifecycle != "VERIFIED"
            || self.variant.availability != "UNKNOWN"
            || self.variant.ownership != Ownership::LarmManaged
            || self.variant.format.as_deref() != Some(&self.measured.target_format)
            || !self.variant.capabilities.is_empty()
            || self.variant.model_limits != crate::model::ModelLimits::default()
        {
            return Err(invalid());
        }
        for id in [
            &self.model.id,
            &self.distribution.id,
            &self.revision.id,
            &self.variant.id,
            &self.bundle.id,
            &self.destination.id,
        ] {
            if !is_uuid_v7(id) || !ids.insert(id) {
                return Err(invalid());
            }
        }
        let mut required = Vec::new();
        let mut optional = Vec::new();
        for (((artifact, location), provenance), measured) in self
            .artifacts
            .iter()
            .zip(&self.locations)
            .zip(&self.provenance)
            .zip(&self.measured.artifacts)
        {
            artifact.validate()?;
            provenance.validate()?;
            let entry = &measured.entry;
            for id in [&artifact.id, &location.id, &provenance.id] {
                if !is_uuid_v7(id) || !ids.insert(id) {
                    return Err(invalid());
                }
            }
            if artifact.variant_id != self.variant.id
                || artifact.filename != entry.relative_path
                || artifact.actual_size.map(|s| s.get()) != Some(entry.size_bytes)
                || artifact.expected_size != artifact.actual_size
                || artifact.sha256.as_ref() != Some(&entry.sha256)
                || artifact.required != entry.required
                || location.artifact_id != artifact.id
                || location.storage_location_id != self.destination.id
                || location.sha256 != artifact.sha256
                || location.file_size != artifact.actual_size
                || location.state != ArtifactState::Online
                || location.ownership != Ownership::LarmManaged
                || location.verified_at.is_none()
                || location.relative_path
                    != format!("blobs/sha256/{}/{}", &entry.sha256[..2], entry.sha256)
                || provenance.artifact_id != artifact.id
                || provenance.trust != crate::model::Trust::Declared
                || provenance.source_id.as_ref() != Some(&self.distribution.source_id)
                || provenance.revision_key.as_ref() != Some(&self.revision.revision_key)
                || provenance.parents
                    != self
                        .input_artifacts
                        .iter()
                        .map(|a| a.artifact_id.clone())
                        .collect::<Vec<_>>()
            {
                return Err(invalid());
            }
            if entry.required {
                required.push(artifact.id.clone());
            } else {
                optional.push(artifact.id.clone());
            }
        }
        let entries = self
            .measured
            .artifacts
            .iter()
            .map(|a| a.entry.clone())
            .collect::<Vec<_>>();
        if required != self.bundle.required_artifact_ids
            || optional != self.bundle.optional_artifact_ids
            || bundle_hash(&entries)? != self.bundle.manifest_hash
            || self.variant.bundle_hash.as_ref() != Some(&required_hash(&entries)?)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

pub fn same_destination(a: &StorageLocation, b: &StorageLocation) -> bool {
    a.id == b.id
        && a.canonical_path == b.canonical_path
        && a.volume_key == b.volume_key
        && a.volume_guid == b.volume_guid
        && a.unc_path == b.unc_path
        && a.metadata.get(VOLUME_RELATIVE_KEY) == b.metadata.get(VOLUME_RELATIVE_KEY)
}

#[async_trait::async_trait]
pub trait TransformationPublicationGuard: Send + Sync {
    fn fence(&self) -> &TransformationAttemptFence;
    fn record_sha256(&self) -> &str;
    async fn recheck(&self) -> Result<(), LarmError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::units::DecimalU64;
    fn measured() -> MeasuredTransformationOutput {
        let id = || uuid::Uuid::now_v7().to_string();
        let entry = BundleEntry {
            relative_path: "weights.bin".into(),
            sha256: "a".repeat(64),
            size_bytes: 8,
            required: true,
        };
        let mut worker = entry.clone();
        worker.relative_path = "owned-attempt/weights.bin".into();
        MeasuredTransformationOutput {
            fence: TransformationAttemptFence {
                job_id: id(),
                claim_id: id(),
                generation: 0,
                attempt: 1,
                plan_hash: "b".repeat(64),
            },
            binding: TransformationExecutionBinding {
                adapter_installation_id: id(),
                conformance_run_id: id(),
                conformance_run_sha256: "c".repeat(64),
                profile_sha256: "d".repeat(64),
                worker_plan_id: id(),
                worker_plan_hash: "e".repeat(64),
            },
            worker_job_id: id(),
            worker_manifest_hash: bundle_hash(&[worker]).unwrap(),
            target_format: "GGUF".into(),
            policy: TransformationOutputPolicy {
                worker_volume_key: "v".into(),
                cas_volume_key: "v".into(),
                max_total_bytes: DecimalU64::new(10).unwrap(),
                max_file_bytes: DecimalU64::new(10).unwrap(),
                max_artifacts: 2,
                worker_control_bytes: DecimalU64::new(0).unwrap(),
                control_bytes: DecimalU64::new(128).unwrap(),
            },
            artifacts: vec![MeasuredTransformationArtifact {
                entry,
                worker_relative_path: "owned-attempt/weights.bin".into(),
                worker_role: "weights".into(),
            }],
            runtime: TransformationRuntimeValidation::NotRegistered,
        }
    }
    #[test]
    fn durable_measured_snapshot_is_closed_and_preserves_exact_large_integer_representations() {
        let m = measured();
        m.validate().unwrap();
        let value = serde_json::to_value(&m).unwrap();
        assert_eq!(
            serde_json::from_value::<MeasuredTransformationOutput>(value.clone()).unwrap(),
            m
        );
        assert_eq!(value["policy"]["max_total_bytes"], "10");
        let mut extra = value;
        extra["verified"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MeasuredTransformationOutput>(extra).is_err());
    }
    #[test]
    fn mismatched_manifest_size_limits_fence_or_runtime_identity_are_rejected() {
        for case in [
            "digest", "size", "fence", "worker", "runtime", "empty", "optional", "role",
        ] {
            let mut m = measured();
            match case {
                "digest" => m.worker_manifest_hash = "f".repeat(64),
                "size" => m.artifacts[0].entry.size_bytes = 11,
                "fence" => m.fence.attempt = 0,
                "worker" => m.worker_job_id = "bad".into(),
                "runtime" => {
                    m.runtime = TransformationRuntimeValidation::Passed {
                        validator_id: String::new(),
                        validator_version: "1".into(),
                    }
                }
                "empty" => m.artifacts.clear(),
                "optional" => m.artifacts[0].entry.required = false,
                "role" => m.artifacts[0].worker_role.clear(),
                _ => unreachable!(),
            }
            assert!(m.validate().is_err(), "{case}");
        }
    }
    #[test]
    fn outputs_from_sibling_namespaces_cannot_form_one_measured_attempt() {
        let mut m = measured();
        let mut second = m.artifacts[0].clone();
        second.entry.relative_path = "config.json".into();
        second.worker_relative_path = "other-attempt/config.json".into();
        m.artifacts.push(second);
        m.policy.max_total_bytes = DecimalU64::new(20).unwrap();
        let worker = m
            .artifacts
            .iter()
            .map(|a| BundleEntry {
                relative_path: a.worker_relative_path.clone(),
                ..a.entry.clone()
            })
            .collect::<Vec<_>>();
        m.worker_manifest_hash = bundle_hash(&worker).unwrap();
        assert!(m.validate().is_err());
    }
}
