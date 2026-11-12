//! Rutas de mantenimiento (P018) con el guard real: scopes, schema,
//! aprobación obligatoria ligada al plan, If-Match y parámetros de DELETE.
//! La conducta de disco y base se prueba en `larm-models/tests/maintenance.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::maintenance::MaintenanceState;
use larm_api::security::guard;
use larm_core::security::memory::{
    MemoryApprovals, MemoryCredentials, MemoryIdempotency, MemorySecrets,
};
use larm_core::security::{Approvals, Principals};
use larm_domain::approval::ApprovalRequest;
use larm_domain::artifacts::Plan;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::{ApiError, JobPhase, JobReceipt};
use larm_domain::estimate::{Confidence, StorageEstimate};
use larm_domain::ports::{
    BackgroundTask, MutationContext, PrincipalAuthenticator, StorageMaintenance,
};
use tower::ServiceExt;

const LOC: &str = "0192f000-0000-7000-8000-00000000c001";
const VARIANT: &str = "0192f000-0000-7000-8000-000000000001";
const PLAN_ID: &str = "0192f000-0000-7000-8000-0000000000d1";
const HASH: &str = "4d1c1f08e5a3b1b8b9d1a6f6f1a2c3d4e5f60718293a4b5c6d7e8f9012345678";

fn plan(resource: &str) -> Plan {
    Plan {
        plan_id: PLAN_ID.into(),
        plan_hash: HASH.into(),
        operation: "x".into(),
        resource_ids: vec![resource.into()],
        storage_estimate: StorageEstimate {
            download_bytes: None,
            final_artifact_bytes: None,
            installed_bytes: None,
            derived_cache_bytes: None,
            temporary_peak_bytes: None,
            existing_bytes: None,
            peak_extra_bytes: None,
            required_free_bytes: None,
            upper_bound_bytes: None,
            confidence: Confidence::Exact,
            reason: String::new(),
            evidence_refs: vec![],
            per_volume: vec![],
        },
        steps: vec![],
        expires_at: "2026-10-11T00:15:00.000Z".into(),
        approval_required: true,
    }
}

#[derive(Default)]
struct Fake {
    cleaned: AtomicUsize,
    deleted: AtomicUsize,
}

#[async_trait]
impl StorageMaintenance for Fake {
    async fn analysis(&self, _: &str) -> Result<serde_json::Value, ApiError> {
        Ok(serde_json::json!({"storage_location_id": LOC, "categories": []}))
    }
    async fn cleanup_plan(&self, _: &str, _: &MutationContext) -> Result<Plan, ApiError> {
        Ok(plan(&format!("storage_location:{LOC}")))
    }
    async fn planned_cleanup(&self, _: &str, _: &str, h: &str) -> Result<Plan, ApiError> {
        if h == HASH {
            Ok(plan(&format!("storage_location:{LOC}")))
        } else {
            Err(ApiError::new("STALE_REVISION", "plan", "plan_unknown"))
        }
    }
    async fn cleanup(
        &self,
        _: &str,
        id: &str,
        _: &str,
        _: &MutationContext,
    ) -> Result<(JobReceipt, BackgroundTask), ApiError> {
        self.cleaned.fetch_add(1, Ordering::SeqCst);
        Ok((
            JobReceipt {
                job_id: id.into(),
                status_url: format!("/api/v1/jobs/{id}"),
                state: JobPhase::Running,
            },
            Box::pin(async {}),
        ))
    }
    async fn delete_plan(&self, _: &str, _: &MutationContext) -> Result<Plan, ApiError> {
        Ok(plan(&format!("variant:{VARIANT}")))
    }
    async fn planned_delete(&self, _: &str, h: &str) -> Result<Plan, ApiError> {
        if h == HASH {
            Ok(plan(&format!("variant:{VARIANT}")))
        } else {
            Err(ApiError::new("STALE_REVISION", "plan", "plan_unknown"))
        }
    }
    async fn delete(
        &self,
        _: &str,
        if_match: i64,
        _: &str,
        _: &MutationContext,
    ) -> Result<serde_json::Value, ApiError> {
        if if_match != 3 {
            return Err(ApiError::new("STALE_REVISION", "cambió", "stale_revision"));
        }
        self.deleted.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({}))
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    storage: String,
    models: String,
    models_id: String,
    storage_id: String,
    approvals: Arc<Approvals>,
    fake: Arc<Fake>,
}

async fn setup() -> Api {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "t".into(),
        correlation_id: "c".into(),
    };
    let mut tokens = Vec::new();
    for (name, scopes) in [
        ("lector", vec![ApiScope::Read]),
        ("almacen", vec![ApiScope::Read, ApiScope::StorageWrite]),
        ("modelos", vec![ApiScope::Read, ApiScope::ModelsWrite]),
    ] {
        tokens.push(
            principals
                .enroll(PrincipalKind::Operator, name, scopes, vec![], None, &ctx)
                .await
                .unwrap(),
        );
    }
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let approvals = Arc::new(Approvals {
        repo: Arc::new(MemoryApprovals::default()),
        ttl: chrono::Duration::seconds(900),
    });
    let fake = Arc::new(Fake::default());
    let app = larm_api::maintenance::routes(MaintenanceState {
        service: fake.clone(),
        approvals: approvals.clone(),
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        reader: tokens[0].1.clone(),
        storage: tokens[1].1.clone(),
        storage_id: tokens[1].0.id.clone(),
        models: tokens[2].1.clone(),
        models_id: tokens[2].0.id.clone(),
        approvals,
        fake,
    }
}

async fn call(
    api: &Api,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let res = api
        .app
        .clone()
        .oneshot(
            req.body(Body::from(body.unwrap_or_default().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn approve(api: &Api, actor: &str, resource: &str) -> String {
    api.approvals
        .grant(
            &ApprovalRequest {
                operation_digest: HASH.into(),
                resource_ids: vec![resource.into()],
                code_hash: None,
                permissions: serde_json::Map::new(),
                approved: true,
            },
            &MutationContext {
                actor_id: actor.into(),
                correlation_id: "c".into(),
            },
        )
        .await
        .unwrap()
        .resource_id
}

const NULLS: &str = r#"{"resource_id":null,"profile_id":null}"#;

#[tokio::test]
async fn analisis_y_limpieza_con_aprobacion() {
    let api = setup().await;
    let (s, _) = call(
        &api,
        "GET",
        &format!("/api/v1/storage-locations/{LOC}/analysis"),
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let plan_path = format!("/api/v1/storage-locations/{LOC}/cleanup-plan");
    let (s, _) = call(&api, "POST", &plan_path, &api.reader, Some(NULLS), &[]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "planificar es storage:write");
    let (s, p) = call(&api, "POST", &plan_path, &api.storage, Some(NULLS), &[]).await;
    assert_eq!(
        (s, p["approval_required"].as_bool()),
        (StatusCode::CREATED, Some(true))
    );
    let other = r#"{"resource_id":"0192f000-0000-7000-8000-0000000000ff","profile_id":null}"#;
    let (s, e) = call(&api, "POST", &plan_path, &api.storage, Some(other), &[]).await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("probe_not_applicable"))
    );
    let run = format!("/api/v1/storage-locations/{LOC}/cleanup");
    let body = |approval: &str| {
        format!(r#"{{"plan_id":"{PLAN_ID}","plan_hash":"{HASH}","approval_id":{approval}}}"#)
    };
    let (s, e) = call(&api, "POST", &run, &api.storage, Some(&body("null")), &[]).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("APPROVAL_REQUIRED"))
    );
    // Aprobación de otro recurso: no vale.
    let wrong = approve(&api, &api.storage_id, "storage_location:otra").await;
    let (s, e) = call(
        &api,
        "POST",
        &run,
        &api.storage,
        Some(&body(&format!("\"{wrong}\""))),
        &[],
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("approval_resources_mismatch"))
    );
    assert_eq!(api.fake.cleaned.load(Ordering::SeqCst), 0);
    let ok = approve(&api, &api.storage_id, &format!("storage_location:{LOC}")).await;
    let (s, r) = call(
        &api,
        "POST",
        &run,
        &api.storage,
        Some(&body(&format!("\"{ok}\""))),
        &[],
    )
    .await;
    assert_eq!(
        (s, r["job_id"].as_str()),
        (StatusCode::ACCEPTED, Some(PLAN_ID))
    );
    assert_eq!(api.fake.cleaned.load(Ordering::SeqCst), 1);
    let bad = r#"{"plan_id":"x","plan_hash":"y","approval_id":null}"#;
    let (s, _) = call(&api, "POST", &run, &api.storage, Some(bad), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn borrado_con_plan_aprobacion_e_if_match() {
    let api = setup().await;
    let plan_path = format!("/api/v1/models/{VARIANT}/delete-plan");
    let (s, _) = call(
        &api,
        "POST",
        &plan_path,
        &api.storage,
        Some(NULLS),
        &[("idempotency-key", "k")],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "borrar es models:write");
    let (s, p) = call(
        &api,
        "POST",
        &plan_path,
        &api.models,
        Some(NULLS),
        &[("idempotency-key", "k")],
    )
    .await;
    assert_eq!(
        (s, p["plan_hash"].as_str()),
        (StatusCode::CREATED, Some(HASH))
    );
    let path = |q: &str| format!("/api/v1/models/{VARIANT}?{q}");
    let (s, _) = call(
        &api,
        "DELETE",
        &path(&format!("plan_hash={HASH}")),
        &api.models,
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED, "sin If-Match");
    let (s, e) = call(
        &api,
        "DELETE",
        &path("approval_id=x"),
        &api.models,
        None,
        &[("if-match", "3")],
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("plan_hash_required"))
    );
    let (s, e) = call(
        &api,
        "DELETE",
        &path(&format!("plan_hash={HASH}&force=1")),
        &api.models,
        None,
        &[("if-match", "3")],
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("query_invalid"))
    );
    let (s, e) = call(
        &api,
        "DELETE",
        &path(&format!("plan_hash={HASH}")),
        &api.models,
        None,
        &[("if-match", "3")],
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("APPROVAL_REQUIRED"))
    );
    let a = approve(&api, &api.models_id, &format!("variant:{VARIANT}")).await;
    let (s, e) = call(
        &api,
        "DELETE",
        &path(&format!("plan_hash={HASH}&approval_id={a}")),
        &api.models,
        None,
        &[("if-match", "9")],
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("STALE_REVISION"))
    );
    // La aprobación se consumió en el intento anterior: hace falta otra.
    let a2 = approve(&api, &api.models_id, &format!("variant:{VARIANT}")).await;
    let (s, _) = call(
        &api,
        "DELETE",
        &path(&format!("plan_hash={HASH}&approval_id={a2}")),
        &api.models,
        None,
        &[("if-match", "3")],
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(api.fake.deleted.load(Ordering::SeqCst), 1);
}
