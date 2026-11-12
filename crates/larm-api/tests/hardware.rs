//! `GET /api/v1/hardware` (P010) y `POST /api/v1/hardware/refresh` (P011) con
//! el guard real, paginación firmada, idempotencia y `GET /jobs/{id}`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::hardware::HardwareState;
use larm_api::security::guard;
use larm_core::hardware::memory::{MemoryJournalJobs, MemoryProfiles, ScriptedProber};
use larm_core::hardware::HardwareRefresh;
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::hardware::{
    CpuInfo, HardwareProbe, HardwareProfile, HardwareSnapshot, MemoryInfo, ObservedMetrics, OsInfo,
    SYSTEM_RAM_POOL,
};
use larm_domain::ports::{HardwareRepository, JobSource, MutationContext, PrincipalAuthenticator};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn snapshot(build: &str) -> HardwareSnapshot {
    HardwareSnapshot {
        os: OsInfo {
            edition: Some("Professional".into()),
            build: Some(build.into()),
            architecture: Some("AMD64".into()),
        },
        cpu: CpuInfo {
            vendor: None,
            model: None,
            physical_cores: None,
            logical_cores: Some(32),
            instruction_sets: vec!["avx2".into()],
        },
        ram: MemoryInfo {
            physical_bytes: None,
            usable_bytes: larm_domain::units::DecimalU64::new(64 << 30),
            pool_id: SYSTEM_RAM_POOL.into(),
        },
        devices: vec![],
        dependencies: vec![],
        observed_metrics: ObservedMetrics {
            available_ram_bytes: None,
            cpu_usage_percent: Some(1.5),
        },
    }
}

fn profile(n: u32) -> HardwareProfile {
    let snapshot = snapshot(&format!("26200.{n}"));
    HardwareProfile {
        id: format!("0192aaaa-0000-7000-8000-{n:012}"),
        created_at: format!("2026-10-11T10:00:0{n}.000Z"),
        updated_at: format!("2026-10-11T10:00:0{n}.000Z"),
        row_version: 1,
        metadata: serde_json::Map::new(),
        hardware_fingerprint: snapshot.fingerprint().unwrap(),
        snapshot,
        evidence_refs: vec![],
        probed_at: format!("2026-10-11T10:00:0{n}.000Z"),
    }
}

/// Repositorio que falla al listar (base caída).
struct Broken;

#[async_trait]
impl HardwareRepository for Broken {
    async fn latest(&self) -> Result<Option<HardwareProfile>, ApiError> {
        Err(ApiError::new("INTERNAL", "base caída", "db_down"))
    }

    async fn record(&self, _: &HardwareProbe) -> Result<HardwareProfile, ApiError> {
        unreachable!()
    }
    async fn list(
        &self,
        _: Option<(String, String)>,
        _: u32,
    ) -> Result<Vec<HardwareProfile>, ApiError> {
        Err(ApiError::new("INTERNAL", "base caída", "db_down"))
    }
    async fn get(&self, _: &str) -> Result<Option<HardwareProfile>, ApiError> {
        Err(ApiError::new("INTERNAL", "base caída", "db_down"))
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    admin: String,
    refresh: Arc<HardwareRefresh>,
}

async fn setup_with(
    rows: Vec<HardwareProfile>,
    broken: bool,
    probe_ok: bool,
    delay_ms: u64,
) -> Api {
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
        ("escritor", ApiScope::ModelsWrite),
        ("admin", ApiScope::Admin),
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
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let memory = Arc::new(MemoryProfiles {
        rows: parking_lot::Mutex::new(rows),
    });
    let repo: Arc<dyn HardwareRepository> = if broken {
        Arc::new(Broken)
    } else {
        memory.clone()
    };
    let probe = probe_ok.then(|| HardwareProbe {
        snapshot: snapshot("26200.9457"),
        metadata: serde_json::Map::new(),
        evidence_refs: vec!["test".into()],
        probed_at: "2026-10-11T11:00:00.000Z".into(),
    });
    let refresh = Arc::new(HardwareRefresh {
        prober: Arc::new(ScriptedProber {
            probe,
            delay: Duration::from_millis(delay_ms),
        }),
        profiles: memory,
        jobs: Arc::new(MemoryJournalJobs::default()),
        storage: None,
    });
    let state = HardwareState {
        repo,
        cursors: Arc::new(CursorSigner::default()),
        refresh: refresh.clone(),
        idempotency: Arc::new(MemoryIdempotency::default()),
    };
    let sources: Vec<Arc<dyn JobSource>> = vec![refresh.clone()];
    let app = larm_api::hardware::routes(state)
        .merge(larm_api::jobs::routes(Arc::new(sources)))
        .layer(axum::middleware::from_fn_with_state(auth, guard));
    let admin = tokens.remove(2);
    let writer = tokens.remove(1);
    Api {
        app,
        reader: tokens.remove(0),
        writer,
        admin,
        refresh,
    }
}

async fn setup(rows: Vec<HardwareProfile>, broken: bool) -> Api {
    setup_with(rows, broken, true, 0).await
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn get(app: &axum::Router, query: &str, token: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::get(format!("/api/v1/hardware{query}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    call(app, req).await
}

async fn refresh(
    app: &axum::Router,
    token: &str,
    key: Option<&str>,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::post("/api/v1/hardware/refresh")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    call(app, req.body(Body::from(body.to_string())).unwrap()).await
}

async fn job(app: &axum::Router, token: &str, id: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::get(format!("/api/v1/jobs/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    call(app, req).await
}

const NULLS: &str = r#"{"resource_id": null, "profile_id": null}"#;

#[tokio::test]
async fn lista_vacia_y_forma_de_la_respuesta() {
    let api = setup(vec![], false).await;
    let (s, v) = get(&api.app, "", &api.reader).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["items"], serde_json::json!([]));
    assert!(v["next_cursor"].is_null());
    assert!(v["snapshot_at"].as_str().unwrap().ends_with('Z'));

    let api = setup(vec![profile(1)], false).await;
    let (_, v) = get(&api.app, "", &api.reader).await;
    let item = &v["items"][0];
    assert_eq!(item["snapshot"]["ram"]["usable_bytes"], "68719476736");
    assert!(item["snapshot"]["ram"]["physical_bytes"].is_null());
    assert_eq!(item["snapshot"]["devices"], serde_json::json!([]));
}

#[tokio::test]
async fn pagina_con_cursor_firmado() {
    let api = setup((1..=3).map(profile).collect(), false).await;
    let (s, v) = get(&api.app, "?limit=2", &api.reader).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    let cursor = v["next_cursor"].as_str().unwrap().to_string();
    let (s, v) = get(&api.app, &format!("?limit=2&cursor={cursor}"), &api.reader).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["items"][0]["snapshot"]["os"]["build"], "26200.3");
    assert!(v["next_cursor"].is_null());

    // Cursor manipulado o de otra lista: 400 bad_cursor.
    let tampered = format!("{}x", &cursor[..cursor.len() - 1]);
    let (s, v) = get(&api.app, &format!("?cursor={tampered}"), &api.reader).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_cursor"))
    );
}

#[tokio::test]
async fn parametros_invalidos_autorizacion_y_fallo_del_repositorio() {
    let api = setup(vec![profile(1)], false).await;
    for (query, reason) in [
        ("?state=ONLINE", "filter_not_allowed"),
        ("?limit=0", "limit_invalid"),
        ("?limit=201", "limit_invalid"),
        ("?limit=x", "limit_invalid"),
    ] {
        let (s, v) = get(&api.app, query, &api.reader).await;
        assert_eq!(
            (s, v["details"]["reason"].as_str()),
            (StatusCode::BAD_REQUEST, Some(reason)),
            "{query}"
        );
    }
    let (s, _) = get(&api.app, "", "token-falso").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = get(&api.app, "", &api.writer).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("FORBIDDEN"))
    );

    let api = setup(vec![], true).await;
    let (s, v) = get(&api.app, "", &api.reader).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, Some("INTERNAL"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresco_202_job_en_curso_reutilizado_y_resultado_por_jobs() {
    let api = setup_with(vec![], false, true, 400).await;
    let (s, receipt) = refresh(&api.app, &api.admin, Some("r1"), NULLS).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let id = receipt["job_id"].as_str().unwrap().to_string();
    assert_eq!(receipt["status_url"], format!("/api/v1/jobs/{id}"));
    assert!(matches!(
        receipt["state"].as_str(),
        Some("QUEUED" | "RUNNING")
    ));

    // Misma clave y cuerpo: la misma respuesta. Clave nueva mientras sondea: el mismo job.
    let (s, again) = refresh(&api.app, &api.admin, Some("r1"), NULLS).await;
    assert_eq!((s, &again), (StatusCode::ACCEPTED, &receipt));
    let (s, other) = refresh(&api.app, &api.admin, Some("r2"), NULLS).await;
    assert_eq!(
        (s, other["job_id"].as_str()),
        (StatusCode::ACCEPTED, Some(id.as_str()))
    );
    // Misma clave con otro cuerpo (aunque solo cambien los espacios): conflicto.
    let (s, v) = refresh(
        &api.app,
        &api.admin,
        Some("r1"),
        r#"{"resource_id": null, "profile_id": null }  "#,
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("IDEMPOTENCY_CONFLICT"))
    );

    // El job termina y su resultado apunta al perfil registrado.
    let mut last = serde_json::Value::Null;
    for _ in 0..50 {
        let (s, v) = job(&api.app, &api.admin, &id).await;
        assert_eq!(s, StatusCode::OK);
        last = v;
        if last["state"] == "COMPLETED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(last["state"], "COMPLETED", "{last}");
    assert_eq!(last["resource_type"], "hardware_profile");
    let profile_id = last["result"]["hardware_profile_id"].as_str().unwrap();
    assert_eq!(last["resource_id"], profile_id);
    let (_, list) = get(&api.app, "", &api.admin).await;
    assert_eq!(list["items"][0]["id"], profile_id);

    // Un lector no puede pedir el refresco ni ve el job de otro.
    let (s, _) = refresh(&api.app, &api.reader, Some("r3"), NULLS).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = job(&api.app, &api.reader, &id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refresco_con_cuerpo_o_cabecera_invalidos() {
    let api = setup(vec![profile(1)], false).await;
    let unknown = "0192aaaa-0000-7000-8000-0000000000ff";
    let known = profile(1).id;
    let cases = [
        (
            r#"{"resource_id": null}"#,
            StatusCode::BAD_REQUEST,
            "schema_violation",
        ),
        (
            r#"{"resource_id": null, "profile_id": null, "x": 1}"#,
            StatusCode::BAD_REQUEST,
            "schema_violation",
        ),
        ("no es json", StatusCode::BAD_REQUEST, "schema_violation"),
        (
            r#"{"resource_id": null, "profile_id": "perfil"}"#,
            StatusCode::BAD_REQUEST,
            "profile_id_not_applicable",
        ),
        (
            r#"{"resource_id": "abc", "profile_id": null}"#,
            StatusCode::BAD_REQUEST,
            "resource_id_invalid",
        ),
    ];
    for (body, status, reason) in cases {
        let (s, v) = refresh(&api.app, &api.admin, Some("k"), body).await;
        assert_eq!(
            (s, v["details"]["reason"].as_str()),
            (status, Some(reason)),
            "{body}"
        );
    }
    let (s, v) = refresh(
        &api.app,
        &api.admin,
        Some("k2"),
        &format!(r#"{{"resource_id": "{unknown}", "profile_id": null}}"#),
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
    );
    let (s, v) = refresh(&api.app, &api.admin, None, NULLS).await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("idempotency_key_invalid"))
    );
    let (s, v) = refresh(
        &api.app,
        &api.admin,
        Some("k3"),
        &format!(r#"{{"resource_id": "{known}", "profile_id": null}}"#),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let state = api
        .refresh
        .job(
            &larm_domain::auth::Principal::new(
                "x",
                "x",
                PrincipalKind::Operator,
                [ApiScope::Admin],
                [],
            )
            .unwrap(),
            v["job_id"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.resource_id.as_deref(), Some(known.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sondeo_fallido_termina_en_failed_con_error_del_contrato() {
    let api = setup_with(vec![], false, false, 0).await;
    let (s, receipt) = refresh(&api.app, &api.admin, Some("f"), NULLS).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let id = receipt["job_id"].as_str().unwrap();
    let mut last = serde_json::Value::Null;
    for _ in 0..50 {
        last = job(&api.app, &api.admin, id).await.1;
        if last["state"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(last["state"], "FAILED", "{last}");
    assert_eq!(last["error"]["details"]["reason"], "hardware_probe_failed");
    assert!(last["result"].is_null());
}
