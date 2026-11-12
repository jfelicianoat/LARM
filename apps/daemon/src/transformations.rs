//! Public P093 composition. Core owns the plan, admission and native attempt;
//! compiled backend recipes supply the private IO/namespace rules.
use async_trait::async_trait;
use larm_adapter_host::{conformance::NativeConformanceRunner, daemon_owner::DaemonOwnership};
use larm_core::config::Settings;
use larm_db::{
    pool::Db, transformation_plans::SqlTransformationPlans, transformations::SqlTransformationJobs,
};
use larm_domain::{
    bridge::JobReceipt,
    error::{ErrorCode, LarmError},
    ports::{MutationContext, TransformationService},
    transformations::*,
};
use larm_storage::estimate::SpacePolicy;
use larm_transform::environment::{TransformationEnvironment, TransformationSpacePolicy};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
mod backend;
mod execution;
mod planning;

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(code, "No se pudo completar la transformación", reason)
}
fn db_error(e: sqlx::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "transformation_database_unavailable",
    )
    .with_technical(e.to_string())
}
fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub struct CurrentPolicy {
    pub repository: larm_db::settings::SqlSettings,
    pub file: Value,
    pub environment: Value,
    pub cli: Value,
}
#[async_trait]
impl TransformationSpacePolicy for CurrentPolicy {
    async fn current(&self) -> Result<SpacePolicy, LarmError> {
        // Admission may already hold SQLite's writer. Read current settings
        // through independent read-only connections; never initialize here.
        let payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM settings WHERE id='global'")
                .fetch_optional(&self.repository.pool)
                .await
                .map_err(db_error)?;
        let payload: Value = serde_json::from_str(&payload.ok_or_else(|| {
            error(
                ErrorCode::ResourceUnavailable,
                "transformation_settings_missing",
            )
        })?)
        .map_err(|_| {
            error(
                ErrorCode::IntegrityFailed,
                "transformation_settings_invalid",
            )
        })?;
        let s = Settings::compose_with_base(
            Some(&payload),
            Some(&self.file),
            Some(&self.environment),
            Some(&self.cli),
        )?;
        Ok(SpacePolicy {
            margin: larm_domain::estimate::MarginPolicy {
                minimum_margin_bytes: s.storage.minimum_margin_bytes.get(),
                margin_basis_points: s.storage.margin_basis_points,
            },
            minimum_free_bytes: s.storage.minimum_free_bytes.get(),
        })
    }
}
struct Inner {
    db: Db,
    database: PathBuf,
    scope: DaemonOwnership,
    runner: Arc<NativeConformanceRunner>,
    env: TransformationEnvironment,
    permit: Arc<tokio::sync::Semaphore>,
    stopping: AtomicBool,
}
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}
impl Service {
    pub fn new(
        db: Db,
        database: PathBuf,
        scope: DaemonOwnership,
        policy: CurrentPolicy,
    ) -> Result<Self, LarmError> {
        scope.recheck()?;
        let prober = crate::hardware::storage_prober().ok_or_else(|| {
            error(
                ErrorCode::ResourceUnavailable,
                "transformation_storage_probe_unavailable",
            )
        })?;
        let runner = crate::probes::runner(&db);
        let env = TransformationEnvironment {
            catalogue: larm_db::transformation_inputs::SqlTransformationInputs {
                pool: db.qualification_reader(),
            },
            locations: larm_db::locations::SqlStorageLocations {
                pool: db.qualification_reader(),
            },
            prober: Arc::new(prober),
            policy: Arc::new(policy),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                database,
                scope,
                runner,
                env,
                permit: Arc::new(tokio::sync::Semaphore::new(1)),
                stopping: AtomicBool::new(false),
            }),
        })
    }
    fn active(&self) -> Result<(), LarmError> {
        self.inner.scope.recheck()?;
        if self.inner.stopping.load(Ordering::SeqCst) {
            return Err(error(
                ErrorCode::ResourceUnavailable,
                "transformation_service_stopping",
            ));
        }
        Ok(())
    }
    fn permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, LarmError> {
        self.active()?;
        self.inner.permit.clone().try_acquire_owned().map_err(|_| {
            error(
                ErrorCode::ResourceUnavailable,
                "transformation_backend_busy",
            )
            .retryable(true)
        })
    }
    fn plans(&self) -> SqlTransformationPlans {
        SqlTransformationPlans {
            pool: self.inner.db.pool.clone(),
            qualification: Some(self.inner.runner.clone()),
        }
    }
    fn jobs(&self) -> SqlTransformationJobs {
        SqlTransformationJobs {
            pool: self.inner.db.pool.clone(),
        }
    }
    pub fn begin_shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
    }
    pub async fn shutdown(&self) -> Result<(), LarmError> {
        self.begin_shutdown();
        let _idle = self
            .inner
            .permit
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| error(ErrorCode::Internal, "transformation_service_permit_closed"))?;
        self.inner.runner.shutdown().await?;
        Ok(())
    }
    async fn confirm(
        &self,
        request: ExecuteTransformationPlan,
        ctx: MutationContext,
        key: TransformationActivationIdempotency,
        restart: Option<TransformationRestart>,
    ) -> Result<JobReceipt, LarmError> {
        // Replay survives expiry, missing input and a changed backend/profile.
        if let Some(receipt) = self
            .plans()
            .admission_replay(&request, &ctx, &key, restart.as_ref())
            .await?
        {
            return Ok(receipt);
        }
        let permit = self.permit()?;
        let s = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            s.active()?;
            let plans = s.plans();
            let plan = plans
                .plan(&request.plan_id, &ctx)
                .await?
                .ok_or_else(|| error(ErrorCode::NotFound, "transformation_plan_missing"))?;
            if plan.response.plan_hash != request.plan_hash {
                return Err(error(
                    ErrorCode::StaleRevision,
                    "transformation_plan_changed",
                ));
            }
            let backend = backend::Backend::for_plan(&s, &plan).await?;
            let input = s
                .inner
                .env
                .input(&plan.request.input_variant_id, Some(&plan.input_artifacts))
                .await?;
            backend.check_input(&plan.request, &input)?;
            backend.check_destination(&s, &plan).await?;
            let space = s.inner.env.space(&plan, backend.workspaces(&plan)?).await?;
            s.active()?;
            Box::pin(plans.admit_for_dispatch(
                &request,
                &ctx,
                &key,
                TransformationAdmission {
                    input: &input,
                    space: &space,
                },
                restart.as_ref(),
            ))
            .await
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "transformation_admission_task_failed"))?
    }
}
#[async_trait]
impl TransformationService for Service {
    async fn create_plan(
        &self,
        request: &TransformationRequest,
        ctx: &MutationContext,
    ) -> Result<TransformationPlanResponse, LarmError> {
        let permit = self.permit()?;
        let s = self.clone();
        let request = request.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            s.active()?;
            Box::pin(s.plan(&request, &ctx)).await
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "transformation_planning_task_failed"))?
    }
    async fn execute_plan(
        &self,
        request: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, LarmError> {
        self.confirm(request.clone(), ctx.clone(), key.clone(), None)
            .await
    }
    async fn restart_plan(
        &self,
        restart: &TransformationRestart,
        request: &ExecuteTransformationPlan,
        ctx: &MutationContext,
        key: &TransformationActivationIdempotency,
    ) -> Result<JobReceipt, LarmError> {
        self.confirm(
            request.clone(),
            ctx.clone(),
            key.clone(),
            Some(restart.clone()),
        )
        .await
    }
    async fn list_jobs(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        ctx: &MutationContext,
    ) -> Result<Vec<TransformationJob>, LarmError> {
        self.jobs().list_jobs(after, limit, ctx).await
    }
    async fn cancel_job(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        // La señal es duradera y no consume el permiso del motor: en cola se
        // cierra aquí y en ejecución el consumidor observa CANCELLING y cierra
        // el intento con su testigo (o lo hace la reconciliación tras caída).
        // request_cancellation sólo devuelve Ok en CANCELLING.
        let job = self
            .jobs()
            .request_cancellation(job_id, principal_id, ctx)
            .await?;
        if let Ok(cancelled) = self.jobs().release_queued_cancellation(job_id).await {
            return Ok(JobReceipt {
                job_id: cancelled.id.clone(),
                status_url: format!("/api/v1/jobs/{}", cancelled.id),
                state: larm_domain::bridge::JobPhase::Cancelled,
            });
        }
        // El consumidor reclamó el trabajo en la carrera: la señal CANCELLING
        // duradera lo cierra con su testigo.
        Ok(JobReceipt {
            job_id: job.id.clone(),
            status_url: format!("/api/v1/jobs/{}", job.id),
            state: larm_domain::bridge::JobPhase::Running,
        })
    }
}
