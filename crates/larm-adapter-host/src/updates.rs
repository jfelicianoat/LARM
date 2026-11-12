//! P024: coordinación física side-by-side. Un hilo conserva el mutex y el
//! lock de proceso durante TODO el IO; cancelar el futuro no los libera.
//! La publicación es el punto de compromiso: después se finaliza el registro
//! o se conserva PUBLISHING para recuperación, nunca se borra por un Drop.
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{
    adapter_resource, elevation_digest, PermissionDelta, Trust, TrustStore,
};
use larm_domain::adapter_updates::{
    AdapterUpdate, UpdateApproval, UpdatePlan, UpdateStep, UPDATE_GENERATION,
};
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::timestamp;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterInstallationRepository, AdapterPackageSpace, AdapterUpdateRepository,
    ApprovalRepository, ResourceGuard, VolumeReservationRepository,
};
use larm_domain::units::DecimalU64;

use crate::package::{
    check_managed_path, component, lock_version, verify_published, ArchivePlan, PackageControl,
    StagedPackage,
};
use crate::policy::{Policy, UpgradeDecision};

#[derive(Clone)]
pub struct UpdatePorts {
    pub jobs: Arc<dyn AdapterUpdateRepository>,
    pub installations: Arc<dyn AdapterInstallationRepository>,
    pub approvals: Arc<dyn ApprovalRepository>,
    pub reservations: Arc<dyn VolumeReservationRepository>,
    pub guard: Arc<dyn ResourceGuard>,
    pub space: Arc<dyn AdapterPackageSpace>,
}

pub struct UpdateCoordinator {
    ports: UpdatePorts,
    root: PathBuf,
    trust: TrustStore,
    // También lo conservan los closures spawn_blocking si su futuro desaparece.
    _process_lock: File,
    serial: Mutex<()>,
}

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo completar la actualización del adaptador",
        reason,
    )
}
fn io(e: impl std::fmt::Display) -> LarmError {
    error(ErrorCode::ResourceUnavailable, "adapter_update_io_failed").with_technical(e.to_string())
}
fn decimal(n: u64) -> Result<DecimalU64, LarmError> {
    DecimalU64::new(n).ok_or_else(|| error(ErrorCode::ValidationFailed, "size_overflow"))
}

// Sólo señala cancelación; el hilo conserva propiedad del IO hasta terminar.
struct CancelOnDrop(PackageControl);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl UpdateCoordinator {
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Bootstrap de metadatos (directorio y fichero de lock vacíos). Los bytes
    /// de paquetes se escriben exclusivamente después de prepare_update.
    pub fn open(
        root: PathBuf,
        ports: UpdatePorts,
        trust: TrustStore,
    ) -> Result<Arc<Self>, LarmError> {
        check_managed_path(&root)?;
        std::fs::create_dir_all(&root).map_err(io)?;
        let root = std::fs::canonicalize(root).map_err(io)?;
        check_managed_path(&root)?;
        let lock_path = root.join(".update-coordinator.lock");
        check_managed_path(&lock_path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(io)?;
        file.try_lock()
            .map_err(|_| error(ErrorCode::ResourceUnavailable, "adapter_coordinator_busy"))?;
        Ok(Arc::new(Self {
            ports,
            root,
            trust,
            _process_lock: file,
            serial: Mutex::new(()),
        }))
    }

    fn workspace(&self, id: &str) -> Result<PathBuf, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(id) {
            return Err(error(ErrorCode::ValidationFailed, "job_id_invalid"));
        }
        Ok(self.root.join(".staging").join(id))
    }
    fn destination(&self, p: &UpdatePlan) -> Result<PathBuf, LarmError> {
        component(&p.candidate.adapter_id)?;
        component(&p.candidate.version)?;
        if p.candidate.adapter_id.eq_ignore_ascii_case(".staging") {
            return Err(error(ErrorCode::IntegrityFailed, "package_path_invalid"));
        }
        Ok(self
            .root
            .join(&p.candidate.adapter_id)
            .join(&p.candidate.version))
    }
    fn paths(&self, r: &AdapterUpdate, p: &UpdatePlan) -> Result<(PathBuf, PathBuf), LarmError> {
        let (workspace, destination) = (self.workspace(&r.job_id)?, self.destination(p)?);
        if Path::new(&p.workspace_path) != workspace
            || Path::new(&p.destination_path) != destination
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "adapter_update_paths_changed",
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
        p: &UpdatePlan,
    ) -> Result<(), LarmError> {
        for (resource, mode) in p.locks() {
            if !runtime.block_on(
                self.ports
                    .guard
                    .holds(id, UPDATE_GENERATION, &resource, mode),
            )? {
                return Err(error(ErrorCode::Conflict, "fence_lost").with_resource(id));
            }
        }
        Ok(())
    }
    fn control(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        r: &AdapterUpdate,
        base: PackageControl,
    ) -> Result<PackageControl, LarmError> {
        let p = r.plan.clone().ok_or_else(|| {
            error(
                ErrorCode::InvalidStateTransition,
                "adapter_update_plan_missing",
            )
        })?;
        let (this, handle, id) = (self.clone(), runtime.clone(), r.job_id.clone());
        let (repo, writer_handle, writer_id, volume) = (
            self.ports.reservations.clone(),
            runtime.clone(),
            r.job_id.clone(),
            p.volume_key.clone(),
        );
        let live = AtomicU64::new(0);
        Ok(base.with_io_hooks(
            Arc::new(move || this.fence(&handle, &id, &p)),
            Arc::new(move |n| {
                let count = live
                    .fetch_add(n, Ordering::AcqRel)
                    .checked_add(n)
                    .ok_or_else(|| error(ErrorCode::ValidationFailed, "size_overflow"))?;
                writer_handle.block_on(repo.consume(&writer_id, &volume, count))
            }),
        ))
    }
    fn clean_workspace(&self, r: &AdapterUpdate) -> Result<(), LarmError> {
        let path = if let Some(p) = &r.plan {
            self.paths(r, p)?.0
        } else {
            self.workspace(&r.job_id)?
        };
        check_managed_path(&path)?;
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(io)?;
        }
        Ok(())
    }

    /// El task es dueño de Arc<Self> y mantiene el lock aunque se cancele la
    /// espera. No hay timeout que suelte reservas mientras siga el hilo vivo.
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
                    "adapter_coordinator_poisoned",
                )
            })?;
            self.execute_locked(&runtime, &id, control)
        })
        .await
        .map_err(|e| {
            error(ErrorCode::Internal, "adapter_update_worker_failed").with_technical(e.to_string())
        })?
    }

    fn execute_locked(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        id: &str,
        control: PackageControl,
    ) -> Result<(), LarmError> {
        let r = runtime
            .block_on(self.ports.jobs.update_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        if matches!(r.step, UpdateStep::Completed | UpdateStep::Failed) {
            return Ok(());
        }
        if r.step != UpdateStep::Queued {
            return Err(error(
                ErrorCode::InvalidStateTransition,
                "adapter_update_requires_recovery",
            ));
        }
        // Sólo quien gana QUEUED->INSPECTING puede fallar este intento.
        runtime.block_on(self.ports.jobs.advance_update(
            id,
            UpdateStep::Queued,
            UpdateStep::Inspecting,
        ))?;
        let mut published = false;
        let result = self.install(runtime, id, &control, &mut published);
        if let Err(ref cause) = result {
            let latest = runtime
                .block_on(self.ports.jobs.update_job(id))?
                .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
            if latest.step == UpdateStep::Completed {
                return Ok(());
            }
            // PUBLISHING puede tener bytes confirmados por un COMMIT cuyo ack
            // falló. Se conserva para recover, nunca se interpreta como fallo.
            if !published {
                if let Some(p) = &latest.plan {
                    self.fence(runtime, id, p)?;
                }
                self.clean_workspace(&latest)?;
                runtime.block_on(self.ports.jobs.fail_update(id, cause))?;
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
        let r = runtime
            .block_on(self.ports.jobs.update_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        let target = runtime
            .block_on(self.ports.installations.get(&r.target_installation_id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "adapter_unknown"))?;
        let old = AdapterManifest::parse(&target.manifest.to_string())?;
        let archive = ArchivePlan::inspect(
            Path::new(&r.request.package_path),
            &r.request.expected_hash,
            base,
        )?;
        let assessment = self.trust.assess(&archive.manifest, false)?;
        if assessment.trust == Trust::Revoked {
            return Err(error(ErrorCode::Forbidden, "revoked"));
        }
        let approval_record = r
            .request
            .approval_id
            .as_deref()
            .map(|id| runtime.block_on(self.ports.approvals.get(id)))
            .transpose()?
            .flatten();
        let decision = Policy::review_upgrade(
            &old.permissions,
            &archive.manifest,
            approval_record.as_ref().map(|a| (a, r.actor_id.as_str())),
            chrono::Utc::now(),
        )?;
        let approval = match decision {
            UpgradeDecision::ApprovalRequired { .. } => {
                return Err(
                    error(ErrorCode::ApprovalRequired, "permission_elevation_required")
                        .with_resource(adapter_resource(&archive.manifest)),
                )
            }
            UpgradeDecision::Accepted { approval_id: None } => None,
            UpgradeDecision::Accepted {
                approval_id: Some(id),
            } => Some(UpdateApproval {
                id,
                operation_digest: elevation_digest(
                    &archive.manifest,
                    &PermissionDelta::between(&old.permissions, &archive.manifest.permissions),
                )?,
                resource_ids: vec![adapter_resource(&archive.manifest)],
                code_hash: archive.manifest.package_sha256.clone(),
            }),
        };
        let now = timestamp(chrono::Utc::now());
        let candidate = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata:
                serde_json::json!({"signature_state":assessment.signature_state,"update_job_id":id})
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
        let mut p = UpdatePlan {
            candidate,
            workspace_path: self.workspace(id)?.to_string_lossy().into_owned(),
            destination_path: String::new(),
            volume_key: String::new(),
            archive_bytes: decimal(archive.archive_bytes)?,
            unpacked_bytes: decimal(archive.unpacked_bytes)?,
            required_bytes: decimal(archive.required_bytes())?,
            approval,
        };
        let destination = self.destination(&p)?;
        check_managed_path(&destination)?;
        if destination.exists() {
            return Err(error(ErrorCode::Conflict, "adapter_version_exists"));
        }
        p.destination_path = destination.to_string_lossy().into_owned();
        let (demand, observed) = runtime.block_on(
            self.ports
                .space
                .measure_package_space(&self.root.to_string_lossy(), p.required_bytes.get()),
        )?;
        base.check()?;
        p.volume_key = demand.volume_key.clone();
        p.validate(&target)?;
        runtime.block_on(self.ports.jobs.prepare_update(id, &p, &demand, observed))?;
        let r = runtime
            .block_on(self.ports.jobs.update_job(id))?
            .ok_or_else(|| error(ErrorCode::NotFound, "job_unknown"))?;
        let control = self.control(runtime, &r, base.clone())?;
        let workspace = self.paths(&r, &p)?.0;
        control.check()?;
        if workspace.exists() {
            return Err(error(ErrorCode::Conflict, "adapter_workspace_exists"));
        }
        std::fs::create_dir_all(&workspace).map_err(io)?;
        runtime.block_on(self.ports.jobs.advance_update(
            id,
            UpdateStep::Reserved,
            UpdateStep::Extracting,
        ))?;
        let stage = StagedPackage::extract(archive, &workspace, &self.trust, &control)?;
        runtime.block_on(self.ports.jobs.advance_update(
            id,
            UpdateStep::Extracting,
            UpdateStep::Staged,
        ))?;
        control.check()?;
        runtime.block_on(self.ports.jobs.advance_update(
            id,
            UpdateStep::Staged,
            UpdateStep::Publishing,
        ))?;
        let mut published = stage.publish(&self.root, &control)?;
        published.retain_for_recovery(); // ANTES de cualquier await/commit.
        *published_bytes = true;
        // Después del rename se completa este punto de compromiso; una
        // cancelación posterior no convierte el paquete registrado en huérfano.
        let committing = self.control(
            runtime,
            &r,
            PackageControl::with_timeout(Duration::from_secs(120)),
        )?;
        verify_published(&published.destination, published.manifest(), &committing)?;
        runtime.block_on(self.ports.jobs.complete_update(id))?;
        published.commit();
        self.clean_workspace(&r)
    }

    /// Sólo con el lock de proceso y el mutex: ningún IO anterior sigue vivo.
    /// Antes de publicar se aborta y limpia; tras PUBLISHING se verifica el
    /// paquete final y se termina la misma transacción, sin ejecutar código.
    pub async fn recover(self: Arc<Self>) -> Result<usize, LarmError> {
        self.recover_mode(true).await
    }

    /// El servidor recupera IO iniciado antes de escuchar; QUEUED se deja a
    /// su supervisor para no bloquear el arranque por toda la cola durable.
    pub async fn recover_started(self: Arc<Self>) -> Result<usize, LarmError> {
        self.recover_mode(false).await
    }

    async fn recover_mode(self: Arc<Self>, start_queued: bool) -> Result<usize, LarmError> {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _serial = self.serial.lock().map_err(|_| {
                error(
                    ErrorCode::ResourceUnavailable,
                    "adapter_coordinator_poisoned",
                )
            })?;
            let pending = runtime.block_on(self.ports.jobs.pending_updates())?;
            let mut done = 0;
            for r in pending {
                if r.step == UpdateStep::Queued && !start_queued {
                    continue;
                }
                let result = if r.step == UpdateStep::Queued {
                    self.execute_locked(&runtime, &r.job_id, PackageControl::default())
                } else {
                    self.recover_locked(&runtime, &r)
                };
                if let Err(cause) = result {
                    let latest = runtime.block_on(self.ports.jobs.update_job(&r.job_id))?;
                    if !latest.is_some_and(|r| {
                        matches!(r.step, UpdateStep::Completed | UpdateStep::Failed)
                    }) {
                        return Err(cause);
                    }
                }
                done += 1;
            }
            let staging = self.root.join(".staging");
            check_managed_path(&staging)?;
            if staging.exists() {
                for entry in std::fs::read_dir(&staging).map_err(io)? {
                    let entry = entry.map_err(io)?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !larm_domain::bridge::is_uuid_v7(&name) {
                        continue;
                    }
                    if let Some(r) = runtime.block_on(self.ports.jobs.update_job(&name))? {
                        if matches!(r.step, UpdateStep::Completed | UpdateStep::Failed) {
                            self.clean_workspace(&r)?;
                        }
                    }
                }
            }
            Ok(done)
        })
        .await
        .map_err(|e| {
            error(ErrorCode::Internal, "adapter_update_recovery_failed")
                .with_technical(e.to_string())
        })?
    }

    fn recover_locked(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        r: &AdapterUpdate,
    ) -> Result<(), LarmError> {
        if let Some(p) = &r.plan {
            let (_, destination) = self.paths(r, p)?;
            self.fence(runtime, &r.job_id, p)?;
            if r.step == UpdateStep::Publishing && destination.exists() {
                let manifest = AdapterManifest::parse(&p.candidate.manifest.to_string())?;
                let control = self.control(runtime, r, PackageControl::default())?;
                let (_version_lock, _) = lock_version(&self.root, &manifest, &control)?;
                let checked = self.trust.assess(&manifest, false).and_then(|assessment| {
                    if assessment.trust == Trust::Revoked {
                        Err(error(ErrorCode::Forbidden, "revoked"))
                    } else {
                        verify_published(&destination, &manifest, &control)
                    }
                });
                if let Err(cause) = checked {
                    if cause.details.reason == "fence_lost" {
                        return Err(cause);
                    }
                    self.clean_workspace(r)?;
                    // Directorio final conservado para inspección, nunca se
                    // ejecuta ni se incorpora una copia corrupta/revocada.
                    return runtime.block_on(self.ports.jobs.fail_update(&r.job_id, &cause));
                }
                if let Err(cause) = runtime.block_on(self.ports.jobs.complete_update(&r.job_id)) {
                    if matches!(
                        cause.code,
                        ErrorCode::StaleRevision
                            | ErrorCode::NotFound
                            | ErrorCode::ValidationFailed
                            | ErrorCode::Conflict
                    ) && cause.details.reason != "fence_lost"
                    {
                        self.clean_workspace(r)?;
                        return runtime.block_on(self.ports.jobs.fail_update(&r.job_id, &cause));
                    }
                    return Err(cause);
                }
                return self.clean_workspace(r);
            }
        }
        self.clean_workspace(r)?;
        runtime.block_on(self.ports.jobs.fail_update(
            &r.job_id,
            &error(ErrorCode::ResourceUnavailable, "adapter_update_interrupted"),
        ))
    }
}
