//! Read-only catalogue and fresh physical inputs for a public Core preview.
use super::*;
use larm_domain::estimate::VolumeSnapshot;
use larm_domain::ports::VolumeReservationRepository;

pub struct PlanningStorage {
    pub destination: StorageLocation,
    pub destination_path: PathBuf,
    pub workspaces: Vec<WorkspaceRoot>,
    pub volumes: Vec<VolumeSnapshot>,
    pub policy: SpacePolicy,
}

impl TransformationEnvironment {
    pub async fn planning_storage(
        &self,
        destination_id: &str,
        paths: &[PathBuf],
        reservations: &dyn VolumeReservationRepository,
    ) -> Result<PlanningStorage, LarmError> {
        if paths.is_empty() || paths.len() > 32 || paths.iter().any(|p| !p.is_absolute()) {
            return Err(unavailable("transformation_planning_roots_invalid"));
        }
        let locations = self.locations.all().await?;
        let destination = locations
            .iter()
            .find(|l| l.id == destination_id)
            .ok_or_else(|| unavailable("transformation_destination_missing"))?
            .clone();
        if destination.state != LocationState::Online {
            return Err(unavailable("transformation_destination_unavailable"));
        }
        let mut uncs = unc_roots(std::iter::once(&destination), &[]);
        uncs.extend(
            paths
                .iter()
                .filter_map(|p| p.to_str().and_then(|p| parse_unc(p).ok()).map(|p| p.root())),
        );
        uncs.sort();
        uncs.dedup();
        let observation = self.prober.probe(&uncs).await?;
        observation_time(&observation)?;
        let destination_path = resolve_location(&destination, &observation)?;
        let mut workspaces = Vec::new();
        for path in paths {
            let text = path
                .to_str()
                .ok_or_else(|| unavailable("transformation_root_encoding_invalid"))?;
            let key = if let Some((guid, _)) = guid_path(text) {
                guid.to_owned()
            } else if let Ok(unc) = parse_unc(text) {
                unc.volume_key()
            } else {
                let plain = plain_path(path);
                let kind = check_syntax(&plain)?;
                volume_for(&observation, &plain, &kind)
                    .and_then(|v| v.identity.volume_key.clone())
                    .ok_or_else(|| unavailable("transformation_volume_missing"))?
            };
            if unique_volume(&observation, &key)?.read_only {
                return Err(unavailable("transformation_allocation_read_only"));
            }
            workspaces.push(WorkspaceRoot {
                path: stable_path(&observation, path, &key)?,
                volume_key: key,
            });
        }
        if unique_volume(&observation, &destination.volume_key)?.read_only {
            return Err(unavailable("transformation_destination_read_only"));
        }
        let mut checks = workspaces
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>();
        checks.push(destination_path.clone());
        tokio::task::spawn_blocking(move || {
            for path in checks {
                PinnedDirectory::open(&path)?.check_writable()?;
            }
            Ok::<_, LarmError>(())
        })
        .await
        .map_err(|_| unavailable("transformation_root_task_failed"))??;
        let policy = self.policy.current().await?;
        let volumes =
            larm_storage::estimate::snapshots(&observation, &locations, reservations, policy, None)
                .await?;
        observation_time(&observation)?;
        Ok(PlanningStorage {
            destination,
            destination_path,
            workspaces,
            volumes,
            policy,
        })
    }
}
