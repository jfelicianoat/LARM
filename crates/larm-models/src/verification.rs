//! Worker durable de verificación de bytes, ubicaciones y bundle (P042).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use larm_domain::artifacts::{join, normalize_relative, resolve_root, ArtifactState, ResolveError};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{bundle_hash, bundle_state, missing_roles, BundleState, Observed};
use larm_domain::model_verification::{
    LocationVerification, ModelVerificationJobs, VerificationClaim, VerificationCommit,
};
use larm_domain::ports::{
    ArtifactLocationRepository, StorageLocationRepository, StorageProber, VariantRepository,
};
use larm_domain::storage::StorageObservation;
use larm_domain::units::DecimalU64;
use sha2::Digest;

use crate::bundle;

pub struct VerificationPorts {
    pub jobs: Arc<dyn ModelVerificationJobs>,
    pub variants: Arc<dyn VariantRepository>,
    pub artifact_locations: Arc<dyn ArtifactLocationRepository>,
    pub storage_locations: Arc<dyn StorageLocationRepository>,
    pub prober: Arc<dyn StorageProber>,
}

pub struct ModelVerificationWorker {
    ports: VerificationPorts,
}

#[derive(Debug, Clone)]
struct FileObservation {
    sha256: String,
    size_bytes: u64,
}

impl ModelVerificationWorker {
    #[must_use]
    pub fn new(ports: VerificationPorts) -> Self {
        Self { ports }
    }

    /// Procesa como máximo un job. `Ok(false)` significa que la cola estaba vacía.
    pub async fn run_once(&self) -> Result<bool, LarmError> {
        let Some(claim) = self.ports.jobs.claim_verification().await? else {
            return Ok(false);
        };
        match self.process(&claim).await {
            Ok(commit) => {
                if let Err(error) = self.ports.jobs.complete_verification(&claim, &commit).await {
                    self.ports.jobs.fail_verification(&claim, &error).await?;
                    return Err(error);
                }
            }
            Err(error) => self.ports.jobs.fail_verification(&claim, &error).await?,
        }
        Ok(true)
    }

    async fn process(&self, claim: &VerificationClaim) -> Result<VerificationCommit, LarmError> {
        let variant = self
            .ports
            .variants
            .get_variant(&claim.variant_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
                    .with_resource(claim.variant_id.clone())
            })?;
        let bundle_record = self
            .ports
            .variants
            .bundle_of(&claim.variant_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::InvalidStateTransition,
                    "La variante todavía no tiene un bundle verificable",
                    "bundle_missing",
                )
                .with_resource(claim.variant_id.clone())
            })?;
        let artifacts = self.ports.variants.bundle_artifacts(&bundle_record).await?;
        let entries = bundle::entries(&artifacts).ok_or_else(|| {
            LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Faltan hashes o tamaños esperados para verificar el bundle",
                "bundle_descriptors_incomplete",
            )
            .with_resource(bundle_record.id.clone())
        })?;
        if bundle_hash(&entries)? != bundle_record.manifest_hash {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "Los descriptores actuales no coinciden con el manifiesto guardado",
                "manifest_mismatch",
            )
            .with_resource(bundle_record.id.clone()));
        }
        let variant_locations = self
            .ports
            .artifact_locations
            .for_variant(&claim.variant_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
                    .with_resource(claim.variant_id.clone())
            })?;
        let storage_locations = self.ports.storage_locations.all().await?;
        let locations_by_id: BTreeMap<_, _> = storage_locations
            .into_iter()
            .map(|l| (l.id.clone(), l))
            .collect();
        let unc_roots: Vec<String> = locations_by_id
            .values()
            .filter_map(|l| l.unc_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let observation = self.ports.prober.probe(&unc_roots).await?;
        let artifact_by_id: BTreeMap<_, _> = artifacts.iter().map(|a| (a.id.as_str(), a)).collect();
        let mut location_results = Vec::with_capacity(variant_locations.len());
        let mut observations_by_artifact: BTreeMap<
            String,
            Vec<(ArtifactState, Option<FileObservation>)>,
        > = BTreeMap::new();
        for variant_location in &variant_locations {
            let location = &variant_location.location;
            if !artifact_by_id.contains_key(location.artifact_id.as_str()) {
                continue;
            }
            let artifact = artifact_by_id
                .get(location.artifact_id.as_str())
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::Internal,
                        "La ubicación referencia un artefacto ajeno a la variante",
                        "verification_artifact_mismatch",
                    )
                })?;
            let (mut state, observed) = match locations_by_id.get(&location.storage_location_id) {
                None => (ArtifactState::Offline, None),
                Some(storage) => {
                    observe_location(storage, &observation, &location.relative_path).await?
                }
            };
            if state == ArtifactState::Online
                && observed.as_ref().is_some_and(|item| {
                    artifact.sha256.as_deref() != Some(item.sha256.as_str())
                        || artifact.expected_size.map(DecimalU64::get) != Some(item.size_bytes)
                })
            {
                state = ArtifactState::Corrupt;
            }
            let verified_at = observed
                .as_ref()
                .map(|_| larm_domain::bridge::timestamp(chrono::Utc::now()));
            location_results.push(LocationVerification {
                id: location.id.clone(),
                expected_row_version: location.row_version,
                state,
                sha256: observed.as_ref().map(|v| v.sha256.clone()),
                size_bytes: observed.as_ref().map(|v| v.size_bytes),
                verified_at,
            });
            observations_by_artifact
                .entry(artifact.id.clone())
                .or_default()
                .push((state, observed));
        }

        let mut bundle_observed = BTreeMap::<String, Observed>::new();
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();
        for (entry, artifact) in entries.iter().zip(&artifacts) {
            let checks = observations_by_artifact
                .get(&artifact.id)
                .cloned()
                .unwrap_or_default();
            let valid = checks.iter().find_map(|(state, observation)| {
                (*state == ArtifactState::Online)
                    .then_some(observation.as_ref())
                    .flatten()
                    .filter(|observed| {
                        observed.sha256 == entry.sha256 && observed.size_bytes == entry.size_bytes
                    })
            });
            if let Some(valid) = valid {
                bundle_observed.insert(
                    entry.relative_path.clone(),
                    Observed {
                        sha256: valid.sha256.clone(),
                        size_bytes: valid.size_bytes,
                    },
                );
                continue;
            }
            let bad = checks.iter().find_map(|(state, observation)| {
                (*state == ArtifactState::Corrupt)
                    .then_some(observation.as_ref())
                    .flatten()
            });
            if let Some(bad) = bad {
                bundle_observed.insert(
                    entry.relative_path.clone(),
                    Observed {
                        sha256: bad.sha256.clone(),
                        size_bytes: bad.size_bytes,
                    },
                );
                mismatched.push(entry.relative_path.clone());
            } else if entry.required {
                missing.push(entry.relative_path.clone());
            }
        }
        let missing_roles =
            missing_roles(variant.format.as_deref(), &variant.capabilities, &artifacts);
        let mut state = bundle_state(&entries, &bundle_observed);
        if !entries.iter().any(|entry| entry.required)
            || (state == BundleState::Verified
                && missing_roles
                    .as_ref()
                    .is_some_and(|roles| !roles.is_empty()))
        {
            state = BundleState::Incomplete;
        }
        missing.sort();
        mismatched.sort();
        let verified_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let location_summary: BTreeMap<&str, usize> = [
            ArtifactState::Online,
            ArtifactState::Offline,
            ArtifactState::Corrupt,
            ArtifactState::Missing,
            ArtifactState::Unknown,
        ]
        .into_iter()
        .map(|state| {
            (
                state.as_str(),
                location_results.iter().filter(|l| l.state == state).count(),
            )
        })
        .collect();
        let evidence = serde_json::json!({
            "verified_at": verified_at,
            "missing": missing,
            "mismatched": mismatched,
            "missing_roles": missing_roles,
            "location_counts": location_summary,
            "observed": bundle_observed.iter().map(|(path, item)| (path.clone(), serde_json::json!({"sha256":item.sha256,"size_bytes":item.size_bytes}))).collect::<serde_json::Map<_,_>>(),
        });
        let result = serde_json::json!({
            "variant_id": claim.variant_id,
            "bundle_id": bundle_record.id,
            "bundle_state": state.as_str(),
            "missing": missing,
            "mismatched": mismatched,
            "missing_roles": missing_roles,
            "location_counts": location_summary,
            "verified_at": verified_at,
        });
        Ok(VerificationCommit {
            variant_id: claim.variant_id.clone(),
            expected_variant_row_version: variant.row_version,
            bundle_id: bundle_record.id,
            expected_bundle_row_version: bundle_record.row_version,
            bundle_state: state,
            evidence,
            result,
            locations: location_results,
        })
    }
}

async fn observe_location(
    storage: &larm_domain::locations::StorageLocation,
    observation: &StorageObservation,
    relative_path: &str,
) -> Result<(ArtifactState, Option<FileObservation>), LarmError> {
    let storage = storage.clone();
    let observation = observation.clone();
    let relative_path = relative_path.to_owned();
    tokio::task::spawn_blocking(move || {
        observe_location_blocking(&storage, &observation, &relative_path)
    })
    .await
    .map_err(|error| {
        LarmError::new(
            ErrorCode::Internal,
            "Falló el worker de verificación de artefactos",
            "verification_worker_join",
        )
        .with_technical(error.to_string())
    })?
}

fn observe_location_blocking(
    storage: &larm_domain::locations::StorageLocation,
    observation: &StorageObservation,
    relative_path: &str,
) -> Result<(ArtifactState, Option<FileObservation>), LarmError> {
    let relative = normalize_relative(relative_path)?;
    let root = match resolve_root(storage, &observation.volumes) {
        Ok(root) => root,
        Err(ResolveError::VolumeOffline) => return Ok((ArtifactState::Offline, None)),
    };
    let root_path = PathBuf::from(&root);
    let file_path = PathBuf::from(join(&root, &relative));
    let canonical_root = match std::fs::canonicalize(&root_path) {
        Ok(root) => root,
        Err(_) => return Ok((ArtifactState::Offline, None)),
    };
    let canonical_file = match std::fs::canonicalize(&file_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ArtifactState::Missing, None))
        }
        Err(_) => return Ok((ArtifactState::Offline, None)),
    };
    if !canonical_file.starts_with(&canonical_root) {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "La ruta del artefacto escapa de la ubicación registrada",
            "artifact_path_escape",
        ));
    }
    let observed = hash_file(&canonical_file)?;
    Ok((ArtifactState::Online, Some(observed)))
}

fn hash_file(path: &Path) -> Result<FileObservation, LarmError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo leer un artefacto",
            "artifact_read_failed",
        )
        .retryable(true)
        .with_technical(error.to_string())
    })?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 8 << 20];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo leer un artefacto",
                "artifact_read_failed",
            )
            .retryable(true)
            .with_technical(error.to_string())
        })?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or_else(|| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "El tamaño observado no es representable",
                "artifact_size_overflow",
            )
        })?;
        hasher.update(&buffer[..read]);
    }
    Ok(FileObservation {
        sha256: hex::encode(hasher.finalize()),
        size_bytes: total,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn hashes_real_bytes_and_size() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"larm verification").unwrap();
        let observed = hash_file(temp.path()).unwrap();
        assert_eq!(
            observed.sha256,
            hex::encode(sha2::Sha256::digest(b"larm verification"))
        );
        assert_eq!(observed.size_bytes, 17);
    }

    #[test]
    fn rejects_paths_outside_storage_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = root.path().join("escape");
        #[cfg(windows)]
        let symlink = std::os::windows::fs::symlink_file(outside.path(), &link);
        #[cfg(unix)]
        let symlink = std::os::unix::fs::symlink(outside.path(), &link);
        if symlink.is_err() {
            assert!(normalize_relative("../outside.bin").is_err());
            return;
        }
        let storage = larm_domain::locations::StorageLocation {
            id: "loc".into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: "root".into(),
            root_path: root.path().display().to_string(),
            canonical_path: root.path().display().to_string(),
            volume_guid: None,
            drive_letter: None,
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::new(1).unwrap(),
            free_bytes: DecimalU64::new(1).unwrap(),
            reserved_bytes: DecimalU64::new(0).unwrap(),
            priority: 0,
            auto_placement: false,
            state: larm_domain::locations::LocationState::Online,
            last_seen_at: None,
            volume_key: "test".into(),
            performance_class: larm_domain::storage::PerformanceClass::Unknown,
        };
        let observation = StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: String::new(),
        };
        let result = observe_location_blocking(&storage, &observation, "escape");
        assert!(result.is_err());
    }
}
