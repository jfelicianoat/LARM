//! Private native guardian composition. Qualification and queued/registered
//! ownership are read from SQLite without migrations, API or catalogue writes.
use larm_adapter_host::guardian::{
    GuardianBootstrap, GuardianDescriptor, GuardianLaunch, GuardianWorker, TransformationRpc,
};
use larm_adapter_host::transformation::{SessionIdentity, TransformationWorker};
use larm_adapter_sdk::protocol::Progress;
use larm_domain::transformation_execution::TransformationNativeOwner;
use larm_domain::{
    error::{ErrorCode, LarmError},
    ports::{ConformanceProfileRepository, MutationContext},
};
use std::io::{BufRead, Read};
use std::sync::Arc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};
mod input;

struct Qualified {
    db: larm_db::pool::Db,
    worker: TransformationWorker,
    identity: SessionIdentity,
    context: MutationContext,
    backend_actor: String,
    registration: Option<TransformationNativeOwner>,
    input: Option<input::Pins>,
    execute_started: AtomicBool,
    worker_job: OnceLock<String>,
    /// Notificaciones `core.progress` acumuladas del worker, retirables por
    /// Action::Progress. El drenador vive mientras exista el worker.
    progress: Arc<std::sync::Mutex<std::collections::VecDeque<Progress>>>,
}

fn changed() -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "La cualificación del supervisor ha cambiado",
        "transformation_guardian_qualification_changed",
    )
}

impl Qualified {
    async fn physical_current(&self) -> Result<(), LarmError> {
        let (installation, run) = larm_db::conformance::SqlAdapterConformance {
            pool: self.db.pool.clone(),
        }
        .current_for_consumer(&self.identity.installation_id)
        .await?
        .ok_or_else(changed)?;
        let profile = larm_db::conformance_profiles::SqlConformanceProfiles {
            pool: self.db.pool.clone(),
        }
        .conformance_profile(&installation.id)
        .await?
        .ok_or_else(changed)?;
        if installation.adapter_id != self.identity.adapter_id
            || run.id != self.identity.run_id
            || run.digest()? != self.identity.run_sha256
            || larm_domain::conformance::hash(&profile)? != self.identity.profile_sha256
            || profile.actor_id != self.backend_actor
            || run.actor_id != self.backend_actor
        {
            return Err(changed());
        }
        self.worker.recheck().await?;
        if let Some(input) = &self.input {
            input.recheck().await?;
        }
        Ok(())
    }
    async fn current(&self) -> Result<(), LarmError> {
        if let Some(owner) = &self.registration {
            larm_db::transformations::SqlTransformationJobs {
                pool: self.db.pool.clone(),
            }
            .check_registered_native_owner(owner, &self.context)
            .await?;
        }
        self.physical_current().await?;
        if let Some(owner) = &self.registration {
            larm_db::transformations::SqlTransformationJobs {
                pool: self.db.pool.clone(),
            }
            .check_registered_native_owner(owner, &self.context)
            .await?;
        }
        Ok(())
    }
    async fn prepared(&self) -> Result<(), LarmError> {
        if let Some(owner) = &self.registration {
            larm_db::transformations::SqlTransformationJobs {
                pool: self.db.pool.clone(),
            }
            .check_pending_native_owner(owner, &self.context)
            .await?;
        }
        self.physical_current().await?;
        if let Some(owner) = &self.registration {
            larm_db::transformations::SqlTransformationJobs {
                pool: self.db.pool.clone(),
            }
            .check_pending_native_owner(owner, &self.context)
            .await?;
        }
        if self.worker.state() != larm_adapter_host::worker::WorkerState::Ready
            || self.worker.io_drained()
        {
            return Err(changed());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl GuardianWorker for Qualified {
    fn pid(&self) -> u32 {
        self.worker.pid()
    }
    fn io_drained(&self) -> bool {
        self.worker.io_drained()
    }
    async fn close(&self) -> Result<(), LarmError> {
        self.worker.close().await
    }
    async fn ready(&self) -> Result<(), LarmError> {
        self.prepared().await
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.current().await
    }
    fn drain_progress(&self) -> Vec<Progress> {
        let mut ring = self
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ring.drain(..).collect()
    }
    async fn invoke(&self, request: &TransformationRpc) -> Result<serde_json::Value, LarmError> {
        self.current().await?;
        if let Some(owner) = &self.registration {
            request.check_registered_scope(
                &owner.binding,
                self.worker_job.get().map(String::as_str),
            )?;
            if request.is_execute() && self.execute_started.swap(true, Ordering::SeqCst) {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La ejecución del intento ya se inició",
                    "transformation_native_execute_already_started",
                ));
            }
        } else if request.is_cancel() {
            // Una sesión básica no tiene ejecución en vuelo que cancelar.
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La sesión básica no tiene ejecución que cancelar",
                "transformation_native_cancel_unregistered",
            ));
        }
        let result = request.invoke(&self.worker).await;
        if self.registration.is_some() && request.is_execute() {
            if let Ok(value) = &result {
                if let Some(id) = value.get("job_id").and_then(serde_json::Value::as_str) {
                    let _ = self.worker_job.set(id.into());
                }
            }
        }
        let current = self.current().await;
        match result {
            Err(original) => Err(original),
            Ok(value) => {
                current?;
                Ok(value)
            }
        }
    }
}

async fn start(
    launch: GuardianLaunch,
    descriptor: GuardianDescriptor,
) -> Result<Arc<dyn GuardianWorker>, LarmError> {
    match launch {
        basic @ GuardianLaunch::Basic { .. } => {
            basic.start_basic(descriptor.fence.generation).await
        }
        GuardianLaunch::Transformation {
            database,
            actor_id,
            correlation_id,
        } => {
            let context = MutationContext {
                actor_id,
                correlation_id,
            };
            larm_adapter_host::package::check_managed_path(&database)?;
            // The helper neither creates nor migrates a database and cannot
            // write catalogue/jobs/permissions through this pool.
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database)
                .read_only(true)
                .foreign_keys(true)
                .pragma("query_only", "ON")
                .busy_timeout(std::time::Duration::from_secs(5));
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(4)
                .connect_with(options)
                .await
                .map_err(|_| changed())?;
            let db = larm_db::pool::Db { pool };
            let registration = descriptor.native_owner.clone();
            let input = if let Some(owner) = &registration {
                let jobs = larm_db::transformations::SqlTransformationJobs {
                    pool: db.pool.clone(),
                };
                jobs.check_pending_native_owner(owner, &context).await?;
                let proposal = jobs.proposed_native_claim(owner, &context).await?;
                let pins = input::Pins::capture(&proposal).await?;
                jobs.check_pending_native_owner(owner, &context).await?;
                Some(pins)
            } else {
                None
            };
            let identity = descriptor.session_identity.ok_or_else(changed)?;
            let (installation, run) = larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            }
            .current_for_consumer(&identity.installation_id)
            .await?
            .ok_or_else(changed)?;
            let runner = crate::probes::runner(&db);
            let profile = larm_db::conformance_profiles::SqlConformanceProfiles {
                pool: db.pool.clone(),
            }
            .conformance_profile(&installation.id)
            .await?
            .ok_or_else(changed)?;
            let backend_actor = profile.actor_id;
            let backend_context = MutationContext {
                actor_id: backend_actor.clone(),
                correlation_id: context.correlation_id.clone(),
            };
            let worker = runner
                .transformation_worker_expected(
                    &installation,
                    &run,
                    &backend_context,
                    Some(&identity),
                )
                .await?;
            let progress: Arc<std::sync::Mutex<std::collections::VecDeque<Progress>>> =
                Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
            let mut feed = worker.progress();
            let progress_feed = progress.clone();
            tokio::spawn(async move {
                loop {
                    match feed.recv().await {
                        Ok(event) => {
                            if let Ok(mut ring) = progress_feed.lock() {
                                if ring.len() >= 64 {
                                    ring.pop_front();
                                }
                                ring.push_back(event);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
            let owner = Qualified {
                db,
                worker,
                identity,
                context,
                backend_actor,
                registration,
                input,
                execute_started: AtomicBool::new(false),
                worker_job: OnceLock::new(),
                progress,
            };
            owner.prepared().await?;
            Ok(Arc::new(owner))
        }
    }
}

async fn run() -> Result<(), String> {
    let (bootstrap_tx, bootstrap_rx) = tokio::sync::oneshot::channel();
    let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
    // Tokio's Stdin uses a blocking-pool read that cannot be cancelled. A live
    // owner keeping stdin open would then prevent Runtime::drop after Release.
    // This bounded, dedicated reader is deliberately not joined: process exit
    // closes it, while neither service retirement nor runtime shutdown waits on
    // input from a live owner. No Tokio blocking-pool stdin reads are started.
    std::thread::Builder::new()
        .name("guardian-owner-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            let mut line = Vec::new();
            let result = (&mut reader)
                .take(512 * 1024 + 2)
                .read_until(b'\n', &mut line)
                .map(|_| line)
                .map_err(|_| ());
            let _ = bootstrap_tx.send(result);
            let mut byte = [0; 1];
            let _ = reader.read(&mut byte);
            let _ = lost_tx.send(());
        })
        .map_err(|_| "guardian_input_unavailable".to_string())?;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(10), bootstrap_rx)
        .await
        .map_err(|_| "guardian_bootstrap_timeout".to_string())?
        .map_err(|_| "guardian_input_unavailable".to_string())?
        .map_err(|_| "guardian_input_unavailable".to_string())?;
    let bootstrap = GuardianBootstrap::read_from(&mut tokio::io::BufReader::new(bytes.as_slice()))
        .await
        .map_err(|error| error.details.reason)?;
    let sid = crate::bridge::current_user_sid()
        .ok_or_else(|| "guardian_identity_unavailable".to_string())?;
    let name = bootstrap.descriptor().pipe_name();
    larm_adapter_host::guardian::serve_with(
        bootstrap,
        async move {
            let _ = lost_rx.await;
        },
        |first| crate::winsec::create_pipe(&name, &sid, first, 16 * 1024),
        start,
    )
    .await
    .map_err(|error| error.details.reason)
}

pub fn main() {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "guardian_runtime_unavailable".to_string())
        .and_then(|runtime| runtime.block_on(run()));
    if let Err(reason) = result {
        eprintln!("No se pudo mantener el supervisor del proceso ({reason})");
        std::process::exit(1);
    }
}
