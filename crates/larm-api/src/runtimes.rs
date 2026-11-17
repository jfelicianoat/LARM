//! P030/P031: catálogo, política y lecturas de versiones según los contratos.
use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Extension, Router,
};
use larm_core::security::CursorSigner;
use larm_domain::runtime_install::{InstallRequest, RuntimeInstallSubmitter};
use larm_domain::{bridge::is_uuid_v7, error::LarmError, runtime::*, runtime_ports::*};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub struct RuntimesState {
    pub repo: Arc<dyn RuntimeRepository>,
    pub service: Arc<dyn RuntimeCatalogService>,
    pub cursors: Arc<CursorSigner>,
    pub install_submitter: Arc<dyn RuntimeInstallSubmitter>,
}
pub fn routes(state: RuntimesState) -> Router {
    let install_submitter = state.install_submitter.clone();
    Router::new()
        .route("/api/v1/runtimes", get(list_runtimes).post(create))
        .route("/api/v1/runtimes/{id}", get(read_runtime))
        .route(
            "/api/v1/runtimes/{id}/channel-policy",
            axum::routing::patch(change_policy),
        )
        .route(
            "/api/v1/runtimes/{id}/retention-policy",
            axum::routing::patch(change_retention_policy),
        )
        .route("/api/v1/runtimes/{id}/versions", get(list_versions))
        .route("/api/v1/runtime-versions", get(list_all_versions))
        .route("/api/v1/runtime-versions/{id}", get(read_version))
        .route(
            "/api/v1/runtime-versions/{id}/update",
            axum::routing::post(submit_update),
        )
        .route(
            "/api/v1/runtime-versions/{id}/rollback",
            axum::routing::post(rollback_version),
        )
        .route(
            "/api/v1/runtime-versions/{id}/pin",
            axum::routing::post(pin_version),
        )
        .route("/api/v1/runtime-pins", get(list_pins))
        .route(
            "/api/v1/runtime-pins/{variant_id}/{launch_profile_id}",
            axum::routing::delete(unpin),
        )
        .route(
            "/api/v1/runtime-installations",
            get(list_installations).post(submit_install),
        )
        .route("/api/v1/runtime-installations/{id}", get(read_installation))
        .route("/api/v1/execution-hosts", get(list_execution_hosts))
        .route("/api/v1/runtime-backends", get(list_backends))
        .route("/api/v1/runtime-backends/{id}", get(read_backend))
        .route(
            "/api/v1/external-hosts",
            get(list_hosts).post(create_external_host),
        )
        .route("/api/v1/external-hosts/{id}", get(read_host))
        .layer(Extension(install_submitter))
        .with_state(state)
}
fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> Failure + '_ {
    move |e| Failure::from(e).recorrelate(&ctx.correlation_id)
}
fn filter_error() -> LarmError {
    LarmError::new(
        "VALIDATION_FAILED",
        "Filtro de runtime no válido",
        "runtime_filter_invalid",
    )
}
fn filters(
    params: &BTreeMap<String, String>,
    allowed: &[&str],
) -> Result<(Option<String>, Option<u32>, RuntimeFilter), LarmError> {
    let (cursor, limit) = crate::http::list_params(params, allowed)?;
    let filter = RuntimeFilter {
        q: params.get("q").cloned(),
        state: params.get("state").cloned(),
        capability: params.get("capability").cloned(),
        runtime_id: params.get("runtime_id").cloned(),
        storage_location_id: params.get("storage_location_id").cloned(),
    };
    if [&filter.runtime_id, &filter.storage_location_id]
        .iter()
        .any(|id| id.as_deref().is_some_and(|id| !is_uuid_v7(id)))
    {
        return Err(filter_error());
    }
    Ok((cursor, limit, filter))
}
fn digest(list: &str, filter: &RuntimeFilter) -> Result<String, LarmError> {
    larm_domain::canonical::sha256_hex(&serde_json::json!({"list":list,"q":filter.q,"state":filter.state,"capability":filter.capability,"runtime_id":filter.runtime_id,"storage_location_id":filter.storage_location_id})).map_err(|_|filter_error())
}
macro_rules! read {
    ($fn:ident,$method:ident) => {
        async fn $fn(
            State(state): State<RuntimesState>,
            Extension(ctx): Extension<AuthContext>,
            Path(id): Path<String>,
            headers: HeaderMap,
        ) -> Answer {
            crate::http::uuid_param(&id, &headers)?;
            let value = state
                .repo
                .$method(&id)
                .await
                .map_err(fail(&ctx))?
                .ok_or_else(|| {
                    fail(&ctx)(
                        LarmError::new(
                            "NOT_FOUND",
                            "Recurso de runtime desconocido",
                            "runtime_resource_not_found",
                        )
                        .with_resource(id),
                    )
                })?;
            Ok(json(StatusCode::OK, &value))
        }
    };
}
read!(read_runtime, runtime);
read!(read_version, version);
read!(read_installation, installation);
read!(read_backend, backend);
read!(read_host, external_host);
macro_rules! page {
    ($state:ident,$ctx:ident,$cursor:ident,$limit:ident,$filter:ident,$kind:literal,$ty:ty,$method:ident) => {{
        let digest = digest($kind, &$filter).map_err(fail(&$ctx))?;
        let page = $state
            .cursors
            .page(
                &digest,
                $cursor.as_deref(),
                $limit,
                |value: &$ty| (value.created_at.clone(), value.id.clone()),
                |after, n| $state.repo.$method(after, n, &$filter),
            )
            .await
            .map_err(fail(&$ctx))?;
        Ok(json(StatusCode::OK, &page))
    }};
}
async fn list_runtimes(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, filter) = filters(&params, &["q", "capability"]).map_err(fail(&ctx))?;
    page!(state, ctx, cursor, limit, filter, "runtimes", Runtime, runtimes)
}
async fn list_versions(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    if state.repo.runtime(&id).await.map_err(fail(&ctx))?.is_none() {
        return Err(fail(&ctx)(LarmError::new(
            "NOT_FOUND",
            "Runtime desconocido",
            "runtime_not_found",
        )));
    }
    let (cursor, limit, mut filter) = filters(&params, &["q"]).map_err(fail(&ctx))?;
    filter.runtime_id = Some(id);
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "runtime-versions",
        RuntimeVersion,
        versions
    )
}
async fn list_all_versions(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, filter) = filters(&params, &["q", "runtime_id"]).map_err(fail(&ctx))?;
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "runtime-version-catalog",
        RuntimeVersion,
        versions
    )
}
async fn list_installations(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, mut filter) =
        filters(&params, &["state", "runtime_id", "storage_location_id"]).map_err(fail(&ctx))?;
    if let Some(raw) = &filter.state {
        let value: InstallationState = serde_json::from_value(serde_json::json!(raw))
            .map_err(|_| fail(&ctx)(filter_error()))?;
        filter.state = Some(value.as_str().into());
    }
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "runtime-installations",
        RuntimeInstallation,
        installations
    )
}

async fn submit_install(
    Extension(submitter): Extension<Arc<dyn RuntimeInstallSubmitter>>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: InstallRequest = strict(&body, &headers)?;
    request.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let receipt = submitter
        .submit(
            &request,
            &ctx.principal.id,
            key,
            &hex::encode(Sha256::digest(&body)),
            &ctx.mutation(),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

fn p032_hash(value: &serde_json::Value) -> Result<String, LarmError> {
    larm_domain::canonical::sha256_hex(value).map_err(|_| filter_error())
}

fn update_install_request(
    runtime: &Runtime,
    version: &RuntimeVersion,
    request: &RuntimeUpdateRequest,
) -> Result<InstallRequest, LarmError> {
    if version.runtime_id != runtime.id || version.lifecycle != RuntimeVersionLifecycle::Candidate {
        return Err(LarmError::new(
            "CONFLICT",
            "La versión no admite una instalación de actualización",
            "runtime_update_target_not_candidate",
        ));
    }
    if !runtime.channel_policy.allows(version.channel) {
        return Err(LarmError::new(
            "CONFLICT",
            "El canal de esta versión está bloqueado por la política del runtime",
            "runtime_update_channel_blocked",
        ));
    }
    let install = InstallRequest {
        runtime_id: runtime.id.clone(),
        upstream_version: version.version_string.clone(),
        strategy: request.strategy,
        storage_location_id: request.storage_location_id.clone(),
        profile_lock_hash: request.profile_lock_hash.clone(),
        external_host_id: request.external_host_id.clone(),
        execution_host_id: request.execution_host_id.clone(),
    };
    install.validate()?;
    Ok(install)
}

async fn submit_update(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&version_id, &headers)?;
    let request: RuntimeUpdateRequest = strict(&body, &headers)?;
    request.validate().map_err(fail(&ctx))?;
    let version = state
        .repo
        .version(&version_id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(filter_error()))?;
    if version.lifecycle != RuntimeVersionLifecycle::Candidate {
        return Err(fail(&ctx)(LarmError::new(
            "CONFLICT",
            "La versión no admite una instalación de actualización",
            "runtime_update_target_not_candidate",
        )));
    }
    let runtime = state
        .repo
        .runtime(&version.runtime_id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(filter_error()))?;
    let install = update_install_request(&runtime, &version, &request).map_err(fail(&ctx))?;
    let request_hash = p032_hash(&serde_json::json!({
        "runtime_version_id": &version_id,
        "request": &request,
    }))
    .map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let receipt = state
        .install_submitter
        .submit(
            &install,
            &ctx.principal.id,
            key,
            &request_hash,
            &ctx.mutation(),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

async fn rollback_version(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&version_id, &headers)?;
    let request: RuntimeRollbackRequest = strict(&body, &headers)?;
    request.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let hash = p032_hash(&serde_json::json!({
        "runtime_version_id": version_id,
        "request": request,
    }))
    .map_err(fail(&ctx))?;
    let receipt = state
        .repo
        .rollback_version(&version_id, &request, key, &hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &receipt))
}

async fn pin_version(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(version_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&version_id, &headers)?;
    let request: RuntimePinRequest = strict(&body, &headers)?;
    request.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let hash = p032_hash(&serde_json::json!({
        "runtime_version_id": version_id,
        "request": request,
    }))
    .map_err(fail(&ctx))?;
    let receipt = state
        .repo
        .pin_version(&version_id, &request, key, &hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &receipt))
}

async fn list_pins(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(&params, &["runtime_version_id"]).map_err(fail(&ctx))?;
    let version_id = params.get("runtime_version_id").cloned();
    if version_id.as_deref().is_some_and(|id| !is_uuid_v7(id)) {
        return Err(fail(&ctx)(filter_error()));
    }
    let filter = format!(
        "runtime-pins:{}",
        p032_hash(&serde_json::json!({ "runtime_version_id": version_id })).map_err(fail(&ctx))?
    );
    let repo = state.repo.clone();
    let filter_version_id = version_id.clone();
    let page = state
        .cursors
        .page(
            &filter,
            cursor.as_deref(),
            limit,
            |pin: &RuntimeVersionPin| (pin.created_at.clone(), pin.id.clone()),
            move |after, count| async move {
                repo.pins(after, count, filter_version_id.as_deref()).await
            },
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn unpin(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path((variant_id, launch_profile_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&variant_id, &headers)?;
    crate::http::uuid_param(&launch_profile_id, &headers)?;
    let key = crate::http::idempotency_key(&headers)?;
    let hash = p032_hash(&serde_json::json!({
        "variant_id": variant_id,
        "launch_profile_id": launch_profile_id,
    }))
    .map_err(fail(&ctx))?;
    state
        .repo
        .unpin(&variant_id, &launch_profile_id, key, &hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok((StatusCode::NO_CONTENT, axum::body::Body::empty()).into_response())
}

async fn change_retention_policy(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let if_match = i64::try_from(crate::http::if_match(&headers)?).map_err(|_| {
        fail(&ctx)(LarmError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })?;
    let policy: RuntimeRetentionPolicy = strict(&body, &headers)?;
    policy.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let hash = p032_hash(&serde_json::json!({
        "runtime_id": id,
        "if_match": if_match,
        "policy": policy,
    }))
    .map_err(fail(&ctx))?;
    let value = state
        .repo
        .change_retention_policy(&id, if_match, &policy, key, &hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &value))
}

async fn list_execution_hosts(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, filter) = filters(&params, &["q", "state"]).map_err(fail(&ctx))?;
    if let Some(raw) = &filter.state {
        serde_json::from_value::<ExternalHostState>(serde_json::json!(raw))
            .map_err(|_| fail(&ctx)(filter_error()))?;
    }
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "execution-hosts",
        ExecutionHost,
        execution_hosts
    )
}
async fn list_backends(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, filter) =
        filters(&params, &["q", "state", "runtime_id"]).map_err(fail(&ctx))?;
    if let Some(raw) = &filter.state {
        serde_json::from_value::<BackendStatus>(serde_json::json!(raw))
            .map_err(|_| fail(&ctx)(filter_error()))?;
    }
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "runtime-backends",
        RuntimeBackend,
        backends
    )
}
async fn list_hosts(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit, filter) = filters(&params, &["q", "state"]).map_err(fail(&ctx))?;
    if let Some(raw) = &filter.state {
        serde_json::from_value::<ExternalHostState>(serde_json::json!(raw))
            .map_err(|_| fail(&ctx)(filter_error()))?;
    }
    page!(
        state,
        ctx,
        cursor,
        limit,
        filter,
        "external-hosts",
        ExternalHost,
        external_hosts
    )
}
async fn create_external_host(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: ExternalHostCreate =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let host = request.to_record().map_err(fail(&ctx))?;
    state
        .repo
        .record_external_host(&host, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &host))
}
async fn create(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: RuntimeCreate = strict(&body, &headers)?;
    let key = crate::http::idempotency_key(&headers)?;
    let value = state
        .service
        .create(
            &request,
            key,
            &hex::encode(Sha256::digest(&body)),
            &ctx.mutation(),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::CREATED, &value))
}
async fn change_policy(
    State(state): State<RuntimesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)?;
    let version = i64::try_from(crate::http::if_match(&headers)?).map_err(|_| {
        fail(&ctx)(LarmError::new(
            "VALIDATION_FAILED",
            "If-Match fuera de rango",
            "if_match_invalid",
        ))
    })?;
    let policy: RuntimeChannelPolicy = strict(&body, &headers)?;
    let value = state
        .service
        .change_policy(&id, version, &policy, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &value))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::{submit_install, update_install_request};
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Extension, Router,
    };
    use larm_core::security::{memory::MemoryCredentials, memory::MemorySecrets, Principals};
    use larm_domain::{
        auth::{ApiScope, McpPermission, PrincipalKind},
        bridge::{JobPhase, JobReceipt},
        error::LarmError,
        ports::{MutationContext, PrincipalAuthenticator},
        runtime::{
            InstallationStrategy, Runtime, RuntimeChannel, RuntimeChannelPolicy, RuntimePromotion,
            RuntimeRetentionPolicy, RuntimeUpdateRequest, RuntimeVersion, RuntimeVersionLifecycle,
        },
        runtime_install::{InstallRequest, RuntimeInstallSubmitter},
    };
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[derive(Debug, Clone)]
    struct Call {
        request: InstallRequest,
        principal_id: String,
        idempotency_key: String,
        request_hash: String,
        ctx: MutationContext,
    }

    #[derive(Default)]
    struct Submitter {
        calls: Mutex<Vec<Call>>,
    }

    /// Sin transformaciones en este entorno de prueba: la cancelación cae
    /// siempre a la de instalaciones de runtime.
    #[derive(Default)]
    struct NoTransformations;

    #[async_trait]
    impl larm_domain::ports::TransformationService for NoTransformations {
        async fn create_plan(
            &self,
            _: &larm_domain::transformations::TransformationRequest,
            _: &MutationContext,
        ) -> Result<larm_domain::transformations::TransformationPlanResponse, LarmError> {
            Err(LarmError::new(
                larm_domain::error::ErrorCode::BackendUnavailable,
                "Sin motor de transformaciones en este entorno de prueba",
                "transformation_engine_unavailable",
            ))
        }

        async fn execute_plan(
            &self,
            _: &larm_domain::transformations::ExecuteTransformationPlan,
            _: &MutationContext,
            _: &larm_domain::transformations::TransformationActivationIdempotency,
        ) -> Result<JobReceipt, LarmError> {
            Err(LarmError::new(
                larm_domain::error::ErrorCode::BackendUnavailable,
                "Sin motor de transformaciones en este entorno de prueba",
                "transformation_engine_unavailable",
            ))
        }

        async fn restart_plan(
            &self,
            _: &larm_domain::transformations::TransformationRestart,
            _: &larm_domain::transformations::ExecuteTransformationPlan,
            _: &MutationContext,
            _: &larm_domain::transformations::TransformationActivationIdempotency,
        ) -> Result<JobReceipt, LarmError> {
            Err(LarmError::new(
                larm_domain::error::ErrorCode::BackendUnavailable,
                "Sin motor de transformaciones en este entorno de prueba",
                "transformation_engine_unavailable",
            ))
        }

        async fn list_jobs(
            &self,
            _: Option<(String, String)>,
            _: u32,
            _: &MutationContext,
        ) -> Result<Vec<larm_domain::transformations::TransformationJob>, LarmError> {
            Err(LarmError::new(
                larm_domain::error::ErrorCode::BackendUnavailable,
                "Sin motor de transformaciones en este entorno de prueba",
                "transformation_engine_unavailable",
            ))
        }

        async fn cancel_job(
            &self,
            _: &str,
            _: &str,
            _: &MutationContext,
        ) -> Result<JobReceipt, LarmError> {
            Err(LarmError::new(
                larm_domain::error::ErrorCode::NotFound,
                "Trabajo desconocido",
                "transformation_job_missing",
            ))
        }
    }

    #[async_trait]
    impl RuntimeInstallSubmitter for Submitter {
        async fn submit(
            &self,
            request: &InstallRequest,
            principal_id: &str,
            idempotency_key: &str,
            request_hash: &str,
            ctx: &MutationContext,
        ) -> Result<JobReceipt, LarmError> {
            self.calls.lock().unwrap().push(Call {
                request: request.clone(),
                principal_id: principal_id.into(),
                idempotency_key: idempotency_key.into(),
                request_hash: request_hash.into(),
                ctx: ctx.clone(),
            });
            let job_id = uuid::Uuid::now_v7().to_string();
            Ok(JobReceipt {
                status_url: format!("/api/v1/jobs/{job_id}"),
                job_id,
                state: JobPhase::Queued,
            })
        }

        async fn cancel(
            &self,
            job_id: &str,
            _principal_id: &str,
            _ctx: &MutationContext,
        ) -> Result<JobReceipt, LarmError> {
            Ok(JobReceipt {
                job_id: job_id.into(),
                status_url: format!("/api/v1/jobs/{job_id}"),
                state: JobPhase::Cancelled,
            })
        }
    }

    struct Api {
        router: Router,
        submitter: Arc<Submitter>,
        tokens: Vec<String>,
        ids: Vec<String>,
    }

    async fn setup() -> Api {
        let principals = Arc::new(Principals::new(
            Arc::new(MemoryCredentials::default()),
            Arc::new(MemorySecrets::default()),
        ));
        let mut tokens = Vec::new();
        let mut ids = Vec::new();
        for (name, kind, scopes, mcp) in [
            (
                "writer",
                PrincipalKind::Operator,
                vec![ApiScope::Read, ApiScope::RuntimesWrite],
                vec![],
            ),
            (
                "reader",
                PrincipalKind::Operator,
                vec![ApiScope::Read],
                vec![],
            ),
            (
                "mcp-reader",
                PrincipalKind::Mcp,
                vec![ApiScope::RuntimesWrite],
                vec![McpPermission::Read],
            ),
        ] {
            let (principal, token) = principals
                .enroll(
                    kind,
                    name,
                    scopes,
                    mcp,
                    None,
                    &MutationContext {
                        actor_id: "fixture".into(),
                        correlation_id: uuid::Uuid::now_v7().to_string(),
                    },
                )
                .await
                .unwrap();
            if name == "reader" {
                assert!(!principal.has_api_scope(ApiScope::RuntimesWrite));
            }
            ids.push(principal.id);
            tokens.push(token);
        }
        let reader = principals.authenticate(&tokens[1]).await.unwrap().unwrap();
        assert!(!reader.has_api_scope(ApiScope::RuntimesWrite));
        let operation = larm_domain::auth::Catalog::contract()
            .find("POST", "/api/v1/runtime-installations")
            .unwrap();
        assert!(larm_domain::auth::authorize(&reader, operation).is_err());
        let auth: Arc<dyn PrincipalAuthenticator> = principals;
        let submitter = Arc::new(Submitter::default());
        let submitter_extension: Arc<dyn RuntimeInstallSubmitter> = submitter.clone();
        let router = Router::new()
            .route("/api/v1/runtime-installations", post(submit_install))
            .merge(crate::jobs::cancellation_routes(
                submitter_extension.clone(),
                Arc::new(NoTransformations),
            ))
            .layer(Extension(submitter_extension))
            .layer(axum::middleware::from_fn_with_state(
                auth,
                crate::security::guard,
            ));
        Api {
            router,
            submitter,
            tokens,
            ids,
        }
    }

    const CID: &str = "0192aaaa-0000-7000-8000-000000000077";

    async fn call(
        api: &Api,
        token: Option<&str>,
        body: &str,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/runtime-installations")
            .header("x-correlation-id", CID)
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = api
            .router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "respuesta HTTP {status} sin JSON: {error}; {:?}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, value)
    }

    async fn call_cancel(api: &Api, token: Option<&str>, job_id: &str) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/jobs/{job_id}/cancel"))
            .header("x-correlation-id", CID);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = api
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "respuesta HTTP {status} sin JSON: {error}; {:?}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, value)
    }

    fn request_body() -> Value {
        json!({
            "runtime_id": uuid::Uuid::now_v7().to_string(),
            "upstream_version": "1.2.3",
            "strategy": "PREBUILT_BINARY",
            "storage_location_id": uuid::Uuid::now_v7().to_string(),
            "profile_lock_hash": "a".repeat(64),
            "external_host_id": null,
            "execution_host_id": null
        })
    }

    #[test]
    fn update_resuelve_candidato_y_aplica_canal_permitido_antes_de_admision() {
        let runtime_id = uuid::Uuid::now_v7().to_string();
        let version_id = uuid::Uuid::now_v7().to_string();
        let runtime = Runtime {
            id: runtime_id.clone(),
            created_at: "2026-10-25T00:00:00.000Z".into(),
            updated_at: "2026-10-25T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Default::default(),
            name: "fixture".into(),
            adapter_id: uuid::Uuid::now_v7().to_string(),
            capabilities: vec![],
            default_version_id: None,
            channel_policy: RuntimeChannelPolicy {
                allowed_channels: vec![RuntimeChannel::Stable],
            },
            retention_policy: RuntimeRetentionPolicy::default(),
        };
        let version = RuntimeVersion {
            id: version_id,
            created_at: "2026-10-25T00:00:00.000Z".into(),
            updated_at: "2026-10-25T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Default::default(),
            runtime_id: runtime_id.clone(),
            version_string: "1.2.3-custom".into(),
            commit: None,
            build: None,
            channel: RuntimeChannel::Stable,
            released_at: None,
            artifact_hash: None,
            promotion: RuntimePromotion::None,
            lifecycle: RuntimeVersionLifecycle::Candidate,
        };
        let request = RuntimeUpdateRequest {
            storage_location_id: uuid::Uuid::now_v7().to_string(),
            strategy: InstallationStrategy::PrebuiltBinary,
            profile_lock_hash: "a".repeat(64),
            external_host_id: None,
            execution_host_id: None,
        };

        let install = update_install_request(&runtime, &version, &request).unwrap();
        assert_eq!(install.runtime_id, runtime_id);
        assert_eq!(install.upstream_version, "1.2.3-custom");
        assert_eq!(install.profile_lock_hash, request.profile_lock_hash);

        let mut rejected = version.clone();
        rejected.lifecycle = RuntimeVersionLifecycle::Validated;
        assert_eq!(
            update_install_request(&runtime, &rejected, &request)
                .unwrap_err()
                .details
                .reason,
            "runtime_update_target_not_candidate"
        );
        let mut blocked = version;
        blocked.channel = RuntimeChannel::Nightly;
        assert_eq!(
            update_install_request(&runtime, &blocked, &request)
                .unwrap_err()
                .details
                .reason,
            "runtime_update_channel_blocked"
        );
    }

    #[tokio::test]
    async fn post_delega_solicitud_actor_hash_crudo_y_recibo_202() {
        let api = setup().await;
        let expected = request_body();
        let raw = format!("\n {expected} \n");
        let (status, receipt) = call(&api, Some(&api.tokens[0]), &raw, Some("install:one")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let receipt: JobReceipt = serde_json::from_value(receipt).unwrap();
        assert_eq!(receipt.state, JobPhase::Queued);
        assert_eq!(
            receipt.status_url,
            format!("/api/v1/jobs/{}", receipt.job_id)
        );

        let recorded = api.submitter.calls.lock().unwrap()[0].clone();
        assert_eq!(serde_json::to_value(recorded.request).unwrap(), expected);
        assert_eq!(recorded.principal_id, api.ids[0]);
        assert_eq!(recorded.idempotency_key, "install:one");
        assert_eq!(
            recorded.request_hash,
            hex::encode(Sha256::digest(raw.as_bytes()))
        );
        assert_eq!(recorded.ctx.actor_id, api.ids[0]);
        assert_eq!(recorded.ctx.correlation_id, CID);
    }

    #[tokio::test]
    async fn autorizacion_y_entrada_invalida_no_inician_trabajo() {
        let api = setup().await;
        for (token, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(api.tokens[1].as_str()), StatusCode::FORBIDDEN),
            (Some(api.tokens[2].as_str()), StatusCode::FORBIDDEN),
        ] {
            let (status, error) = call(&api, token, "invalid-json", Some("install:key")).await;
            assert_eq!(status, expected, "token {token:?}, error: {error}");
            assert_eq!(error["correlation_id"], CID);
        }

        let mut extra = request_body();
        extra["trust"] = json!("VERIFIED");
        for (body, key) in [
            ("invalid-json".to_string(), Some("install:key")),
            (request_body().to_string(), None),
            (request_body().to_string(), Some("invalid key")),
            (extra.to_string(), Some("install:key")),
        ] {
            let (status, error) = call(&api, Some(&api.tokens[0]), &body, key).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["correlation_id"], CID);
        }
        let mut invalid = request_body();
        invalid["profile_lock_hash"] = json!("A".repeat(64));
        let (status, _) = call(
            &api,
            Some(&api.tokens[0]),
            &invalid.to_string(),
            Some("install:key"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(api.submitter.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelar_job_exige_scope_de_escritura_y_devuelve_recibo_cancelado() {
        let api = setup().await;
        let job_id = uuid::Uuid::now_v7().to_string();
        let (status, body) = call_cancel(&api, Some(&api.tokens[0]), &job_id).await;
        assert_eq!(status, StatusCode::OK);
        let receipt: JobReceipt = serde_json::from_value(body).unwrap();
        assert_eq!(receipt.job_id, job_id);
        assert_eq!(receipt.state, JobPhase::Cancelled);

        let (status, error) = call_cancel(&api, Some(&api.tokens[1]), &job_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["correlation_id"], CID);

        let (status, error) = call_cancel(&api, Some(&api.tokens[0]), "not-uuidv7").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["correlation_id"], CID);
    }
}
