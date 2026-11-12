//! Reutiliza exactamente la medición/P022 de la suite validada. No ejecuta RPC.
use super::*;
use larm_domain::conformance::{Receipt, METADATA_KEY};
use larm_domain::ports::{AdapterQualification, AdapterQualificationGuard};
use std::fs::{File, OpenOptions};

struct Qualified {
    // En Windows se niega escritura/borrado de todos los archivos existentes
    // mientras se confirma la operación. Inventario/rutas se remiden al cerrar.
    // Orden de Drop: cerrar handles antes de liberar ActiveRun/admisión.
    files: Arc<PinnedInputs>,
    prepared: Arc<Prepared>,
    authorization: LaunchAuthorization,
}
#[async_trait]
impl AdapterQualificationGuard for Qualified {
    async fn recheck(&self) -> Result<(), LarmError> {
        let prepared = self.prepared.clone();
        let files = self.files.clone();
        // Cancelar al consumidor no libera los handles de un hash aún en curso.
        tokio::spawn(async move {
            let result = prepared
                .recheck_with_paths(files.paths.clone())
                .await
                .map(|_| ());
            drop(files);
            drop(prepared);
            result
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "conformance_measurement_failed"))?
    }
}
fn pin(path: &Path) -> Result<File, LarmError> {
    check_managed_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ; nunca WRITE/DELETE.
    }
    let file = options.open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(invalid("conformance_file_invalid"));
    }
    Ok(file)
}
struct PinnedInputs {
    paths: crate::package_paths::PinnedPaths,
    // Retain every file even if the bounded path owner later falls back.
    _files: Vec<Arc<File>>,
}

impl PinnedInputs {
    fn pin(&mut self, path: &Path) -> Result<(), LarmError> {
        self.paths.check(path)?;
        let file = match self.paths.owned_file(path) {
            Some(file) => file,
            None => Arc::new(pin(path)?),
        };
        if !file.metadata().map_err(io)?.is_file() {
            return Err(invalid("conformance_file_invalid"));
        }
        self._files.push(file);
        Ok(())
    }
}

fn pin_inputs(target: &NativeTarget) -> Result<PinnedInputs, LarmError> {
    let mut files = PinnedInputs {
        paths: crate::package_paths::PinnedPaths::default(),
        _files: Vec::new(),
    };
    let mut pending = vec![target.spec.package_dir.clone()];
    let mut entries = 0;
    while let Some(dir) = pending.pop() {
        files.paths.check(&dir)?;
        for entry in std::fs::read_dir(dir).map_err(io)? {
            entries += 1;
            if entries > crate::package::MAX_FILES {
                return Err(invalid("conformance_package_too_many_files"));
            }
            let path = entry.map_err(io)?.path();
            files.paths.check(&path)?;
            if path.metadata().map_err(io)?.is_dir() {
                pending.push(path);
            } else {
                files.pin(&path)?;
            }
        }
    }
    if target.immutable_inputs.len() > 128 {
        return Err(invalid("conformance_inputs_invalid"));
    }
    for path in &target.immutable_inputs {
        files.pin(path)?;
    }
    files.pin(&std::env::current_exe().map_err(io)?)?;
    Ok(files)
}
impl NativeConformanceRunner {
    async fn qualified(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<Qualified, LarmError> {
        self.qualified_context(installation, run, ctx, true).await
    }

    /// Admisión de un job: mide íntegramente el contexto y sus permisos sin
    /// abrir procesos ni reservar la exclusión de una sesión de ejecución.
    /// No autoriza RPC: runtime_worker/source_worker conservan su guard propio.
    pub async fn verify_qualification_snapshot(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.qualified_context(installation, run, ctx, false)
            .await?;
        Ok(())
    }

    async fn qualified_context(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
        execution: bool,
    ) -> Result<Qualified, LarmError> {
        if ctx.actor_id.is_empty()
            || ctx.actor_id.len() > 256
            || ctx.actor_id.chars().any(char::is_control)
            || !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id)
        {
            return Err(invalid("conformance_context_invalid"));
        }
        let receipt: Receipt = installation
            .metadata
            .get(METADATA_KEY)
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| invalid("conformance_receipt_missing"))?;
        if !receipt.matches_committed(installation)?
            || receipt.run_sha256 != run.digest()?
            || receipt.run_id != run.id
            || run.installation_id != installation.id
            || run.package_sha256 != installation.package_hash
            || run.manifest_sha256 != hash(&installation.manifest)?
        {
            return Err(error(
                ErrorCode::InvalidStateTransition,
                "conformance_binding_stale",
            ));
        }
        let active = if execution {
            Some(self.activity.acquire(&installation.id)?)
        } else {
            if !self
                .activity
                .state
                .lock()
                .map_err(|_| error(ErrorCode::Internal, "conformance_activity_unavailable"))?
                .0
            {
                return Err(error(
                    ErrorCode::ResourceUnavailable,
                    "conformance_runner_closed",
                ));
            }
            None
        };
        let mut target = self.targets.target(installation, ctx).await?;
        if serde_json::to_value(&target.spec.manifest)
            .map_err(|_| invalid("conformance_manifest_invalid"))?
            != installation.manifest
        {
            return Err(invalid("conformance_manifest_mismatch"));
        }
        if target
            .grant
            .as_ref()
            .is_some_and(|g| g.actor_id != ctx.actor_id)
        {
            return Err(error(ErrorCode::Forbidden, "conformance_actor_mismatch"));
        }
        // Los conversores de producto reales (cuantización K en Python
        // stdlib) legítimamente superan los 10 minutos por suite: el timeout
        // sigue acotado y es un detector de cuelgue.
        if target.timeout.is_zero() || target.timeout > Duration::from_secs(2400) {
            return Err(invalid("conformance_timeout_invalid"));
        }
        let policy = self.target_policy(&target);
        let config = self.config;
        let run = run.clone();
        tokio::task::spawn_blocking(move || {
            target.spec.package_dir = canonical(&target.spec.package_dir)?;
            // La cualificación de uso no crea work ni prepara recursos del plugin.
            target.spec.work_dir = canonical(&target.spec.work_dir)?;
            let mut environment = crate::worker::minimal_env(&target.spec.work_dir);
            environment.extend(target.spec.environment.clone());
            target.spec.environment = environment;
            let started = std::time::Instant::now();
            let files = pin_inputs(&target)?;
            measurement_phase("pin_inputs", started);
            let (bindings, authorization) =
                measure_with_paths(&target, &policy, config, &mut files.paths.clone())?;
            if bindings != run.bindings {
                return Err(error(
                    ErrorCode::StaleRevision,
                    "conformance_context_changed",
                ));
            }
            Ok(Qualified {
                prepared: Arc::new(Prepared {
                    target,
                    policy,
                    config,
                    run,
                    _active: active,
                }),
                files: Arc::new(files),
                authorization,
            })
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "conformance_measurement_failed"))?
    }

    /// Consumo interno de una instalación: la composición obtiene Run del journal
    /// verificado, nunca de la API. Conserva P022, handles, admisión y ownership
    /// durante handshake/RPC/cierre; no ejecuta otra suite ni concede VALIDATED.
    pub async fn runtime_worker(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        runtime_id: &str,
        ctx: &MutationContext,
    ) -> Result<crate::runtime::RuntimeWorker, LarmError> {
        if installation.family != "runtime" {
            return Err(invalid("runtime_worker_family_mismatch"));
        }
        if !larm_domain::bridge::is_uuid_v7(runtime_id) {
            return Err(invalid("runtime_worker_id_invalid"));
        }
        let binding = larm_domain::runtime_execution::RuntimeSessionBinding::new(
            runtime_id,
            installation,
            run,
            &ctx.actor_id,
        )?;
        let owner = Arc::new(self.qualified(installation, run, ctx).await?);
        owner.recheck().await?;
        let worker = GuardedWorker::start_owned(
            owner.prepared.target.spec.clone(),
            &owner.authorization,
            owner.prepared.config,
            1,
            Arc::new(crate::worker::TreeKill),
            Some(owner.clone()),
        )
        .await?;
        let work_dir = owner.prepared.target.spec.work_dir.clone();
        Ok(crate::runtime::RuntimeWorker::from_owned(
            worker, owner, binding, work_dir,
        ))
    }

    /// Consumo tipado de una fuente cualificada. La sesión conserva la
    /// cualificación física y vuelve a medirla antes y después de cada RPC.
    /// La composición todavía debe resolver la fuente, licencia y Access
    /// concretos antes de entregar este cliente al consumidor.
    pub async fn source_worker(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        source_id: &str,
        support: larm_adapter_sdk::source::SearchSupport,
        ctx: &MutationContext,
    ) -> Result<crate::source::SourceWorker, LarmError> {
        if installation.family != "source" {
            return Err(invalid("source_worker_family_mismatch"));
        }
        if !larm_domain::bridge::is_uuid_v7(source_id) {
            return Err(invalid("source_worker_id_invalid"));
        }
        support.validate()?;
        let owner = Arc::new(self.qualified(installation, run, ctx).await?);
        owner.recheck().await?;
        let worker = GuardedWorker::start_owned(
            owner.prepared.target.spec.clone(),
            &owner.authorization,
            owner.prepared.config,
            1,
            Arc::new(crate::worker::TreeKill),
            Some(owner.clone()),
        )
        .await?;
        crate::source::SourceWorker::new_qualified(
            Arc::new(worker),
            source_id.to_owned(),
            support,
            owner,
        )
    }

    /// Uses the exact registered P026 profile, roots, permissions and retained
    /// physical guard. This does not perform Core job admission or output IO.
    pub async fn transformation_worker(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<crate::transformation::TransformationWorker, LarmError> {
        self.transformation_worker_expected(installation, run, ctx, None)
            .await
    }

    /// A detached helper must match the server-selected persisted identity
    /// before executing even the adapter handshake. This does not trust an
    /// identity supplied by an HTTP client or substitute for qualification.
    pub async fn transformation_worker_expected(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
        expected: Option<&crate::transformation::SessionIdentity>,
    ) -> Result<crate::transformation::TransformationWorker, LarmError> {
        if installation.family != "transformation" {
            return Err(invalid("transformation_worker_family_mismatch"));
        }
        let owner = Arc::new(self.qualified(installation, run, ctx).await?);
        let profile_sha256 = owner
            .prepared
            .target
            .suite
            .descriptor()
            .get("registered_profile_sha256")
            .and_then(Value::as_str)
            .filter(|hash| larm_domain::adapter_updates::is_sha256(hash))
            .ok_or_else(|| invalid("transformation_worker_registered_profile_missing"))?
            .to_owned();
        let identity = crate::transformation::SessionIdentity {
            adapter_id: installation.adapter_id.clone(),
            installation_id: installation.id.clone(),
            run_id: run.id.clone(),
            run_sha256: run.digest()?,
            profile_sha256,
        };
        identity.validate()?;
        if expected.is_some_and(|expected| expected != &identity) {
            return Err(error(
                ErrorCode::StaleRevision,
                "transformation_guardian_identity_changed",
            ));
        }
        owner.recheck().await?;
        let worker = GuardedWorker::start_owned(
            owner.prepared.target.spec.clone(),
            &owner.authorization,
            owner.prepared.config,
            1,
            Arc::new(crate::worker::TreeKill),
            Some(owner.clone()),
        )
        .await?;
        Ok(crate::transformation::TransformationWorker::from_owned(
            worker, owner, identity,
        ))
    }
}

#[async_trait]
impl AdapterQualification for NativeConformanceRunner {
    async fn acquire(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<Box<dyn AdapterQualificationGuard>, LarmError> {
        Ok(Box::new(self.qualified(installation, run, ctx).await?))
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod pin_tests {
    use super::*;
    #[test]
    fn fallback_keeps_already_acquired_byte_pins_and_releases_them_with_the_scope() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("input.bin");
        std::fs::write(&file, b"stable").unwrap();
        let mut pins = PinnedInputs {
            paths: crate::package_paths::PinnedPaths::default(),
            _files: Vec::new(),
        };
        pins.pin(&file).unwrap();
        // Existing handles need no new slot. Reach the limit with a path
        // that is not already owned, as the inventory walker would do.
        pins.paths
            .force_test_fallback(&root.path().join("next.bin"))
            .unwrap();
        assert!(pins.paths.owned_file(&file).is_none());
        assert!(std::fs::write(&file, b"not admitted").is_err());
        drop(pins);
        std::fs::write(&file, b"scope closed").unwrap();
    }
}
