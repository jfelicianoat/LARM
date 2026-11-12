//! Lecturas de RuntimeInstance según P033 y las operaciones de API v1.

use crate::{http::json, security::AuthContext};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Extension, Router,
};
use larm_core::security::CursorSigner;
use larm_domain::{
    canonical,
    error::LarmError,
    runtime_ports::{RuntimeInstanceRepository, RuntimeInstanceStopRepository},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub struct InstancesState {
    pub repo: Arc<dyn RuntimeInstanceRepository>,
    pub stop_jobs: Arc<dyn RuntimeInstanceStopRepository>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: InstancesState) -> Router {
    Router::new()
        .route("/api/v1/instances", get(list_instances))
        .route("/api/v1/instances/{id}", get(read_instance))
        .route("/api/v1/instances/{id}/stop", post(stop_instance))
        .route(
            "/api/v1/runtimes/{id}/instances",
            get(list_runtime_instances),
        )
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> crate::http::Failure + '_ {
    move |error| crate::http::Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn not_found(id: &str, reason: &str) -> LarmError {
    LarmError::new("NOT_FOUND", "Instancia o runtime desconocido", reason)
        .with_resource(id.to_owned())
}

fn query_error() -> LarmError {
    LarmError::new(
        "VALIDATION_FAILED",
        "Consulta de instancias no válida",
        "runtime_instance_query_invalid",
    )
}

async fn list_instances(
    State(state): State<InstancesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> crate::http::Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    page(state, ctx, cursor, limit, None).await
}

async fn list_runtime_instances(
    State(state): State<InstancesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(runtime_id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<BTreeMap<String, String>>,
) -> crate::http::Answer {
    crate::http::uuid_param(&runtime_id, &headers)?;
    if !state
        .repo
        .runtime_exists(&runtime_id)
        .await
        .map_err(fail(&ctx))?
    {
        return Err(fail(&ctx)(not_found(&runtime_id, "runtime_not_found")));
    }
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    page(state, ctx, cursor, limit, Some(runtime_id)).await
}

async fn page(
    state: InstancesState,
    ctx: AuthContext,
    cursor: Option<String>,
    limit: Option<u32>,
    runtime_id: Option<String>,
) -> crate::http::Answer {
    let digest = canonical::sha256_hex(&serde_json::json!({
        "list": "runtime-instances",
        "runtime_id": runtime_id.as_deref(),
    }))
    .map_err(|_| fail(&ctx)(query_error()))?;
    let filter = format!("runtime-instances:{digest}");
    let repo = state.repo.clone();
    let filter_runtime_id = runtime_id.clone();
    let page = state
        .cursors
        .page(
            &filter,
            cursor.as_deref(),
            limit,
            |instance: &larm_domain::instances::RuntimeInstance| {
                (instance.created_at.clone(), instance.id.clone())
            },
            move |after, count| async move {
                repo.instances(after, count, filter_runtime_id.as_deref())
                    .await
            },
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_instance(
    State(state): State<InstancesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> crate::http::Answer {
    crate::http::uuid_param(&id, &headers)?;
    let instance = state
        .repo
        .instance(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found(&id, "runtime_instance_not_found")))?;
    Ok(json(StatusCode::OK, &instance))
}

async fn stop_instance(
    State(state): State<InstancesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> crate::http::Answer {
    crate::http::uuid_param(&id, &headers)?;
    let request: larm_domain::instances::StopRequest = crate::http::strict(&body, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail(&ctx))?;
    let key = crate::http::idempotency_key(&headers)?;
    let body_hash = hex::encode(Sha256::digest(&body));
    let receipt = state
        .stop_jobs
        .submit_stop(&id, &request, &body_hash, key, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use axum::{body::Body, http::Request};
    use larm_domain::{
        auth::{ApiScope, McpPermission, PrincipalKind},
        error::{ErrorCode, LarmError},
        instances::{
            InstanceState, RuntimeInstance, RuntimeInstanceStopJob, StopRequest, StopResult,
        },
        ports::{JobSource, MutationContext, Principal, PrincipalAuthenticator},
        runtime::RuntimeOwnership,
        runtime_ports::{RuntimeInstanceRepository, RuntimeInstanceStopRepository},
    };
    use serde_json::{Map, Value};
    use std::{collections::BTreeMap, sync::Mutex};
    use tower::ServiceExt;

    #[derive(Default)]
    struct MemoryInstances {
        instances: Mutex<BTreeMap<String, RuntimeInstance>>,
        runtimes: Mutex<Vec<String>>,
        installations: Mutex<BTreeMap<String, String>>,
    }

    #[async_trait]
    impl RuntimeInstanceRepository for MemoryInstances {
        async fn create_instance(
            &self,
            instance: &RuntimeInstance,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            instance.validate_record()?;
            let mut rows = self.instances.lock().unwrap();
            if rows.contains_key(&instance.id) {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Instancia ya existente",
                    "runtime_instance_exists",
                ));
            }
            rows.insert(instance.id.clone(), instance.clone());
            Ok(())
        }

        async fn instance(&self, id: &str) -> Result<Option<RuntimeInstance>, LarmError> {
            Ok(self.instances.lock().unwrap().get(id).cloned())
        }

        async fn instances(
            &self,
            after: Option<(String, String)>,
            limit: u32,
            runtime_id: Option<&str>,
        ) -> Result<Vec<RuntimeInstance>, LarmError> {
            let mut rows: Vec<_> = self
                .instances
                .lock()
                .unwrap()
                .values()
                .filter(|row| {
                    after.as_ref().is_none_or(|(at, id)| {
                        (row.created_at.as_str(), row.id.as_str()) > (at.as_str(), id.as_str())
                    }) && runtime_id.is_none_or(|id| {
                        self.installations
                            .lock()
                            .unwrap()
                            .get(&row.runtime_installation_id)
                            .is_some_and(|runtime| runtime == id)
                    })
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
            rows.truncate(limit as usize);
            Ok(rows)
        }

        async fn runtime_exists(&self, id: &str) -> Result<bool, LarmError> {
            Ok(self
                .runtimes
                .lock()
                .unwrap()
                .iter()
                .any(|value| value == id))
        }

        async fn transition_instance(
            &self,
            instance: &RuntimeInstance,
            expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            instance.validate_record()?;
            let mut rows = self.instances.lock().unwrap();
            let current = rows.get(&instance.id).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instancia de runtime desconocida",
                    "runtime_instance_not_found",
                )
            })?;
            if current.row_version != expected_row_version {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La instancia cambió desde la última lectura",
                    "runtime_instance_stale",
                ));
            }
            if !current.state.can_transition_to(instance.state) {
                return Err(LarmError::new(
                    ErrorCode::InvalidStateTransition,
                    "Transición de RuntimeInstance no permitida",
                    "runtime_instance_transition_invalid",
                ));
            }
            rows.insert(instance.id.clone(), instance.clone());
            Ok(instance.clone())
        }

        async fn begin_stop_instance(
            &self,
            instance: &RuntimeInstance,
            expected_row_version: i64,
            ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            self.transition_instance(instance, expected_row_version, ctx)
                .await
        }
    }

    struct Api {
        router: Router,
        token: String,
        stop_token: String,
    }

    #[derive(Default)]
    struct MemoryStopJobs {
        replays: Mutex<BTreeMap<(String, String), (String, larm_domain::bridge::JobReceipt)>>,
        submitted: Mutex<Vec<(String, StopRequest, String, String)>>,
    }

    #[async_trait]
    impl JobSource for MemoryStopJobs {
        async fn job(
            &self,
            _principal: &Principal,
            _job_id: &str,
        ) -> Result<Option<larm_domain::bridge::JobState>, LarmError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl RuntimeInstanceStopRepository for MemoryStopJobs {
        async fn submit_stop(
            &self,
            instance_id: &str,
            request: &StopRequest,
            body_hash: &str,
            idempotency_key: &str,
            ctx: &MutationContext,
        ) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
            let replay_key = (ctx.actor_id.clone(), idempotency_key.to_owned());
            let mut replays = self.replays.lock().unwrap();
            if let Some((prior_hash, receipt)) = replays.get(&replay_key) {
                if prior_hash != body_hash {
                    return Err(LarmError::new(
                        ErrorCode::IdempotencyConflict,
                        "Idempotency-Key ya usada con otro cuerpo",
                        "idempotency_body_mismatch",
                    ));
                }
                return Ok(receipt.clone());
            }
            let sequence = u16::try_from(replays.len()).unwrap() + 90;
            let receipt = larm_domain::bridge::JobReceipt {
                job_id: id(sequence),
                status_url: format!("/api/v1/jobs/{}", id(sequence)),
                state: larm_domain::bridge::JobPhase::Queued,
            };
            replays.insert(replay_key, (body_hash.to_owned(), receipt.clone()));
            self.submitted.lock().unwrap().push((
                instance_id.to_owned(),
                request.clone(),
                body_hash.to_owned(),
                idempotency_key.to_owned(),
            ));
            Ok(receipt)
        }

        async fn claim_stop_job(&self) -> Result<Option<RuntimeInstanceStopJob>, LarmError> {
            Ok(None)
        }

        async fn complete_stop_job(
            &self,
            _job_id: &str,
            _result: StopResult,
        ) -> Result<(), LarmError> {
            Ok(())
        }

        async fn fail_stop_job(&self, _job_id: &str, _error: &LarmError) -> Result<(), LarmError> {
            Ok(())
        }

        async fn recover_stop_jobs(&self) -> Result<u64, LarmError> {
            Ok(0)
        }
    }

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    fn sample(n: u16, installation: u16) -> RuntimeInstance {
        RuntimeInstance {
            id: id(n),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: id(installation),
            variant_id: id(30),
            backend_id: id(31),
            launch_profile_id: id(32),
            state: InstanceState::Ready,
            pid: Some(1234),
            process_started_at: Some("2026-10-22T00:00:00.000Z".into()),
            executable_hash: Some("a".repeat(64)),
            generation: 0,
            execution_fingerprint: Some("b".repeat(64)),
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        }
    }

    async fn setup() -> Api {
        let principals = Arc::new(larm_core::security::Principals::new(
            Arc::new(larm_core::security::memory::MemoryCredentials::default()),
            Arc::new(larm_core::security::memory::MemorySecrets::default()),
        ));
        let (principal, token) = principals
            .enroll(
                PrincipalKind::Operator,
                "reader",
                vec![ApiScope::Read],
                Vec::<McpPermission>::new(),
                None,
                &MutationContext {
                    actor_id: "fixture".into(),
                    correlation_id: id(77),
                },
            )
            .await
            .unwrap();
        let _ = principal;
        let (_, stop_token) = principals
            .enroll(
                PrincipalKind::Operator,
                "stopper",
                vec![ApiScope::Read, ApiScope::LeasesManage],
                Vec::<McpPermission>::new(),
                None,
                &MutationContext {
                    actor_id: "fixture".into(),
                    correlation_id: id(76),
                },
            )
            .await
            .unwrap();
        let rows = Arc::new(MemoryInstances::default());
        rows.runtimes.lock().unwrap().push(id(10));
        rows.installations.lock().unwrap().insert(id(12), id(10));
        rows.installations.lock().unwrap().insert(id(22), id(20));
        rows.instances
            .lock()
            .unwrap()
            .extend([(id(1), sample(1, 12)), (id(2), sample(2, 22))]);
        let repository: Arc<dyn RuntimeInstanceRepository> = rows.clone();
        let stop_jobs: Arc<dyn RuntimeInstanceStopRepository> = Arc::new(MemoryStopJobs::default());
        let auth: Arc<dyn PrincipalAuthenticator> = principals;
        let router = Router::new()
            .merge(routes(InstancesState {
                repo: repository,
                stop_jobs,
                cursors: Arc::new(CursorSigner::default()),
            }))
            .layer(axum::middleware::from_fn_with_state(
                auth,
                crate::security::guard,
            ));
        Api {
            router,
            token,
            stop_token,
        }
    }

    async fn call(api: &Api, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .uri(path)
            .header("x-correlation-id", id(88));
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
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn call_stop(
        api: &Api,
        instance_id: &str,
        token: &str,
        key: Option<&str>,
        body: &str,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/instances/{instance_id}/stop"))
            .header("x-correlation-id", id(88))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key);
        }
        let response = api
            .router
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn list_detail_runtime_filter_and_signed_pagination_follow_contract() {
        let api = setup().await;
        let (status, first) = call(&api, "/api/v1/instances?limit=1", Some(&api.token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["items"][0]["state"], "READY");
        let cursor = first["next_cursor"].as_str().unwrap();
        let (status, next) = call(
            &api,
            &format!("/api/v1/instances?limit=1&cursor={cursor}"),
            Some(&api.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(next["items"][0]["id"], id(2));

        let (status, detail) = call(
            &api,
            &format!("/api/v1/instances/{}", id(1)),
            Some(&api.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["id"], id(1));

        let (status, scoped) = call(
            &api,
            &format!("/api/v1/runtimes/{}/instances", id(10)),
            Some(&api.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(scoped["items"].as_array().unwrap().len(), 1);
        assert_eq!(scoped["items"][0]["id"], id(1));
    }

    #[tokio::test]
    async fn authorization_query_and_missing_resources_fail_closed() {
        let api = setup().await;
        let (status, _) = call(&api, "/api/v1/instances", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(&api, "/api/v1/instances?q=ready", Some(&api.token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, missing) = call(
            &api,
            &format!("/api/v1/instances/{}", id(99)),
            Some(&api.token),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["details"]["reason"], "runtime_instance_not_found");
        let (status, _) = call(
            &api,
            &format!("/api/v1/runtimes/{}/instances", id(99)),
            Some(&api.token),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(ErrorCode::NotFound.http_status(), 404);
    }

    #[tokio::test]
    async fn stop_requires_scope_and_key_then_replays_without_duplicate_submission() {
        let api = setup().await;
        let body = r#"{"reason":"operator_requested","approval_id":null}"#;
        let (status, denied) = call_stop(&api, &id(1), &api.token, Some("stop-1"), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(denied["details"]["reason"], "scope_missing");

        let (status, missing_key) = call_stop(&api, &id(1), &api.stop_token, None, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(missing_key["details"]["reason"], "idempotency_key_invalid");

        let (status, receipt) =
            call_stop(&api, &id(1), &api.stop_token, Some("stop-1"), body).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(receipt["state"], "QUEUED");
        assert_eq!(
            receipt["status_url"],
            format!("/api/v1/jobs/{}", receipt["job_id"].as_str().unwrap())
        );

        let (status, replay) = call_stop(&api, &id(1), &api.stop_token, Some("stop-1"), body).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(replay, receipt);

        let changed_body = r#"{"reason":"another_reason","approval_id":null}"#;
        let (status, conflict) =
            call_stop(&api, &id(1), &api.stop_token, Some("stop-1"), changed_body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["details"]["reason"], "idempotency_body_mismatch");
    }

    #[tokio::test]
    async fn stop_rejects_unknown_fields_and_invalid_instance_ids() {
        let api = setup().await;
        let (status, _) = call_stop(
            &api,
            &id(1),
            &api.stop_token,
            Some("stop-2"),
            r#"{"reason":"operator_requested","approval_id":null,"extra":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call_stop(
            &api,
            "not-a-uuid",
            &api.stop_token,
            Some("stop-3"),
            r#"{"reason":"operator_requested","approval_id":null}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
