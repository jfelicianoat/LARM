//! Supervisor de jobs durable para `source.auth_probe` (P045).

use async_trait::async_trait;
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::Probe;
use larm_domain::ports::{
    JobSource, MutationContext, SourceAuthProbeExecutor, SourceProbeRepository, SourceProber,
};
use larm_domain::source_probes::{self, SourceProbeFailure, SourceProbeJob};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

struct Work {
    accepting: bool,
    closing: bool,
    tasks: tokio::task::JoinSet<(String, Result<(), LarmError>)>,
    active: BTreeSet<String>,
}

pub struct SourceProbeService {
    jobs: Arc<dyn SourceProbeRepository>,
    executor: Arc<dyn SourceAuthProbeExecutor>,
    work: tokio::sync::Mutex<Work>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn unavailable() -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "El servicio de sondeos de fuentes no está disponible",
        "source_probe_service_closing",
    )
    .retryable(true)
}

impl SourceProbeService {
    pub fn new(
        jobs: Arc<dyn SourceProbeRepository>,
        executor: Arc<dyn SourceAuthProbeExecutor>,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            jobs,
            executor,
            work: tokio::sync::Mutex::new(Work {
                accepting: false,
                closing: false,
                tasks: tokio::task::JoinSet::new(),
                active: BTreeSet::new(),
            }),
            supervisor: Mutex::new(None),
        });
        let weak = Arc::downgrade(&service);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let Some(service) = weak.upgrade() else {
                    return;
                };
                let mut work = service.work.lock().await;
                if work.closing {
                    return;
                }
                if work.accepting {
                    if let Err(error) = service.start_next(&mut work).await {
                        tracing::warn!(reason=%error.details.reason,"cola de sondeos source conservada para retry");
                    }
                }
            }
        });
        if let Ok(mut supervisor) = service.supervisor.lock() {
            *supervisor = Some(task);
        }
        service
    }

    pub async fn start(&self) {
        let mut work = self.work.lock().await;
        if !work.closing {
            work.accepting = true;
        }
    }

    pub async fn begin_shutdown(&self) {
        let mut work = self.work.lock().await;
        work.accepting = false;
        work.closing = true;
    }

    pub async fn finish_shutdown(&self) {
        self.begin_shutdown().await;
        let task = self.supervisor.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut work = self.work.lock().await;
        while let Some(result) = work.tasks.join_next().await {
            Self::finished(&mut work, result);
        }
    }

    fn finished(
        work: &mut Work,
        result: Result<(String, Result<(), LarmError>), tokio::task::JoinError>,
    ) {
        match result {
            Ok((id, result)) => {
                work.active.remove(&id);
                if let Err(error) = result {
                    tracing::warn!(job_id=%id,reason=%error.details.reason,"sondeo source pendiente de reconciliación");
                }
            }
            Err(_) => {
                tracing::error!("supervisión del sondeo source interrumpida; journal conservado")
            }
        }
    }

    async fn execute(
        jobs: Arc<dyn SourceProbeRepository>,
        executor: Arc<dyn SourceAuthProbeExecutor>,
        job: SourceProbeJob,
    ) -> Result<(), LarmError> {
        let task = tokio::spawn({
            let executor = executor.clone();
            let job = job.clone();
            async move { executor.execute_source_probe(&job).await }
        });
        let result = task.await.unwrap_or_else(|_| {
            Err(SourceProbeFailure::unconfirmed(LarmError::new(
                ErrorCode::Internal,
                "El sondeo de fuente se interrumpió",
                "source_probe_task_failed",
            )))
        });
        executor.wait_source_probe_io(&job.target).await?;
        if let Some(current) = jobs.source_probe_job(&job.id).await? {
            if current.phase == JobPhase::Running {
                match result {
                    Ok(result) => jobs.finish_source_probe(&job.id, &result).await?,
                    Err(failure) => {
                        jobs.fail_source_probe(&job.id, &failure.error, failure.authority_pending)
                            .await?
                    }
                }
            }
        }
        Ok(())
    }

    async fn start_next(&self, work: &mut Work) -> Result<(), LarmError> {
        while let Some(result) = work.tasks.try_join_next() {
            Self::finished(work, result);
        }
        if work.tasks.is_empty() {
            if let Some(job) = self.jobs.claim_source_probe().await? {
                let id = job.id.clone();
                work.active.insert(id.clone());
                let (jobs, executor) = (self.jobs.clone(), self.executor.clone());
                work.tasks.spawn(async move {
                    let result = Self::execute(jobs, executor, job).await;
                    (id, result)
                });
            }
        }
        Ok(())
    }

    async fn accept(
        &self,
        target: &str,
        request: &Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        source_probes::validate_request(target, request)?;
        let mut work = self.work.lock().await;
        if !work.accepting {
            return Err(unavailable());
        }
        let job = self
            .jobs
            .enqueue_source_probe(target, request, body_hash, key, ctx)
            .await?;
        self.start_next(&mut work).await?;
        Ok(job.accepted())
    }
}

#[async_trait]
impl SourceProber for SourceProbeService {
    async fn request_source_probe(
        self: Arc<Self>,
        target: &str,
        request: &Probe,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        let (target, request, body_hash, key, ctx) = (
            target.to_string(),
            request.clone(),
            body_hash.to_string(),
            key.to_string(),
            ctx.clone(),
        );
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = self.accept(&target, &request, &body_hash, &key, &ctx).await;
            let _ = send.send(result);
        });
        receive.await.map_err(|_| unavailable())?
    }
}

#[async_trait]
impl JobSource for SourceProbeService {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        let Some(job) = self.jobs.source_probe_job(id).await? else {
            return Ok(None);
        };
        if job.actor_id != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        let work = self.work.lock().await;
        Ok(Some(job.public_state(
            job.phase == JobPhase::Running && !work.active.contains(id),
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::source_probe_service::SourceProbeService;
    use larm_domain::{
        auth::PrincipalKind,
        bridge::JobPhase,
        error::ApiError,
        hardware::Probe,
        ports::{SourceAuthProbeExecutor, SourceProbeRepository},
        source_probes::{SourceAuthProbeResult, SourceProbeFailure},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SOURCE_ID: &str = "0192f000-0000-7000-8000-00000000f201";
    const JOB_ID: &str = "0192f000-0000-7000-8000-00000000f202";

    #[derive(Default)]
    struct MemoryJobs {
        job: Mutex<Option<SourceProbeJob>>,
    }

    #[async_trait]
    impl SourceProbeRepository for MemoryJobs {
        async fn enqueue_source_probe(
            &self,
            target: &str,
            request: &Probe,
            _body_hash: &str,
            _key: &str,
            ctx: &MutationContext,
        ) -> Result<SourceProbeJob, ApiError> {
            let job = SourceProbeJob {
                id: JOB_ID.into(),
                target: target.into(),
                actor_id: ctx.actor_id.clone(),
                correlation_id: ctx.correlation_id.clone(),
                request: request.clone(),
                source_row_version: 1,
                phase: JobPhase::Queued,
                version: 1,
                result: None,
                error: None,
                authority_pending: None,
            };
            *self.job.lock().unwrap() = Some(job.clone());
            Ok(job)
        }

        async fn claim_source_probe(&self) -> Result<Option<SourceProbeJob>, ApiError> {
            let mut slot = self.job.lock().unwrap();
            let Some(job) = slot.as_mut().filter(|job| job.phase == JobPhase::Queued) else {
                return Ok(None);
            };
            job.phase = JobPhase::Running;
            Ok(Some(job.clone()))
        }

        async fn source_probe_job(&self, id: &str) -> Result<Option<SourceProbeJob>, ApiError> {
            Ok(self
                .job
                .lock()
                .unwrap()
                .as_ref()
                .filter(|job| job.id == id)
                .cloned())
        }

        async fn finish_source_probe(
            &self,
            id: &str,
            result: &SourceAuthProbeResult,
        ) -> Result<(), ApiError> {
            let mut slot = self.job.lock().unwrap();
            let job = slot.as_mut().filter(|job| job.id == id).unwrap();
            job.phase = JobPhase::Completed;
            job.result = Some(result.clone());
            Ok(())
        }

        async fn fail_source_probe(
            &self,
            id: &str,
            error: &ApiError,
            authority_pending: bool,
        ) -> Result<(), ApiError> {
            let mut slot = self.job.lock().unwrap();
            let job = slot.as_mut().filter(|job| job.id == id).unwrap();
            job.phase = JobPhase::Failed;
            job.error = Some(error.clone());
            job.authority_pending = Some(authority_pending);
            Ok(())
        }
    }

    #[derive(Default)]
    struct SuccessfulProbe {
        executed: AtomicUsize,
        drained: AtomicUsize,
    }

    #[async_trait]
    impl SourceAuthProbeExecutor for SuccessfulProbe {
        async fn execute_source_probe(
            &self,
            _job: &SourceProbeJob,
        ) -> Result<SourceAuthProbeResult, SourceProbeFailure> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(SourceAuthProbeResult {
                authenticated: true,
                auth_required: false,
                license_required: false,
                reason: "authenticated".into(),
            })
        }

        async fn wait_source_probe_io(&self, _source_id: &str) -> Result<(), ApiError> {
            self.drained.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn accepted_probe_runs_to_durable_result_and_remains_owner_scoped() {
        let jobs = Arc::new(MemoryJobs::default());
        let executor = Arc::new(SuccessfulProbe::default());
        let service = SourceProbeService::new(jobs, executor.clone());
        service.start().await;
        let receipt = service
            .clone()
            .request_source_probe(
                SOURCE_ID,
                &Probe {
                    resource_id: None,
                    profile_id: None,
                },
                "body-hash",
                "idempotency-key",
                &MutationContext {
                    actor_id: "0192f000-0000-7000-8000-00000000f203".into(),
                    correlation_id: "0192f000-0000-7000-8000-00000000f204".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(receipt.job_id, JOB_ID);
        assert_eq!(receipt.state, JobPhase::Queued);

        let owner = Principal::new(
            "0192f000-0000-7000-8000-00000000f203",
            "technician",
            PrincipalKind::Operator,
            [],
            [],
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if service
                    .job(&owner, JOB_ID)
                    .await
                    .unwrap()
                    .is_some_and(|job| job.state == JobPhase::Completed && job.result.is_some())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("el job debe alcanzar estado terminal");
        assert_eq!(executor.executed.load(Ordering::SeqCst), 1);
        assert_eq!(executor.drained.load(Ordering::SeqCst), 1);

        let other = Principal::new(
            "0192f000-0000-7000-8000-00000000f205",
            "other-technician",
            PrincipalKind::Operator,
            [],
            [],
        )
        .unwrap();
        assert!(service.job(&other, JOB_ID).await.unwrap().is_none());
        service.finish_shutdown().await;
    }
}
