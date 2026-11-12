//! P028: publicación side-by-side de una instalación inicial.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{Trust, TrustStore};
use larm_domain::adapter_updates::{AdapterImportJournal, AdapterImportPlan, UpdateStep};
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::timestamp;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterImportRepository, AdapterPackageSpace, ResourceGuard, VolumeReservationRepository,
};
use larm_domain::units::DecimalU64;

use crate::package::{
    check_managed_path, component, lock_version, verify_published, ArchivePlan, PackageControl,
    StagedPackage,
};

const GENERATION: i64 = 1;

#[derive(Clone)]
pub struct ImportPorts {
    pub jobs: Arc<dyn AdapterImportRepository>,
    pub reservations: Arc<dyn VolumeReservationRepository>,
    pub guard: Arc<dyn ResourceGuard>,
    pub space: Arc<dyn AdapterPackageSpace>,
}

pub struct ImportCoordinator {
    ports: ImportPorts,
    root: PathBuf,
    trust: TrustStore,
    serial: Mutex<()>,
}

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo completar la importación del adaptador",
        reason,
    )
}
fn io(e: impl std::fmt::Display) -> LarmError {
    error(ErrorCode::ResourceUnavailable, "adapter_import_io_failed").with_technical(e.to_string())
}
fn decimal(n: u64) -> Result<DecimalU64, LarmError> {
    DecimalU64::new(n).ok_or_else(|| error(ErrorCode::ValidationFailed, "size_overflow"))
}

struct CancelOnDrop(PackageControl);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl ImportCoordinator {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn open(
        root: PathBuf,
        ports: ImportPorts,
        trust: TrustStore,
    ) -> Result<Arc<Self>, LarmError> {
        check_managed_path(&root)?;
        std::fs::create_dir_all(&root).map_err(io)?;
        let root = std::fs::canonicalize(root).map_err(io)?;
        check_managed_path(&root)?;
        Ok(Arc::new(Self {
            ports,
            root,
            trust,
            serial: Mutex::new(()),
        }))
    }

    fn workspace(&self, id: &str) -> Result<PathBuf, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(id) {
            return Err(error(ErrorCode::ValidationFailed, "job_id_invalid"));
        }
        Ok(self.root.join(".staging").join(id))
    }

    fn manifest(&self, plan: &AdapterImportPlan) -> Result<AdapterManifest, LarmError> {
        let manifest = AdapterManifest::parse(&plan.candidate.manifest.to_string())?;
        if plan.candidate.adapter_id != manifest.id
            || plan.candidate.version != manifest.version
            || plan.candidate.family != manifest.family.as_str()
            || plan.candidate.package_hash != manifest.package_sha256
            || plan.candidate.publisher != manifest.publisher
            || plan.candidate.row_version != 1
            || plan.candidate.state != AdapterState::Installed
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "adapter_import_candidate_manifest_mismatch",
            ));
        }
        Ok(manifest)
    }

    fn paths(
        &self,
        record: &AdapterImportJournal,
        plan: &AdapterImportPlan,
    ) -> Result<(PathBuf, PathBuf), LarmError> {
        let workspace = self.workspace(&record.job_id)?;
        let manifest = self.manifest(plan)?;
        component(&manifest.id)?;
        component(&manifest.version)?;
        let destination = self.root.join(&manifest.id).join(&manifest.version);
        if Path::new(&plan.workspace_path) != workspace
            || Path::new(&plan.destination_path) != destination
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "adapter_import_paths_changed",
            ));
        }
        check_managed_path(&workspace)?;
        check_managed_path(&destination)?;
        Ok((workspace, destination))
    }

    fn fence(
        &self,
        runtime: &tokio::runtime::Handle,
        id: &str,
        plan: &AdapterImportPlan,
    ) -> Result<(), LarmError> {
        for (resource, mode) in plan.locks() {
            if !runtime.block_on(self.ports.guard.holds(id, GENERATION, &resource, mode))? {
                return Err(error(ErrorCode::Conflict, "fence_lost").with_resource(id));
            }
        }
        Ok(())
    }

    fn control(
        &self,
        runtime: &tokio::runtime::Handle,
        record: &AdapterImportJournal,
        plan: &AdapterImportPlan,
        base: PackageControl,
    ) -> PackageControl {
        let (guard, guard_runtime, guard_id, guard_plan) = (
            self.ports.guard.clone(),
            runtime.clone(),
            record.job_id.clone(),
            plan.clone(),
        );
        let (reservations, writer_runtime, writer_id, volume) = (
            self.ports.reservations.clone(),
            runtime.clone(),
            record.job_id.clone(),
            plan.volume_key.clone(),
        );
        let live = AtomicU64::new(0);
        base.with_io_hooks(
            Arc::new(move || {
                for (resource, mode) in guard_plan.locks() {
                    if !guard_runtime
                        .block_on(guard.holds(&guard_id, GENERATION, &resource, mode))?
                    {
                        return Err(
                            error(ErrorCode::Conflict, "fence_lost").with_resource(&guard_id)
                        );
                    }
                }
                Ok(())
            }),
            Arc::new(move |n| {
                let count = live
                    .fetch_add(n, Ordering::AcqRel)
                    .checked_add(n)
                    .ok_or_else(|| error(ErrorCode::ValidationFailed, "size_overflow"))?;
                writer_runtime.block_on(reservations.consume(&writer_id, &volume, count))
            }),
        )
    }

    fn clean_workspace(&self, record: &AdapterImportJournal) -> Result<(), LarmError> {
        let workspace = self.workspace(&record.job_id)?;
        if record
            .plan
            .as_ref()
            .is_some_and(|p| Path::new(&p.workspace_path) != workspace)
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "adapter_import_paths_changed",
            ));
        }
        check_managed_path(&workspace)?;
        if workspace.exists() {
            std::fs::remove_dir_all(workspace).map_err(io)?;
        }
        Ok(())
    }

    pub async fn execute(
        self: Arc<Self>,
        id: String,
        control: PackageControl,
    ) -> Result<(), LarmError> {
        let _cancel = CancelOnDrop(control.clone());
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _serial = self.serial.lock().map_err(|_| {
                error(
                    ErrorCode::ResourceUnavailable,
                    "adapter_import_coordinator_poisoned",
                )
            })?;
            self.execute_locked(&runtime, &id, control)
        })
        .await
        .map_err(|e| {
            error(ErrorCode::Internal, "adapter_import_worker_failed").with_technical(e.to_string())
        })?
    }

    fn execute_locked(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        id: &str,
        control: PackageControl,
    ) -> Result<(), LarmError> {
        let record = runtime
            .block_on(self.ports.jobs.import_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        if matches!(record.step, UpdateStep::Completed | UpdateStep::Failed) {
            return Ok(());
        }
        if record.step != UpdateStep::Queued {
            return Err(error(
                ErrorCode::InvalidStateTransition,
                "adapter_import_requires_recovery",
            ));
        }
        runtime.block_on(self.ports.jobs.advance_import(
            id,
            UpdateStep::Queued,
            UpdateStep::Inspecting,
        ))?;
        let mut published = false;
        let result = self.install(runtime, id, &control, &mut published);
        if let Err(ref cause) = result {
            let latest = runtime
                .block_on(self.ports.jobs.import_job(id))?
                .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
            if latest.step == UpdateStep::Completed {
                return Ok(());
            }
            if !published {
                if let Some(plan) = &latest.plan {
                    self.fence(runtime, id, plan)?;
                }
                self.clean_workspace(&latest)?;
                runtime.block_on(self.ports.jobs.fail_import(id, cause))?;
            }
        }
        result
    }

    fn install(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        id: &str,
        base: &PackageControl,
        published_bytes: &mut bool,
    ) -> Result<(), LarmError> {
        base.check()?;
        let record = runtime
            .block_on(self.ports.jobs.import_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        let archive = ArchivePlan::inspect(
            Path::new(&record.request.package_path),
            &record.request.expected_hash,
            base,
        )?;
        let assessment = self.trust.assess(&archive.manifest, false)?;
        if assessment.trust == Trust::Revoked {
            return Err(error(ErrorCode::Forbidden, "revoked"));
        }
        let now = timestamp(chrono::Utc::now());
        let candidate = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::json!({
                "signature_state":assessment.signature_state,
                "import_job_id":id
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
            adapter_id: archive.manifest.id.clone(),
            version: archive.manifest.version.clone(),
            family: archive.manifest.family.as_str().into(),
            package_hash: archive.manifest.package_sha256.clone(),
            publisher: archive.manifest.publisher.clone(),
            trust: match assessment.trust {
                Trust::Untrusted => AdapterTrust::Untrusted,
                Trust::Verified => AdapterTrust::Verified,
                Trust::Revoked => AdapterTrust::Revoked,
            },
            manifest: serde_json::to_value(&archive.manifest).map_err(io)?,
            state: AdapterState::Installed,
        };
        let workspace = self.workspace(id)?;
        let destination = self
            .root
            .join(&archive.manifest.id)
            .join(&archive.manifest.version);
        check_managed_path(&destination)?;
        if destination.exists() {
            return Err(error(ErrorCode::Conflict, "adapter_version_exists"));
        }
        let mut plan = AdapterImportPlan {
            candidate,
            workspace_path: workspace.to_string_lossy().into_owned(),
            destination_path: destination.to_string_lossy().into_owned(),
            volume_key: String::new(),
            archive_bytes: decimal(archive.archive_bytes)?,
            unpacked_bytes: decimal(archive.unpacked_bytes)?,
            required_bytes: decimal(archive.required_bytes())?,
            approval_id: record.request.approval_id.clone(),
        };
        let (demand, observed) = runtime.block_on(
            self.ports
                .space
                .measure_package_space(&self.root.to_string_lossy(), plan.required_bytes.get()),
        )?;
        base.check()?;
        plan.volume_key = demand.volume_key.clone();
        plan.validate()?;
        runtime.block_on(self.ports.jobs.prepare_import(id, &plan, &demand, observed))?;
        let record = runtime
            .block_on(self.ports.jobs.import_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        let saved_plan = record.plan.as_ref().ok_or_else(|| {
            error(
                ErrorCode::InvalidStateTransition,
                "adapter_import_plan_missing",
            )
        })?;
        let control = self.control(runtime, &record, saved_plan, base.clone());
        let (workspace, _) = self.paths(&record, saved_plan)?;
        control.check()?;
        if workspace.exists() {
            return Err(error(ErrorCode::Conflict, "adapter_workspace_exists"));
        }
        std::fs::create_dir_all(&workspace).map_err(io)?;
        runtime.block_on(self.ports.jobs.advance_import(
            id,
            UpdateStep::Reserved,
            UpdateStep::Extracting,
        ))?;
        let stage = StagedPackage::extract(archive, &workspace, &self.trust, &control)?;
        runtime.block_on(self.ports.jobs.advance_import(
            id,
            UpdateStep::Extracting,
            UpdateStep::Staged,
        ))?;
        control.check()?;
        runtime.block_on(self.ports.jobs.advance_import(
            id,
            UpdateStep::Staged,
            UpdateStep::Publishing,
        ))?;
        let mut published = stage.publish(&self.root, &control)?;
        published.retain_for_recovery();
        *published_bytes = true;
        let committing = self.control(
            runtime,
            &record,
            saved_plan,
            PackageControl::with_timeout(Duration::from_secs(120)),
        );
        verify_published(&published.destination, published.manifest(), &committing)?;
        runtime.block_on(self.ports.jobs.complete_import(id))?;
        published.commit();
        self.clean_workspace(&record)
    }

    /// Recupera trabajos activos antes de abrir el listener. Los QUEUED se
    /// reservan al supervisor para evitar bloquear el arranque por la cola.
    pub async fn recover_started(self: Arc<Self>) -> Result<usize, LarmError> {
        self.recover_mode(false).await
    }

    pub async fn recover(self: Arc<Self>) -> Result<usize, LarmError> {
        self.recover_mode(true).await
    }

    async fn recover_mode(self: Arc<Self>, include_queued: bool) -> Result<usize, LarmError> {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _serial = self.serial.lock().map_err(|_| {
                error(
                    ErrorCode::ResourceUnavailable,
                    "adapter_import_coordinator_poisoned",
                )
            })?;
            let pending = runtime.block_on(self.ports.jobs.pending_imports())?;
            let mut done = 0;
            for record in pending {
                if record.step == UpdateStep::Queued && !include_queued {
                    continue;
                }
                let result = if record.step == UpdateStep::Queued {
                    self.execute_locked(&runtime, &record.job_id, PackageControl::default())
                } else {
                    self.recover_one(&runtime, &record)
                };
                result?;
                done += 1;
            }
            Ok(done)
        })
        .await
        .map_err(|e| {
            error(ErrorCode::Internal, "adapter_import_recovery_failed")
                .with_technical(e.to_string())
        })?
    }

    fn recover_one(
        &self,
        runtime: &tokio::runtime::Handle,
        record: &AdapterImportJournal,
    ) -> Result<(), LarmError> {
        if let Some(plan) = &record.plan {
            let (_, destination) = self.paths(record, plan)?;
            self.fence(runtime, &record.job_id, plan)?;
            if record.step == UpdateStep::Publishing && destination.exists() {
                let manifest = self.manifest(plan)?;
                let control = PackageControl::default();
                let (_lock, _) = lock_version(&self.root, &manifest, &control)?;
                let checked = self.trust.assess(&manifest, false).and_then(|assessment| {
                    if assessment.trust == Trust::Revoked {
                        Err(error(ErrorCode::Forbidden, "revoked"))
                    } else if plan.candidate.trust
                        != match assessment.trust {
                            Trust::Untrusted => AdapterTrust::Untrusted,
                            Trust::Verified => AdapterTrust::Verified,
                            Trust::Revoked => AdapterTrust::Revoked,
                        }
                    {
                        Err(error(
                            ErrorCode::IntegrityFailed,
                            "adapter_import_trust_mismatch",
                        ))
                    } else {
                        verify_published(&destination, &manifest, &control)
                    }
                });
                if let Err(cause) = checked {
                    if cause.details.reason == "fence_lost" {
                        return Err(cause);
                    }
                    self.clean_workspace(record)?;
                    return runtime.block_on(self.ports.jobs.fail_import(&record.job_id, &cause));
                }
                if let Err(cause) =
                    runtime.block_on(self.ports.jobs.complete_import(&record.job_id))
                {
                    if matches!(
                        cause.code,
                        ErrorCode::NotFound | ErrorCode::ValidationFailed | ErrorCode::Conflict
                    ) && cause.details.reason != "fence_lost"
                    {
                        self.clean_workspace(record)?;
                        return runtime
                            .block_on(self.ports.jobs.fail_import(&record.job_id, &cause));
                    }
                    return Err(cause);
                }
                return self.clean_workspace(record);
            }
        }
        self.clean_workspace(record)?;
        runtime.block_on(self.ports.jobs.fail_import(
            &record.job_id,
            &error(ErrorCode::ResourceUnavailable, "adapter_import_interrupted"),
        ))
    }
}
