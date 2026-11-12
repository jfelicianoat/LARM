//! Puente de consumo. La composición proporciona un resolver de Access del host,
//! nunca permisos vacíos implícitos ni un documento ejecutable de la petición.
use super::*;
use crate::conformance::NativeConformanceRunner;
use larm_domain::ports::MutationContext;
use larm_domain::runtime_execution::{
    RuntimeDiscoveryHost, RuntimeDiscoverySession, RuntimeExecutionContext, RuntimeOperationAccess,
};

fn operation_access(access: &RuntimeOperationAccess) -> Access {
    Access {
        reads: access.reads.clone(),
        writes: access.writes.clone(),
        urls: access.urls.clone(),
        processes: access.processes.clone(),
        secrets: access.secrets.clone(),
    }
}

fn validate_operation_timeout(timeout: Option<Duration>) -> Result<(), LarmError> {
    if timeout.is_some_and(|value| value.is_zero() || value > Duration::from_secs(600)) {
        return Err(error(
            ErrorCode::ValidationFailed,
            "runtime_lifecycle_timeout_invalid",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct DiscoveryAccess {
    pub access: Access,
    pub timeout: Duration,
}
#[async_trait::async_trait]
pub trait DiscoveryAccessResolver: Send + Sync {
    async fn resolve(
        &self,
        context: &RuntimeExecutionContext,
        ctx: &MutationContext,
    ) -> Result<DiscoveryAccess, LarmError>;
}
pub struct NativeDiscoveryHost {
    pub runner: Arc<NativeConformanceRunner>,
    pub access: Arc<dyn DiscoveryAccessResolver>,
}
struct Session {
    worker: RuntimeWorker,
    access: DiscoveryAccess,
}
#[async_trait::async_trait]
impl RuntimeDiscoveryHost for NativeDiscoveryHost {
    async fn open(
        &self,
        context: &RuntimeExecutionContext,
        ctx: &MutationContext,
    ) -> Result<Box<dyn RuntimeDiscoverySession>, LarmError> {
        if context.actor_id != ctx.actor_id {
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_discovery_actor_mismatch",
            ));
        }
        let expected_binding = context.binding()?;
        let access = self.access.resolve(context, ctx).await?;
        if access.timeout.is_zero() || access.timeout > Duration::from_secs(600) {
            return Err(error(
                ErrorCode::ValidationFailed,
                "runtime_discovery_timeout_invalid",
            ));
        }
        let worker = self
            .runner
            .runtime_worker(
                &context.installation,
                &context.run,
                &context.runtime.id,
                ctx,
            )
            .await?;
        if worker.binding() != &expected_binding {
            let _ = worker.close().await;
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_discovery_session_mismatch",
            ));
        }
        Ok(Box::new(Session { worker, access }))
    }
}
#[async_trait::async_trait]
impl RuntimeVersionSource for Session {
    async fn list_versions(&self, query: &VersionQuery) -> Result<RuntimeVersionPage, LarmError> {
        self.worker
            .versions(
                &uuid::Uuid::now_v7().to_string(),
                query,
                &self.access.access,
                Some(self.access.timeout),
            )
            .await
    }
}
#[async_trait::async_trait]
impl AdapterQualificationGuard for Session {
    async fn recheck(&self) -> Result<(), LarmError> {
        self.worker.recheck().await
    }
}
impl RuntimeSessionGuard for Session {
    fn binding(&self) -> &RuntimeSessionBinding {
        self.worker.binding()
    }
}
#[async_trait::async_trait]
impl RuntimeDiscoverySession for Session {
    async fn launch_plan(
        &self,
        request: &larm_domain::instances::LaunchRequest,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
    ) -> Result<larm_domain::instances::LaunchPlan, LarmError> {
        validate_operation_timeout(timeout)?;
        self.worker
            .launch_plan(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &operation_access(access),
                timeout,
            )
            .await
    }

    async fn start(
        &self,
        request: &larm_domain::runtime_install::ExecutePlan,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
    ) -> Result<larm_domain::instances::InstanceHandle, LarmError> {
        validate_operation_timeout(timeout)?;
        self.worker
            .start(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &operation_access(access),
                timeout,
            )
            .await
    }

    async fn load(
        &self,
        request: &larm_domain::instances::LaunchRequest,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
    ) -> Result<larm_domain::instances::InstanceHandle, LarmError> {
        validate_operation_timeout(timeout)?;
        self.worker
            .load(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &operation_access(access),
                timeout,
            )
            .await
    }

    async fn health(
        &self,
        request: &larm_domain::instances::InstanceRef,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
    ) -> Result<larm_domain::instances::HealthResult, LarmError> {
        validate_operation_timeout(timeout)?;
        self.worker
            .health(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &operation_access(access),
                timeout,
            )
            .await
    }

    async fn stop(
        &self,
        request: &larm_domain::instances::InstanceRef,
        access: &RuntimeOperationAccess,
        timeout: Option<Duration>,
    ) -> Result<larm_domain::instances::StopResult, LarmError> {
        validate_operation_timeout(timeout)?;
        self.worker
            .stop(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &operation_access(access),
                timeout,
            )
            .await
    }

    async fn plan_install(&self, request: &InstallRequest) -> Result<Plan, LarmError> {
        self.worker
            .plan_install(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &self.access.access,
                Some(self.access.timeout),
            )
            .await
    }

    async fn install(
        &self,
        request: &larm_domain::runtime_install::ExecutePlan,
    ) -> Result<JobReceipt, LarmError> {
        let workspace = self
            .worker
            .install_workspace(&request.plan_id, &request.plan_hash, true)?
            .ok_or_else(|| {
                error(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_workspace_unavailable",
                )
            })?;
        let mut access = self.access.access.clone();
        access.writes.push(workspace.path);
        self.worker
            .install(
                &request.plan_id,
                request,
                &access,
                Some(Duration::from_secs(600)),
            )
            .await
    }

    async fn cancel_install(&self, plan_id: &str) -> Result<bool, LarmError> {
        self.worker.cancel(plan_id).await
    }

    async fn install_status(
        &self,
        request: &larm_domain::runtime_install::InstallStatusRequest,
    ) -> Result<JobReceipt, LarmError> {
        let mut access = self.access.access.clone();
        if let Some(workspace) =
            self.worker
                .install_workspace(&request.plan_id, &request.plan_hash, false)?
        {
            access.reads.push(workspace.path);
        }
        self.worker
            .install_status(
                &uuid::Uuid::now_v7().to_string(),
                request,
                &access,
                Some(self.access.timeout.min(Duration::from_secs(15))),
            )
            .await
    }

    async fn install_workspace(
        &self,
        plan_id: &str,
        plan_hash: &str,
        create: bool,
    ) -> Result<Option<larm_domain::runtime_execution::RuntimeInstallWorkspace>, LarmError> {
        self.worker.install_workspace(plan_id, plan_hash, create)
    }

    async fn close(&self) -> Result<(), LarmError> {
        self.worker.close().await
    }
}
