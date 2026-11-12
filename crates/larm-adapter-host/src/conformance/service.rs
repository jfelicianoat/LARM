//! Aceptación HTTP durable, un worker supervisado y privacidad por propietario.
use async_trait::async_trait;
use larm_domain::adapter_probes::{validate_request, ProbeFailure, ProbeJob};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::Probe;
use larm_domain::ports::{
    AdapterProbeExecutor, AdapterProbeRepository, AdapterProber, JobSource, MutationContext,
};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

struct Work {
    accepting: bool,
    closing: bool,
    tasks: tokio::task::JoinSet<(String, Result<(), LarmError>)>,
    active: BTreeSet<String>,
}
pub struct ProbeService {
    jobs: Arc<dyn AdapterProbeRepository>,
    executor: Arc<dyn AdapterProbeExecutor>,
    work: tokio::sync::Mutex<Work>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
fn unavailable() -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "El servicio de sondeos no está disponible",
        "adapter_probe_service_closing",
    )
    .retryable(true)
}
impl ProbeService {
    pub fn new(
        jobs: Arc<dyn AdapterProbeRepository>,
        executor: Arc<dyn AdapterProbeExecutor>,
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
                    if let Err(e) = service.start_next(&mut work).await {
                        tracing::warn!(reason=%e.details.reason,"cola de sondeos conservada para retry");
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
                if let Err(e) = result {
                    tracing::warn!(job_id=%id,reason=%e.details.reason,"sondeo pendiente de reconciliación; no se acredita cierre");
                }
            }
            Err(_) => {
                tracing::error!("supervisión de sondeo interrumpida; journal conservado");
            }
        }
    }
    async fn execute(
        jobs: Arc<dyn AdapterProbeRepository>,
        executor: Arc<dyn AdapterProbeExecutor>,
        job: ProbeJob,
    ) -> Result<(), LarmError> {
        let task = tokio::spawn({
            let executor = executor.clone();
            let job = job.clone();
            async move { executor.execute_probe(&job).await }
        });
        let result = task.await.unwrap_or_else(|_| {
            Err(ProbeFailure::unconfirmed(LarmError::new(
                ErrorCode::Internal,
                "El sondeo se interrumpió",
                "adapter_probe_task_failed",
            )))
        });
        // Incluso un timeout de cleanup conserva ownership nativo. No declarar
        // terminal el trabajo hasta que ese IO haya terminado realmente.
        executor.wait_probe_io(&job.target).await?;
        if let Some(current) = jobs.probe_job(&job.id).await? {
            if current.phase == JobPhase::Running {
                let failure = result.err().unwrap_or_else(|| {
                    ProbeFailure::unconfirmed(LarmError::new(
                        ErrorCode::Internal,
                        "Falta confirmación atómica",
                        "probe_result_not_committed",
                    ))
                });
                jobs.fail_probe(&job.id, &failure.error, failure.authority_pending)
                    .await?;
            }
        }
        Ok(())
    }
    async fn start_next(&self, work: &mut Work) -> Result<(), LarmError> {
        while let Some(result) = work.tasks.try_join_next() {
            Self::finished(work, result);
        }
        if work.tasks.is_empty() {
            if let Some(job) = self.jobs.claim_probe().await? {
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
        hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        validate_request(target, request)?;
        let mut work = self.work.lock().await;
        if !work.accepting {
            return Err(unavailable());
        }
        let job = self
            .jobs
            .enqueue_probe(target, request, hash, key, ctx)
            .await?;
        self.start_next(&mut work).await?;
        Ok(job.accepted())
    }
}
#[async_trait]
impl AdapterProber for ProbeService {
    async fn request_probe(
        self: Arc<Self>,
        target: &str,
        request: &Probe,
        hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        let (target, request, hash, key, ctx) = (
            target.to_string(),
            request.clone(),
            hash.to_string(),
            key.to_string(),
            ctx.clone(),
        );
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = self.accept(&target, &request, &hash, &key, &ctx).await;
            let _ = send.send(result);
        });
        receive.await.map_err(|_| unavailable())?
    }
}
#[async_trait]
impl JobSource for ProbeService {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        let Some(job) = self.jobs.probe_job(id).await? else {
            return Ok(None);
        };
        if job.actor_id != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        let work = self.work.lock().await;
        // Un RUNNING de un proceso anterior no se reejecuta ni se transforma en
        // PASS/FAILED por tiempo. Requiere demostrar quiescencia (P130).
        Ok(Some(job.public_state(
            job.phase == JobPhase::Running && !work.active.contains(id),
        )))
    }
}
