use super::*;
use larm_adapter_host::guardian::{
    GuardianBootstrap, GuardianProcess, NativeTransformationAttempt,
};
use larm_domain::{
    ports::VolumeReservationRepository,
    transformation_execution::{TransformationAttemptGuard, TransformationReadyOwner},
};
use larm_transform::{
    output::{TransformationOutputScope, VerifiedTransformationOutput},
    output_validation::TransformationOutputValidators,
    publication::PublishedTransformationOutput,
};
use serde_json::json;

impl Service {
    pub fn spawn_consumer(
        &self,
        mut recovery: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let s = self.clone();
        tokio::spawn(async move {
            // Startup inventory must finish before creating new native owners;
            // otherwise recovery could mistake a new PREPARING launch for an
            // interrupted one. This signal is task ordering, not closure proof.
            loop {
                if s.inner.stopping.load(Ordering::SeqCst) {
                    return;
                }
                if *recovery.borrow() {
                    break;
                }
                tokio::select! {r=recovery.changed()=>if r.is_err(){tracing::error!("no terminó la recuperación de transformaciones");return;},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
            }
            while !s.inner.stopping.load(Ordering::SeqCst) {
                let delay = match Box::pin(s.run_once()).await {
                    Ok(true) => Duration::from_millis(1),
                    Ok(false) => Duration::from_millis(250),
                    Err(e) => {
                        tracing::error!(reason=%e.details.reason,"no terminó el despacho de transformación; no se infiere cierre");
                        Duration::from_secs(5)
                    }
                };
                tokio::select! {
                    _=tokio::time::sleep(delay)=>{},
                    _=s.wait_stopping()=>{},
                }
            }
        })
    }
    async fn wait_stopping(&self) {
        while !self.inner.stopping.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    /// Resuelve sólo cuando el trabajo propio pasa a CANCELLING: la señal
    /// duradera del operador. Parar el servicio no cancela trabajos.
    async fn wait_cancellation(&self, job: &str) {
        loop {
            let state: Option<String> =
                sqlx::query_scalar("SELECT state FROM transformation_job WHERE id=?")
                    .bind(job)
                    .fetch_optional(&self.inner.db.pool)
                    .await
                    .ok()
                    .flatten();
            if state.as_deref() == Some("CANCELLING") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    async fn run_once(&self) -> Result<bool, LarmError> {
        let _permit = match self.permit() {
            Ok(p) => p,
            Err(e) if e.details.reason == "transformation_backend_busy" => return Ok(false),
            Err(e) => return Err(e),
        };
        let jobs = self.jobs();
        let ids = jobs.dispatch_candidates(1).await?;
        let Some(job) = ids.first() else {
            return Ok(false);
        };
        Box::pin(self.run_job(job)).await?;
        Ok(true)
    }
    async fn run_job(&self, job: &str) -> Result<(), LarmError> {
        let jobs = self.jobs();
        let proposal = jobs.propose_native_preparation(job).await?;
        let backend = backend::Backend::for_plan(self, &proposal.claim.plan).await?;
        backend
            .check_destination(self, &proposal.claim.plan)
            .await?;
        let input = self
            .inner
            .env
            .input(
                &proposal.claim.plan.request.input_variant_id,
                Some(&proposal.claim.plan.input_artifacts),
            )
            .await?;
        backend.check_input(&proposal.claim.plan.request, &input)?;
        let space = self
            .inner
            .env
            .space(
                &proposal.claim.plan,
                backend.workspaces(&proposal.claim.plan)?,
            )
            .await?;
        self.active()?;
        let bootstrap = GuardianBootstrap::registered_transformation(
            self.inner.database.clone(),
            backend.identity.clone(),
            &proposal,
        )?;
        let owner = bootstrap.descriptor().native_owner.clone().ok_or_else(|| {
            error(
                ErrorCode::IntegrityFailed,
                "transformation_native_owner_missing",
            )
        })?;
        jobs.prepare_native_owner(&proposal, &owner).await?;
        // From this durable point, even shutdown must obtain a live witness
        // and close it, or retain the intention/resources for reconciliation.
        let executable = std::env::current_exe().map_err(|e| {
            error(
                ErrorCode::ResourceUnavailable,
                "transformation_daemon_binary_unavailable",
            )
            .with_technical(e.to_string())
        })?;
        let mut process = GuardianProcess::start(&executable, &bootstrap).await?;
        let client = process.client(&bootstrap).await?;
        let guard = NativeTransformationAttempt::attach(owner.clone()).await?;
        let ready = guard.require_ready().await;
        let claimed = if let Err(original) = ready {
            close(&guard).await?;
            let claim = jobs
                .claim_closed_preparation(&proposal, &guard)
                .await?
                .claim;
            jobs.finish_failed_attempt(&claim, &guard, &original, true)
                .await?;
            guard.release().await?;
            process.wait().await?;
            return Err(original);
        } else {
            match Box::pin(jobs.claim_with_native_owner(&proposal, &guard)).await {
                Ok(c) => c.claim,
                Err(original) => {
                    close(&guard).await?;
                    let claim = match jobs.owned_claim(job).await {
                        Ok(c) if c.owner == owner => c.claim,
                        _ => {
                            jobs.claim_closed_preparation(&proposal, &guard)
                                .await?
                                .claim
                        }
                    };
                    jobs.finish_failed_attempt(&claim, &guard, &original, true)
                        .await?;
                    guard.release().await?;
                    process.wait().await?;
                    return Err(original);
                }
            }
        };
        let result=async {
            input.recheck().await?;jobs.check_active_claim(&claimed,&guard).await?;
            let snapshot=space.observe().await?;
            for d in snapshot.demands {
                let others=larm_db::reservations::SqlVolumeReservations{pool:self.inner.db.pool.clone()}.others(&d.volume_key,Some(job)).await?;
                if d.os_free.saturating_sub(d.policy_minimum).saturating_sub(others)<d.required_bytes {return Err(error(ErrorCode::StorageInsufficient,"transformation_execution_space_insufficient"));}
            }
            let binding=&owner.binding;
            let execute=ExecuteTransformationPlan{plan_id:binding.worker_plan_id.clone(),plan_hash:binding.worker_plan_hash.clone(),approval_id:None};
            let execute_id=id();
            let verify_id=id();
            // Relé de progreso: un cliente propio retira del helper las
            // notificaciones core.progress de ESTA intento y las persiste
            // como observación pura. Abortado al salir de execute/verify.
            let progress_client=bootstrap.transformation_client().await?;
            let progress_jobs=jobs.clone();
            let progress_job=job.to_string();
            let progress_attempt=claimed.job.attempt;
            let progress_execute=execute_id.clone();
            let progress_verify=verify_id.clone();
            let progress_poller=tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    if let Ok(events)=progress_client.progress().await {
                        for event in events {
                            if event.request_id!=progress_execute && event.request_id!=progress_verify {continue;}
                            let observed=json!({
                                "request_id":event.request_id,
                                "phase":event.phase,
                                "completed_bytes":event.completed_bytes,
                                "total_bytes":event.total_bytes,
                                "message_code":event.message_code,
                                "observed_at":larm_domain::bridge::timestamp(chrono::Utc::now()),
                            });
                            let _=progress_jobs.observe_progress(&progress_job,progress_attempt,&observed).await;
                        }
                    }
                }
            });
            let executed=async {
                let execute_fut=client.execute(&execute_id,&execute,&backend.access,Some(Duration::from_secs(600)));
                tokio::pin!(execute_fut);
                let worker_receipt=tokio::select! {
                    r=&mut execute_fut=>r?,
                    _=self.wait_cancellation(job)=>{
                        let cancel_id=id();
                        let _=tokio::time::timeout(Duration::from_secs(90),client.cancel(&cancel_id,&execute_id,&backend.access,Some(Duration::from_secs(60)))).await;
                        let _=tokio::time::timeout(Duration::from_secs(120),&mut execute_fut).await;
                        return Err(cancel_observed());
                    },
                    _=self.wait_stopping()=>return Err(error(ErrorCode::AdapterCrashed,"transformation_server_stopping").retryable(true)),
                };
                jobs.mark_verifying(&claimed,&guard).await?;
                let worker_job_id=worker_receipt.job_id.clone();
                let verify_fut=client.verify_claim_output(&claimed,&verify_id,&worker_job_id,&backend.access,Some(Duration::from_secs(60)));
                tokio::pin!(verify_fut);
                let reply=tokio::select! {
                    r=&mut verify_fut=>r?,
                    _=self.wait_cancellation(job)=>{
                        let cancel_id=id();
                        let _=tokio::time::timeout(Duration::from_secs(90),client.cancel(&cancel_id,&verify_id,&backend.access,Some(Duration::from_secs(60)))).await;
                        let _=tokio::time::timeout(Duration::from_secs(120),&mut verify_fut).await;
                        return Err(cancel_observed());
                    },
                    _=self.wait_stopping()=>return Err(error(ErrorCode::AdapterCrashed,"transformation_server_stopping").retryable(true)),
                };
                Ok((worker_receipt,reply))
            }.await;
            progress_poller.abort();
            let (worker_receipt,reply)=executed?;
            close(&guard).await?;
            backend.check_destination(self,&claimed.plan).await?;
            // Namespace observado del worker: la caída con conversión parcial
            // hace que el worker REUTILICE su job (intentos worker-side), así
            // que el prefijo es `{worker_job_id}-{n}` con el mismo n en todos
            // los artefactos; nunca se acepta un namespace ajeno al recibo.
            let bundle_prefix=observed_namespace(&reply, &worker_receipt.job_id)?;
            let output=Box::pin(VerifiedTransformationOutput::capture(&claimed,&reply,&guard,TransformationOutputScope {
                root:backend.local.work_dir.join("output"),
                bundle_prefix,
            },self.inner.env.prober.clone())).await?;
            let checked=Arc::new(TransformationOutputValidators::default().validate(output).await?);
            let prepared=Arc::new(Box::pin(jobs.prepare_publication(&claimed,&guard,&input,checked.as_ref())).await?);
            let physical=Box::pin(PublishedTransformationOutput::publish(prepared.clone(),checked,&guard,self.inner.env.prober.clone(),&space,&jobs)).await?;
            Box::pin(jobs.complete_publication(&claimed,&guard,&input,&prepared,&physical)).await?;
            let confirmed=jobs.confirm_publication(&prepared).await?;
            physical.finish(&confirmed)?;
            Ok::<_,LarmError>(())
        }.await;
        if let Err(original) = result {
            close(&guard).await?;
            let recoverable = matches!(
                original.code,
                ErrorCode::AdapterCrashed
                    | ErrorCode::Timeout
                    | ErrorCode::ResourceUnavailable
                    | ErrorCode::UpstreamUnavailable
            );
            // Do not rewrite a completed publication if post-commit receipt
            // housekeeping failed. It is retriable housekeeping, not rollback.
            let current = jobs.owned_claim(job).await?;
            if current.claim.job.state != TransformationState::Completed {
                if original.details.reason == "transformation_cancel_observed"
                    || current.claim.job.state == TransformationState::Cancelling
                {
                    // La cancelación del operador cierra el intento como
                    // CANCELLED (terminal): es un cierre normal, no un fallo.
                    jobs.finish_cancelled_attempt(&claimed, &guard).await?;
                    guard.release().await?;
                    process.wait().await?;
                    return Ok(());
                }
                jobs.finish_failed_attempt(&claimed, &guard, &original, recoverable)
                    .await?;
            }
            guard.release().await?;
            process.wait().await?;
            return Err(original);
        }
        guard.release().await?;
        process.wait().await?;
        Ok(())
    }
}
/// Namespace del intento worker-side observado en el manifiesto verificado:
/// un único primer componente `{worker_job_id}-{n}` (n decimal sin ceros a la
/// izquierda) compartido por todos los artefactos.
fn observed_namespace(
    reply: &larm_adapter_host::transformation::TransformationOutputManifest,
    worker_job_id: &str,
) -> Result<String, LarmError> {
    let outside = || {
        error(
            ErrorCode::IntegrityFailed,
            "transformation_output_outside_attempt",
        )
    };
    let mut namespace: Option<String> = None;
    for artifact in &reply.manifest().artifacts {
        let first = artifact
            .relative_path
            .split('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(outside)?;
        let suffix = first
            .strip_prefix(&format!("{worker_job_id}-"))
            .filter(|rest| {
                !rest.is_empty()
                    && !rest.starts_with('0')
                    && rest.bytes().all(|byte| byte.is_ascii_digit())
            })
            .ok_or_else(outside)?;
        if suffix.is_empty() {
            return Err(outside());
        }
        match &namespace {
            None => namespace = Some(first.to_owned()),
            Some(current) if *current == first => {}
            Some(_) => return Err(outside()),
        }
    }
    namespace.ok_or_else(outside)
}
async fn close(owner: &NativeTransformationAttempt) -> Result<(), LarmError> {
    owner.observer().stop().await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if owner.require_closed().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        error(
            ErrorCode::ResourceUnavailable,
            "transformation_native_closure_unavailable",
        )
    })
}
/// Marcador interno: el consumidor observó CANCELLING y drenó el RPC en vuelo.
/// Nunca sale del daemon con este motivo: la ruta de cierre lo convierte en
/// CANCELLED terminal.
fn cancel_observed() -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "La transformación fue cancelada por el operador",
        "transformation_cancel_observed",
    )
}
