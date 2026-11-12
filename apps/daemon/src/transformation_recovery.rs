//! Startup reconciliation uses retained native witnesses, including launches
//! interrupted before RUNNING registration. Missing witnesses retain resources.
use larm_adapter_host::{daemon_owner::DaemonOwnership, guardian::NativeTransformationAttempt};
use larm_db::transformations::SqlTransformationJobs;
use larm_domain::{
    error::{ErrorCode, LarmError},
    transformation_execution::{
        TransformationAttemptGuard, TransformationClaimProposal, TransformationNativePreparation,
    },
    transformations::TransformationState,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

/// Época del arranque actual: prueba del sistema operativo de que ningún
/// proceso ni handle de una sesión anterior sobrevive a un reinicio.
fn system_boot() -> u64 {
    sysinfo::System::boot_time()
}

async fn recover_one(
    jobs: &SqlTransformationJobs,
    item: TransformationNativePreparation,
    scope: Arc<DaemonOwnership>,
    stop: watch::Receiver<bool>,
) -> Result<(), LarmError> {
    scope.recheck()?;
    ensure_active(&stop)?;
    // Política de testigo definitivamente perdido: una intención de una
    // sesión de arranque anterior no puede tener proceso ni handle vivo.
    // REGISTERED cierra su intento sin testigo (WORKER_INTERRUPTED, recursos
    // liberados); PREPARING se retira y el trabajo vuelve a despacharse.
    if item.owner.boot_session != 0 && item.owner.boot_session != system_boot() {
        scope.recheck()?;
        ensure_active(&stop)?;
        let previous_boot = item.owner.boot_session;
        if item.registered {
            let registered = jobs.owned_claim(&item.owner.fence.job_id).await?;
            if registered.owner != item.owner || registered.claim.context != item.context {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "El propietario de la transformación cambió",
                    "transformation_recovery_owner_changed",
                ));
            }
            jobs.finish_interrupted_system_restart(&registered.claim, previous_boot, system_boot())
                .await?;
        } else {
            jobs.close_preparation_system_restart(&item.owner, system_boot())
                .await?;
        }
        return Ok(());
    }
    let registered = if item.registered {
        let registered = jobs.owned_claim(&item.owner.fence.job_id).await?;
        if registered.owner != item.owner || registered.claim.context != item.context {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El propietario de la transformación cambió",
                "transformation_recovery_owner_changed",
            ));
        }
        if !matches!(
            registered.claim.job.state,
            TransformationState::Failed
                | TransformationState::FailedRecoverable
                | TransformationState::Cancelling
        ) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El intento no está marcado como interrumpido o en cancelación",
                "transformation_recovery_not_interrupted",
            ));
        }
        Some(registered.claim)
    } else {
        None
    };
    let queued = if registered.is_none() {
        Some(
            jobs.proposed_native_claim(&item.owner, &item.context)
                .await?,
        )
    } else {
        None
    };
    let owner = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            ensure_active(&stop)?;
            match NativeTransformationAttempt::attach(item.owner.clone()).await {
                Ok(owner) => return Ok(owner),
                Err(error)
                    if matches!(
                        error.details.reason.as_str(),
                        "guardian_io_unavailable" | "guardian_observation_timeout"
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo contactar con el testigo nativo",
            "transformation_recovery_witness_unavailable",
        )
    })??;
    owner.observer().stop().await?;
    wait_closed(&owner, &stop).await?;
    scope.recheck()?;
    ensure_active(&stop)?;
    let claim = if let Some(claim) = registered {
        claim
    } else {
        let proposal = TransformationClaimProposal {
            queued_row_version: item.owner.queued_row_version,
            claim: queued.ok_or_else(|| {
                LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "Falta la preparación observada",
                    "transformation_recovery_preparation_missing",
                )
            })?,
        };
        jobs.claim_closed_preparation(&proposal, &owner)
            .await?
            .claim
    };
    if claim.job.state == TransformationState::Cancelling {
        // La cancelación es la intención duradera del operador: el intento
        // termina CANCELLED (terminal), no como fallo recuperable.
        jobs.finish_cancelled_attempt(&claim, &owner).await?;
    } else {
        let error = LarmError::new(
            ErrorCode::AdapterCrashed,
            "La preparación o ejecución se interrumpió antes de completarse",
            "transformation_owner_interrupted",
        )
        .retryable(true);
        jobs.finish_failed_attempt(&claim, &owner, &error, true)
            .await?;
    }
    // Keep the witness until its exact job/journal/resources commit is durable.
    owner.release().await
}

fn ensure_active(stop: &watch::Receiver<bool>) -> Result<(), LarmError> {
    if *stop.borrow() || stop.has_changed().is_err() {
        return Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "La reconciliación se detiene con el servidor",
            "transformation_recovery_stopping",
        ));
    }
    Ok(())
}
async fn wait_closed(
    owner: &NativeTransformationAttempt,
    stop: &watch::Receiver<bool>,
) -> Result<(), LarmError> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            ensure_active(stop)?;
            if owner.require_closed().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo acreditar el cierre nativo",
            "transformation_recovery_closure_unavailable",
        )
    })?
}
async fn retire_one(
    owner: larm_domain::transformation_execution::TransformationNativeOwner,
    scope: Arc<DaemonOwnership>,
    stop: watch::Receiver<bool>,
) -> Result<(), LarmError> {
    scope.recheck()?;
    ensure_active(&stop)?;
    // Core already closed this exact owner (including archived attempts).
    // Absence here performs NO resource mutation and grants no closure proof.
    let observer = match NativeTransformationAttempt::attach(owner).await {
        Ok(observer) => observer,
        Err(error) if error.details.reason == "guardian_io_unavailable" => return Ok(()),
        Err(error) => return Err(error),
    };
    observer.observer().stop().await?;
    wait_closed(&observer, &stop).await?;
    scope.recheck()?;
    observer.release().await
}

/// Owns shutdown signalling while every cleanup task retains the same native
/// host/database anchors. Dropping the supervisor requests cooperative stop.
pub struct Supervisor {
    stop: watch::Sender<bool>,
    done: watch::Receiver<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Supervisor {
    pub fn completion(&self) -> watch::Receiver<bool> {
        self.done.clone()
    }
    pub fn signal(&self) -> watch::Sender<bool> {
        self.stop.clone()
    }
    pub async fn shutdown(mut self) -> Result<(), LarmError> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| {
                LarmError::new(
                    ErrorCode::Internal,
                    "No terminó el supervisor de recuperación",
                    "transformation_recovery_supervisor_failed",
                )
            })?;
        }
        Ok(())
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub fn start(jobs: Arc<SqlTransformationJobs>, scope: DaemonOwnership) -> Supervisor {
    let (stop, receiver) = watch::channel(false);
    let (finished, done) = watch::channel(false);
    let task = tokio::spawn(async move {
        if let Err(error) = recover(&jobs, Arc::new(scope), receiver).await {
            tracing::error!(reason=%error.details.reason,"reconciliación transformation incompleta; se conserva la evidencia durable");
        }
        finished.send_replace(true);
    });
    Supervisor {
        stop,
        done,
        task: Some(task),
    }
}
enum Work {
    Interrupted(TransformationNativePreparation),
    Retire(larm_domain::transformation_execution::TransformationNativeOwner),
}
async fn recover(
    jobs: &SqlTransformationJobs,
    scope: Arc<DaemonOwnership>,
    stop: watch::Receiver<bool>,
) -> Result<(), LarmError> {
    let mut first_error = None;
    for retiring in [false, true] {
        let mut cursor = None;
        loop {
            if ensure_active(&stop).is_err() {
                return first_error.map_or(Ok(()), Err);
            }
            scope.recheck()?;
            let page: Vec<Work> = if retiring {
                jobs.native_retirements(cursor.as_deref(), 64)
                    .await?
                    .into_iter()
                    .map(Work::Retire)
                    .collect()
            } else {
                jobs.native_preparations(cursor.as_deref(), 64)
                    .await?
                    .into_iter()
                    .map(Work::Interrupted)
                    .collect()
            };
            if page.is_empty() {
                break;
            }
            let mut tasks = tokio::task::JoinSet::new();
            for item in page {
                if ensure_active(&stop).is_err() {
                    break;
                }
                let owner = match &item {
                    Work::Interrupted(i) => &i.owner,
                    Work::Retire(o) => o,
                };
                cursor = Some(owner.session_id.clone());
                let job_id = owner.fence.job_id.clone();
                let repo = jobs.clone();
                let task_scope = scope.clone();
                let task_stop = stop.clone();
                tasks.spawn(async move {
                    let result = match item {
                        Work::Interrupted(item) => {
                            recover_one(&repo, item, task_scope, task_stop).await
                        }
                        Work::Retire(owner) => retire_one(owner, task_scope, task_stop).await,
                    };
                    (job_id, result)
                });
            }
            while let Some(result) = tasks.join_next().await {
                let (job_id, result) = result.map_err(|_| {
                    LarmError::new(
                        ErrorCode::Internal,
                        "No terminó la reconciliación del propietario",
                        "transformation_recovery_task_failed",
                    )
                })?;
                if let Err(error) = result {
                    tracing::error!(%job_id,reason=%error.details.reason,"no se completó la reconciliación del propietario; el error no acredita cierre");
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
