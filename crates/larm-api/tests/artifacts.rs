//! Movimiento de modelos y ubicaciones de artefactos (P016) con el guard
//! real: scopes, schema, idempotencia, aprobación ligada al plan, errores
//! del servicio y paginación. El servicio es un doble: la conducta de disco
//! y base se prueba en `apps/daemon/tests/move.rs` y en la prueba real.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::artifacts::ArtifactsState;
use larm_api::security::guard;
use larm_core::security::memory::{
    MemoryApprovals, MemoryCredentials, MemoryIdempotency, MemorySecrets,
};
use larm_core::security::{Approvals, CursorSigner, Principals};
use larm_domain::approval::ApprovalRequest;
use larm_domain::artifacts::{ArtifactLocation, ArtifactState, MoveRequest, Ownership, Plan};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::estimate::{Confidence, StorageEstimate};
use larm_domain::guard::JournalStep;
use larm_domain::ports::{
    ArtifactLocationRepository, BackgroundTask, Fence, LocationSwitch, ModelMover, MutationContext,
    PrincipalAuthenticator, VariantArtifact,
};
use parking_lot::Mutex;
use tower::ServiceExt;

const VARIANT: &str = "0192f000-0000-7000-8000-000000000001";
const TARGET: &str = "0192f000-0000-7000-8000-0000000000c2";
const HASH: &str = "4d1c1f08e5a3b1b8b9d1a6f6f1a2c3d4e5f60718293a4b5c6d7e8f9012345678";

fn plan() -> Plan {
    Plan {
        plan_id: "0192f000-0000-7000-8000-0000000000d1".into(),
        plan_hash: HASH.into(),
        operation: "model.move".into(),
        resource_ids: vec![format!("variant:{VARIANT}"), "artifact:a".into()],
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
        approval_required: false,
    }
}

/// Doble del servicio: cuenta llamadas y puede fallar con un error dado.
#[derive(Default)]
struct Mover {
    executed: AtomicUsize,
    ran: Arc<AtomicUsize>,
    fail_with: Mutex<Option<ApiError>>,
}

#[async_trait]
impl ModelMover for Mover {
    async fn plan(&self, v: &str, t: &str, _: &MutationContext) -> Result<Plan, ApiError> {
        assert_eq!((v, t), (VARIANT, TARGET));
        Ok(plan())
    }
    async fn planned(&self, h: &str) -> Result<Option<Plan>, ApiError> {
        Ok((h == HASH).then(plan))
    }
    async fn execute(
        &self,
        _: &str,
        r: &MoveRequest,
        _: &MutationContext,
    ) -> Result<(Plan, BackgroundTask), ApiError> {
        if let Some(e) = self.fail_with.lock().clone() {
            return Err(e);
        }
        assert_eq!(r.plan_hash.as_deref(), Some(HASH));
        self.executed.fetch_add(1, Ordering::SeqCst);
        let ran = self.ran.clone();
        Ok((
            plan(),
            Box::pin(async move {
                ran.fetch_add(1, Ordering::SeqCst);
            }),
        ))
    }
}

/// Tres ubicaciones en memoria.
struct Rows(Vec<ArtifactLocation>);

#[async_trait]
impl ArtifactLocationRepository for Rows {
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<ArtifactLocation>, ApiError> {
        let after = after.unwrap_or_default();
        Ok(self
            .0
            .iter()
            .filter(|l| (l.created_at.clone(), l.id.clone()) > after)
            .take(limit as usize)
            .cloned()
            .collect())
    }
    async fn get(&self, id: &str) -> Result<Option<ArtifactLocation>, ApiError> {
        Ok(self.0.iter().find(|l| l.id == id).cloned())
    }
    async fn for_variant(&self, _: &str) -> Result<Option<Vec<VariantArtifact>>, ApiError> {
        Ok(None)
    }
    async fn insert_locations(
        &self,
        _: &[ArtifactLocation],
        _: &MutationContext,
    ) -> Result<usize, ApiError> {
        unreachable!("la API no registra ubicaciones")
    }
    async fn at_path(&self, _: &str, _: &str) -> Result<Vec<ArtifactLocation>, ApiError> {
        unreachable!()
    }
    async fn mark_shared(&self, _: &str, _: &str, _: &MutationContext) -> Result<usize, ApiError> {
        unreachable!()
    }
    async fn duplicates(&self) -> Result<Vec<larm_domain::ports::DuplicateGroup>, ApiError> {
        unreachable!()
    }
    async fn by_location(&self, _: &str) -> Result<Vec<ArtifactLocation>, ApiError> {
        unreachable!()
    }
    async fn set_state(
        &self,
        _: &str,
        _: i64,
        _: ArtifactState,
        _: Option<&str>,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn variants_at(&self, _: &str) -> Result<Vec<String>, ApiError> {
        unreachable!()
    }
    async fn switch(
        &self,
        _: &[LocationSwitch],
        _: &Fence,
        _: &JournalStep,
        _: &MutationContext,
    ) -> Result<Vec<i64>, ApiError> {
        unreachable!("la API no cambia el registro")
    }
}

fn row(i: u8) -> ArtifactLocation {
    ArtifactLocation {
        id: format!("0192f000-0000-7000-8000-0000000000e{i}"),
        created_at: format!("2026-10-11T00:00:0{i}.000Z"),
        updated_at: format!("2026-10-11T00:00:0{i}.000Z"),
        row_version: 1,
        metadata: serde_json::Map::new(),
        artifact_id: "0192f000-0000-7000-8000-0000000000a1".into(),
        storage_location_id: TARGET.into(),
        relative_path: format!("m/{i}.gguf"),
        file_size: larm_domain::units::DecimalU64::new(10),
        sha256: None,
        state: ArtifactState::Online,
        verified_at: None,
        ownership: Ownership::LarmManaged,
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    writer_id: String,
    mover: Arc<Mover>,
    approvals: Arc<Approvals>,
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
    let (_, reader) = principals
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
    let (writer_principal, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "gestor",
            vec![ApiScope::Read, ApiScope::StorageWrite],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let mover = Arc::new(Mover::default());
    let approvals = Arc::new(Approvals {
        repo: Arc::new(MemoryApprovals::default()),
        ttl: chrono::Duration::seconds(900),
    });
    let app = larm_api::artifacts::routes(ArtifactsState {
        locations: Arc::new(Rows(vec![row(1), row(2), row(3)])),
        mover: mover.clone(),
        approvals: approvals.clone(),
        cursors: Arc::new(CursorSigner::default()),
        idempotency: Arc::new(MemoryIdempotency::default()),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        reader,
        writer,
        writer_id: writer_principal.id,
        mover,
        approvals,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
    key: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let res = app
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

fn body(dry: bool, hash: Option<&str>, approval: Option<&str>) -> String {
    serde_json::json!({
        "target_storage_location_id": TARGET,
        "plan_hash": hash,
        "dry_run": dry,
        "approval_id": approval,
    })
    .to_string()
}

const MOVE: &str = "/api/v1/models/0192f000-0000-7000-8000-000000000001/move";

#[tokio::test]
async fn plan_y_ejecucion() {
    let api = setup().await;
    let (s, p) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(true, None, None)),
        Some("k1"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["plan_hash"], HASH);
    assert_eq!(
        api.mover.executed.load(Ordering::SeqCst),
        0,
        "dry_run no ejecuta"
    );
    let (s, p) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), None)),
        Some("k2"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["plan_id"], "0192f000-0000-7000-8000-0000000000d1");
    // Reintento con la misma clave: misma respuesta, sin ejecutar otra vez.
    let (s2, p2) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), None)),
        Some("k2"),
    )
    .await;
    assert_eq!((s2, p2), (s, p));
    assert_eq!(api.mover.executed.load(Ordering::SeqCst), 1);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(api.mover.ran.load(Ordering::SeqCst), 1, "el job se lanzó");
}

#[tokio::test]
async fn negativos_de_autorizacion_y_schema() {
    let api = setup().await;
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.reader,
        Some(&body(true, None, None)),
        Some("k"),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("FORBIDDEN"))
    );
    let (s, _) = call(
        &api.app,
        "POST",
        MOVE,
        "token-falso",
        Some(&body(true, None, None)),
        Some("k"),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(true, None, None)),
        None,
    )
    .await;
    assert!(s.is_client_error(), "{s} {e}");
    let extra = r#"{"target_storage_location_id":"0192f000-0000-7000-8000-0000000000c2","plan_hash":null,"dry_run":true,"approval_id":null,"path":"C:\\x"}"#;
    let (s, e) = call(&api.app, "POST", MOVE, &api.writer, Some(extra), Some("k3")).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("VALIDATION_FAILED"))
    );
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some("XYZ"), None)),
        Some("k4"),
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("schema_violation"))
    );
    let (s, e) = call(
        &api.app,
        "POST",
        "/api/v1/models/no-uuid/move",
        &api.writer,
        Some(&body(true, None, None)),
        Some("k5"),
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("path_id_invalid"))
    );
    assert_eq!(api.mover.executed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn errores_del_servicio_llegan_tal_cual() {
    let api = setup().await;
    *api.mover.fail_with.lock() = Some(
        ApiError::new(
            "ACTIVE_LEASE",
            "El modelo tiene una lease activa",
            "active_lease",
        )
        .retryable(true),
    );
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), None)),
        Some("k"),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("ACTIVE_LEASE"))
    );
    assert_eq!(e["retryable"], true);
    *api.mover.fail_with.lock() = Some(ApiError::new("STALE_REVISION", "Cambió", "plan_changed"));
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), None)),
        Some("k2"),
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("plan_changed"))
    );
}

#[tokio::test]
async fn aprobacion_ligada_al_plan_y_de_un_uso() {
    let api = setup().await;
    let unknown = "0192f000-0000-7000-8000-0000000000f9";
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), Some(unknown))),
        Some("a1"),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("APPROVAL_REQUIRED"))
    );
    assert_eq!(
        api.mover.executed.load(Ordering::SeqCst),
        0,
        "sin aprobación válida no se ejecuta"
    );
    // Aprobación de otros recursos: no vale.
    let actor = MutationContext {
        actor_id: api.writer_id.clone(),
        correlation_id: "c".into(),
    };
    let wrong = api
        .approvals
        .grant(
            &ApprovalRequest {
                operation_digest: HASH.into(),
                resource_ids: vec!["variant:otra".into()],
                code_hash: None,
                permissions: serde_json::Map::new(),
                approved: true,
            },
            &actor,
        )
        .await
        .unwrap();
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), Some(&wrong.resource_id))),
        Some("a2"),
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("approval_resources_mismatch"))
    );
    let good = api
        .approvals
        .grant(
            &ApprovalRequest {
                operation_digest: HASH.into(),
                resource_ids: plan().resource_ids,
                code_hash: None,
                permissions: serde_json::Map::new(),
                approved: true,
            },
            &actor,
        )
        .await
        .unwrap();
    let (s, _) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), Some(&good.resource_id))),
        Some("a3"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, e) = call(
        &api.app,
        "POST",
        MOVE,
        &api.writer,
        Some(&body(false, Some(HASH), Some(&good.resource_id))),
        Some("a4"),
    )
    .await;
    assert_eq!(
        (s, e["details"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("approval_consumed"))
    );
    assert_eq!(api.mover.executed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lectura_paginada_y_por_id() {
    let api = setup().await;
    let (s, page) = call(
        &api.app,
        "GET",
        "/api/v1/artifact-locations?limit=2",
        &api.reader,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    assert!(page["snapshot_at"].is_string());
    let (_, page2) = call(
        &api.app,
        "GET",
        &format!("/api/v1/artifact-locations?limit=2&cursor={cursor}"),
        &api.reader,
        None,
        None,
    )
    .await;
    assert_eq!(page2["items"].as_array().unwrap().len(), 1);
    assert!(page2["next_cursor"].is_null());
    let (s, one) = call(
        &api.app,
        "GET",
        "/api/v1/artifact-locations/0192f000-0000-7000-8000-0000000000e2",
        &api.reader,
        None,
        None,
    )
    .await;
    assert_eq!(
        (s, one["relative_path"].as_str()),
        (StatusCode::OK, Some("m/2.gguf"))
    );
    assert_eq!(one["file_size"], "10", "decimal como texto");
    let (s, _) = call(
        &api.app,
        "GET",
        "/api/v1/artifact-locations/0192f000-0000-7000-8000-0000000000ef",
        &api.reader,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &api.app,
        "GET",
        "/api/v1/artifact-locations?cursor=manipulado",
        &api.reader,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
