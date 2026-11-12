//! P028: admisión durable, workers supervisados y recuperación de imports.
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::adapter_updates::{AdapterImport, UpdateStep};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterImportRepository, AdapterImporter, JobSource, MutationContext};

use crate::imports::ImportCoordinator;
use crate::package::PackageControl;

struct Work {
    accepting: bool,
    closing: bool,
    tasks: tokio::task::JoinSet<(String, Result<(), LarmError>)>,
    controls: BTreeMap<String, PackageControl>,
}

pub struct ImportService {
    coordinator: Arc<ImportCoordinator>,
    jobs: Arc<dyn AdapterImportRepository>,
    work: tokio::sync::Mutex<Work>,
    supervisor: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "El servicio de importaciones no está disponible",
        reason,
    )
    .retryable(true)
}

impl ImportService {
    pub fn new(
        coordinator: Arc<ImportCoordinator>,
        jobs: Arc<dyn AdapterImportRepository>,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            coordinator,
            jobs,
            work: tokio::sync::Mutex::new(Work {
                accepting: false,
                closing: false,
                tasks: tokio::task::JoinSet::new(),
                controls: BTreeMap::new(),
            }),
            supervisor: std::sync::Mutex::new(None),
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
                if !work.accepting {
                    continue;
                }
                if let Err(e) = service.start_next(&mut work).await {
                    tracing::warn!(reason=%e.details.reason,"la cola de importaciones se conserva para reintentar");
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
        for control in work.controls.values() {
            control.cancel();
        }
    }

    pub async fn finish_shutdown(&self) {
        self.begin_shutdown().await;
        let supervisor = self.supervisor.lock().ok().and_then(|mut h| h.take());
        if let Some(supervisor) = supervisor {
            let _ = supervisor.await;
        }
        let mut work = self.work.lock().await;
        while let Some(result) = work.tasks.join_next().await {
            Self::finished(&mut work, result);
        }
    }

    async fn start_next(&self, work: &mut Work) -> Result<(), LarmError> {
        while let Some(result) = work.tasks.try_join_next() {
            Self::finished(work, result);
        }
        if work.tasks.is_empty() {
            if let Some(record) = self.jobs.queued_import().await? {
                if record.step != UpdateStep::Queued {
                    return Ok(());
                }
                let (coordinator, id, control) = (
                    self.coordinator.clone(),
                    record.job_id,
                    PackageControl::default(),
                );
                work.controls.insert(id.clone(), control.clone());
                work.tasks.spawn(async move {
                    let result = coordinator.execute(id.clone(), control).await;
                    (id, result)
                });
            }
        }
        Ok(())
    }

    fn finished(
        work: &mut Work,
        result: Result<(String, Result<(), LarmError>), tokio::task::JoinError>,
    ) {
        match result {
            Ok((id, result)) => {
                work.controls.remove(&id);
                if let Err(e) = result {
                    tracing::warn!(job_id=%id,reason=%e.details.reason,"import terminada con error o pendiente de reconciliar");
                }
            }
            Err(e) => {
                tracing::error!(error=%e,"task de importación terminado inesperadamente; journal conservado")
            }
        }
    }

    async fn accept(
        &self,
        request: &AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        let mut work = self.work.lock().await;
        if !work.accepting {
            return Err(unavailable("adapter_import_service_closing"));
        }
        let (receipt, _new) = self
            .jobs
            .enqueue_import(request, body_hash, key, ctx)
            .await?;
        self.start_next(&mut work).await?;
        Ok(receipt)
    }
}

#[async_trait]
impl AdapterImporter for ImportService {
    async fn request_import(
        self: Arc<Self>,
        request: &AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        let (request, body_hash, key, ctx) = (
            request.clone(),
            body_hash.to_string(),
            key.to_string(),
            ctx.clone(),
        );
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = self.accept(&request, &body_hash, &key, &ctx).await;
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| unavailable("adapter_import_acceptance_interrupted"))?
    }
}

#[async_trait]
impl JobSource for ImportService {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        Ok(self
            .jobs
            .import_job(id)
            .await?
            .filter(|r| r.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
            .map(|r| r.public_state()))
    }
}
