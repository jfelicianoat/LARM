//! P024: aceptación independiente del request HTTP y workers supervisados.
//! Shutdown cierra admisión, señala cancelación y espera el IO antes de salir.
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::adapter_updates::{AdapterImport, UpdateStep};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterUpdateRepository, AdapterUpdater, JobSource, MutationContext};

use crate::package::PackageControl;
use crate::updates::UpdateCoordinator;

struct Work {
    accepting: bool,
    closing: bool,
    tasks: tokio::task::JoinSet<(String, Result<(), LarmError>)>,
    controls: BTreeMap<String, PackageControl>,
}
pub struct UpdateService {
    coordinator: Arc<UpdateCoordinator>,
    jobs: Arc<dyn AdapterUpdateRepository>,
    work: tokio::sync::Mutex<Work>,
    supervisor: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "El servicio de actualizaciones no está disponible",
        reason,
    )
    .retryable(true)
}
impl UpdateService {
    pub fn new(
        coordinator: Arc<UpdateCoordinator>,
        jobs: Arc<dyn AdapterUpdateRepository>,
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
                    tracing::warn!(reason=%e.details.reason,"la cola de actualizaciones se conserva para reintentar");
                }
            }
        });
        if let Ok(mut supervisor) = service.supervisor.lock() {
            *supervisor = Some(task);
        }
        service
    }
    /// Activar sólo después de que el listener y las demás rutas estén listos.
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
        // El coordinador serializa una raíz: un solo hilo activo, el resto
        // queda QUEUED en SQLite. No hay un thread bloqueado por cada request.
        if work.tasks.is_empty() {
            if let Some(record) = self.jobs.queued_update().await? {
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
                    tracing::warn!(job_id=%id,reason=%e.details.reason,"actualización terminada con error o pendiente de reconciliar");
                }
            }
            Err(e) => {
                tracing::error!(error=%e,"task de actualización terminado inesperadamente; journal conservado")
            }
        }
    }
    async fn accept(
        &self,
        target: &str,
        request: &AdapterImport,
        body_hash: &str,
        key: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        let mut work = self.work.lock().await;
        if !work.accepting {
            return Err(unavailable("adapter_service_closing"));
        }
        let (receipt, _new) = self
            .jobs
            .enqueue_update(target, request, body_hash, key, ctx)
            .await?;
        // Un replay reanuda QUEUED si el ack se perdió antes de registrar el
        // worker. Nunca arranca dos workers del mismo job ni reinicia RUNNING.
        // Si el ack del commit o esta lectura fallan, el supervisor vuelve a
        // descubrir QUEUED. La admisión no depende de un request aún conectado.
        self.start_next(&mut work).await?;
        Ok(receipt)
    }
}

#[async_trait]
impl AdapterUpdater for UpdateService {
    async fn request_update(
        self: Arc<Self>,
        target: &str,
        request: &AdapterImport,
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
        // El dueño de la aceptación es este task, no el timeout/desconexión
        // del request. El worker queda registrado antes de enviar el receipt.
        tokio::spawn(async move {
            let result = self.accept(&target, &request, &body_hash, &key, &ctx).await;
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| unavailable("adapter_acceptance_interrupted"))?
    }
}
#[async_trait]
impl JobSource for UpdateService {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        Ok(self
            .jobs
            .update_job(id)
            .await?
            .filter(|r| r.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
            .map(|r| r.public_state()))
    }
}
