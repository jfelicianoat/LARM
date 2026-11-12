//! P031: cliente tipado sobre la sesión P021/P022 con cualificación física P026.
//! La DB/coordinación deben resolver journal, actor, aprobación y reservas antes
//! de usarlo; este cliente no acepta Proof ni convierte el plan en autoridad IO.
use crate::policy::GuardedWorker;
use larm_adapter_sdk::permissions::Access;
use larm_adapter_sdk::protocol::{Progress, Request};
use larm_domain::artifacts::Plan;
use larm_domain::bridge::{is_uuid_v7, JobReceipt};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::instances::{
    HealthResult, InstanceHandle, InstanceRef, LaunchPlan, LaunchRequest, StopResult,
};
use larm_domain::ports::AdapterQualificationGuard;
use larm_domain::runtime_execution::{RuntimeSessionBinding, RuntimeSessionGuard};
use larm_domain::runtime_install::{
    ExecutePlan, InstallRequest, InstallStatusRequest, RuntimeInstallWorkspaceMarker,
    RuntimeVersionPage, RuntimeVersionSource, VersionQuery, INSTALL_WORKSPACE_MARKER,
};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub mod discovery;

pub struct RuntimeWorker {
    worker: GuardedWorker,
    qualification: Arc<dyn AdapterQualificationGuard>,
    binding: RuntimeSessionBinding,
    work_dir: PathBuf,
}

/// Vista de lectura configurada por el perfil del host. No asigna Access vacío
/// implícitamente: la coordinación declara los recursos de la operación.
pub struct RuntimeVersionReader<'a> {
    worker: &'a RuntimeWorker,
    access: Access,
    timeout: Option<Duration>,
}

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo completar la operación del runtime",
        reason,
    )
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
impl RuntimeWorker {
    pub(crate) fn from_owned(
        worker: GuardedWorker,
        qualification: Arc<dyn AdapterQualificationGuard>,
        binding: RuntimeSessionBinding,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            worker,
            qualification,
            binding,
            work_dir,
        }
    }

    /// Crea o resuelve un directorio de trabajo exclusivo para una instalación.
    /// El directorio de trabajo cualificado de P026 permanece como cwd del
    /// proceso, por lo que su hash no cambia.
    pub fn install_workspace(
        &self,
        plan_id: &str,
        plan_hash: &str,
        create: bool,
    ) -> Result<Option<larm_domain::runtime_execution::RuntimeInstallWorkspace>, LarmError> {
        if !is_uuid_v7(plan_id)
            || plan_hash.len() != 64
            || !plan_hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(error(
                ErrorCode::ValidationFailed,
                "runtime_install_workspace_binding_invalid",
            ));
        }
        let root = self.work_dir.clone();
        let root_meta = std::fs::symlink_metadata(&root).map_err(|e| {
            error(
                ErrorCode::ResourceUnavailable,
                "runtime_install_work_root_unavailable",
            )
            .with_technical(e.to_string())
        })?;
        if !root_meta.is_dir() || is_reparse(&root_meta) {
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_install_work_root_invalid",
            ));
        }
        let parent = root.join("runtime-installs");
        match std::fs::symlink_metadata(&parent) {
            Ok(meta) => {
                if !meta.is_dir() || is_reparse(&meta) {
                    return Err(error(
                        ErrorCode::Forbidden,
                        "runtime_install_workspace_parent_invalid",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(error(
                            ErrorCode::ResourceUnavailable,
                            "runtime_install_workspace_create_failed",
                        )
                        .with_technical(e.to_string()));
                    }
                }
                let meta = std::fs::symlink_metadata(&parent).map_err(|e| {
                    error(
                        ErrorCode::ResourceUnavailable,
                        "runtime_install_workspace_parent_invalid",
                    )
                    .with_technical(e.to_string())
                })?;
                if !meta.is_dir() || is_reparse(&meta) {
                    return Err(error(
                        ErrorCode::Forbidden,
                        "runtime_install_workspace_parent_invalid",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(error(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_workspace_parent_invalid",
                )
                .with_technical(e.to_string()));
            }
        }
        let path = parent.join(plan_id);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.is_dir() || is_reparse(&meta) {
                    return Err(error(
                        ErrorCode::Forbidden,
                        "runtime_install_workspace_invalid",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                std::fs::create_dir(&path).map_err(|e| {
                    error(
                        ErrorCode::ResourceUnavailable,
                        "runtime_install_workspace_create_failed",
                    )
                    .with_technical(e.to_string())
                })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(error(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_workspace_invalid",
                )
                .with_technical(e.to_string()));
            }
        }
        let path_meta = std::fs::symlink_metadata(&path).map_err(|e| {
            error(
                ErrorCode::ResourceUnavailable,
                "runtime_install_workspace_invalid",
            )
            .with_technical(e.to_string())
        })?;
        if !path_meta.is_dir() || is_reparse(&path_meta) {
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_install_workspace_invalid",
            ));
        }
        let marker_path = path.join(INSTALL_WORKSPACE_MARKER);
        let marker = RuntimeInstallWorkspaceMarker {
            job_id: plan_id.into(),
            adapter_plan_hash: plan_hash.into(),
        };
        match std::fs::symlink_metadata(&marker_path) {
            Ok(marker_meta) => {
                if !marker_meta.is_file() || is_reparse(&marker_meta) {
                    return Err(error(
                        ErrorCode::Forbidden,
                        "runtime_install_workspace_marker_invalid",
                    ));
                }
                let existing: RuntimeInstallWorkspaceMarker =
                    serde_json::from_slice(&std::fs::read(&marker_path).map_err(|e| {
                        error(
                            ErrorCode::ResourceUnavailable,
                            "runtime_install_workspace_marker_invalid",
                        )
                        .with_technical(e.to_string())
                    })?)
                    .map_err(|e| {
                        error(
                            ErrorCode::IntegrityFailed,
                            "runtime_install_workspace_marker_invalid",
                        )
                        .with_technical(e.to_string())
                    })?;
                if existing != marker {
                    return Err(error(
                        ErrorCode::Conflict,
                        "runtime_install_workspace_binding_changed",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker_path)
                    .map_err(|e| {
                        error(
                            ErrorCode::ResourceUnavailable,
                            "runtime_install_workspace_marker_failed",
                        )
                        .with_technical(e.to_string())
                    })?;
                serde_json::to_writer(&mut file, &marker).map_err(|e| {
                    error(
                        ErrorCode::ResourceUnavailable,
                        "runtime_install_workspace_marker_failed",
                    )
                    .with_technical(e.to_string())
                })?;
                file.sync_all().map_err(|e| {
                    error(
                        ErrorCode::ResourceUnavailable,
                        "runtime_install_workspace_marker_failed",
                    )
                    .with_technical(e.to_string())
                })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(error(
                    ErrorCode::IntegrityFailed,
                    "runtime_install_workspace_marker_missing",
                ));
            }
            Err(e) => {
                return Err(error(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_workspace_marker_failed",
                )
                .with_technical(e.to_string()));
            }
        }
        Ok(Some(
            larm_domain::runtime_execution::RuntimeInstallWorkspace { root, path },
        ))
    }

    fn require_runtime(&self, id: &str) -> Result<(), LarmError> {
        if id != self.binding.runtime_id {
            return Err(error(ErrorCode::Forbidden, "runtime_scope_mismatch"));
        }
        Ok(())
    }

    async fn call(
        &self,
        request: Request,
        access: &Access,
        timeout: Option<Duration>,
        resource: Option<&str>,
    ) -> Result<Value, LarmError> {
        if !is_uuid_v7(&request.id) {
            return Err(error(ErrorCode::ValidationFailed, "request_id_invalid"));
        }
        self.qualification.recheck().await?;
        let result = self
            .worker
            .call_request(request, access, timeout, resource)
            .await;
        let recheck = self.qualification.recheck().await;
        match result {
            Err(original) => Err(original),
            Ok(value) => {
                recheck?;
                Ok(value)
            }
        }
    }

    pub async fn versions(
        &self,
        id: &str,
        query: &VersionQuery,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<RuntimeVersionPage, LarmError> {
        query.validate()?;
        self.require_runtime(&query.runtime_id)?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.list_versions".into(),
                    params: serde_json::to_value(query).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                None,
            )
            .await?;
        let page: RuntimeVersionPage = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_reply_invalid"))?;
        page.validate_for(query)?;
        let mut page = page;
        for version in &mut page.items {
            version.normalize_observed_channel_metadata()?;
        }
        Ok(page)
    }

    /// Pide y valida un plan de lanzamiento antes de que el core reserve
    /// recursos o permita iniciar el proceso.
    pub async fn launch_plan(
        &self,
        id: &str,
        request: &LaunchRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<LaunchPlan, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.launch_plan".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                None,
            )
            .await?;
        let plan: LaunchPlan = serde_json::from_value(value).map_err(|_| {
            error(
                ErrorCode::ProtocolError,
                "runtime_launch_plan_reply_invalid",
            )
        })?;
        plan.validate(chrono::Utc::now())?;
        Ok(plan)
    }

    /// Ejecuta una vez un plan de lanzamiento verificado por el core.
    pub async fn start(
        &self,
        id: &str,
        request: &ExecutePlan,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<InstanceHandle, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.start".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                Some(&request.plan_id),
            )
            .await?;
        let handle: InstanceHandle = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_instance_handle_invalid"))?;
        handle.validate()?;
        Ok(handle)
    }

    /// Carga una solicitud de lanzamiento en un adaptador que ofrece el atajo
    /// contractual `runtime.load`.
    pub async fn load(
        &self,
        id: &str,
        request: &LaunchRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<InstanceHandle, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.load".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                None,
            )
            .await?;
        let handle: InstanceHandle = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_instance_handle_invalid"))?;
        handle.validate()?;
        Ok(handle)
    }

    pub async fn health(
        &self,
        id: &str,
        request: &InstanceRef,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<HealthResult, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.health".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                Some(&request.instance_id),
            )
            .await?;
        let health: HealthResult = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_health_reply_invalid"))?;
        health.validate()?;
        Ok(health)
    }

    pub async fn stop(
        &self,
        id: &str,
        request: &InstanceRef,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<StopResult, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.stop".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                Some(&request.instance_id),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_stop_reply_invalid"))
    }

    pub async fn plan_install(
        &self,
        id: &str,
        request: &InstallRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<Plan, LarmError> {
        request.validate()?;
        self.require_runtime(&request.runtime_id)?;
        self.worker.manifest().require_strategy(request.strategy)?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.install_plan".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                None,
            )
            .await?;
        let plan: Plan = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_plan_reply_invalid"))?;
        larm_domain::runtime_install::validate_plan_reply(request, &plan, chrono::Utc::now())?;
        Ok(plan)
    }

    /// El coordinador sólo envía ExecutePlan tras consumir su plan durable,
    /// aprobación y reservas. La mutación se serializa por plan_id en P021.
    pub async fn install(
        &self,
        id: &str,
        request: &ExecutePlan,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<JobReceipt, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.install".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                Some(&request.plan_id),
            )
            .await?;
        let receipt: JobReceipt = serde_json::from_value(value)
            .map_err(|_| error(ErrorCode::ProtocolError, "runtime_install_reply_invalid"))?;
        let expected_status_url = format!("/api/v1/jobs/{}", request.plan_id);
        if !is_uuid_v7(&receipt.job_id)
            || receipt.job_id != request.plan_id
            || receipt.status_url != expected_status_url
        {
            return Err(error(
                ErrorCode::ProtocolError,
                "runtime_install_reply_invalid",
            ));
        }
        Ok(receipt)
    }
    /// Consulta el estado durable de este mismo plan en el adaptador.
    pub async fn install_status(
        &self,
        id: &str,
        request: &InstallStatusRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<JobReceipt, LarmError> {
        request.validate()?;
        let value = self
            .call(
                Request {
                    jsonrpc: "2.0".into(),
                    id: id.into(),
                    method: "runtime.install_status".into(),
                    params: serde_json::to_value(request).map_err(|_| {
                        error(ErrorCode::ValidationFailed, "runtime_request_invalid")
                    })?,
                },
                access,
                timeout,
                Some(&request.plan_id),
            )
            .await?;
        let receipt: JobReceipt = serde_json::from_value(value).map_err(|_| {
            error(
                ErrorCode::ProtocolError,
                "runtime_install_status_reply_invalid",
            )
        })?;
        if receipt.job_id != request.plan_id
            || receipt.status_url != format!("/api/v1/jobs/{}", request.plan_id)
        {
            return Err(error(
                ErrorCode::ProtocolError,
                "runtime_install_status_binding_mismatch",
            ));
        }
        Ok(receipt)
    }
    pub async fn recheck(&self) -> Result<(), LarmError> {
        self.qualification.recheck().await
    }
    pub fn version_source(
        &self,
        access: Access,
        timeout: Option<Duration>,
    ) -> RuntimeVersionReader<'_> {
        RuntimeVersionReader {
            worker: self,
            access,
            timeout,
        }
    }
    pub async fn cancel(&self, id: &str) -> Result<bool, LarmError> {
        if !is_uuid_v7(id) {
            return Err(error(ErrorCode::ValidationFailed, "request_id_invalid"));
        }
        self.worker.cancel(id).await
    }
    /// El cierre no exige una cualificación aún vigente: limpiar siempre es posible.
    pub async fn close(&self) -> Result<(), LarmError> {
        self.worker.stop_and_wait().await
    }
    pub fn progress(&self) -> tokio::sync::broadcast::Receiver<Progress> {
        self.worker.subscribe_progress()
    }
    pub fn io_drained(&self) -> bool {
        self.worker.io_drained()
    }
}

#[async_trait::async_trait]
impl AdapterQualificationGuard for RuntimeWorker {
    async fn recheck(&self) -> Result<(), LarmError> {
        self.qualification.recheck().await
    }
}
#[async_trait::async_trait]
impl RuntimeSessionGuard for RuntimeWorker {
    fn binding(&self) -> &RuntimeSessionBinding {
        &self.binding
    }
}

#[async_trait::async_trait]
impl RuntimeVersionSource for RuntimeVersionReader<'_> {
    async fn list_versions(&self, query: &VersionQuery) -> Result<RuntimeVersionPage, LarmError> {
        self.worker
            .versions(
                &uuid::Uuid::now_v7().to_string(),
                query,
                &self.access,
                self.timeout,
            )
            .await
    }
}
