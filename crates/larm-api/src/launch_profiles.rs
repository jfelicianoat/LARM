//! CRUD de LaunchProfile (P034).

use crate::{
    http::{idempotency_key, if_match, json, strict, Answer, Failure},
    security::AuthContext,
};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Router,
};
use larm_core::security::CursorSigner;
use larm_domain::{
    bridge::{ApiError, Page},
    error::LarmError,
    launch_profile::{LaunchProfile, LaunchProfileCreate, LaunchProfileRepository},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub struct LaunchProfilesState {
    pub repo: Arc<dyn LaunchProfileRepository>,
    pub cursors: Arc<CursorSigner>,
}

pub fn routes(state: LaunchProfilesState) -> Router {
    Router::new()
        .route(
            "/api/v1/launch-profiles",
            get(list_profiles).post(create_profile),
        )
        .route(
            "/api/v1/launch-profiles/{id}",
            get(read_profile)
                .patch(update_profile)
                .delete(delete_profile),
        )
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> Failure + '_ {
    move |error| Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn api_error(error: ApiError, ctx: &AuthContext) -> Failure {
    Failure::from(error).recorrelate(&ctx.correlation_id)
}

async fn list_profiles(
    State(state): State<LaunchProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(api_error_for(&ctx))?;
    let page: Page<LaunchProfile> = state
        .cursors
        .page(
            "launch-profiles",
            cursor.as_deref(),
            limit,
            |profile: &LaunchProfile| (profile.created_at.clone(), profile.id.clone()),
            |after, count| state.repo.list_launch_profiles(after, count),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

fn api_error_for(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |error| api_error(error, ctx)
}

async fn read_profile(
    State(state): State<LaunchProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let profile = state
        .repo
        .launch_profile(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(profile_not_found(&id)))?;
    Ok(profile_json(StatusCode::OK, &profile))
}

async fn create_profile(
    State(state): State<LaunchProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let key =
        idempotency_key(&headers).map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let request: LaunchProfileCreate =
        strict(&body, &headers).map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail(&ctx))?;
    let body_hash = hex::encode(Sha256::digest(&body));
    let profile = state
        .repo
        .create_launch_profile(&request, &body_hash, key, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(profile_json(StatusCode::CREATED, &profile))
}

async fn update_profile(
    State(state): State<LaunchProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    crate::http::uuid_param(&id, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected =
        if_match(&headers).map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected = i64::try_from(expected).map_err(|_| {
        api_error(
            ApiError::new(
                "VALIDATION_FAILED",
                "If-Match debe ser la row_version",
                "if_match_invalid",
            ),
            &ctx,
        )
    })?;
    let request: LaunchProfileCreate =
        strict(&body, &headers).map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    request.validate().map_err(fail(&ctx))?;
    let profile = state
        .repo
        .update_launch_profile(&id, expected, &request, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(profile_json(StatusCode::OK, &profile))
}

async fn delete_profile(
    State(state): State<LaunchProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Answer {
    crate::http::uuid_param(&id, &headers)
        .map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected =
        if_match(&headers).map_err(|failure| failure.recorrelate(&ctx.correlation_id))?;
    let expected = i64::try_from(expected).map_err(|_| {
        api_error(
            ApiError::new(
                "VALIDATION_FAILED",
                "If-Match debe ser la row_version",
                "if_match_invalid",
            ),
            &ctx,
        )
    })?;
    state
        .repo
        .delete_launch_profile(&id, expected, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn profile_json(status: StatusCode, profile: &LaunchProfile) -> Response {
    let mut response = json(status, profile);
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", profile.row_version)) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response
}

fn profile_not_found(id: &str) -> ApiError {
    ApiError::new(
        "NOT_FOUND",
        "Perfil de lanzamiento desconocido",
        "launch_profile_not_found",
    )
    .with_resource(id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use axum::{body::Body, http::Request, middleware::from_fn_with_state};
    use larm_core::security::{
        memory::{MemoryCredentials, MemorySecrets},
        Principals,
    };
    use larm_domain::{
        auth::{ApiScope, McpPermission, PrincipalKind},
        launch::LaunchParameters,
        launch_profile::{LaunchProfile, LaunchProfileCreate},
        ports::{MutationContext, PrincipalAuthenticator},
    };
    use serde_json::Value;
    use std::{collections::BTreeMap, sync::Mutex};
    use tower::ServiceExt;

    const CID: &str = "0192aaaa-0000-7000-8000-000000000077";
    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000001";

    #[derive(Default)]
    struct MemoryState {
        profiles: BTreeMap<String, LaunchProfile>,
        idempotency: BTreeMap<String, (String, String)>,
    }

    #[derive(Default)]
    struct MemoryProfiles(Mutex<MemoryState>);

    fn repository_error(code: larm_domain::error::ErrorCode, reason: &'static str) -> LarmError {
        LarmError::new(code, "Error de repositorio para test de API", reason)
    }

    #[async_trait::async_trait]
    impl LaunchProfileRepository for MemoryProfiles {
        async fn list_launch_profiles(
            &self,
            after: Option<(String, String)>,
            limit: u32,
        ) -> Result<Vec<LaunchProfile>, LarmError> {
            let state = self.0.lock().unwrap();
            let mut profiles: Vec<_> = state
                .profiles
                .values()
                .filter(|profile| {
                    after.as_ref().is_none_or(|(created_at, id)| {
                        (profile.created_at.as_str(), profile.id.as_str())
                            > (created_at.as_str(), id.as_str())
                    })
                })
                .cloned()
                .collect();
            profiles.sort_by(|left, right| {
                (&left.created_at, &left.id).cmp(&(&right.created_at, &right.id))
            });
            profiles.truncate(limit as usize);
            Ok(profiles)
        }

        async fn launch_profile(&self, id: &str) -> Result<Option<LaunchProfile>, LarmError> {
            Ok(self.0.lock().unwrap().profiles.get(id).cloned())
        }

        async fn create_launch_profile(
            &self,
            profile: &LaunchProfileCreate,
            body_hash: &str,
            idempotency_key: &str,
            _ctx: &MutationContext,
        ) -> Result<LaunchProfile, LarmError> {
            profile.validate()?;
            let mut state = self.0.lock().unwrap();
            if let Some((old_hash, id)) = state.idempotency.get(idempotency_key) {
                if old_hash != body_hash {
                    return Err(repository_error(
                        larm_domain::error::ErrorCode::IdempotencyConflict,
                        "idempotency_conflict",
                    ));
                }
                return state.profiles.get(id).cloned().ok_or_else(|| {
                    repository_error(
                        larm_domain::error::ErrorCode::IntegrityFailed,
                        "idempotency_record_corrupt",
                    )
                });
            }
            let at = larm_domain::bridge::timestamp(chrono::Utc::now());
            let created = profile.clone().into_record(
                uuid::Uuid::now_v7().to_string(),
                at,
                1,
                serde_json::Map::new(),
            )?;
            state.profiles.insert(created.id.clone(), created.clone());
            state.idempotency.insert(
                idempotency_key.to_owned(),
                (body_hash.to_owned(), created.id.clone()),
            );
            Ok(created)
        }

        async fn update_launch_profile(
            &self,
            id: &str,
            expected_row_version: i64,
            profile: &LaunchProfileCreate,
            _ctx: &MutationContext,
        ) -> Result<LaunchProfile, LarmError> {
            profile.validate()?;
            let mut state = self.0.lock().unwrap();
            let current = state.profiles.get(id).cloned().ok_or_else(|| {
                repository_error(
                    larm_domain::error::ErrorCode::NotFound,
                    "launch_profile_not_found",
                )
            })?;
            if current.row_version != expected_row_version {
                return Err(repository_error(
                    larm_domain::error::ErrorCode::StaleRevision,
                    "stale_revision",
                ));
            }
            let at = larm_domain::bridge::timestamp(chrono::Utc::now());
            let mut updated = profile.clone().into_record(
                id.to_owned(),
                at,
                expected_row_version + 1,
                current.metadata.clone(),
            )?;
            updated.created_at.clone_from(&current.created_at);
            updated.validate_record()?;
            state.profiles.insert(id.to_owned(), updated.clone());
            Ok(updated)
        }

        async fn delete_launch_profile(
            &self,
            id: &str,
            expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            let mut state = self.0.lock().unwrap();
            let current = state.profiles.get(id).ok_or_else(|| {
                repository_error(
                    larm_domain::error::ErrorCode::NotFound,
                    "launch_profile_not_found",
                )
            })?;
            if current.row_version != expected_row_version {
                return Err(repository_error(
                    larm_domain::error::ErrorCode::StaleRevision,
                    "stale_revision",
                ));
            }
            state.profiles.remove(id);
            Ok(())
        }
    }

    struct TestApi {
        app: Router,
        writer_token: String,
        reader_token: String,
    }

    async fn setup() -> TestApi {
        let principals = Arc::new(Principals::new(
            Arc::new(MemoryCredentials::default()),
            Arc::new(MemorySecrets::default()),
        ));
        let ctx = MutationContext {
            actor_id: "fixture".into(),
            correlation_id: CID.into(),
        };
        let (_, writer_token) = principals
            .enroll(
                PrincipalKind::Operator,
                "profile-writer",
                vec![ApiScope::Read, ApiScope::RuntimesWrite],
                Vec::<McpPermission>::new(),
                None,
                &ctx,
            )
            .await
            .unwrap();
        let (_, reader_token) = principals
            .enroll(
                PrincipalKind::Operator,
                "profile-reader",
                vec![ApiScope::Read],
                Vec::<McpPermission>::new(),
                None,
                &ctx,
            )
            .await
            .unwrap();
        let auth: Arc<dyn PrincipalAuthenticator> = principals;
        let app = routes(LaunchProfilesState {
            repo: Arc::new(MemoryProfiles::default()),
            cursors: Arc::new(CursorSigner::default()),
        })
        .layer(from_fn_with_state(auth, crate::security::guard));
        TestApi {
            app,
            writer_token,
            reader_token,
        }
    }

    fn request_body(batch: u32) -> String {
        serde_json::json!({
            "name":"BALANCED",
            "backend_id":null,
            "parameters":LaunchParameters::builtin(BACKEND,4,Some(4096),batch)
        })
        .to_string()
    }

    async fn call(
        api: &TestApi,
        method: &str,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-correlation-id", CID)
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = api
            .app
            .clone()
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, Body::from))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, value)
    }

    #[tokio::test]
    async fn crud_strict_etag_scope_and_idempotency_use_repository_contract() {
        let api = setup().await;
        let initial = call(
            &api,
            "GET",
            "/api/v1/launch-profiles",
            Some(&api.reader_token),
            &[],
            None,
        )
        .await;
        assert_eq!(initial.0, StatusCode::OK);
        assert_eq!(initial.2["items"].as_array().unwrap().len(), 0);

        let denied = call(
            &api,
            "POST",
            "/api/v1/launch-profiles",
            Some(&api.reader_token),
            &[("idempotency-key", "profile:create")],
            Some(request_body(512)),
        )
        .await;
        assert_eq!(denied.0, StatusCode::FORBIDDEN);

        let created = call(
            &api,
            "POST",
            "/api/v1/launch-profiles",
            Some(&api.writer_token),
            &[("idempotency-key", "profile:create")],
            Some(request_body(512)),
        )
        .await;
        assert_eq!(created.0, StatusCode::CREATED);
        assert_eq!(created.1.get(header::ETAG).unwrap(), "\"1\"");
        let id = created.2["id"].as_str().unwrap();
        let replay = call(
            &api,
            "POST",
            "/api/v1/launch-profiles",
            Some(&api.writer_token),
            &[("idempotency-key", "profile:create")],
            Some(request_body(512)),
        )
        .await;
        assert_eq!(replay.0, StatusCode::CREATED);
        assert_eq!(replay.2["id"], id);
        let conflict = call(
            &api,
            "POST",
            "/api/v1/launch-profiles",
            Some(&api.writer_token),
            &[("idempotency-key", "profile:create")],
            Some(request_body(256)),
        )
        .await;
        assert_eq!(conflict.0, StatusCode::CONFLICT);

        let fetched = call(
            &api,
            "GET",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.reader_token),
            &[],
            None,
        )
        .await;
        assert_eq!(fetched.0, StatusCode::OK);
        assert_eq!(fetched.2["profile_hash"], created.2["profile_hash"]);
        let missing_etag = call(
            &api,
            "PATCH",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.writer_token),
            &[],
            Some(request_body(256)),
        )
        .await;
        assert_eq!(missing_etag.0, StatusCode::PRECONDITION_REQUIRED);
        let updated = call(
            &api,
            "PATCH",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.writer_token),
            &[("if-match", "\"1\"")],
            Some(request_body(256)),
        )
        .await;
        assert_eq!(updated.0, StatusCode::OK);
        assert_eq!(updated.2["row_version"], 2);
        assert_ne!(updated.2["profile_hash"], created.2["profile_hash"]);
        let stale = call(
            &api,
            "DELETE",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.writer_token),
            &[("if-match", "1")],
            None,
        )
        .await;
        assert_eq!(stale.0, StatusCode::CONFLICT);
        let deleted = call(
            &api,
            "DELETE",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.writer_token),
            &[("if-match", "2")],
            None,
        )
        .await;
        assert_eq!(deleted.0, StatusCode::NO_CONTENT);
        let gone = call(
            &api,
            "GET",
            &format!("/api/v1/launch-profiles/{id}"),
            Some(&api.reader_token),
            &[],
            None,
        )
        .await;
        assert_eq!(gone.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rechaza_query_y_json_fuera_del_contrato() {
        let api = setup().await;
        let query = call(
            &api,
            "GET",
            "/api/v1/launch-profiles?unexpected=true",
            Some(&api.reader_token),
            &[],
            None,
        )
        .await;
        assert_eq!(query.0, StatusCode::BAD_REQUEST);
        let unknown = call(
            &api,
            "POST",
            "/api/v1/launch-profiles",
            Some(&api.writer_token),
            &[("idempotency-key", "profile:invalid")],
            Some(r#"{"name":"BALANCED","backend_id":null,"parameters":{},"script":"x"}"#.into()),
        )
        .await;
        assert_eq!(unknown.0, StatusCode::BAD_REQUEST);
    }
}
