//! API para adquirir y consultar locks inmutables de perfiles externos (P099).

use crate::{http::json, security::AuthContext};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Extension, Router,
};
use larm_domain::{
    error::LarmError,
    runtime_profiles::{RuntimeProfileLock, RuntimeProfileLockRepository},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct RuntimeProfilesState {
    pub profiles: Arc<dyn RuntimeProfileLockRepository>,
}

pub fn routes(state: RuntimeProfilesState) -> Router {
    Router::new()
        .route(
            "/api/v1/runtime-profile-locks/{lock_sha256}",
            get(read_lock).put(acquire_lock),
        )
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(LarmError) -> crate::http::Failure + '_ {
    move |error| crate::http::Failure::from(error).recorrelate(&ctx.correlation_id)
}

fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash_error() -> LarmError {
    LarmError::new(
        "VALIDATION_FAILED",
        "Hash de lock no válido",
        "runtime_profile_lock_hash_invalid",
    )
}

async fn read_lock(
    State(state): State<RuntimeProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(lock_sha256): Path<String>,
) -> crate::http::Answer {
    if !hash_valid(&lock_sha256) {
        return Err(fail(&ctx)(hash_error()));
    }
    let lock = state
        .profiles
        .get_by_hash(&lock_sha256)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| {
            fail(&ctx)(LarmError::new(
                "NOT_FOUND",
                "Lock de perfil runtime desconocido",
                "runtime_profile_lock_not_found",
            ))
        })?;
    Ok(json(StatusCode::OK, &lock))
}

async fn acquire_lock(
    State(state): State<RuntimeProfilesState>,
    Extension(ctx): Extension<AuthContext>,
    Path(lock_sha256): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> crate::http::Answer {
    if !hash_valid(&lock_sha256) {
        return Err(fail(&ctx)(hash_error()));
    }
    let lock: RuntimeProfileLock = crate::http::strict(&body, &headers)?;
    lock.validate().map_err(fail(&ctx))?;
    if lock.lock_sha256 != lock_sha256 {
        return Err(fail(&ctx)(LarmError::new(
            "VALIDATION_FAILED",
            "El hash del lock no coincide con la ruta",
            "runtime_profile_lock_hash_path_mismatch",
        )));
    }
    state
        .profiles
        .acquire(&lock, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &lock))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use larm_core::security::{memory::MemoryCredentials, memory::MemorySecrets, Principals};
    use larm_domain::{
        auth::{ApiScope, McpPermission, PrincipalKind},
        ports::{MutationContext, PrincipalAuthenticator},
        runtime_profiles::{RuntimeProfileKey, LOCK_SCHEMA, REQUIRED_FIELDS},
    };
    use serde_json::{json, Value};
    use std::{collections::BTreeMap, sync::Mutex};
    use tower::ServiceExt;

    #[derive(Default)]
    struct MemoryProfiles {
        locks: Mutex<BTreeMap<String, RuntimeProfileLock>>,
        contexts: Mutex<Vec<MutationContext>>,
    }

    #[async_trait]
    impl RuntimeProfileLockRepository for MemoryProfiles {
        async fn acquire(
            &self,
            lock: &RuntimeProfileLock,
            ctx: &MutationContext,
        ) -> Result<bool, LarmError> {
            lock.validate()?;
            self.contexts.lock().unwrap().push(ctx.clone());
            let mut locks = self.locks.lock().unwrap();
            match locks.get(&lock.lock_sha256) {
                Some(existing) if existing == lock => Ok(false),
                Some(_) => Err(LarmError::new(
                    "CONFLICT",
                    "Lock conflictivo",
                    "runtime_profile_lock_identity_conflict",
                )),
                None => {
                    locks.insert(lock.lock_sha256.clone(), lock.clone());
                    Ok(true)
                }
            }
        }

        async fn get_by_hash(
            &self,
            lock_sha256: &str,
        ) -> Result<Option<RuntimeProfileLock>, LarmError> {
            Ok(self.locks.lock().unwrap().get(lock_sha256).cloned())
        }
    }

    struct Api {
        router: Router,
        profiles: Arc<MemoryProfiles>,
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
        for (name, scopes) in [
            ("writer", vec![ApiScope::Read, ApiScope::RuntimesWrite]),
            ("reader", vec![ApiScope::Read]),
        ] {
            let (principal, token) = principals
                .enroll(
                    PrincipalKind::Operator,
                    name,
                    scopes,
                    Vec::<McpPermission>::new(),
                    None,
                    &MutationContext {
                        actor_id: "fixture".into(),
                        correlation_id: uuid::Uuid::now_v7().to_string(),
                    },
                )
                .await
                .unwrap();
            ids.push(principal.id);
            tokens.push(token);
        }
        let auth: Arc<dyn PrincipalAuthenticator> = principals;
        let profiles = Arc::new(MemoryProfiles::default());
        let repository: Arc<dyn RuntimeProfileLockRepository> = profiles.clone();
        let router = Router::new()
            .merge(routes(RuntimeProfilesState {
                profiles: repository,
            }))
            .layer(axum::middleware::from_fn_with_state(
                auth,
                crate::security::guard,
            ));
        Api {
            router,
            profiles,
            tokens,
            ids,
        }
    }

    const CID: &str = "0192aaaa-0000-7000-8000-000000000077";

    async fn call(
        api: &Api,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-correlation-id", CID)
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = api
            .router
            .clone()
            .oneshot(request.body(Body::from(body.unwrap_or_default())).unwrap())
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

    fn sample_lock() -> RuntimeProfileLock {
        let key = RuntimeProfileKey {
            runtime: "llama.cpp".into(),
            source_url: "https://example.org/llama-server/1.0".into(),
            source_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            source_sha256: "a".repeat(64),
            upstream_version: "1.0.0".into(),
            os: "windows".into(),
            os_build: "10.0.22631".into(),
            backend: "CPU".into(),
            protocol: "openai-compatible".into(),
        };
        let mut fields = serde_json::Map::new();
        for field in REQUIRED_FIELDS {
            fields.insert((*field).into(), Value::Null);
        }
        fields.insert("upstream_version".into(), json!("1.0.0"));
        fields.insert("install_strategy".into(), json!("PREBUILT_BINARY"));
        fields.insert("installation_strategies".into(), json!(["PREBUILT_BINARY"]));
        fields.insert("fixture_hashes".into(), json!(["b".repeat(64)]));
        fields.insert(
            "evidence_refs".into(),
            json!(["docs/evidence/profile.json"]),
        );
        let mut lock = RuntimeProfileLock {
            schema_version: LOCK_SCHEMA.into(),
            key,
            fields: Value::Object(fields),
            qualification: "UNQUALIFIED".into(),
            lock_sha256: String::new(),
        };
        lock.lock_sha256 = lock.computed_hash().unwrap();
        lock
    }

    #[tokio::test]
    async fn put_adquiere_lock_inmutable_y_get_lo_consulta() {
        let api = setup().await;
        let lock = sample_lock();
        let path = format!("/api/v1/runtime-profile-locks/{}", lock.lock_sha256);
        let (status, received) = call(
            &api,
            "PUT",
            &path,
            Some(&api.tokens[0]),
            Some(serde_json::to_string(&lock).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(received, serde_json::to_value(&lock).unwrap());
        assert_eq!(
            api.profiles.contexts.lock().unwrap()[0].actor_id,
            api.ids[0]
        );
        assert_eq!(api.profiles.contexts.lock().unwrap()[0].correlation_id, CID);

        let (status, replayed) = call(
            &api,
            "PUT",
            &path,
            Some(&api.tokens[0]),
            Some(serde_json::to_string(&lock).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed, serde_json::to_value(&lock).unwrap());
        assert_eq!(api.profiles.locks.lock().unwrap().len(), 1);

        let (status, received) = call(&api, "GET", &path, Some(&api.tokens[1]), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(received, serde_json::to_value(lock).unwrap());

        let missing = format!("/api/v1/runtime-profile-locks/{}", "c".repeat(64));
        let (status, error) = call(&api, "GET", &missing, Some(&api.tokens[1]), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["details"]["reason"], "runtime_profile_lock_not_found");
    }

    #[tokio::test]
    async fn put_rechaza_scope_hash_o_lock_invalidos_sin_persistir() {
        let api = setup().await;
        let lock = sample_lock();
        let path = format!("/api/v1/runtime-profile-locks/{}", lock.lock_sha256);
        let (status, error) = call(
            &api,
            "PUT",
            &path,
            Some(&api.tokens[1]),
            Some(serde_json::to_string(&lock).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["correlation_id"], CID);

        let mismatch = format!("/api/v1/runtime-profile-locks/{}", "c".repeat(64));
        let (status, error) = call(
            &api,
            "PUT",
            &mismatch,
            Some(&api.tokens[0]),
            Some(serde_json::to_string(&lock).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["details"]["reason"],
            "runtime_profile_lock_hash_path_mismatch"
        );

        let mut invalid: Value = serde_json::to_value(&lock).unwrap();
        invalid["qualification"] = json!("QUALIFIED");
        let (status, _) = call(
            &api,
            "PUT",
            &path,
            Some(&api.tokens[0]),
            Some(invalid.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(api.profiles.locks.lock().unwrap().is_empty());

        let (status, _) = call(
            &api,
            "GET",
            "/api/v1/runtime-profile-locks/not-a-hash",
            Some(&api.tokens[1]),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
