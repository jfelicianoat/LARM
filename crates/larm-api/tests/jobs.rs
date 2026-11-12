//! `GET /api/v1/jobs/{id}` (P007) con fuentes registradas y el guard real.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobState};
use larm_domain::ports::{JobSource, MutationContext, PrincipalAuthenticator};
use std::sync::Arc;
use tower::ServiceExt;

const MINE: &str = "0192aaaa-0000-7000-8000-00000000000a";
const OTHERS: &str = "0192aaaa-0000-7000-8000-00000000000b";
const BROKEN: &str = "0192aaaa-0000-7000-8000-00000000000c";

/// Fuente que solo enseña los jobs de su dueño.
struct Owned;

#[async_trait]
impl JobSource for Owned {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, ApiError> {
        let owner = if id == MINE {
            "lector"
        } else if id == OTHERS {
            "otro"
        } else {
            return Ok(None);
        };
        if principal.name != owner {
            return Ok(None);
        }
        Ok(Some(JobState {
            job_id: id.into(),
            state: JobPhase::Running,
            resource_type: "download_job".into(),
            resource_id: None,
            progress: serde_json::Map::new(),
            result: None,
            error: None,
            evidence_refs: vec![],
        }))
    }
}

/// Fuente que no sabe nada y otra que falla.
struct Empty;
struct Broken;

#[async_trait]
impl JobSource for Empty {
    async fn job(&self, _: &Principal, _: &str) -> Result<Option<JobState>, ApiError> {
        Ok(None)
    }
}

#[async_trait]
impl JobSource for Broken {
    async fn job(&self, _: &Principal, id: &str) -> Result<Option<JobState>, ApiError> {
        if id == BROKEN {
            Err(
                ApiError::new("UPSTREAM_UNAVAILABLE", "fuente caída", "source_down")
                    .retryable(true),
            )
        } else {
            Ok(None)
        }
    }
}

async fn setup() -> (axum::Router, String) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "t".into(),
        correlation_id: "c".into(),
    };
    let (_, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "lector",
            vec![ApiScope::Read],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let sources: Vec<Arc<dyn JobSource>> = vec![Arc::new(Empty), Arc::new(Broken), Arc::new(Owned)];
    let app = larm_api::jobs::routes(Arc::new(sources))
        .layer(axum::middleware::from_fn_with_state(auth, guard));
    (app, token)
}

async fn get(app: &axum::Router, id: &str, token: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::get(format!("/api/v1/jobs/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn busca_en_las_fuentes_y_respeta_la_visibilidad() {
    let (app, token) = setup().await;
    let (s, v) = get(&app, MINE, &token).await;
    assert_eq!(
        (s, v["job_id"].as_str(), v["state"].as_str()),
        (StatusCode::OK, Some(MINE), Some("RUNNING"))
    );
    // el de otro principal no se distingue de uno inexistente
    let (s, v) = get(&app, OTHERS, &token).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
    );
    let (s, _) = get(&app, "0192aaaa-0000-7000-8000-0000000000ff", &token).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn id_invalido_fuente_caida_y_sin_token() {
    let (app, token) = setup().await;
    let (s, v) = get(&app, "no-uuid", &token).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("path_id_invalid"))
    );
    let (s, v) = get(&app, BROKEN, &token).await;
    assert_eq!(
        (s, v["code"].as_str(), v["retryable"].as_bool()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("UPSTREAM_UNAVAILABLE"),
            Some(true)
        )
    );
    let (s, _) = get(&app, MINE, "malo").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
