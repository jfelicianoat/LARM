//! Ubicaciones de almacenamiento (P014) con el guard real: CRUD, If-Match,
//! idempotencia, filtros con cursor ligado y sondeo como job.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::security::guard;
use larm_api::storage::StorageState;
use larm_core::hardware::memory::MemoryJournalJobs;
use larm_core::hardware::StorageRefresh;
use larm_core::locations::LocationProbe;
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::locations::{LocationState, StorageLocation, StorageLocationCreate};
use larm_domain::ports::{
    JobSource, LocationFilter, LocationRoot, LocationService, MutationContext, ObservationContext,
    PrincipalAuthenticator, RootInspector, StorageProber, StorageStateChange,
    StorageStateRepository,
};
use larm_domain::storage::{PerformanceClass, StorageObservation};
use larm_domain::units::DecimalU64;
use parking_lot::Mutex;
use tower::ServiceExt;

/// Servicio en memoria: valida lo mínimo (raíz que empieza por `C:\`) y
/// aplica If-Match; las reglas de disco reales se prueban en larm-storage.
#[derive(Default)]
struct Memory {
    rows: Mutex<Vec<StorageLocation>>,
    with_artifacts: Mutex<Vec<String>>,
}

fn not_found(id: &str) -> ApiError {
    ApiError::new("NOT_FOUND", "Ubicación desconocida", "not_found").with_resource(id.to_string())
}

#[async_trait]
impl LocationService for Memory {
    async fn create(
        &self,
        r: &StorageLocationCreate,
        _: &MutationContext,
    ) -> Result<StorageLocation, ApiError> {
        if !r.root_path.starts_with("C:\\") {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "Ruta no válida",
                "root_path_invalid",
            ));
        }
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let l = StorageLocation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: r.name.clone(),
            root_path: r.root_path.clone(),
            canonical_path: r.root_path.clone(),
            volume_guid: Some("fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3".into()),
            drive_letter: Some("C:".into()),
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::new(1000).unwrap(),
            free_bytes: DecimalU64::new(500).unwrap(),
            reserved_bytes: r.reserved_bytes,
            priority: r.priority,
            auto_placement: r.auto_placement,
            state: LocationState::Online,
            last_seen_at: None,
            volume_key: "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3".into(),
            performance_class: PerformanceClass::Nvme,
        };
        self.rows.lock().push(l.clone());
        Ok(l)
    }
    async fn update(
        &self,
        id: &str,
        v: i64,
        r: &StorageLocationCreate,
        _: &MutationContext,
    ) -> Result<StorageLocation, ApiError> {
        let mut rows = self.rows.lock();
        let l = rows
            .iter_mut()
            .find(|l| l.id == id)
            .ok_or_else(|| not_found(id))?;
        if l.row_version != v {
            return Err(ApiError::new("STALE_REVISION", "Cambió", "stale_revision"));
        }
        l.name = r.name.clone();
        l.reserved_bytes = r.reserved_bytes;
        l.row_version += 1;
        Ok(l.clone())
    }
    async fn delete(&self, id: &str, v: i64, _: &MutationContext) -> Result<(), ApiError> {
        if self.with_artifacts.lock().iter().any(|a| a == id) {
            return Err(ApiError::new(
                "CONFLICT",
                "Tiene modelos",
                "location_has_artifacts",
            ));
        }
        let mut rows = self.rows.lock();
        let i = rows
            .iter()
            .position(|l| l.id == id)
            .ok_or_else(|| not_found(id))?;
        if rows[i].row_version != v {
            return Err(ApiError::new("STALE_REVISION", "Cambió", "stale_revision"));
        }
        rows.remove(i);
        Ok(())
    }
    async fn get(&self, id: &str) -> Result<StorageLocation, ApiError> {
        self.rows
            .lock()
            .iter()
            .find(|l| l.id == id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }
    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        f: &LocationFilter,
    ) -> Result<Vec<StorageLocation>, ApiError> {
        let after = after.unwrap_or_default();
        Ok(self
            .rows
            .lock()
            .iter()
            .filter(|l| (l.created_at.clone(), l.id.clone()) > after)
            .filter(|l| {
                f.q.as_deref()
                    .is_none_or(|q| l.name.to_lowercase().contains(&q.to_lowercase()))
            })
            .filter(|l| f.state.is_none_or(|s| s == l.state))
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

/// Almacenamiento falso para el job de sondeo.
struct Storage;

#[async_trait]
impl StorageProber for Storage {
    async fn probe(&self, _: &[String]) -> Result<StorageObservation, ApiError> {
        Ok(StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: "t".into(),
        })
    }
}

#[async_trait]
impl StorageStateRepository for Storage {
    async fn roots(&self) -> Result<Vec<LocationRoot>, ApiError> {
        Ok(vec![])
    }
    async fn apply(
        &self,
        _: &StorageObservation,
        _: &ObservationContext,
        _: &MutationContext,
    ) -> Result<Vec<StorageStateChange>, ApiError> {
        Ok(vec![])
    }
}

#[async_trait]
impl RootInspector for Storage {
    async fn present(&self, _: &str) -> Option<bool> {
        Some(true)
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    memory: Arc<Memory>,
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
    for (name, scope) in [
        ("lector", ApiScope::Read),
        ("escritor", ApiScope::StorageWrite),
    ] {
        let (_, token) = principals
            .enroll(
                PrincipalKind::Operator,
                name,
                vec![scope],
                vec![],
                None,
                &ctx,
            )
            .await
            .unwrap();
        tokens.push(token);
    }
    // El escritor también lee.
    let (_, writer) = principals
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
    let memory = Arc::new(Memory::default());
    let probe = Arc::new(LocationProbe {
        storage: StorageRefresh {
            prober: Arc::new(Storage),
            state: Arc::new(Storage),
            roots: Arc::new(Storage),
            thresholds: Arc::new(parking_lot::RwLock::new((
                larm_domain::locations::DiskThresholds::Percent {
                    warning_bp: 1500,
                    critical_bp: 500,
                },
                1,
            ))),
        },
        jobs: Arc::new(MemoryJournalJobs::for_resource("storage_location")),
        locations: memory.clone(),
    });
    let state = StorageState {
        locations: memory.clone(),
        probe: probe.clone(),
        cursors: Arc::new(CursorSigner::default()),
        idempotency: Arc::new(MemoryIdempotency::default()),
    };
    let sources: Vec<Arc<dyn JobSource>> = vec![probe];
    let app = larm_api::storage::routes(state)
        .merge(larm_api::jobs::routes(Arc::new(sources)))
        .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        reader: tokens.remove(0),
        writer,
        memory,
    }
}

async fn call(
    app: &axum::Router,
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

fn create_body(name: &str) -> String {
    format!(
        r#"{{"name":"{name}","root_path":"C:\\IA\\{name}","reserved_bytes":"100","priority":1,"auto_placement":true,"performance_class":"UNKNOWN"}}"#
    )
}

#[tokio::test]
async fn crud_con_idempotencia_e_if_match() {
    let api = setup().await;
    let (s, created) = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.writer,
        Some(&create_body("modelos")),
        &[("idempotency-key", "c1")],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let replay = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.writer,
        Some(&create_body("modelos")),
        &[("idempotency-key", "c1")],
    )
    .await;
    assert_eq!(
        replay,
        (StatusCode::CREATED, created.clone()),
        "misma clave: misma respuesta, sin crear otra"
    );
    assert_eq!(api.memory.rows.lock().len(), 1);
    let (s, v) = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.writer,
        Some(&create_body("otra")),
        &[("idempotency-key", "c1")],
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("IDEMPOTENCY_CONFLICT"))
    );

    let (s, v) = call(
        &api.app,
        "GET",
        &format!("/api/v1/storage-locations/{id}"),
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!((s, v["name"].as_str()), (StatusCode::OK, Some("modelos")));

    let patch = create_body("renombrada");
    let (s, v) = call(
        &api.app,
        "PATCH",
        &format!("/api/v1/storage-locations/{id}"),
        &api.writer,
        Some(&patch),
        &[],
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            Some("PRECONDITION_REQUIRED")
        )
    );
    let (s, v) = call(
        &api.app,
        "PATCH",
        &format!("/api/v1/storage-locations/{id}"),
        &api.writer,
        Some(&patch),
        &[("if-match", "1")],
    )
    .await;
    assert_eq!((s, v["row_version"].as_i64()), (StatusCode::OK, Some(2)));
    let (s, v) = call(
        &api.app,
        "PATCH",
        &format!("/api/v1/storage-locations/{id}"),
        &api.writer,
        Some(&patch),
        &[("if-match", "1")],
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("STALE_REVISION"))
    );

    api.memory.with_artifacts.lock().push(id.clone());
    let (s, v) = call(
        &api.app,
        "DELETE",
        &format!("/api/v1/storage-locations/{id}"),
        &api.writer,
        None,
        &[("if-match", "2")],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("location_has_artifacts"))
    );
    api.memory.with_artifacts.lock().clear();
    let (s, _) = call(
        &api.app,
        "DELETE",
        &format!("/api/v1/storage-locations/{id}"),
        &api.writer,
        None,
        &[("if-match", "2")],
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &api.app,
        "GET",
        &format!("/api/v1/storage-locations/{id}"),
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validacion_y_permisos() {
    let api = setup().await;
    let cases = [
        (
            r#"{"name":"x","root_path":"C:\\x","reserved_bytes":"1","priority":0,"auto_placement":true,"performance_class":"UNKNOWN","volume_guid":"x"}"#,
            "schema_violation",
        ),
        (
            r#"{"name":"x","root_path":"C:\\x","reserved_bytes":1,"priority":0,"auto_placement":true,"performance_class":"UNKNOWN"}"#,
            "schema_violation",
        ),
        (
            r#"{"name":"x","root_path":"C:\\x","reserved_bytes":"1","priority":0,"auto_placement":true,"performance_class":"SSD_RAPIDO"}"#,
            "schema_violation",
        ),
        (
            r#"{"name":"x","root_path":"x","reserved_bytes":"1","priority":0,"auto_placement":true,"performance_class":"UNKNOWN"}"#,
            "root_path_invalid",
        ),
    ];
    for (i, (body, reason)) in cases.iter().enumerate() {
        let key = format!("k{i}");
        let (s, v) = call(
            &api.app,
            "POST",
            "/api/v1/storage-locations",
            &api.writer,
            Some(body),
            &[("idempotency-key", key.as_str())],
        )
        .await;
        assert_eq!(
            (s, v["details"]["reason"].as_str()),
            (StatusCode::BAD_REQUEST, Some(*reason)),
            "{body}"
        );
    }
    let (s, v) = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.writer,
        Some(&create_body("a")),
        &[],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("idempotency_key_invalid"))
    );
    let (s, _) = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.reader,
        Some(&create_body("a")),
        &[("idempotency-key", "r")],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "read no basta para crear");
    let (s, _) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations",
        "token-falso",
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations/no-uuid",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("path_id_invalid"))
    );
}

#[tokio::test]
async fn lista_filtros_alias_y_cursor_ligado_a_los_filtros() {
    let api = setup().await;
    for (i, name) in ["Modelos", "Cache", "Difusion"].iter().enumerate() {
        let key = format!("l{i}");
        call(
            &api.app,
            "POST",
            "/api/v1/storage-locations",
            &api.writer,
            Some(&create_body(name)),
            &[("idempotency-key", key.as_str())],
        )
        .await;
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    let names = |v: &serde_json::Value| {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    for path in [
        "/api/v1/storage-locations",
        "/api/v1/storage",
        "/api/v1/storage/locations",
    ] {
        let (s, v) = call(&api.app, "GET", path, &api.reader, None, &[]).await;
        assert_eq!(
            (s, names(&v)),
            (
                StatusCode::OK,
                vec!["Modelos".to_string(), "Cache".into(), "Difusion".into()]
            ),
            "{path}"
        );
    }
    let (_, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations?q=dif",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(names(&v), ["Difusion"]);
    let (_, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations?state=OFFLINE",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert!(names(&v).is_empty());
    let (s, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations?state=ROTO",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("filter_invalid"))
    );
    let (s, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations?capability=llm",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("filter_not_allowed"))
    );
    let (_, v) = call(
        &api.app,
        "GET",
        "/api/v1/storage-locations?q=o&limit=1",
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(names(&v), ["Modelos"]);
    let cursor = v["next_cursor"].as_str().unwrap().to_string();
    let (s, v) = call(
        &api.app,
        "GET",
        &format!("/api/v1/storage-locations?q=o&limit=1&cursor={cursor}"),
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(
        (s, names(&v)),
        (StatusCode::OK, vec!["Difusion".to_string()])
    );
    let (s, v) = call(
        &api.app,
        "GET",
        &format!("/api/v1/storage-locations?q=b&cursor={cursor}"),
        &api.reader,
        None,
        &[],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_cursor")),
        "cursor de otros filtros"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sondeo_de_una_ubicacion_como_job() {
    let api = setup().await;
    let (_, created) = call(
        &api.app,
        "POST",
        "/api/v1/storage-locations",
        &api.writer,
        Some(&create_body("modelos")),
        &[("idempotency-key", "c")],
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let nulls = r#"{"resource_id":null,"profile_id":null}"#;
    let (s, receipt) = call(
        &api.app,
        "POST",
        &format!("/api/v1/storage-locations/{id}/probe"),
        &api.writer,
        Some(nulls),
        &[("idempotency-key", "p1")],
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{receipt}");
    let job = receipt["job_id"].as_str().unwrap();
    let mut last = serde_json::Value::Null;
    for _ in 0..50 {
        last = call(
            &api.app,
            "GET",
            &format!("/api/v1/jobs/{job}"),
            &api.writer,
            None,
            &[],
        )
        .await
        .1;
        if last["state"] == "COMPLETED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        (last["state"].as_str(), last["resource_type"].as_str()),
        (Some("COMPLETED"), Some("storage_location")),
        "{last}"
    );
    assert_eq!(last["result"]["storage_location_id"], id);
    let unknown = "0192aaaa-0000-7000-8000-0000000000ff";
    let (s, _) = call(
        &api.app,
        "POST",
        &format!("/api/v1/storage-locations/{unknown}/probe"),
        &api.writer,
        Some(nulls),
        &[("idempotency-key", "p2")],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let other = format!(r#"{{"resource_id":"{unknown}","profile_id":null}}"#);
    let (s, v) = call(
        &api.app,
        "POST",
        &format!("/api/v1/storage-locations/{id}/probe"),
        &api.writer,
        Some(&other),
        &[("idempotency-key", "p3")],
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("resource_id_mismatch"))
    );
}
