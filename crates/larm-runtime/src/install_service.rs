//! P031: planifica una instalación y la admite como job durable.
//!
//! La planificación usa el worker retenido por P026. Sólo el servidor añade
//! los bindings de catálogo y el destino determinista; SQLite vuelve a validar
//! el plan y realiza locks, reservas, journal, evento e idempotencia en una tx.

use crate::{install::InstallSpec, nuget_lock};
use async_trait::async_trait;
use larm_domain::{
    adapters::AdapterInstallation,
    bridge::{timestamp, JobPhase, JobReceipt},
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    runtime::{
        InstallationStage, InstallationState, InstallationStrategy, RegistrationMode,
        RuntimeInstallation, RuntimeOwnership,
    },
    runtime_execution::{
        RuntimeDiscoveryHost, RuntimeDiscoverySession, RuntimeExecutionContext,
        RuntimeExecutionRepository,
    },
    runtime_install::{
        request_from_plan, validate_plan_reply, ExecutePlan, InstallStatusRequest,
        ManagedRuntimeInstallStore, RuntimeInstallAdmission, RuntimeInstallJob,
        RuntimeInstallJobRepository, RuntimeInstallSubmitter,
    },
    runtime_ports::RuntimeRepository,
    runtime_profiles::{RuntimeProfileLockRepository, RuntimeProfileQualificationRepository},
};
use serde_json::{Map, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{watch, Mutex as AsyncMutex};

const IDEMPOTENCY_DAYS: i64 = 30;

pub struct RuntimeInstallService {
    pub runtimes: Arc<dyn RuntimeRepository>,
    pub execution: Arc<dyn RuntimeExecutionRepository>,
    pub profiles: Arc<dyn RuntimeProfileLockRepository>,
    pub qualifications: Arc<dyn RuntimeProfileQualificationRepository>,
    pub host: Arc<dyn RuntimeDiscoveryHost>,
    pub jobs: Arc<dyn RuntimeInstallJobRepository>,
    /// Ausente sólo en composición de plan-only; el consumidor durable exige
    /// almacenamiento gestionado antes de arrancar cualquier worker.
    pub artifact_store: Option<Arc<dyn ManagedRuntimeInstallStore>>,
    active_installs: ActiveInstallRegistry,
}

type ActiveInstallRegistry = Arc<AsyncMutex<HashMap<String, Arc<ActiveInstall>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveCancelState {
    Idle,
    Requested,
    Confirmed,
    Rejected,
    Finished,
}

struct ActiveInstall {
    session: Arc<dyn RuntimeDiscoverySession>,
    cancel_state: watch::Sender<ActiveCancelState>,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Solicitud de instalación runtime no válida",
        reason,
    )
}

fn conflict(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "No se pudo planificar la instalación runtime",
        reason,
    )
}

fn permanent_publish_error(error: &LarmError) -> bool {
    matches!(
        &error.code,
        ErrorCode::ValidationFailed | ErrorCode::Conflict | ErrorCode::IntegrityFailed
    ) || error.details.reason == "runtime_install_workspace_missing"
}

fn profile_allows(
    profile: &larm_domain::runtime_profiles::RuntimeProfileLock,
    strategy: InstallationStrategy,
) -> Result<(), LarmError> {
    let strategy = strategy.as_str();
    let values = profile
        .fields
        .get("installation_strategies")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("runtime_profile_installation_strategies_invalid"))?;
    if !values.iter().any(|value| value.as_str() == Some(strategy)) {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El perfil runtime no anuncia la estrategia solicitada",
            "runtime_install_strategy_not_in_profile",
        ));
    }
    if profile
        .fields
        .get("install_strategy")
        .and_then(Value::as_str)
        .is_some_and(|declared| declared != strategy)
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La estrategia no coincide con la fijada por el perfil runtime",
            "runtime_install_strategy_profile_mismatch",
        ));
    }
    Ok(())
}

async fn load_install_qualified_profile(
    profiles: &dyn RuntimeProfileLockRepository,
    qualifications: &dyn RuntimeProfileQualificationRepository,
    lock_hash: &str,
    strategy: InstallationStrategy,
) -> Result<larm_domain::runtime_profiles::RuntimeProfileLock, LarmError> {
    let profile = profiles
        .get_by_hash(lock_hash)
        .await?
        .ok_or_else(|| conflict("runtime_install_profile_lock_missing"))?;
    let qualification = qualifications
        .get_by_lock_hash(lock_hash)
        .await?
        .ok_or_else(|| {
            LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El perfil runtime aún no tiene evidencia real de cualificación",
                "runtime_profile_unqualified",
            )
        })?;
    qualification
        .validate_for_lock(&profile)
        .map_err(|_| conflict("runtime_profile_qualification_binding_invalid"))?;
    if !qualification
        .strategy_test_ids
        .contains_key(strategy.as_str())
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La estrategia runtime no tiene pruebas reales de cualificación",
            "runtime_profile_strategy_unqualified",
        ));
    }
    Ok(profile)
}

fn dependencies_lock_hash(
    profile: &larm_domain::runtime_profiles::RuntimeProfileLock,
    strategy: InstallationStrategy,
) -> Result<Option<String>, LarmError> {
    let isolated = matches!(
        strategy,
        InstallationStrategy::Pip
            | InstallationStrategy::Uv
            | InstallationStrategy::Conda
            | InstallationStrategy::Mamba
            | InstallationStrategy::PythonVenv
    );
    let nuget = strategy == InstallationStrategy::Nuget;
    let Some(lock) = profile.fields.get("dependencies_lock") else {
        return Err(invalid("runtime_profile_dependencies_lock_missing"));
    };
    if lock.is_null() {
        if isolated || nuget {
            return Err(invalid("runtime_profile_dependencies_lock_missing"));
        }
        return Ok(None);
    }
    let empty = match lock {
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    };
    if empty {
        return Err(invalid("runtime_profile_dependencies_lock_empty"));
    }
    if nuget {
        let architecture = profile
            .fields
            .get("architecture")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control));
        if architecture.is_none() {
            return Err(invalid("runtime_nuget_architecture_missing"));
        }
        nuget_lock::validate(lock)?;
    }
    let hash = larm_domain::canonical::sha256_hex(lock)
        .map_err(|_| invalid("runtime_profile_dependencies_lock_invalid"))?;
    Ok(Some(hash))
}

fn supported_strategy(strategy: InstallationStrategy) -> Result<(), LarmError> {
    match strategy {
        InstallationStrategy::Winget | InstallationStrategy::MsiManaged => Err(LarmError::new(
            ErrorCode::ApprovalRequired,
            "La estrategia puede tener efectos globales no descritos por el lock actual",
            "runtime_install_effects_unqualified",
        )),
        InstallationStrategy::ExternalAdoption => Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La adopción externa todavía no admite una reserva durable sin ruta local",
            "runtime_external_adoption_admission_unavailable",
        )),
        InstallationStrategy::Custom => Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La estrategia CUSTOM requiere un perfil de efectos explícito",
            "runtime_custom_install_effects_unqualified",
        )),
        _ => Ok(()),
    }
}

fn verify_adapter_binding(
    installed: &AdapterInstallation,
    adapter_installation_id: &str,
    package_hash: &str,
) -> Result<Vec<InstallationStrategy>, LarmError> {
    if installed.id != adapter_installation_id || installed.package_hash != package_hash {
        return Err(conflict("runtime_install_adapter_binding_changed"));
    }
    if installed.manifest.get("id").and_then(Value::as_str) != Some(installed.adapter_id.as_str())
        || installed
            .manifest
            .get("package_sha256")
            .and_then(Value::as_str)
            != Some(installed.package_hash.as_str())
        || installed.manifest.get("family").and_then(Value::as_str) != Some("runtime")
    {
        return Err(conflict("runtime_install_adapter_binding_changed"));
    }
    let strategies = installed
        .manifest
        .get("installation_strategies")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("runtime_install_manifest_invalid"))?;
    strategies
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|_| invalid("runtime_install_manifest_invalid"))
        })
        .collect()
}

struct PlanAdmission<'a> {
    jobs: &'a dyn RuntimeInstallJobRepository,
    session: &'a dyn RuntimeDiscoverySession,
    request: &'a larm_domain::runtime_install::InstallRequest,
    principal_id: &'a str,
    idempotency_key: &'a str,
    request_hash: &'a str,
    ctx: &'a MutationContext,
    context: &'a RuntimeExecutionContext,
    version: larm_domain::runtime::RuntimeVersion,
    declared_strategies: &'a [InstallationStrategy],
    dependencies_lock_hash: Option<String>,
}

struct PlanInputs {
    context: RuntimeExecutionContext,
    version: larm_domain::runtime::RuntimeVersion,
    declared_strategies: Vec<InstallationStrategy>,
    dependencies_lock_hash: Option<String>,
}

async fn build_installation_plan(
    session: &dyn RuntimeDiscoverySession,
    request: &larm_domain::runtime_install::InstallRequest,
    context: &RuntimeExecutionContext,
    version: &larm_domain::runtime::RuntimeVersion,
    declared_strategies: &[InstallationStrategy],
    dependencies_lock_hash: Option<String>,
) -> Result<(RuntimeInstallation, larm_domain::artifacts::Plan), LarmError> {
    session.recheck().await?;
    let mut plan = session.plan_install(request).await?;
    validate_plan_reply(request, &plan, chrono::Utc::now())?;
    let adapter_plan_hash = plan.plan_hash.clone();
    session.recheck().await?;

    let relative_destination = format!(
        "runtimes/{}/{}/{}",
        request.runtime_id, version.id, request.profile_lock_hash
    );
    let environment_prefix = matches!(
        request.strategy,
        InstallationStrategy::Pip
            | InstallationStrategy::Uv
            | InstallationStrategy::Conda
            | InstallationStrategy::Mamba
            | InstallationStrategy::PythonVenv
    )
    .then(|| format!("{relative_destination}/env"));
    let spec = InstallSpec {
        request: request.clone(),
        runtime_version_id: version.id.clone(),
        adapter_plan_hash,
        adapter_installation_id: context.installation.id.clone(),
        adapter_package_hash: context.installation.package_hash.clone(),
        registration_mode: RegistrationMode::Managed,
        ownership: RuntimeOwnership::LarmManaged,
        relative_destination: Some(relative_destination.clone()),
        environment_prefix,
        dependencies_lock_hash,
        global_effects: false,
        reboot_required: false,
    };
    plan = spec.bind(
        request,
        declared_strategies,
        plan,
        &context.installation.adapter_id,
    )?;
    let now = chrono::Utc::now();
    let installation = RuntimeInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: Map::new(),
        runtime_version_id: version.id.clone(),
        storage_location_id: Some(request.storage_location_id.clone()),
        relative_path: Some(relative_destination),
        strategy: request.strategy,
        external_host_id: None,
        state: InstallationState::InstallPlanned,
        ownership: RuntimeOwnership::LarmManaged,
        lock_hash: Some(request.profile_lock_hash.clone()),
        stage: InstallationStage::InstallPlanned,
        registration_mode: RegistrationMode::Managed,
    };
    Ok((installation, plan))
}

async fn preview_with_session(
    host: &dyn RuntimeDiscoveryHost,
    request: &larm_domain::runtime_install::InstallRequest,
    ctx: &MutationContext,
    inputs: PlanInputs,
) -> Result<larm_domain::artifacts::Plan, LarmError> {
    let expected = inputs.context.binding()?;
    let session = host.open(&inputs.context, ctx).await?;
    if session.binding() != &expected {
        if session.close().await.is_err() {
            tracing::warn!("no se pudo cerrar una sesión de planificación de runtime no enlazada");
        }
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "La sesión pertenece a otro runtime",
            "runtime_install_session_mismatch",
        ));
    }
    let planned = async {
        let (_, plan) = build_installation_plan(
            session.as_ref(),
            request,
            &inputs.context,
            &inputs.version,
            &inputs.declared_strategies,
            inputs.dependencies_lock_hash,
        )
        .await?;
        Ok(plan)
    }
    .await;
    let closed = session.close().await;
    match (planned, closed) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(plan), Ok(())) => Ok(plan),
    }
}

async fn plan_and_enqueue(admission: PlanAdmission<'_>) -> Result<JobReceipt, LarmError> {
    let PlanAdmission {
        jobs,
        session,
        request,
        principal_id,
        idempotency_key,
        request_hash,
        ctx,
        context,
        version,
        declared_strategies,
        dependencies_lock_hash,
    } = admission;
    let (installation, plan) = build_installation_plan(
        session,
        request,
        context,
        &version,
        declared_strategies,
        dependencies_lock_hash,
    )
    .await?;
    if plan.approval_required {
        return Err(LarmError::new(
            ErrorCode::ApprovalRequired,
            "El plan requiere una aprobación que no forma parte de esta solicitud",
            "runtime_install_approval_required",
        ));
    }
    let expires = timestamp(chrono::Utc::now() + chrono::Duration::days(IDEMPOTENCY_DAYS));
    let (receipt, _) = jobs
        .enqueue_plan(RuntimeInstallAdmission {
            installation: &installation,
            plan: &plan,
            principal_id,
            idempotency_key,
            request_hash,
            idempotency_expires_at: &expires,
            ctx,
            session,
        })
        .await?;
    Ok(receipt)
}

#[async_trait]
impl RuntimeInstallSubmitter for RuntimeInstallService {
    async fn submit(
        &self,
        request: &larm_domain::runtime_install::InstallRequest,
        principal_id: &str,
        idempotency_key: &str,
        request_hash: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        request.validate()?;
        if principal_id.is_empty() || principal_id != ctx.actor_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El actor de la instalación no coincide con la credencial",
                "runtime_install_actor_mismatch",
            ));
        }
        if let Some(receipt) = self
            .jobs
            .replay(principal_id, idempotency_key, request_hash)
            .await?
        {
            return Ok(receipt);
        }
        let inputs = self.resolve_plan_inputs(request, ctx, true).await?;
        let expected = inputs.context.binding()?;
        let session = self.host.open(&inputs.context, ctx).await?;
        if session.binding() != &expected {
            if session.close().await.is_err() {
                tracing::warn!(
                    "no se pudo cerrar una sesión de planificación de runtime no enlazada"
                );
            }
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La sesión pertenece a otro runtime",
                "runtime_install_session_mismatch",
            ));
        }
        let result = plan_and_enqueue(PlanAdmission {
            jobs: self.jobs.as_ref(),
            session: session.as_ref(),
            request,
            principal_id,
            idempotency_key,
            request_hash,
            ctx,
            context: &inputs.context,
            version: inputs.version,
            declared_strategies: &inputs.declared_strategies,
            dependencies_lock_hash: inputs.dependencies_lock_hash,
        })
        .await;
        if session.close().await.is_err() {
            tracing::warn!("no se pudo cerrar la sesión de planificación de runtime");
        }
        result
    }

    async fn cancel(
        &self,
        job_id: &str,
        principal_id: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        if principal_id.is_empty() || principal_id != ctx.actor_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El actor de la cancelación no coincide con la credencial",
                "runtime_install_actor_mismatch",
            ));
        }
        let job = self.jobs.get_job(job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            )
        })?;
        if job.actor_id != principal_id {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            ));
        }
        let job = match job.state {
            JobPhase::Queued => {
                self.jobs
                    .cancel_queued_job(job_id, principal_id, ctx)
                    .await?
            }
            JobPhase::Running => {
                let active = self
                    .active_installs
                    .lock()
                    .await
                    .get(job_id)
                    .cloned()
                    .ok_or_else(|| {
                        LarmError::new(
                            ErrorCode::InvalidStateTransition,
                            "No hay una sesión activa que pueda confirmar la cancelación",
                            "runtime_install_cancel_session_unavailable",
                        )
                    })?;
                let mut state = active.cancel_state.subscribe();
                let should_request = active.cancel_state.send_if_modified(|current| {
                    if *current == ActiveCancelState::Idle {
                        *current = ActiveCancelState::Requested;
                        true
                    } else {
                        false
                    }
                });
                let state_after_request = *state.borrow();
                match state_after_request {
                    ActiveCancelState::Requested => {}
                    ActiveCancelState::Confirmed => {
                        return Ok(job_receipt(
                            self.jobs
                                .get_job(job_id)
                                .await?
                                .unwrap_or_else(|| job.clone()),
                        ));
                    }
                    ActiveCancelState::Rejected | ActiveCancelState::Finished => {
                        return Err(cancel_not_accepted());
                    }
                    ActiveCancelState::Idle => return Err(cancel_not_accepted()),
                }
                if should_request {
                    match active.session.cancel_install(job_id).await {
                        Ok(true) => {
                            active
                                .cancel_state
                                .send_replace(ActiveCancelState::Confirmed);
                        }
                        Ok(false) => {
                            active
                                .cancel_state
                                .send_replace(ActiveCancelState::Rejected);
                        }
                        Err(error) => {
                            active
                                .cancel_state
                                .send_replace(ActiveCancelState::Rejected);
                            return Err(error);
                        }
                    }
                }
                loop {
                    let observed_state = *state.borrow_and_update();
                    match observed_state {
                        ActiveCancelState::Confirmed => {
                            break self
                                .jobs
                                .get_job(job_id)
                                .await?
                                .unwrap_or_else(|| job.clone());
                        }
                        ActiveCancelState::Rejected => return Err(cancel_not_accepted()),
                        ActiveCancelState::Finished => {
                            return Err(cancel_not_accepted());
                        }
                        ActiveCancelState::Idle | ActiveCancelState::Requested => {
                            state.changed().await.map_err(|_| {
                                LarmError::new(
                                    ErrorCode::ResourceUnavailable,
                                    "La sesión de instalación terminó antes de confirmar la cancelación",
                                    "runtime_install_cancel_session_ended",
                                )
                            })?;
                        }
                    }
                }
            }
            JobPhase::Completed | JobPhase::Failed | JobPhase::Cancelled => job,
        };
        Ok(job_receipt(job))
    }
}

fn job_receipt(job: RuntimeInstallJob) -> JobReceipt {
    JobReceipt {
        job_id: job.job_id.clone(),
        status_url: format!("/api/v1/jobs/{}", job.job_id),
        state: job.state,
    }
}

fn cancel_not_accepted() -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "El worker no aceptó cancelar la instalación; el job continúa activo",
        "runtime_install_cancel_not_accepted",
    )
}

impl RuntimeInstallService {
    pub fn new(
        runtimes: Arc<dyn RuntimeRepository>,
        execution: Arc<dyn RuntimeExecutionRepository>,
        profiles: Arc<dyn RuntimeProfileLockRepository>,
        qualifications: Arc<dyn RuntimeProfileQualificationRepository>,
        host: Arc<dyn RuntimeDiscoveryHost>,
        jobs: Arc<dyn RuntimeInstallJobRepository>,
        artifact_store: Option<Arc<dyn ManagedRuntimeInstallStore>>,
    ) -> Self {
        Self {
            runtimes,
            execution,
            profiles,
            qualifications,
            host,
            jobs,
            artifact_store,
            active_installs: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    async fn resolve_plan_inputs(
        &self,
        request: &larm_domain::runtime_install::InstallRequest,
        ctx: &MutationContext,
        require_qualified: bool,
    ) -> Result<PlanInputs, LarmError> {
        supported_strategy(request.strategy)?;
        let profile = if require_qualified {
            load_install_qualified_profile(
                self.profiles.as_ref(),
                self.qualifications.as_ref(),
                &request.profile_lock_hash,
                request.strategy,
            )
            .await?
        } else {
            self.profiles
                .get_by_hash(&request.profile_lock_hash)
                .await?
                .ok_or_else(|| conflict("runtime_install_profile_lock_missing"))?
        };
        if profile.key.upstream_version != request.upstream_version {
            return Err(conflict("runtime_install_profile_lock_binding_changed"));
        }
        profile_allows(&profile, request.strategy)?;
        let dependencies_lock_hash = dependencies_lock_hash(&profile, request.strategy)?;

        let version = self
            .runtimes
            .version_by_runtime_and_string(&request.runtime_id, &request.upstream_version)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "La versión exacta no está en el catálogo runtime",
                    "runtime_install_version_not_found",
                )
            })?;
        let context = self
            .execution
            .prepare_execution(&request.runtime_id, ctx)
            .await?;
        if context.runtime.id != request.runtime_id
            || profile.key.runtime != context.adapter.runtime_family
        {
            return Err(conflict("runtime_install_profile_lock_binding_changed"));
        }
        let declared_strategies = verify_adapter_binding(
            &context.installation,
            &context.adapter.adapter_installation_id,
            &context.installation.package_hash,
        )?;

        Ok(PlanInputs {
            context,
            version,
            declared_strategies,
            dependencies_lock_hash,
        })
    }

    /// Genera y valida una previsualización local sin crear jobs, instalaciones,
    /// reservas, eventos ni claves de idempotencia.
    pub async fn plan_only(
        &self,
        request: &larm_domain::runtime_install::InstallRequest,
        ctx: &MutationContext,
    ) -> Result<larm_domain::artifacts::Plan, LarmError> {
        request.validate()?;
        if ctx.actor_id.is_empty()
            || ctx.actor_id.len() > 256
            || ctx.actor_id.chars().any(char::is_control)
        {
            return Err(invalid("runtime_execution_actor_invalid"));
        }
        let inputs = self.resolve_plan_inputs(request, ctx, false).await?;
        preview_with_session(self.host.as_ref(), request, ctx, inputs).await
    }

    /// Ejecuta un job duradero previamente admitido. El worker mantiene la
    /// sesión cualificada hasta responder; sólo un recibo terminal COMPLETED
    /// seguido de un cierre limpio permite liberar la reserva como INSTALLED.
    pub async fn execute_queued_job(&self, job_id: &str) -> Result<RuntimeInstallJob, LarmError> {
        let queued = self.jobs.get_job(job_id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "El job de instalación no existe",
                "runtime_install_job_not_found",
            )
        })?;
        if queued.state != JobPhase::Queued {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El job de instalación ya no está en cola",
                "runtime_install_execute_state_conflict",
            ));
        }

        let lock_hash = match queued.installation.lock_hash.as_deref() {
            Some(value) => value,
            None => {
                let error = LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "El job de instalación no conserva el lock de perfil",
                    "runtime_install_profile_lock_missing",
                );
                return self.jobs.fail_job(job_id, &error).await;
            }
        };
        let request = request_from_plan(&queued.plan)?;
        if let Err(error) = load_install_qualified_profile(
            self.profiles.as_ref(),
            self.qualifications.as_ref(),
            lock_hash,
            request.strategy,
        )
        .await
        {
            return self.jobs.fail_job(job_id, &error).await;
        }

        let Some(artifact_store) = self.artifact_store.as_deref() else {
            let error = LarmError::new(
                ErrorCode::UnsupportedCapability,
                "No hay un almacén gestionado para publicar la instalación",
                "runtime_install_store_unavailable",
            );
            return self.jobs.fail_job(job_id, &error).await;
        };

        let job = self.jobs.start_job(job_id).await?;
        execute_started_job(
            self.jobs.as_ref(),
            self.execution.as_ref(),
            self.host.as_ref(),
            Some(artifact_store),
            &self.active_installs,
            job,
        )
        .await
    }

    /// Procesa un lote ordenado. El claim CAS de `start_job` arbitra entre
    /// instancias del daemon; un job corrupto o ajeno no cancela el resto.
    pub async fn consume_queued(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, Result<RuntimeInstallJob, LarmError>)>, LarmError> {
        let ids = self.jobs.queued_job_ids(limit).await?;
        let mut outcomes = Vec::with_capacity(ids.len());
        for id in ids {
            let outcome = self.execute_queued_job(&id).await;
            outcomes.push((id, outcome));
        }
        Ok(outcomes)
    }

    /// Consulta un job RUNNING sin repetir su mutación. Sólo un recibo terminal
    /// del adaptador, seguido de cierre limpio, puede cerrar el journal.
    pub async fn reconcile_running_job(
        &self,
        job_id: &str,
    ) -> Result<Option<RuntimeInstallJob>, LarmError> {
        let Some(job) = self.jobs.get_job(job_id).await? else {
            return Ok(None);
        };
        reconcile_started_job(
            self.jobs.as_ref(),
            self.execution.as_ref(),
            self.host.as_ref(),
            self.artifact_store.as_deref(),
            job,
        )
        .await
    }

    /// Reconcilia un lote pequeño; errores por job no impiden consultar los
    /// siguientes. Los jobs no terminales permanecen con sus reservas.
    pub async fn reconcile_running(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, Result<Option<RuntimeInstallJob>, LarmError>)>, LarmError> {
        let ids = self.jobs.running_job_ids(limit).await?;
        let mut outcomes = Vec::with_capacity(ids.len());
        for id in ids {
            let outcome = self.reconcile_running_job(&id).await;
            outcomes.push((id, outcome));
        }
        Ok(outcomes)
    }
}

async fn reconcile_started_job(
    jobs: &dyn RuntimeInstallJobRepository,
    execution: &dyn RuntimeExecutionRepository,
    host: &dyn RuntimeDiscoveryHost,
    artifact_store: Option<&dyn ManagedRuntimeInstallStore>,
    job: RuntimeInstallJob,
) -> Result<Option<RuntimeInstallJob>, LarmError> {
    if job.state != JobPhase::Running
        || job.installation.stage != InstallationStage::Installing
        || job.installation.state != InstallationState::Installing
    {
        return Ok(None);
    }
    let ctx = MutationContext {
        actor_id: job.actor_id.clone(),
        correlation_id: job.correlation_id.clone(),
    };
    let request = request_from_plan(&job.plan)?;
    let binding = crate::install::spec(&job.plan)?;
    // Un job RUNNING anterior a esta versión no guarda el hash que retuvo el
    // adaptador. No se consulta con el hash exterior ni se libera su reserva.
    if binding.adapter_plan_hash.is_empty() {
        return Ok(None);
    }
    binding.validate_for(&request)?;
    let context = execution
        .prepare_execution(&request.runtime_id, &ctx)
        .await?;
    if context.actor_id != job.actor_id
        || context.runtime.id != request.runtime_id
        || context.installation.id != binding.adapter_installation_id
        || context.installation.package_hash != binding.adapter_package_hash
    {
        return Err(conflict("runtime_install_execution_binding_changed"));
    }
    if !context
        .installation
        .manifest
        .get("methods")
        .and_then(Value::as_array)
        .is_some_and(|methods| {
            methods
                .iter()
                .any(|method| method == "runtime.install_status")
        })
    {
        return Ok(None);
    }
    let expected = context.binding()?;
    let session = host.open(&context, &ctx).await?;
    if session.binding() != &expected {
        session.close().await?;
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "La sesión de consulta pertenece a otro runtime",
            "runtime_install_session_mismatch",
        ));
    }
    let workspace = if artifact_store.is_some() {
        match session
            .install_workspace(&job.job_id, &binding.adapter_plan_hash, false)
            .await
        {
            Ok(workspace) => workspace,
            Err(error) => {
                session.close().await?;
                return Err(error);
            }
        }
    } else {
        None
    };
    let status = async {
        session.recheck().await?;
        session
            .install_status(&InstallStatusRequest {
                plan_id: job.job_id.clone(),
                plan_hash: binding.adapter_plan_hash.clone(),
            })
            .await
    }
    .await;
    // Incluso si la consulta falla, se exige drenaje antes de soltar la
    // sesión cualificada. El estado del journal permanece RUNNING abajo.
    session.close().await?;
    let receipt = status?;
    if receipt.job_id != job.job_id || receipt.status_url != format!("/api/v1/jobs/{}", job.job_id)
    {
        return Err(LarmError::new(
            ErrorCode::ProtocolError,
            "El adaptador respondió por otro job de instalación",
            "runtime_install_status_binding_mismatch",
        ));
    }
    match receipt.state {
        JobPhase::Completed => {
            if let Some(store) = artifact_store {
                let publication = store
                    .publish(
                        &job.job_id,
                        &job.plan_hash,
                        &binding.adapter_plan_hash,
                        &request.storage_location_id,
                        binding
                            .relative_destination
                            .as_deref()
                            .ok_or_else(|| invalid("runtime_destination_missing"))?,
                        workspace.as_ref(),
                    )
                    .await;
                if let Err(error) = publication {
                    if permanent_publish_error(&error) {
                        if let Some(workspace) = workspace.as_ref() {
                            store
                                .discard_workspace(
                                    &job.job_id,
                                    &binding.adapter_plan_hash,
                                    workspace,
                                )
                                .await?;
                        }
                        return jobs.fail_job(&job.job_id, &error).await.map(Some);
                    }
                    return Err(error);
                }
                if let Some(workspace) = workspace.as_ref() {
                    store
                        .discard_workspace(&job.job_id, &binding.adapter_plan_hash, workspace)
                        .await?;
                }
            }
            jobs.complete_job(&job.job_id, &receipt).await.map(Some)
        }
        JobPhase::Failed => {
            let error = LarmError::new(
                ErrorCode::DownloadFailed,
                "El worker informa que la instalación no terminó correctamente",
                "runtime_install_worker_failed",
            );
            if let (Some(store), Some(workspace)) = (artifact_store, workspace.as_ref()) {
                store
                    .discard_workspace(&job.job_id, &binding.adapter_plan_hash, workspace)
                    .await?;
            }
            jobs.fail_job(&job.job_id, &error).await.map(Some)
        }
        JobPhase::Cancelled => {
            let error = LarmError::new(
                ErrorCode::Conflict,
                "El adaptador canceló la instalación después de iniciarla",
                "runtime_install_worker_cancelled",
            );
            if let (Some(store), Some(workspace)) = (artifact_store, workspace.as_ref()) {
                store
                    .discard_workspace(&job.job_id, &binding.adapter_plan_hash, workspace)
                    .await?;
            }
            jobs.fail_job(&job.job_id, &error).await.map(Some)
        }
        JobPhase::Queued | JobPhase::Running => Ok(None),
    }
}

async fn execute_started_job(
    jobs: &dyn RuntimeInstallJobRepository,
    execution: &dyn RuntimeExecutionRepository,
    host: &dyn RuntimeDiscoveryHost,
    artifact_store: Option<&dyn ManagedRuntimeInstallStore>,
    active_installs: &ActiveInstallRegistry,
    job: RuntimeInstallJob,
) -> Result<RuntimeInstallJob, LarmError> {
    if job.state != JobPhase::Running
        || job.installation.stage != InstallationStage::Installing
        || job.installation.state != InstallationState::Installing
    {
        return Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "El job no está listo para ejecutar",
            "runtime_install_execute_state_conflict",
        ));
    }
    let job_id = job.job_id.as_str();
    let ctx = MutationContext {
        actor_id: job.actor_id.clone(),
        correlation_id: job.correlation_id.clone(),
    };
    let prepared = async {
        let request = request_from_plan(&job.plan)?;
        let binding = crate::install::spec(&job.plan)?;
        binding.validate_for(&request)?;
        let context = execution
            .prepare_execution(&request.runtime_id, &ctx)
            .await?;
        if context.actor_id != job.actor_id
            || context.runtime.id != request.runtime_id
            || context.installation.id != binding.adapter_installation_id
            || context.installation.package_hash != binding.adapter_package_hash
        {
            return Err(conflict("runtime_install_execution_binding_changed"));
        }
        Ok((context, binding))
    }
    .await;

    let (context, binding) = match prepared {
        Ok(value) => value,
        Err(error) => return jobs.fail_job(job_id, &error).await,
    };
    let expected = match context.binding() {
        Ok(binding) => binding,
        Err(error) => return jobs.fail_job(job_id, &error).await,
    };
    let session = match host.open(&context, &ctx).await {
        Ok(session) => session,
        Err(error) => return jobs.fail_job(job_id, &error).await,
    };
    if session.binding() != &expected {
        session.close().await?;
        let error = LarmError::new(
            ErrorCode::Forbidden,
            "La sesión de ejecución pertenece a otro runtime",
            "runtime_install_session_mismatch",
        );
        return jobs.fail_job(job_id, &error).await;
    }
    if let Err(error) = session.recheck().await {
        session.close().await?;
        return jobs.fail_job(job_id, &error).await;
    }
    let workspace = if artifact_store.is_some() {
        match session
            .install_workspace(job_id, &binding.adapter_plan_hash, true)
            .await
        {
            Ok(Some(workspace)) => Some(workspace),
            Ok(None) => {
                session.close().await?;
                let error = LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El host no pudo crear el área temporal de instalación",
                    "runtime_install_workspace_unavailable",
                );
                return jobs.fail_job(job_id, &error).await;
            }
            Err(error) => {
                session.close().await?;
                return jobs.fail_job(job_id, &error).await;
            }
        }
    } else {
        None
    };
    let session: Arc<dyn RuntimeDiscoverySession> = Arc::from(session);
    let (cancel_state, mut cancel_rx) = watch::channel(ActiveCancelState::Idle);
    let active = Arc::new(ActiveInstall {
        session: session.clone(),
        cancel_state,
    });
    {
        let mut registry = active_installs.lock().await;
        if registry.contains_key(job_id) {
            drop(registry);
            session.close().await?;
            return Err(conflict("runtime_install_worker_session_already_active"));
        }
        registry.insert(job_id.to_string(), active.clone());
    }
    let receipt = session
        .install(&ExecutePlan {
            plan_id: job.job_id.clone(),
            plan_hash: binding.adapter_plan_hash.clone(),
            approval_id: None,
        })
        .await;
    active.cancel_state.send_if_modified(|current| {
        if *current == ActiveCancelState::Idle {
            *current = ActiveCancelState::Finished;
            true
        } else {
            false
        }
    });
    let mut cancel_status = *cancel_rx.borrow_and_update();
    while cancel_status == ActiveCancelState::Requested {
        if cancel_rx.changed().await.is_err() {
            break;
        }
        cancel_status = *cancel_rx.borrow_and_update();
    }
    let cancel_confirmed = cancel_status == ActiveCancelState::Confirmed;
    // El journal y sus guards siguen RUNNING hasta que el host confirme que
    // se drenó el proceso; liberarlos podría competir con IO vivo.
    let close_result = session.close().await;
    {
        let mut registry = active_installs.lock().await;
        if registry
            .get(job_id)
            .is_some_and(|registered| Arc::ptr_eq(registered, &active))
        {
            registry.remove(job_id);
        }
    }
    close_result?;
    let worker_completed = matches!(
        &receipt,
        Ok(receipt)
            if receipt.job_id == job.job_id
                && receipt.status_url == format!("/api/v1/jobs/{job_id}")
                && receipt.state == JobPhase::Completed
    );
    if cancel_confirmed && !worker_completed {
        if let Some(store) = artifact_store {
            let workspace = workspace.as_ref().ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "La sesión no proporcionó el área de instalación",
                    "runtime_install_workspace_unavailable",
                )
            })?;
            store
                .discard_workspace(job_id, &binding.adapter_plan_hash, workspace)
                .await?;
        }
        return jobs.cancel_running_job(job_id, &job.actor_id, &ctx).await;
    }
    match receipt {
        Ok(receipt)
            if receipt.job_id == job.job_id
                && receipt.status_url == format!("/api/v1/jobs/{job_id}")
                && receipt.state == JobPhase::Completed =>
        {
            if let Some(store) = artifact_store {
                let workspace = workspace.as_ref().ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "La sesión no proporcionó el área de instalación",
                        "runtime_install_workspace_unavailable",
                    )
                })?;
                let publication = store
                    .publish(
                        job_id,
                        &job.plan_hash,
                        &binding.adapter_plan_hash,
                        &binding.request.storage_location_id,
                        binding
                            .relative_destination
                            .as_deref()
                            .ok_or_else(|| invalid("runtime_destination_missing"))?,
                        Some(workspace),
                    )
                    .await;
                if let Err(error) = publication {
                    if permanent_publish_error(&error) {
                        store
                            .discard_workspace(job_id, &binding.adapter_plan_hash, workspace)
                            .await?;
                        return jobs.fail_job(job_id, &error).await;
                    }
                    return Err(error);
                }
                store
                    .discard_workspace(job_id, &binding.adapter_plan_hash, workspace)
                    .await?;
            }
            jobs.complete_job(job_id, &receipt).await
        }
        Ok(receipt)
            if receipt.job_id == job.job_id
                && receipt.status_url == format!("/api/v1/jobs/{job_id}")
                && receipt.state == JobPhase::Failed =>
        {
            let error = LarmError::new(
                ErrorCode::DownloadFailed,
                "El worker no pudo instalar el runtime",
                "runtime_install_worker_failed",
            );
            if let (Some(store), Some(workspace)) = (artifact_store, workspace.as_ref()) {
                store
                    .discard_workspace(job_id, &binding.adapter_plan_hash, workspace)
                    .await?;
            }
            jobs.fail_job(job_id, &error).await
        }
        Ok(receipt)
            if receipt.job_id == job.job_id
                && receipt.status_url == format!("/api/v1/jobs/{job_id}")
                && receipt.state == JobPhase::Cancelled =>
        {
            let error = LarmError::new(
                ErrorCode::Conflict,
                "El adaptador canceló la instalación después de iniciarla",
                "runtime_install_worker_cancelled",
            );
            if let (Some(store), Some(workspace)) = (artifact_store, workspace.as_ref()) {
                store
                    .discard_workspace(job_id, &binding.adapter_plan_hash, workspace)
                    .await?;
            }
            jobs.fail_job(job_id, &error).await
        }
        Ok(receipt)
            if receipt.job_id == job.job_id
                && receipt.status_url == format!("/api/v1/jobs/{job_id}") =>
        {
            // No sabemos si el worker mantiene IO activo. El consumidor de
            // recuperación consultará runtime.install_status cuando exista.
            Ok(job)
        }
        Ok(_) => Err(LarmError::new(
            ErrorCode::ProtocolError,
            "El worker respondió por otro job de instalación",
            "runtime_install_worker_job_binding_mismatch",
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use larm_domain::{
        adapters::{AdapterInstallation, AdapterState, AdapterTrust},
        bridge::JobReceipt,
        conformance::{BindingHashes, Run},
        ports::{AdapterQualificationGuard, MutationContext},
        runtime::{Runtime, RuntimeAdapter, RuntimeChannelPolicy},
        runtime_execution::{
            RuntimeDiscoverySession, RuntimeExecutionContext, RuntimeExecutionRepository,
            RuntimeSessionBinding, RuntimeSessionGuard,
        },
        runtime_install::{InstallRequest, RuntimeInstallAdmission},
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[path = "../../../../../tests/fixtures/adapters/runtime_session.rs"]
    mod runtime_fixture;

    struct FixtureAccess(larm_adapter_sdk::permissions::Access);

    #[async_trait]
    impl larm_adapter_host::runtime::discovery::DiscoveryAccessResolver for FixtureAccess {
        async fn resolve(
            &self,
            _context: &RuntimeExecutionContext,
            _ctx: &MutationContext,
        ) -> Result<larm_adapter_host::runtime::discovery::DiscoveryAccess, LarmError> {
            Ok(larm_adapter_host::runtime::discovery::DiscoveryAccess {
                access: self.0.clone(),
                timeout: std::time::Duration::from_secs(3),
            })
        }
    }

    fn profile(
        strategy: &str,
        dependencies_lock: Value,
    ) -> larm_domain::runtime_profiles::RuntimeProfileLock {
        let key = larm_domain::runtime_profiles::RuntimeProfileKey {
            runtime: "llama.cpp".into(),
            source_url: "https://example.org/runtime".into(),
            source_revision: "a".repeat(40),
            source_sha256: "b".repeat(64),
            upstream_version: "v1".into(),
            os: "Windows".into(),
            os_build: "fixture".into(),
            backend: "CPU".into(),
            protocol: "stdio-v1".into(),
        };
        let mut fields = Map::new();
        for name in larm_domain::runtime_profiles::REQUIRED_FIELDS {
            let value = match *name {
                "upstream_version" => json!(key.upstream_version),
                "installation_strategies" => json!([strategy]),
                "install_strategy" => json!(strategy),
                "dependencies_lock" => dependencies_lock.clone(),
                "fixture_hashes" => json!({"positive":"d".repeat(64)}),
                "evidence_refs" => json!(["fixture://p099/profile"]),
                _ => Value::Null,
            };
            fields.insert((*name).into(), value);
        }
        let mut profile = larm_domain::runtime_profiles::RuntimeProfileLock {
            schema_version: larm_domain::runtime_profiles::LOCK_SCHEMA.into(),
            key,
            fields: Value::Object(fields),
            qualification: "UNQUALIFIED".into(),
            lock_sha256: String::new(),
        };
        profile.lock_sha256 = profile.computed_hash().unwrap();
        profile
    }

    struct MemoryProfiles(larm_domain::runtime_profiles::RuntimeProfileLock);

    #[async_trait]
    impl RuntimeProfileLockRepository for MemoryProfiles {
        async fn acquire(
            &self,
            _lock: &larm_domain::runtime_profiles::RuntimeProfileLock,
            _ctx: &MutationContext,
        ) -> Result<bool, LarmError> {
            unreachable!("la instalación sólo consulta locks")
        }

        async fn get_by_hash(
            &self,
            lock_sha256: &str,
        ) -> Result<Option<larm_domain::runtime_profiles::RuntimeProfileLock>, LarmError> {
            Ok((self.0.lock_sha256 == lock_sha256).then(|| self.0.clone()))
        }
    }

    struct MemoryQualifications(Option<larm_domain::runtime_profiles::RuntimeProfileQualification>);

    #[async_trait]
    impl RuntimeProfileQualificationRepository for MemoryQualifications {
        async fn record(
            &self,
            _lock: &larm_domain::runtime_profiles::RuntimeProfileLock,
            _qualification: &larm_domain::runtime_profiles::RuntimeProfileQualification,
            _ctx: &MutationContext,
        ) -> Result<bool, LarmError> {
            unreachable!("el servicio de instalación sólo consulta cualificaciones")
        }

        async fn get_by_lock_hash(
            &self,
            _lock_sha256: &str,
        ) -> Result<Option<larm_domain::runtime_profiles::RuntimeProfileQualification>, LarmError>
        {
            Ok(self
                .0
                .as_ref()
                .filter(|qualification| qualification.lock_sha256 == _lock_sha256)
                .cloned())
        }
    }

    fn qualification(
        lock: &larm_domain::runtime_profiles::RuntimeProfileLock,
        strategies: &[&str],
    ) -> larm_domain::runtime_profiles::RuntimeProfileQualification {
        let fixture_hashes = lock.fields["fixture_hashes"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
            .collect();
        let strategy_test_ids = strategies
            .iter()
            .map(|strategy| ((*strategy).to_owned(), vec!["REAL-smoke".to_owned()]))
            .collect();
        let mut result = larm_domain::runtime_profiles::RuntimeProfileQualification {
            schema_version: larm_domain::runtime_profiles::QUALIFICATION_SCHEMA.into(),
            lock_sha256: lock.lock_sha256.clone(),
            host_id: "fixture-host".into(),
            qualified_at: timestamp(chrono::Utc::now()),
            fixture_hashes,
            strategy_test_ids,
            evidence_sha256: "e".repeat(64),
            qualification_sha256: String::new(),
        };
        result.qualification_sha256 = result.computed_hash().unwrap();
        result
    }

    #[test]
    fn bloquea_estrategias_sin_efectos_o_reservas_definidos() {
        assert!(supported_strategy(InstallationStrategy::PrebuiltBinary).is_ok());
        for strategy in [
            InstallationStrategy::Winget,
            InstallationStrategy::MsiManaged,
        ] {
            let error = supported_strategy(strategy).unwrap_err();
            assert_eq!(error.code, ErrorCode::ApprovalRequired);
            assert_eq!(error.details.reason, "runtime_install_effects_unqualified");
        }
        for strategy in [
            InstallationStrategy::ExternalAdoption,
            InstallationStrategy::Custom,
        ] {
            assert_eq!(
                supported_strategy(strategy).unwrap_err().code,
                ErrorCode::UnsupportedCapability
            );
        }
    }

    #[tokio::test]
    async fn un_lock_no_cualificado_no_autoriza_instalaciones() {
        let lock = profile("PREBUILT_BINARY", Value::Null);
        let store = MemoryProfiles(lock.clone());
        let error = load_install_qualified_profile(
            &store,
            &MemoryQualifications(None),
            &lock.lock_sha256,
            InstallationStrategy::PrebuiltBinary,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedCapability);
        assert_eq!(error.details.reason, "runtime_profile_unqualified");

        assert_eq!(
            load_install_qualified_profile(
                &store,
                &MemoryQualifications(None),
                &"d".repeat(64),
                InstallationStrategy::PrebuiltBinary,
            )
            .await
            .unwrap_err()
            .details
            .reason,
            "runtime_install_profile_lock_missing"
        );
    }

    #[tokio::test]
    async fn cualificacion_separada_habilita_solo_estrategias_probadas() {
        let lock = profile("PREBUILT_BINARY", Value::Null);
        let evidence = qualification(&lock, &["PREBUILT_BINARY"]);
        let profiles = MemoryProfiles(lock.clone());
        let qualifications = MemoryQualifications(Some(evidence));

        assert!(load_install_qualified_profile(
            &profiles,
            &qualifications,
            &lock.lock_sha256,
            InstallationStrategy::PrebuiltBinary,
        )
        .await
        .is_ok());
        assert_eq!(
            load_install_qualified_profile(
                &profiles,
                &qualifications,
                &lock.lock_sha256,
                InstallationStrategy::SourceBuild,
            )
            .await
            .unwrap_err()
            .details
            .reason,
            "runtime_profile_strategy_unqualified"
        );
    }

    #[tokio::test]
    async fn submit_rechaza_lock_no_cualificado_antes_de_preparar_o_abrir_worker() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Running, false);
        let request = request_from_plan(&job.plan).unwrap();
        let mut lock = profile("PREBUILT_BINARY", Value::Null);
        lock.lock_sha256 = request.profile_lock_hash.clone();
        let service = RuntimeInstallService::new(
            Arc::new(larm_db::runtimes::SqlRuntimes {
                pool: sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_lazy("sqlite::memory:")
                    .unwrap(),
            }),
            Arc::new(execution),
            Arc::new(MemoryProfiles(lock)),
            Arc::new(MemoryQualifications(None)),
            Arc::new(host),
            Arc::new(MemoryJobs::new(job, events.clone())),
            None,
        );
        let ctx = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };

        let error = service
            .submit(
                &request,
                "operator",
                "fixture-idempotency-key",
                &"e".repeat(64),
                &ctx,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::UnsupportedCapability);
        assert_eq!(error.details.reason, "runtime_profile_unqualified");
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn fija_hash_de_lock_de_dependencias_y_exige_lock_para_entornos_aislados() {
        let value = json!({"packages":[{"name":"fixture","version":"1.0"}]});
        let lock = profile("PIP", value.clone());
        profile_allows(&lock, InstallationStrategy::Pip).unwrap();
        assert_eq!(
            dependencies_lock_hash(&lock, InstallationStrategy::Pip).unwrap(),
            Some(larm_domain::canonical::sha256_hex(&value).unwrap())
        );
        let missing = profile("PIP", Value::Null);
        assert_eq!(
            dependencies_lock_hash(&missing, InstallationStrategy::Pip)
                .unwrap_err()
                .details
                .reason,
            "runtime_profile_dependencies_lock_missing"
        );
        assert_eq!(
            profile_allows(&lock, InstallationStrategy::Uv)
                .unwrap_err()
                .details
                .reason,
            "runtime_install_strategy_not_in_profile"
        );
    }

    #[test]
    fn nuget_exige_arquitectura_y_lock_packages_con_hashes_y_sella_el_documento() {
        use base64::Engine;

        let content_hash = base64::engine::general_purpose::STANDARD.encode([9_u8; 64]);
        let value = json!({
            "version": 1,
            "dependencies": {
                "net8.0": {
                    "Contoso.Runtime": {
                        "type": "Direct",
                        "requested": "[2.4.1, 2.4.1]",
                        "resolved": "2.4.1",
                        "contentHash": content_hash
                    }
                }
            }
        });
        let mut lock = profile("NUGET", value.clone());
        lock.fields["architecture"] = json!("x64");
        lock.lock_sha256 = lock.computed_hash().unwrap();
        profile_allows(&lock, InstallationStrategy::Nuget).unwrap();
        assert_eq!(
            dependencies_lock_hash(&lock, InstallationStrategy::Nuget).unwrap(),
            Some(larm_domain::canonical::sha256_hex(&value).unwrap())
        );

        lock.fields["architecture"] = Value::Null;
        lock.lock_sha256 = lock.computed_hash().unwrap();
        assert_eq!(
            dependencies_lock_hash(&lock, InstallationStrategy::Nuget)
                .unwrap_err()
                .details
                .reason,
            "runtime_nuget_architecture_missing"
        );

        lock.fields["architecture"] = json!("x64");
        lock.fields["dependencies_lock"] = json!({"version": 1, "dependencies": {}});
        lock.lock_sha256 = lock.computed_hash().unwrap();
        assert_eq!(
            dependencies_lock_hash(&lock, InstallationStrategy::Nuget)
                .unwrap_err()
                .details
                .reason,
            "runtime_nuget_lock_dependencies_missing"
        );
    }

    struct MemoryJobs {
        job: tokio::sync::Mutex<RuntimeInstallJob>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl MemoryJobs {
        fn new(job: RuntimeInstallJob, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                job: tokio::sync::Mutex::new(job),
                events,
            }
        }
    }

    #[async_trait]
    impl RuntimeInstallJobRepository for MemoryJobs {
        async fn replay(
            &self,
            _principal_id: &str,
            _idempotency_key: &str,
            _request_hash: &str,
        ) -> Result<Option<JobReceipt>, LarmError> {
            Ok(None)
        }

        async fn enqueue_plan(
            &self,
            _admission: RuntimeInstallAdmission<'_>,
        ) -> Result<(JobReceipt, bool), LarmError> {
            unreachable!("el executor no admite jobs")
        }

        async fn start_job(&self, _id: &str) -> Result<RuntimeInstallJob, LarmError> {
            unreachable!("el test entrega un job ya iniciado")
        }

        async fn complete_job(
            &self,
            id: &str,
            receipt: &JobReceipt,
        ) -> Result<RuntimeInstallJob, LarmError> {
            self.events.lock().unwrap().push("complete");
            let mut job = self.job.lock().await;
            assert_eq!(job.job_id, id);
            assert_eq!(receipt.job_id, id);
            assert_eq!(receipt.state, JobPhase::Completed);
            job.state = JobPhase::Completed;
            job.installation.stage = InstallationStage::Installed;
            job.installation.state = InstallationState::Installed;
            job.result = Some(json!({"worker_job_id":receipt.job_id,"state":"COMPLETED"}));
            Ok(job.clone())
        }

        async fn queued_job_ids(&self, _limit: u32) -> Result<Vec<String>, LarmError> {
            Ok(Vec::new())
        }

        async fn running_job_ids(&self, _limit: u32) -> Result<Vec<String>, LarmError> {
            Ok(vec![self.job.lock().await.job_id.clone()])
        }

        async fn fail_job(
            &self,
            id: &str,
            error: &LarmError,
        ) -> Result<RuntimeInstallJob, LarmError> {
            self.events.lock().unwrap().push("fail");
            let mut job = self.job.lock().await;
            assert_eq!(job.job_id, id);
            job.state = JobPhase::Failed;
            job.installation.stage = InstallationStage::Failed;
            job.installation.state = InstallationState::Failed;
            job.error = Some(error.clone());
            Ok(job.clone())
        }

        async fn cancel_queued_job(
            &self,
            id: &str,
            principal_id: &str,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstallJob, LarmError> {
            let mut job = self.job.lock().await;
            if job.job_id != id || job.actor_id != principal_id {
                return Err(LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                ));
            }
            if job.state != JobPhase::Queued {
                return Err(LarmError::new(
                    ErrorCode::InvalidStateTransition,
                    "El job ya no está en cola",
                    "runtime_install_cancel_state_conflict",
                ));
            }
            self.events.lock().unwrap().push("cancel");
            job.state = JobPhase::Cancelled;
            job.installation.stage = InstallationStage::Failed;
            job.installation.state = InstallationState::Failed;
            job.result = Some(json!({"state":"CANCELLED"}));
            Ok(job.clone())
        }

        async fn cancel_running_job(
            &self,
            id: &str,
            principal_id: &str,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstallJob, LarmError> {
            let mut job = self.job.lock().await;
            if job.job_id != id || job.actor_id != principal_id {
                return Err(LarmError::new(
                    ErrorCode::NotFound,
                    "El job de instalación no existe",
                    "runtime_install_job_not_found",
                ));
            }
            if job.state != JobPhase::Running {
                return Err(LarmError::new(
                    ErrorCode::InvalidStateTransition,
                    "El job ya no está en ejecución",
                    "runtime_install_cancel_state_conflict",
                ));
            }
            self.events.lock().unwrap().push("cancel_running");
            job.state = JobPhase::Cancelled;
            job.installation.stage = InstallationStage::Failed;
            job.installation.state = InstallationState::Failed;
            job.result = Some(json!({"state":"CANCELLED"}));
            Ok(job.clone())
        }

        async fn get_job(&self, id: &str) -> Result<Option<RuntimeInstallJob>, LarmError> {
            let job = self.job.lock().await;
            Ok((job.job_id == id).then(|| job.clone()))
        }
    }

    struct MemoryExecution {
        context: RuntimeExecutionContext,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl RuntimeExecutionRepository for MemoryExecution {
        async fn prepare_execution(
            &self,
            _runtime: &str,
            _ctx: &MutationContext,
        ) -> Result<RuntimeExecutionContext, LarmError> {
            self.events.lock().unwrap().push("prepare");
            Ok(self.context.clone())
        }

        async fn commit_discovery(
            &self,
            _context: &RuntimeExecutionContext,
            _observations: &[larm_domain::runtime::RuntimeVersion],
            _session: &dyn larm_domain::runtime_execution::RuntimeSessionGuard,
            _ctx: &MutationContext,
        ) -> Result<Vec<larm_domain::runtime::RuntimeVersion>, LarmError> {
            unreachable!("el executor no descubre versiones")
        }
    }

    struct MemorySession {
        binding: RuntimeSessionBinding,
        receipt: JobReceipt,
        close_fails: bool,
        install_failure_reason: Option<&'static str>,
        workspace_root: Arc<tempfile::TempDir>,
        events: Arc<Mutex<Vec<&'static str>>>,
        install_hashes: Arc<Mutex<Vec<String>>>,
        status_hashes: Arc<Mutex<Vec<String>>>,
        cancel_accepted: bool,
        install_gate: Option<Arc<tokio::sync::Notify>>,
        install_started: Option<Arc<tokio::sync::Notify>>,
    }

    struct PlanningSession {
        binding: RuntimeSessionBinding,
        plan: larm_domain::artifacts::Plan,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl AdapterQualificationGuard for PlanningSession {
        async fn recheck(&self) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("recheck");
            Ok(())
        }
    }

    impl RuntimeSessionGuard for PlanningSession {
        fn binding(&self) -> &RuntimeSessionBinding {
            &self.binding
        }
    }

    #[async_trait]
    impl larm_domain::runtime_install::RuntimeVersionSource for PlanningSession {
        async fn list_versions(
            &self,
            _query: &larm_domain::runtime_install::VersionQuery,
        ) -> Result<larm_domain::runtime_install::RuntimeVersionPage, LarmError> {
            unreachable!("la previsualización no consulta versiones")
        }
    }

    #[async_trait]
    impl RuntimeDiscoverySession for PlanningSession {
        async fn plan_install(
            &self,
            _request: &InstallRequest,
        ) -> Result<larm_domain::artifacts::Plan, LarmError> {
            self.events.lock().unwrap().push("plan");
            Ok(self.plan.clone())
        }

        async fn install(&self, _request: &ExecutePlan) -> Result<JobReceipt, LarmError> {
            unreachable!("la previsualización nunca ejecuta")
        }

        async fn close(&self) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("close");
            Ok(())
        }
    }

    struct PlanningHost {
        binding: RuntimeSessionBinding,
        plan: larm_domain::artifacts::Plan,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl RuntimeDiscoveryHost for PlanningHost {
        async fn open(
            &self,
            _context: &RuntimeExecutionContext,
            _ctx: &MutationContext,
        ) -> Result<Box<dyn RuntimeDiscoverySession>, LarmError> {
            Ok(Box::new(PlanningSession {
                binding: self.binding.clone(),
                plan: self.plan.clone(),
                events: self.events.clone(),
            }))
        }
    }

    #[async_trait]
    impl AdapterQualificationGuard for MemorySession {
        async fn recheck(&self) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("recheck");
            Ok(())
        }
    }

    impl RuntimeSessionGuard for MemorySession {
        fn binding(&self) -> &RuntimeSessionBinding {
            &self.binding
        }
    }

    #[async_trait]
    impl larm_domain::runtime_install::RuntimeVersionSource for MemorySession {
        async fn list_versions(
            &self,
            _query: &larm_domain::runtime_install::VersionQuery,
        ) -> Result<larm_domain::runtime_install::RuntimeVersionPage, LarmError> {
            unreachable!("el executor no descubre versiones")
        }
    }

    #[async_trait]
    impl RuntimeDiscoverySession for MemorySession {
        async fn install(&self, request: &ExecutePlan) -> Result<JobReceipt, LarmError> {
            self.events.lock().unwrap().push("install");
            request.validate()?;
            if let Some(gate) = &self.install_gate {
                if let Some(started) = &self.install_started {
                    started.notify_one();
                }
                gate.notified().await;
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "El worker fixture detuvo la instalación",
                    "fixture_runtime_install_cancelled",
                ));
            }
            if let Some(reason) = self.install_failure_reason {
                return Err(LarmError::new(
                    ErrorCode::Timeout,
                    "El worker no confirmó el resultado de la instalación",
                    reason,
                ));
            }
            let artifact = self
                .workspace_root
                .path()
                .join("runtime-installs")
                .join(&request.plan_id)
                .join("engine.bin");
            if artifact.parent().is_some_and(|parent| parent.is_dir()) {
                std::fs::write(artifact, b"fixture-managed-runtime").map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo crear el artefacto fixture",
                        "fixture_install_write_failed",
                    )
                    .with_technical(error.to_string())
                })?;
            }
            self.install_hashes
                .lock()
                .unwrap()
                .push(request.plan_hash.clone());
            Ok(self.receipt.clone())
        }

        async fn cancel_install(&self, _plan_id: &str) -> Result<bool, LarmError> {
            self.events.lock().unwrap().push("cancel_request");
            if self.cancel_accepted {
                if let Some(gate) = &self.install_gate {
                    gate.notify_one();
                }
                Ok(true)
            } else {
                Ok(false)
            }
        }

        async fn install_workspace(
            &self,
            plan_id: &str,
            plan_hash: &str,
            create: bool,
        ) -> Result<Option<larm_domain::runtime_execution::RuntimeInstallWorkspace>, LarmError>
        {
            let root = self.workspace_root.path().to_path_buf();
            let path = root.join("runtime-installs").join(plan_id);
            if create {
                std::fs::create_dir_all(&path).map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo crear el área fixture",
                        "fixture_install_workspace_failed",
                    )
                    .with_technical(error.to_string())
                })?;
                let marker = larm_domain::runtime_install::RuntimeInstallWorkspaceMarker {
                    job_id: plan_id.into(),
                    adapter_plan_hash: plan_hash.into(),
                };
                std::fs::write(
                    path.join(larm_domain::runtime_install::INSTALL_WORKSPACE_MARKER),
                    serde_json::to_vec(&marker).unwrap(),
                )
                .unwrap();
            } else if !path.is_dir() {
                return Ok(None);
            }
            Ok(Some(
                larm_domain::runtime_execution::RuntimeInstallWorkspace { root, path },
            ))
        }

        async fn install_status(
            &self,
            request: &InstallStatusRequest,
        ) -> Result<JobReceipt, LarmError> {
            self.events.lock().unwrap().push("status");
            request.validate()?;
            self.status_hashes
                .lock()
                .unwrap()
                .push(request.plan_hash.clone());
            Ok(self.receipt.clone())
        }

        async fn close(&self) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("close");
            if self.close_fails {
                Err(LarmError::new(
                    ErrorCode::AdapterCrashed,
                    "El worker no terminó de cerrar",
                    "fixture_worker_close_failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    struct MemoryInstallStore {
        root: tempfile::TempDir,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_after_partial_io: bool,
    }

    #[async_trait]
    impl ManagedRuntimeInstallStore for MemoryInstallStore {
        async fn publish(
            &self,
            _job_id: &str,
            _plan_hash: &str,
            _adapter_plan_hash: &str,
            _storage_location_id: &str,
            relative_destination: &str,
            workspace: Option<&larm_domain::runtime_execution::RuntimeInstallWorkspace>,
        ) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("publish");
            let workspace = workspace.ok_or_else(|| invalid("fixture_workspace_missing"))?;
            let source = workspace.path.join("engine.bin");
            let destination = self.root.path().join(relative_destination);
            if self.fail_after_partial_io {
                let staging = self.root.path().join("staging").join(_job_id);
                std::fs::create_dir_all(&staging).map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo crear el staging fixture",
                        "fixture_publish_io_failed",
                    )
                    .with_technical(error.to_string())
                })?;
                std::fs::copy(source, staging.join("engine.partial")).map_err(|error| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo copiar el staging fixture",
                        "fixture_publish_io_failed",
                    )
                    .with_technical(error.to_string())
                })?;
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "La publicación gestionada sufrió un fallo de IO",
                    "fixture_publish_io_failed",
                ));
            }
            std::fs::create_dir_all(&destination).map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo publicar el fixture",
                    "fixture_publish_failed",
                )
                .with_technical(error.to_string())
            })?;
            std::fs::copy(source, destination.join("engine.bin")).map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo publicar el fixture",
                    "fixture_publish_failed",
                )
                .with_technical(error.to_string())
            })?;
            Ok(())
        }

        async fn discard_workspace(
            &self,
            _job_id: &str,
            _adapter_plan_hash: &str,
            workspace: &larm_domain::runtime_execution::RuntimeInstallWorkspace,
        ) -> Result<(), LarmError> {
            self.events.lock().unwrap().push("discard");
            std::fs::remove_dir_all(&workspace.path).map_err(|error| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo limpiar el fixture",
                    "fixture_cleanup_failed",
                )
                .with_technical(error.to_string())
            })
        }
    }

    struct MemoryHost(MemorySession);

    #[async_trait]
    impl RuntimeDiscoveryHost for MemoryHost {
        async fn open(
            &self,
            _context: &RuntimeExecutionContext,
            _ctx: &MutationContext,
        ) -> Result<Box<dyn RuntimeDiscoverySession>, LarmError> {
            Ok(Box::new(MemorySession {
                binding: self.0.binding.clone(),
                receipt: self.0.receipt.clone(),
                close_fails: self.0.close_fails,
                install_failure_reason: self.0.install_failure_reason,
                workspace_root: self.0.workspace_root.clone(),
                events: self.0.events.clone(),
                install_hashes: self.0.install_hashes.clone(),
                status_hashes: self.0.status_hashes.clone(),
                cancel_accepted: self.0.cancel_accepted,
                install_gate: self.0.install_gate.clone(),
                install_started: self.0.install_started.clone(),
            }))
        }
    }

    fn execution_fixture(
        receipt_state: JobPhase,
        close_fails: bool,
    ) -> (
        RuntimeInstallJob,
        MemoryExecution,
        MemoryHost,
        Arc<Mutex<Vec<&'static str>>>,
    ) {
        let now = chrono::Utc::now();
        let at = timestamp(now);
        let runtime_id = uuid::Uuid::now_v7().to_string();
        let version_id = uuid::Uuid::now_v7().to_string();
        let storage_id = uuid::Uuid::now_v7().to_string();
        let adapter_installation_id = uuid::Uuid::now_v7().to_string();
        let job_id = uuid::Uuid::now_v7().to_string();
        let actor_id = "operator".to_owned();
        let profile_hash = "a".repeat(64);
        let request = InstallRequest {
            runtime_id: runtime_id.clone(),
            upstream_version: "fixture-1".into(),
            strategy: InstallationStrategy::PrebuiltBinary,
            storage_location_id: storage_id.clone(),
            profile_lock_hash: profile_hash.clone(),
            external_host_id: None,
        };
        let destination = format!("runtimes/{runtime_id}/{version_id}/{profile_hash}");
        let mut plan: larm_domain::artifacts::Plan = serde_json::from_value(json!({
            "plan_id":job_id,
            "plan_hash":"0".repeat(64),
            "operation":"runtime.install",
            "resource_ids":[runtime_id,version_id,storage_id],
            "steps":[{"phase":"ADAPTER_STEP","adapter_id":"fixture.runtime","request":{"fixture":true}}],
            "expires_at":timestamp(now + chrono::Duration::minutes(15)),
            "approval_required":false,
            "storage_estimate":{"download_bytes":null,"final_artifact_bytes":null,"installed_bytes":null,"derived_cache_bytes":null,"temporary_peak_bytes":null,"existing_bytes":null,"peak_extra_bytes":null,"required_free_bytes":null,"upper_bound_bytes":null,"confidence":"UNKNOWN","reason":"fixture_only","evidence_refs":[],"per_volume":[]}
        })).unwrap();
        plan.plan_hash = crate::install::plan_hash(&plan).unwrap();
        let binding = crate::install::InstallSpec {
            request: request.clone(),
            runtime_version_id: version_id.clone(),
            adapter_plan_hash: plan.plan_hash.clone(),
            adapter_installation_id: adapter_installation_id.clone(),
            adapter_package_hash: "b".repeat(64),
            registration_mode: RegistrationMode::Managed,
            ownership: RuntimeOwnership::LarmManaged,
            relative_destination: Some(destination.clone()),
            environment_prefix: None,
            dependencies_lock_hash: None,
            global_effects: false,
            reboot_required: false,
        };
        plan = binding
            .bind(
                &request,
                &[InstallationStrategy::PrebuiltBinary],
                plan,
                "fixture.runtime",
            )
            .unwrap();
        let installation = larm_domain::runtime::RuntimeInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 2,
            metadata: Default::default(),
            runtime_version_id: binding.runtime_version_id.clone(),
            storage_location_id: Some(request.storage_location_id.clone()),
            relative_path: binding.relative_destination.clone(),
            strategy: request.strategy,
            external_host_id: None,
            state: InstallationState::Installing,
            ownership: RuntimeOwnership::LarmManaged,
            lock_hash: Some(request.profile_lock_hash.clone()),
            stage: InstallationStage::Installing,
            registration_mode: RegistrationMode::Managed,
        };
        let job = RuntimeInstallJob {
            job_id: plan.plan_id.clone(),
            state: JobPhase::Running,
            plan_hash: plan.plan_hash.clone(),
            resource_ids: plan.resource_ids.clone(),
            plan,
            installation,
            actor_id: actor_id.clone(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
            request_hash: "d".repeat(64),
            updated_at: at.clone(),
            result: None,
            error: None,
        };

        let package_hash = "b".repeat(64);
        let manifest = json!({
            "id":"fixture.runtime", "version":"1", "family":"runtime",
            "publisher":"fixture", "package_sha256":package_hash,
            "permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
            "methods":["runtime.install_status"],
            "capabilities":[]
        });
        let adapter_installation = AdapterInstallation {
            id: adapter_installation_id.clone(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: Default::default(),
            adapter_id: "fixture.runtime".into(),
            version: "1".into(),
            family: "runtime".into(),
            package_hash,
            publisher: "fixture".into(),
            trust: AdapterTrust::Untrusted,
            manifest,
            state: AdapterState::Installed,
        };
        let run = Run::new(
            &adapter_installation,
            BindingHashes {
                suite_sha256: "e".repeat(64),
                capability_profile_sha256: "f".repeat(64),
                execution_context_sha256: "0".repeat(64),
            },
            &actor_id,
        )
        .unwrap();
        let context = RuntimeExecutionContext {
            operation_id: job.job_id.clone(),
            runtime: Runtime {
                id: request.runtime_id.clone(),
                created_at: at.clone(),
                updated_at: at.clone(),
                row_version: 1,
                metadata: Default::default(),
                name: "fixture".into(),
                adapter_id: uuid::Uuid::now_v7().to_string(),
                capabilities: vec![],
                default_version_id: None,
                channel_policy: RuntimeChannelPolicy::default(),
                retention_policy: Default::default(),
            },
            adapter: RuntimeAdapter {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: at.clone(),
                updated_at: at.clone(),
                row_version: 1,
                metadata: Default::default(),
                adapter_installation_id,
                runtime_family: "fixture".into(),
                capabilities: vec![],
            },
            installation: adapter_installation,
            run,
            actor_id,
        };
        let session_binding = context.binding().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let install_hashes = Arc::new(Mutex::new(Vec::new()));
        let status_hashes = Arc::new(Mutex::new(Vec::new()));
        let host = MemoryHost(MemorySession {
            binding: session_binding,
            receipt: JobReceipt {
                job_id: job.job_id.clone(),
                status_url: format!("/api/v1/jobs/{}", job.job_id),
                state: receipt_state,
            },
            close_fails,
            install_failure_reason: None,
            workspace_root: Arc::new(tempfile::tempdir().unwrap()),
            events: events.clone(),
            install_hashes,
            status_hashes,
            cancel_accepted: false,
            install_gate: None,
            install_started: None,
        });
        let execution = MemoryExecution {
            context,
            events: events.clone(),
        };
        (job, execution, host, events)
    }

    #[tokio::test]
    async fn ejecuta_y_cierra_worker_antes_de_confirmar_la_instalacion() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let completed = execute_started_job(
            &jobs,
            &execution,
            &host,
            None,
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap();
        assert_eq!(completed.state, JobPhase::Completed);
        assert_eq!(completed.installation.stage, InstallationStage::Installed);
        let adapter_plan_hash = crate::install::spec(&job.plan).unwrap().adapter_plan_hash;
        assert_ne!(job.plan_hash, adapter_plan_hash);
        assert_eq!(*host.0.install_hashes.lock().unwrap(), [adapter_plan_hash]);
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close", "complete"]
        );
    }

    #[tokio::test]
    async fn instalacion_fake_publica_archivo_solo_despues_de_cerrar_el_worker() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let store = MemoryInstallStore {
            root: tempfile::tempdir().unwrap(),
            events: events.clone(),
            fail_after_partial_io: false,
        };
        let completed = execute_started_job(
            &jobs,
            &execution,
            &host,
            Some(&store),
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap();

        let binding = crate::install::spec(&job.plan).unwrap();
        let destination = store
            .root
            .path()
            .join(binding.relative_destination.unwrap());
        assert_eq!(
            std::fs::read(destination.join("engine.bin")).unwrap(),
            b"fixture-managed-runtime"
        );
        assert_eq!(completed.installation.stage, InstallationStage::Installed);
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close", "publish", "discard", "complete"]
        );
        assert!(!host
            .0
            .workspace_root
            .path()
            .join("runtime-installs")
            .join(&job.job_id)
            .exists());
    }

    #[tokio::test]
    async fn fallo_fake_limpia_solo_su_area_sin_publicar() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Failed, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let store = MemoryInstallStore {
            root: tempfile::tempdir().unwrap(),
            events: events.clone(),
            fail_after_partial_io: false,
        };
        let other_id = uuid::Uuid::now_v7().to_string();
        let other = host
            .0
            .workspace_root
            .path()
            .join("runtime-installs")
            .join(other_id);
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("untouched.bin"), b"keep").unwrap();

        let failed = execute_started_job(
            &jobs,
            &execution,
            &host,
            Some(&store),
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap();

        let destination = crate::install::spec(&job.plan)
            .unwrap()
            .relative_destination
            .unwrap();
        assert_eq!(failed.state, JobPhase::Failed);
        assert!(!host
            .0
            .workspace_root
            .path()
            .join("runtime-installs")
            .join(&job.job_id)
            .exists());
        assert_eq!(std::fs::read(other.join("untouched.bin")).unwrap(), b"keep");
        assert!(!store.root.path().join(destination).exists());
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close", "discard", "fail"]
        );
    }

    #[tokio::test]
    async fn fallo_de_io_parcial_al_publicar_conserva_job_running_y_workspace() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let store = MemoryInstallStore {
            root: tempfile::tempdir().unwrap(),
            events: events.clone(),
            fail_after_partial_io: true,
        };

        let error = execute_started_job(
            &jobs,
            &execution,
            &host,
            Some(&store),
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert_eq!(error.details.reason, "fixture_publish_io_failed");
        let current = jobs.get_job(&job.job_id).await.unwrap().unwrap();
        assert_eq!(current.state, JobPhase::Running);
        assert_eq!(current.installation.stage, InstallationStage::Installing);
        let workspace = host
            .0
            .workspace_root
            .path()
            .join("runtime-installs")
            .join(&job.job_id);
        assert!(workspace.join("engine.bin").is_file());
        assert!(store
            .root
            .path()
            .join("staging")
            .join(&job.job_id)
            .join("engine.partial")
            .is_file());
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close", "publish"]
        );
    }

    #[tokio::test]
    async fn worker_fake_recibe_y_reconcilia_hash_original_tras_sellado_larm() {
        let mut fixture = runtime_fixture::Fixture::new();
        fixture.context.actor_id = "operator".into();
        fixture.target.grant.actor_id = fixture.context.actor_id.clone();
        let runner = fixture.runner();
        let (installation, run) = fixture.validated(&runner).await;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        let runtime_id = uuid::Uuid::now_v7().to_string();
        let version_id = uuid::Uuid::now_v7().to_string();
        let storage_id = uuid::Uuid::now_v7().to_string();
        let ctx = MutationContext {
            actor_id: fixture.context.actor_id.clone(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let context = RuntimeExecutionContext {
            operation_id: uuid::Uuid::now_v7().to_string(),
            runtime: Runtime {
                id: runtime_id.clone(),
                created_at: at.clone(),
                updated_at: at.clone(),
                row_version: 1,
                metadata: Default::default(),
                name: "fake-runtime-install".into(),
                adapter_id: uuid::Uuid::now_v7().to_string(),
                capabilities: vec![],
                default_version_id: None,
                channel_policy: RuntimeChannelPolicy::default(),
                retention_policy: Default::default(),
            },
            adapter: RuntimeAdapter {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: at.clone(),
                updated_at: at,
                row_version: 1,
                metadata: Default::default(),
                adapter_installation_id: installation.id.clone(),
                runtime_family: installation.family.clone(),
                capabilities: vec![],
            },
            installation: installation.clone(),
            run,
            actor_id: ctx.actor_id.clone(),
        };
        let host = larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
            runner: runner.clone(),
            access: Arc::new(FixtureAccess(fixture.access())),
        };
        let session = host.open(&context, &ctx).await.unwrap();
        assert_eq!(session.binding(), &context.binding().unwrap());
        let request = InstallRequest {
            runtime_id: runtime_id.clone(),
            upstream_version: "fixture-1".into(),
            strategy: InstallationStrategy::PrebuiltBinary,
            storage_location_id: storage_id.clone(),
            profile_lock_hash: "a".repeat(64),
            external_host_id: None,
        };
        let adapter_plan = session.plan_install(&request).await.unwrap();
        assert!(!adapter_plan.resource_ids.contains(&version_id));
        let adapter_plan_hash = adapter_plan.plan_hash.clone();
        let binding = crate::install::InstallSpec {
            request: request.clone(),
            runtime_version_id: version_id.clone(),
            adapter_plan_hash: adapter_plan_hash.clone(),
            adapter_installation_id: installation.id.clone(),
            adapter_package_hash: installation.package_hash.clone(),
            registration_mode: RegistrationMode::Managed,
            ownership: RuntimeOwnership::LarmManaged,
            relative_destination: Some(format!(
                "runtimes/{runtime_id}/{version_id}/{}",
                request.profile_lock_hash
            )),
            environment_prefix: None,
            dependencies_lock_hash: None,
            global_effects: false,
            reboot_required: false,
        };
        let sealed = binding
            .bind(
                &request,
                &[InstallationStrategy::PrebuiltBinary],
                adapter_plan,
                &installation.adapter_id,
            )
            .unwrap();
        assert!(sealed.resource_ids.contains(&version_id));
        assert_ne!(sealed.plan_hash, adapter_plan_hash);

        std::fs::write(
            fixture.target.spec.work_dir.join("fixture-install-mode"),
            "completed",
        )
        .unwrap();
        let receipt = session
            .install(&ExecutePlan {
                plan_id: sealed.plan_id.clone(),
                plan_hash: adapter_plan_hash.clone(),
                approval_id: None,
            })
            .await
            .unwrap();
        assert_eq!(receipt.state, JobPhase::Completed);
        let recovered = session
            .install_status(&InstallStatusRequest {
                plan_id: sealed.plan_id.clone(),
                plan_hash: adapter_plan_hash.clone(),
            })
            .await
            .unwrap();
        assert_eq!(recovered, receipt);
        let workspace = session
            .install_workspace(&sealed.plan_id, &adapter_plan_hash, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(workspace.path.join("engine.bin")).unwrap(),
            b"fake-managed-runtime"
        );
        session.close().await.unwrap();
        drop(session);
        tokio::time::timeout(std::time::Duration::from_secs(3), runner.shutdown())
            .await
            .unwrap()
            .unwrap();

        let calls =
            std::fs::read_to_string(fixture.target.spec.work_dir.join("requests.jsonl")).unwrap();
        assert!(fixture.calls() >= 4);
        let install = calls
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|call| call["method"] == "runtime.install")
            .unwrap();
        let status = calls
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|call| call["method"] == "runtime.install_status")
            .unwrap();
        assert_eq!(install["params"]["plan_hash"], adapter_plan_hash);
        assert_eq!(status["params"]["plan_hash"], adapter_plan_hash);
    }

    #[tokio::test]
    async fn recibo_no_terminal_conserva_job_y_guards_para_sondeo() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Running, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let active = execute_started_job(
            &jobs,
            &execution,
            &host,
            None,
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap();
        assert_eq!(active.state, JobPhase::Running);
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close"]
        );
    }

    #[tokio::test]
    async fn cancela_running_tras_ack_del_worker_y_cierre_limpio() {
        let (job, execution, mut host, events) = execution_fixture(JobPhase::Running, false);
        let gate = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        host.0.cancel_accepted = true;
        host.0.install_gate = Some(gate);
        host.0.install_started = Some(started.clone());

        let jobs = Arc::new(MemoryJobs::new(job.clone(), events.clone()));
        let execution = Arc::new(execution);
        let host = Arc::new(host);
        let active_installs = ActiveInstallRegistry::default();
        let service = RuntimeInstallService::new(
            Arc::new(larm_db::runtimes::SqlRuntimes {
                pool: sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_lazy("sqlite::memory:")
                    .unwrap(),
            }),
            execution.clone(),
            Arc::new(MemoryProfiles(profile("PREBUILT_BINARY", Value::Null))),
            Arc::new(MemoryQualifications(None)),
            host.clone(),
            jobs.clone(),
            None,
        );
        let mut service = service;
        service.active_installs = active_installs.clone();

        let worker_jobs = jobs.clone();
        let worker_execution = execution.clone();
        let worker_host = host.clone();
        let worker_registry = active_installs.clone();
        let worker_job = job.clone();
        let worker = tokio::spawn(async move {
            execute_started_job(
                worker_jobs.as_ref(),
                worker_execution.as_ref(),
                worker_host.as_ref(),
                None,
                &worker_registry,
                worker_job,
            )
            .await
        });
        started.notified().await;

        let ctx = MutationContext {
            actor_id: job.actor_id.clone(),
            correlation_id: job.correlation_id.clone(),
        };
        let receipt = service
            .cancel(&job.job_id, &job.actor_id, &ctx)
            .await
            .unwrap();
        assert!(matches!(
            receipt.state,
            JobPhase::Running | JobPhase::Cancelled
        ));
        let cancelled = worker.await.unwrap().unwrap();
        assert_eq!(cancelled.state, JobPhase::Cancelled);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "prepare",
                "recheck",
                "install",
                "cancel_request",
                "close",
                "cancel_running"
            ]
        );
    }

    #[tokio::test]
    async fn cancel_running_sin_ack_conserva_estado_y_guards() {
        let (job, execution, mut host, events) = execution_fixture(JobPhase::Running, false);
        let gate = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        host.0.install_gate = Some(gate.clone());
        host.0.install_started = Some(started.clone());

        let jobs = Arc::new(MemoryJobs::new(job.clone(), events.clone()));
        let execution = Arc::new(execution);
        let host = Arc::new(host);
        let active_installs = ActiveInstallRegistry::default();
        let mut service = RuntimeInstallService::new(
            Arc::new(larm_db::runtimes::SqlRuntimes {
                pool: sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_lazy("sqlite::memory:")
                    .unwrap(),
            }),
            execution.clone(),
            Arc::new(MemoryProfiles(profile("PREBUILT_BINARY", Value::Null))),
            Arc::new(MemoryQualifications(None)),
            host.clone(),
            jobs.clone(),
            None,
        );
        service.active_installs = active_installs.clone();

        let worker_jobs = jobs.clone();
        let worker_execution = execution.clone();
        let worker_host = host.clone();
        let worker_registry = active_installs.clone();
        let worker_job = job.clone();
        let worker = tokio::spawn(async move {
            execute_started_job(
                worker_jobs.as_ref(),
                worker_execution.as_ref(),
                worker_host.as_ref(),
                None,
                &worker_registry,
                worker_job,
            )
            .await
        });
        started.notified().await;

        let ctx = MutationContext {
            actor_id: job.actor_id.clone(),
            correlation_id: job.correlation_id.clone(),
        };
        let error = service
            .cancel(&job.job_id, &job.actor_id, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "runtime_install_cancel_not_accepted");
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        gate.notify_one();
        assert!(worker.await.unwrap().is_err());
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        assert!(!events.lock().unwrap().contains(&"cancel_running"));
    }

    #[tokio::test]
    async fn timeout_del_worker_conserva_job_running_y_sus_guards() {
        let (job, execution, mut host, events) = execution_fixture(JobPhase::Running, false);
        host.0.install_failure_reason = Some("rpc_timeout");
        let jobs = MemoryJobs::new(job.clone(), events.clone());

        let error = execute_started_job(
            &jobs,
            &execution,
            &host,
            None,
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Timeout);
        assert_eq!(error.details.reason, "rpc_timeout");
        let current = jobs.get_job(&job.job_id).await.unwrap().unwrap();
        assert_eq!(current.state, JobPhase::Running);
        assert_eq!(current.installation.stage, InstallationStage::Installing);
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close"]
        );
    }

    #[tokio::test]
    async fn cierre_fallido_conserva_job_running_y_sus_guards() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, true);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let error = execute_started_job(
            &jobs,
            &execution,
            &host,
            None,
            &ActiveInstallRegistry::default(),
            job.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.details.reason, "fixture_worker_close_failed");
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "install", "close"]
        );
    }

    #[tokio::test]
    async fn recupera_job_terminal_solo_despues_de_consulta_y_cierre() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let completed = reconcile_started_job(&jobs, &execution, &host, None, job.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, JobPhase::Completed);
        assert_eq!(completed.installation.stage, InstallationStage::Installed);
        let adapter_plan_hash = crate::install::spec(&job.plan).unwrap().adapter_plan_hash;
        assert_ne!(job.plan_hash, adapter_plan_hash);
        assert_eq!(*host.0.status_hashes.lock().unwrap(), [adapter_plan_hash]);
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "status", "close", "complete"]
        );
    }

    #[tokio::test]
    async fn reconciliacion_publica_workspace_persistido_antes_de_completar() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, false);
        let binding = crate::install::spec(&job.plan).unwrap();
        let workspace = host
            .0
            .install_workspace(&job.job_id, &binding.adapter_plan_hash, true)
            .await
            .unwrap()
            .unwrap();
        std::fs::write(
            workspace.path.join("engine.bin"),
            b"recovered-managed-runtime",
        )
        .unwrap();
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let store = MemoryInstallStore {
            root: tempfile::tempdir().unwrap(),
            events: events.clone(),
            fail_after_partial_io: false,
        };

        let completed = reconcile_started_job(&jobs, &execution, &host, Some(&store), job.clone())
            .await
            .unwrap()
            .unwrap();

        let destination = store
            .root
            .path()
            .join(binding.relative_destination.unwrap());
        assert_eq!(
            std::fs::read(destination.join("engine.bin")).unwrap(),
            b"recovered-managed-runtime"
        );
        assert_eq!(completed.state, JobPhase::Completed);
        assert!(!workspace.path.exists());
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "status", "close", "publish", "discard", "complete"]
        );
    }

    #[tokio::test]
    async fn job_aun_activo_conserva_sus_reservas_durante_recuperacion() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Running, false);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        assert!(
            reconcile_started_job(&jobs, &execution, &host, None, job.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "status", "close"]
        );
    }

    #[tokio::test]
    async fn job_antiguo_sin_hash_del_adaptador_no_consulta_con_hash_exterior() {
        let (mut job, execution, host, events) = execution_fixture(JobPhase::Running, false);
        let binding_step = job
            .plan
            .steps
            .iter_mut()
            .find(|step| step.phase == "BIND_INSTALLATION")
            .unwrap();
        binding_step
            .request
            .get_mut(crate::install::INSTALL_SPEC)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("adapter_plan_hash");
        job.plan.plan_hash = crate::install::plan_hash(&job.plan).unwrap();
        job.plan_hash = job.plan.plan_hash.clone();

        let jobs = MemoryJobs::new(job.clone(), events.clone());
        assert!(reconcile_started_job(&jobs, &execution, &host, None, job)
            .await
            .unwrap()
            .is_none());
        assert!(events.lock().unwrap().is_empty());
        assert!(host.0.status_hashes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cierre_fallido_en_recuperacion_conserva_el_job_running() {
        let (job, execution, host, events) = execution_fixture(JobPhase::Completed, true);
        let jobs = MemoryJobs::new(job.clone(), events.clone());
        let error = reconcile_started_job(&jobs, &execution, &host, None, job.clone())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "fixture_worker_close_failed");
        assert_eq!(
            jobs.get_job(&job.job_id).await.unwrap().unwrap().state,
            JobPhase::Running
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["prepare", "recheck", "status", "close"]
        );
    }

    #[tokio::test]
    async fn la_previsualizacion_sella_el_plan_cierra_y_no_invoca_ejecucion() {
        let (job, execution, _host, events) = execution_fixture(JobPhase::Completed, false);
        let request = request_from_plan(&job.plan).unwrap();
        let mut adapter_plan = job.plan.clone();
        adapter_plan
            .steps
            .retain(|step| step.phase != "BIND_INSTALLATION");
        adapter_plan.plan_hash = larm_domain::runtime_install::plan_hash(&adapter_plan).unwrap();
        let host = PlanningHost {
            binding: execution.context.binding().unwrap(),
            plan: adapter_plan,
            events: events.clone(),
        };
        let version = larm_domain::runtime::RuntimeVersion {
            id: job.installation.runtime_version_id.clone(),
            created_at: job.updated_at.clone(),
            updated_at: job.updated_at.clone(),
            row_version: 1,
            metadata: Map::new(),
            runtime_id: request.runtime_id.clone(),
            version_string: request.upstream_version.clone(),
            commit: None,
            build: None,
            channel: larm_domain::runtime::RuntimeChannel::Stable,
            released_at: None,
            artifact_hash: None,
            promotion: larm_domain::runtime::RuntimePromotion::None,
            lifecycle: Default::default(),
        };

        let ctx = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let plan = preview_with_session(
            &host,
            &request,
            &ctx,
            PlanInputs {
                context: execution.context,
                version,
                declared_strategies: vec![InstallationStrategy::PrebuiltBinary],
                dependencies_lock_hash: None,
            },
        )
        .await
        .unwrap();

        let spec = crate::install::spec(&plan).unwrap();
        assert_eq!(spec.request, request);
        assert_eq!(
            spec.relative_destination.as_deref(),
            Some(
                format!(
                    "runtimes/{}/{}/{}",
                    request.runtime_id,
                    job.installation.runtime_version_id,
                    request.profile_lock_hash
                )
                .as_str()
            )
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["recheck", "plan", "recheck", "close"]
        );
    }

    #[tokio::test]
    async fn la_previsualizacion_expone_el_requisito_de_aprobacion_sin_admitirlo() {
        let (job, execution, _host, events) = execution_fixture(JobPhase::Completed, false);
        let request = request_from_plan(&job.plan).unwrap();
        let mut adapter_plan = job.plan.clone();
        adapter_plan
            .steps
            .retain(|step| step.phase != "BIND_INSTALLATION");
        adapter_plan.approval_required = true;
        adapter_plan.plan_hash = larm_domain::runtime_install::plan_hash(&adapter_plan).unwrap();
        let host = PlanningHost {
            binding: execution.context.binding().unwrap(),
            plan: adapter_plan,
            events: events.clone(),
        };
        let version = larm_domain::runtime::RuntimeVersion {
            id: job.installation.runtime_version_id.clone(),
            created_at: job.updated_at.clone(),
            updated_at: job.updated_at.clone(),
            row_version: 1,
            metadata: Map::new(),
            runtime_id: request.runtime_id.clone(),
            version_string: request.upstream_version.clone(),
            commit: None,
            build: None,
            channel: larm_domain::runtime::RuntimeChannel::Stable,
            released_at: None,
            artifact_hash: None,
            promotion: larm_domain::runtime::RuntimePromotion::None,
            lifecycle: Default::default(),
        };
        let ctx = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };

        let plan = preview_with_session(
            &host,
            &request,
            &ctx,
            PlanInputs {
                context: execution.context,
                version,
                declared_strategies: vec![InstallationStrategy::PrebuiltBinary],
                dependencies_lock_hash: None,
            },
        )
        .await
        .unwrap();

        assert!(plan.approval_required);
        assert!(crate::install::spec(&plan).is_ok());
        assert_eq!(
            *events.lock().unwrap(),
            ["recheck", "plan", "recheck", "close"]
        );
    }

    #[tokio::test]
    async fn la_previsualizacion_cierra_la_sesion_si_el_plan_es_invalido() {
        let (job, execution, _host, events) = execution_fixture(JobPhase::Completed, false);
        let request = request_from_plan(&job.plan).unwrap();
        let mut expired_plan = job.plan.clone();
        expired_plan
            .steps
            .retain(|step| step.phase != "BIND_INSTALLATION");
        expired_plan.expires_at = timestamp(chrono::Utc::now() - chrono::Duration::seconds(1));
        expired_plan.plan_hash = larm_domain::runtime_install::plan_hash(&expired_plan).unwrap();
        let host = PlanningHost {
            binding: execution.context.binding().unwrap(),
            plan: expired_plan,
            events: events.clone(),
        };
        let version = larm_domain::runtime::RuntimeVersion {
            id: job.installation.runtime_version_id,
            created_at: job.updated_at.clone(),
            updated_at: job.updated_at.clone(),
            row_version: 1,
            metadata: Map::new(),
            runtime_id: request.runtime_id.clone(),
            version_string: request.upstream_version.clone(),
            commit: None,
            build: None,
            channel: larm_domain::runtime::RuntimeChannel::Stable,
            released_at: None,
            artifact_hash: None,
            promotion: larm_domain::runtime::RuntimePromotion::None,
            lifecycle: Default::default(),
        };
        let ctx = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };

        let error = preview_with_session(
            &host,
            &request,
            &ctx,
            PlanInputs {
                context: execution.context,
                version,
                declared_strategies: vec![InstallationStrategy::PrebuiltBinary],
                dependencies_lock_hash: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.details.reason, "runtime_install_plan_expired");
        assert_eq!(*events.lock().unwrap(), ["recheck", "plan", "close"]);
    }
}
