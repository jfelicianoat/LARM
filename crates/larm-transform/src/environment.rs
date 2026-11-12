//! Production catalogue/physical-storage inputs for P093. These observations
//! grant no adapter qualification and do not create or execute a job.
use crate::input::{InputCandidates, VerifiedTransformationInput};
use larm_adapter_host::input_files::PinnedDirectory;
use larm_db::{locations::SqlStorageLocations, transformation_inputs::SqlTransformationInputs};
use larm_domain::{
    artifacts::{
        below_mount, normalize_relative, resolve_root, ArtifactState, VOLUME_RELATIVE_KEY,
    },
    error::{ErrorCode, LarmError},
    estimate::{margin, StorageEstimate},
    locations::{LocationState, StorageLocation},
    ports::{StorageLocationRepository, StorageProber, VolumeDemand},
    storage::{parse_unc, volume_guid_from_path, StorageObservation, StorageVolume},
    transformations::{
        verify_observed_input_hashes, InputArtifact, SealedTransformationPlan,
        TransformationInputGuard, TransformationInputIdentity, TransformationInputLocation,
        TransformationSpaceObserver, TransformationSpaceSnapshot,
    },
};
use larm_storage::{
    estimate::{policy_minimum, SpacePolicy},
    locations::{check_syntax, plain_path, volume_for, RootKind},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
mod planning;
pub use planning::PlanningStorage;

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "El almacenamiento de la transformación no está disponible",
        reason,
    )
}
fn stale(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "El almacenamiento cambió después de preparar el plan",
        reason,
    )
}
fn same_root(a: &StorageLocation, b: &StorageLocation) -> bool {
    a.id == b.id
        && a.canonical_path == b.canonical_path
        && a.volume_key == b.volume_key
        && a.volume_guid == b.volume_guid
        && a.unc_path == b.unc_path
        && a.metadata.get(VOLUME_RELATIVE_KEY) == b.metadata.get(VOLUME_RELATIVE_KEY)
}
fn readable(root: &StorageLocation) -> bool {
    matches!(
        root.state,
        LocationState::Online
            | LocationState::ReadOnly
            | LocationState::LowSpace
            | LocationState::Degraded
    )
}
pub(crate) fn observation_time(
    observation: &StorageObservation,
) -> Result<chrono::DateTime<chrono::Utc>, LarmError> {
    let time = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
        .map_err(|_| unavailable("transformation_storage_timestamp_invalid"))?
        .with_timezone(&chrono::Utc);
    if !(0..5_000).contains(&(chrono::Utc::now() - time).num_milliseconds()) {
        return Err(unavailable("transformation_storage_observation_stale").retryable(true));
    }
    Ok(time)
}
fn unique_volume<'a>(
    observation: &'a StorageObservation,
    key: &str,
) -> Result<&'a StorageVolume, LarmError> {
    let mut choices = observation
        .volumes
        .iter()
        .filter(|v| v.identity.volume_key.as_deref() == Some(key));
    let volume = choices
        .next()
        .ok_or_else(|| unavailable("transformation_volume_missing"))?;
    if choices.next().is_some() || !volume.online || key.is_empty() || key.starts_with("unknown:") {
        return Err(unavailable("transformation_volume_unavailable"));
    }
    Ok(volume)
}
fn guid_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(r"\\?\Volume{")?;
    rest.split_once("}\\")
}
fn validate_path_volume(
    observation: &StorageObservation,
    path: &Path,
    key: &str,
) -> Result<(), LarmError> {
    let volume = unique_volume(observation, key)?;
    let text = path
        .to_str()
        .ok_or_else(|| unavailable("transformation_root_encoding_invalid"))?;
    if let Some((guid, inside)) = guid_path(text) {
        if guid != key
            || volume.identity.volume_guid.as_deref() != Some(key)
            || volume_guid_from_path(&format!(r"\\?\Volume{{{key}}}\")).as_deref() != Some(key)
        {
            return Err(unavailable("transformation_root_volume_mismatch"));
        }
        if !inside.is_empty() {
            normalize_relative(inside)?;
        }
        return Ok(());
    }
    let plain = plain_path(path);
    let kind = check_syntax(&plain)?;
    match &kind {
        RootKind::Local => {
            if volume.identity.volume_guid.as_deref() != Some(key)
                || volume_guid_from_path(&format!(r"\\?\Volume{{{key}}}\")).as_deref() != Some(key)
                || volume_for(observation, &plain, &kind)
                    .and_then(|v| v.identity.volume_key.as_deref())
                    != Some(key)
            {
                return Err(unavailable("transformation_root_volume_mismatch"));
            }
        }
        RootKind::Unc(unc) => {
            if unc.volume_key() != key
                || volume.identity.volume_guid.is_some()
                || volume
                    .identity
                    .unc_path
                    .as_deref()
                    .and_then(|p| parse_unc(p).ok())
                    .map(|p| p.volume_key())
                    .as_deref()
                    != Some(key)
            {
                return Err(unavailable("transformation_root_volume_mismatch"));
            }
        }
    }
    Ok(())
}
pub(crate) fn resolve_location(
    root: &StorageLocation,
    observation: &StorageObservation,
) -> Result<PathBuf, LarmError> {
    if !readable(root) {
        return Err(unavailable("transformation_root_unavailable"));
    }
    if let Some(value) = root.metadata.get(VOLUME_RELATIVE_KEY) {
        let inside = value
            .as_str()
            .ok_or_else(|| unavailable("transformation_root_relative_invalid"))?;
        if !inside.is_empty() {
            normalize_relative(inside)?;
        }
    }
    let volume = unique_volume(observation, &root.volume_key)?;
    if root.volume_guid.as_deref() != volume.identity.volume_guid.as_deref()
        || root
            .unc_path
            .as_deref()
            .and_then(|p| parse_unc(p).ok())
            .map(|p| p.volume_key())
            != volume
                .identity
                .unc_path
                .as_deref()
                .and_then(|p| parse_unc(p).ok())
                .map(|p| p.volume_key())
    {
        return Err(unavailable("transformation_root_identity_mismatch"));
    }
    let path = if guid_path(&root.canonical_path).is_some() {
        root.canonical_path.clone()
    } else {
        resolve_root(root, &observation.volumes)
            .map_err(|_| unavailable("transformation_root_offline"))?
    };
    let path = PathBuf::from(path);
    stable_path(observation, &path, &root.volume_key)
}

/// Address local IO by Volume GUID, so a drive-letter reassignment cannot
/// redirect a create/write probe between an observation and the actual open.
pub(crate) fn stable_path(
    observation: &StorageObservation,
    path: &Path,
    key: &str,
) -> Result<PathBuf, LarmError> {
    validate_path_volume(observation, path, key)?;
    let text = path
        .to_str()
        .ok_or_else(|| unavailable("transformation_root_encoding_invalid"))?;
    if guid_path(text).is_some() || parse_unc(text).is_ok() {
        return Ok(path.into());
    }
    let plain = plain_path(path);
    let volume = unique_volume(observation, key)?;
    let inside = volume
        .identity
        .mount_points
        .iter()
        .filter_map(|mount| below_mount(mount, &plain).map(|inside| (mount.len(), inside)))
        .max_by_key(|(length, _)| *length)
        .map(|(_, inside)| inside)
        .ok_or_else(|| unavailable("transformation_root_volume_mismatch"))?;
    if !inside.is_empty() {
        normalize_relative(&inside)?;
    }
    Ok(PathBuf::from(format!(r"\\?\Volume{{{key}}}\{inside}")))
}
fn unc_roots<'a>(
    roots: impl Iterator<Item = &'a StorageLocation>,
    extras: &[WorkspaceRoot],
) -> Vec<String> {
    roots
        .filter_map(|r| {
            r.unc_path
                .as_deref()
                .and_then(|p| parse_unc(p).ok())
                .map(|p| p.root())
        })
        .chain(extras.iter().filter_map(|r| {
            r.path
                .to_str()
                .and_then(|p| parse_unc(p).ok())
                .map(|p| p.root())
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Current settings are supplied by the composition. A preview's old minimum
/// or margin is not authority for a later admission.
#[async_trait::async_trait]
pub trait TransformationSpacePolicy: Send + Sync {
    async fn current(&self) -> Result<SpacePolicy, LarmError>;
}

pub struct TransformationEnvironment {
    pub catalogue: SqlTransformationInputs,
    pub locations: SqlStorageLocations,
    pub prober: Arc<dyn StorageProber>,
    pub policy: Arc<dyn TransformationSpacePolicy>,
}

pub struct ResolvedTransformationInput {
    input: VerifiedTransformationInput,
    roots: Vec<(StorageLocation, PathBuf)>,
    locations: SqlStorageLocations,
    prober: Arc<dyn StorageProber>,
}
#[async_trait::async_trait]
impl TransformationInputGuard for ResolvedTransformationInput {
    fn identity(&self) -> &TransformationInputIdentity {
        self.input.identity()
    }
    fn artifacts(&self) -> &[InputArtifact] {
        self.input.artifacts()
    }
    fn locations(&self) -> &[TransformationInputLocation] {
        self.input.locations()
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.input.recheck().await?;
        let current = self.locations.all().await?;
        let observation = self
            .prober
            .probe(&unc_roots(self.roots.iter().map(|(r, _)| r), &[]))
            .await?;
        observation_time(&observation)?;
        for (expected, path) in &self.roots {
            let now = current
                .iter()
                .find(|r| r.id == expected.id)
                .ok_or_else(|| stale("transformation_input_root_missing"))?;
            if !same_root(expected, now) || resolve_location(now, &observation)? != *path {
                return Err(stale("transformation_input_root_changed"));
            }
        }
        self.input.recheck().await
    }
}

impl TransformationEnvironment {
    /// Planning selects required members and usable installed optionals. For
    /// execution pass the sealed artifact snapshot to preserve that selection.
    pub async fn input(
        &self,
        variant_id: &str,
        expected: Option<&[InputArtifact]>,
    ) -> Result<ResolvedTransformationInput, LarmError> {
        let catalogue = self.catalogue.load(variant_id).await?;
        let used_roots = catalogue
            .copies
            .iter()
            .filter(|c| {
                c.state == ArtifactState::Online
                    && expected
                        .is_none_or(|items| items.iter().any(|a| a.artifact_id == c.artifact_id))
            })
            .map(|c| c.storage_location_id.as_str())
            .collect::<BTreeSet<_>>();
        let observation = self
            .prober
            .probe(&unc_roots(
                catalogue
                    .storage_locations
                    .iter()
                    .filter(|r| used_roots.contains(r.id.as_str()) && readable(r)),
                &[],
            ))
            .await?;
        observation_time(&observation)?;
        let roots: BTreeMap<_, _> = catalogue
            .storage_locations
            .iter()
            .map(|r| (r.id.as_str(), r))
            .collect();
        let expected_ids = expected.map(|a| {
            a.iter()
                .map(|a| a.artifact_id.as_str())
                .collect::<BTreeSet<_>>()
        });
        if expected_ids.as_ref().is_some_and(|s| {
            expected.is_some_and(|a| s.len() != a.len())
                || !catalogue
                    .bundle
                    .required_artifact_ids
                    .iter()
                    .all(|id| s.contains(id.as_str()))
                || !s
                    .iter()
                    .all(|id| catalogue.artifacts.iter().any(|a| a.artifact_id == *id))
        }) {
            return Err(stale("transformation_input_selection_changed"));
        }
        let mut selected = Vec::new();
        let mut candidates = InputCandidates::new();
        let mut resolved_roots = BTreeMap::new();
        for artifact in &catalogue.artifacts {
            let required = catalogue
                .bundle
                .required_artifact_ids
                .contains(&artifact.artifact_id);
            if expected_ids
                .as_ref()
                .is_some_and(|s| !s.contains(artifact.artifact_id.as_str()))
            {
                continue;
            }
            let mut copies = catalogue
                .copies
                .iter()
                .filter(|c| {
                    c.artifact_id == artifact.artifact_id
                        && c.state == ArtifactState::Online
                        && c.sha256.as_deref() == Some(artifact.sha256.as_str())
                        && c.file_size == Some(artifact.size_bytes)
                })
                .collect::<Vec<_>>();
            copies.sort_by_key(|c| {
                (
                    roots
                        .get(c.storage_location_id.as_str())
                        .map(|r| r.priority)
                        .unwrap_or(u32::MAX),
                    &c.storage_location_id,
                    &c.id,
                )
            });
            let mut choices = Vec::new();
            for copy in copies {
                let Some(root) = roots.get(copy.storage_location_id.as_str()) else {
                    continue;
                };
                let Ok(path) = resolve_location(root, &observation) else {
                    continue;
                };
                resolved_roots.insert(root.id.clone(), ((*root).clone(), path.clone()));
                choices.push((
                    TransformationInputLocation {
                        artifact_id: artifact.artifact_id.clone(),
                        artifact_location_id: copy.id.clone(),
                        storage_location_id: root.id.clone(),
                        canonical_storage_path: root.canonical_path.clone(),
                        relative_path: copy.relative_path.clone(),
                        volume_key: root.volume_key.clone(),
                    },
                    path,
                ));
            }
            if choices.is_empty() {
                if required || expected.is_some() {
                    return Err(unavailable("transformation_input_copy_unavailable")
                        .with_resource(artifact.artifact_id.clone()));
                }
                continue;
            }
            selected.push(artifact.clone());
            candidates.insert(artifact.artifact_id.clone(), choices);
        }
        if let Some(expected) = expected {
            verify_observed_input_hashes(expected, &selected)
                .map_err(|_| stale("transformation_input_snapshot_changed"))?;
        }
        let input = VerifiedTransformationInput::acquire_candidates(
            catalogue.identity,
            selected,
            candidates,
        )
        .await?;
        let used = input
            .locations()
            .iter()
            .map(|r| r.storage_location_id.as_str())
            .collect::<BTreeSet<_>>();
        let roots = resolved_roots
            .into_iter()
            .filter(|(id, _)| used.contains(id.as_str()))
            .map(|(_, value)| value)
            .collect();
        let result = ResolvedTransformationInput {
            input,
            roots,
            locations: self.locations.clone(),
            prober: self.prober.clone(),
        };
        result.recheck().await?;
        Ok(result)
    }
}

/// Derived from the qualified backend profile by the composition, never from
/// ExecutePlan. Includes tool workspaces on volumes outside the destination.
#[derive(Clone)]
pub struct WorkspaceRoot {
    pub path: PathBuf,
    pub volume_key: String,
}

struct AllocationRoot {
    location: Option<StorageLocation>,
    workspace: WorkspaceRoot,
    pin: PinnedDirectory,
}
pub struct LiveTransformationSpace {
    estimate: StorageEstimate,
    roots: Vec<AllocationRoot>,
    locations: SqlStorageLocations,
    prober: Arc<dyn StorageProber>,
    policy: Arc<dyn TransformationSpacePolicy>,
}

impl TransformationEnvironment {
    pub async fn space(
        &self,
        plan: &SealedTransformationPlan,
        extras: Vec<WorkspaceRoot>,
    ) -> Result<LiveTransformationSpace, LarmError> {
        plan.verify_seal()
            .map_err(|_| stale("transformation_plan_seal_invalid"))?;
        let locations = self.locations.all().await?;
        let target = locations
            .iter()
            .find(|r| r.id == plan.request.storage_location_id)
            .ok_or_else(|| unavailable("transformation_destination_missing"))?;
        if target.state != LocationState::Online {
            return Err(unavailable("transformation_destination_unavailable"));
        }
        let observation = self
            .prober
            .probe(&unc_roots(std::iter::once(target), &extras))
            .await?;
        observation_time(&observation)?;
        let target_path = resolve_location(target, &observation)?;
        let mut definitions = vec![(
            Some(target.clone()),
            WorkspaceRoot {
                path: target_path,
                volume_key: target.volume_key.clone(),
            },
        )];
        definitions.extend(extras.into_iter().map(|r| (None, r)));
        let keys = definitions
            .iter()
            .map(|(_, r)| r.volume_key.as_str())
            .collect::<BTreeSet<_>>();
        let allocated = plan
            .response
            .storage_estimate
            .per_volume
            .iter()
            .map(|v| v.volume_key.as_str())
            .collect::<BTreeSet<_>>();
        if keys != allocated
            || allocated.len() != plan.response.storage_estimate.per_volume.len()
            || allocated.is_empty()
        {
            return Err(stale("transformation_allocation_roots_mismatch"));
        }
        for (_, root) in &mut definitions {
            root.path = stable_path(&observation, &root.path, &root.volume_key)?;
            if unique_volume(&observation, &root.volume_key)?.read_only {
                return Err(unavailable("transformation_allocation_read_only"));
            }
        }
        let roots = tokio::task::spawn_blocking(move || {
            definitions
                .into_iter()
                .map(|(location, workspace)| {
                    let pin = PinnedDirectory::open(&workspace.path)?;
                    pin.check_writable()?;
                    Ok(AllocationRoot {
                        location,
                        workspace,
                        pin,
                    })
                })
                .collect::<Result<Vec<_>, LarmError>>()
        })
        .await
        .map_err(|_| unavailable("transformation_root_task_failed"))??;
        Ok(LiveTransformationSpace {
            estimate: plan.response.storage_estimate.clone(),
            roots,
            locations: self.locations.clone(),
            prober: self.prober.clone(),
            policy: self.policy.clone(),
        })
    }
}
#[async_trait::async_trait]
impl TransformationSpaceObserver for LiveTransformationSpace {
    async fn observe(&self) -> Result<TransformationSpaceSnapshot, LarmError> {
        let policy = self.policy.current().await?;
        let current = self.locations.all().await?;
        let pins = self.roots.iter().map(|r| r.pin.clone()).collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            for pin in pins {
                pin.check_writable()?;
            }
            Ok::<_, LarmError>(())
        })
        .await
        .map_err(|_| unavailable("transformation_root_task_failed"))??;
        let extras = self
            .roots
            .iter()
            .map(|r| r.workspace.clone())
            .collect::<Vec<_>>();
        let observation = self
            .prober
            .probe(&unc_roots(std::iter::empty(), &extras))
            .await?;
        let observed_at = observation_time(&observation)?;
        for root in &self.roots {
            root.pin.recheck()?;
            validate_path_volume(
                &observation,
                &root.pin.canonical_path()?,
                &root.workspace.volume_key,
            )?;
            let volume = unique_volume(&observation, &root.workspace.volume_key)?;
            if volume.read_only {
                return Err(unavailable("transformation_allocation_read_only"));
            }
            if let Some(expected) = &root.location {
                let now = current
                    .iter()
                    .find(|r| r.id == expected.id)
                    .ok_or_else(|| stale("transformation_destination_missing"))?;
                if now.state != LocationState::Online
                    || !same_root(expected, now)
                    || resolve_location(now, &observation)? != root.workspace.path
                    || volume
                        .filesystem
                        .as_deref()
                        .is_none_or(|f| !f.eq_ignore_ascii_case(&now.filesystem))
                {
                    return Err(stale("transformation_destination_changed"));
                }
            }
        }
        let mut demands = Vec::new();
        for allocation in &self.estimate.per_volume {
            let volume = unique_volume(&observation, &allocation.volume_key)?;
            let free = volume
                .free_bytes
                .ok_or_else(|| unavailable("transformation_volume_free_unknown"))?;
            let m = margin(allocation.peak_extra_bytes.get(), policy.margin)
                .map_err(|_| stale("transformation_margin_overflow"))?;
            if m != allocation.margin_bytes.get() {
                return Err(stale("transformation_margin_policy_changed"));
            }
            let required = allocation
                .peak_extra_bytes
                .get()
                .checked_add(m)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| stale("transformation_size_overflow"))?;
            demands.push(VolumeDemand {
                volume_key: allocation.volume_key.clone(),
                required_bytes: required,
                os_free: free.get(),
                policy_minimum: policy_minimum(
                    &allocation.volume_key,
                    &current,
                    policy.minimum_free_bytes,
                ),
            });
        }
        Ok(TransformationSpaceSnapshot {
            observed_at,
            demands,
            margin_policy: policy.margin,
            minimum_free_bytes: policy.minimum_free_bytes,
        })
    }
}
