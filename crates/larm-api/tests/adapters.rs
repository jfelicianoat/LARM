//! `POST /api/v1/adapters/declarative` (P023) con el guard real: scope
//! `adapters:write`, idempotencia, conflicto de versión, validación del
//! documento antes de IO, paquete en disco y exportar → importar sin
//! secretos ni confianza heredada.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::adapters::AdaptersState;
use larm_api::security::guard;
use larm_core::adapters::{DeclarativeAdapter, DeclarativeRegistry};
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::adapters::{AdapterFilter, AdapterInstallation};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::ports::{
    AdapterCatalogRepository, AdapterInstallationRepository, AdapterPackageExporter,
    MutationContext, PrincipalAuthenticator,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Default)]
struct Memory {
    rows: Mutex<Vec<(AdapterInstallation, String)>>,
}

#[async_trait]
impl AdapterCatalogRepository for Memory {
    async fn page(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &AdapterFilter,
    ) -> Result<Vec<AdapterInstallation>, ApiError> {
        let after = after.unwrap_or_default();
        let mut rows: Vec<_> = self
            .rows
            .lock()
            .iter()
            .map(|(a, _)| a.clone())
            .filter(|a| {
                (a.created_at.clone(), a.id.clone()) > after
                    && filter.state.is_none_or(|s| a.state == s)
                    && filter.q.as_ref().is_none_or(|q| a.adapter_id.contains(q))
            })
            .collect();
        rows.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        rows.truncate(limit as usize);
        Ok(rows)
    }
}

#[async_trait]
impl AdapterInstallationRepository for Memory {
    async fn insert(&self, a: &AdapterInstallation, ctx: &MutationContext) -> Result<(), ApiError> {
        self.rows.lock().push((a.clone(), ctx.actor_id.clone()));
        Ok(())
    }
    async fn get(&self, id: &str) -> Result<Option<AdapterInstallation>, ApiError> {
        Ok(self
            .rows
            .lock()
            .iter()
            .find(|(a, _)| a.id == id)
            .map(|(a, _)| a.clone()))
    }
    async fn find(
        &self,
        adapter_id: &str,
        version: &str,
    ) -> Result<Option<AdapterInstallation>, ApiError> {
        Ok(self
            .rows
            .lock()
            .iter()
            .find(|(a, _)| a.adapter_id == adapter_id && a.version == version)
            .map(|(a, _)| a.clone()))
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    memory: Arc<Memory>,
    exporter: Arc<MemoryExporter>,
    dir: tempfile::TempDir,
}

#[derive(Default)]
struct MemoryExporter {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl AdapterPackageExporter for MemoryExporter {
    async fn export_package(
        &self,
        request: &larm_domain::adapter_packages::AdapterExport,
        _ctx: &MutationContext,
    ) -> Result<larm_domain::adapter_packages::AdapterExportReceipt, ApiError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(larm_domain::adapter_packages::AdapterExportReceipt {
            resource_id: request.adapter_installation_id.clone(),
            row_version: 3,
            event_id: "42".into(),
        })
    }
}

async fn setup() -> Api {
    // El TEMP de Windows puede incluir llaves en su ruta dentro del sandbox,
    // y el contrato declarativo las rechaza como metacaracteres. Mantener el
    // fixture en target evita depender de esa ruta del sistema.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let base = repo.join("target/larm-api-adapter-tests");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    for d in ["rt", "models", "adapters"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    std::fs::write(dir.path().join("rt").join("fake.exe"), b"MZ runtime").unwrap();
    std::fs::write(dir.path().join("worker.exe"), b"MZ worker").unwrap();
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
        ("escritor", vec![ApiScope::Read, ApiScope::AdaptersWrite]),
    ] {
        let (_, token) = principals
            .enroll(PrincipalKind::Operator, name, scopes, vec![], None, &ctx)
            .await
            .unwrap();
        tokens.push(token);
    }
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let memory = Arc::new(Memory::default());
    let exporter = Arc::new(MemoryExporter::default());
    let idempotency: Arc<dyn larm_domain::ports::IdempotencyStore> =
        Arc::new(MemoryIdempotency::default());
    let state = AdaptersState {
        registry: Arc::new(DeclarativeRegistry {
            repo: memory.clone(),
            packages_root: dir.path().join("adapters"),
            worker_exe: dir.path().join("worker.exe"),
        }),
        idempotency: idempotency.clone(),
    };
    let app = larm_api::adapters::routes(state)
        .merge(larm_api::adapters::export_routes(
            larm_api::adapters::AdapterExportState {
                exporter: exporter.clone(),
                idempotency,
            },
        ))
        .merge(larm_api::adapters::catalog_routes(
            larm_api::adapters::AdapterCatalogState {
                repo: memory.clone(),
                cursors: Arc::new(larm_core::security::CursorSigner::default()),
            },
        ))
        .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        writer: tokens.pop().unwrap(),
        reader: tokens.pop().unwrap(),
        memory,
        exporter,
        dir,
    }
}

fn document(root: &Path, version: &str) -> Value {
    let exe = root.join("rt").join("fake.exe").display().to_string();
    json!({
        "schema_version": "1.0",
        "manifest": {
            "id": "acme.fake-runtime",
            "version": version,
            "family": "runtime",
            "protocol_version": "1.0",
            "publisher": "operador local",
            "executable": {"relative_path": "bin/larm-generic-adapter.exe", "args": []},
            "platforms": ["windows"],
            "architectures": ["x86_64"],
            "methods": ["core.handshake", "core.cancel", "core.heartbeat", "runtime.discover",
                "runtime.list_versions", "runtime.install_plan", "runtime.install", "runtime.detect_backends",
                "runtime.inspect_model", "runtime.check_compatibility", "runtime.estimate_memory",
                "runtime.launch_plan", "runtime.start", "runtime.load", "runtime.health", "runtime.stop"],
            "permissions": {
                "read_roots": [root.join("rt").display().to_string(), root.join("models").display().to_string()],
                "write_roots": [], "network_origins": [], "process_allowlist": [],
                "secret_refs": ["larm/test/key"]
            },
            "capabilities": ["runtime.text_generate"],
            "package_sha256": "0".repeat(64),
            "signature": null,
            "installation_strategies": []
        },
        "version_probe": {"executable": exe, "args": ["--version"], "parse_mode": "EXACT_TEXT", "json_field": null},
        "start": {"executable": exe, "args": ["--model", "{MODEL_PATH}", "--port", "{PORT}"], "cwd": root.join("rt").display().to_string()},
        "stop": {"mode": "SIGNAL", "endpoint": null},
        "health": {"url": "http://127.0.0.1:{PORT}/health", "expected_status": 200, "json_pointer": null, "expected_value": null},
        "protocol_profile_id": "openai.chat-completions",
        "formats": ["GGUF"],
        "capabilities": ["runtime.text_generate"],
        "environment": {"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": [
            {"name": "LARM_TEST", "value": "ok"},
            {"name": "HF_TOKEN", "credential_reference": "larm/test/key"}]}
    })
}

async fn post(api: &Api, token: &str, body: &Value, key: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/adapters/declarative")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    let res = api
        .app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn post_export(
    api: &Api,
    token: Option<&str>,
    body: &Value,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/adapters/export")
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let response = api
        .app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn reason(v: &Value) -> &str {
    v["details"]["reason"].as_str().unwrap_or_default()
}

async fn get(api: &Api, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::get(path);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = api
        .app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn export_requiere_scope_validacion_e_idempotencia() {
    let api = setup().await;
    let installation_id = "0192f000-0000-7000-8000-000000000099";
    let body = json!({
        "adapter_installation_id": installation_id,
        "destination_path": "C:/exports/fake-runtime.zip"
    });
    assert_eq!(
        post_export(&api, None, &body, Some("exp-1")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_export(&api, Some(&api.reader), &body, Some("exp-1"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_export(&api, Some(&api.writer), &body, None).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut malformed = body.clone();
    malformed["unexpected"] = json!(true);
    assert_eq!(
        post_export(&api, Some(&api.writer), &malformed, Some("exp-bad"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        api.exporter.calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    let (status, receipt) = post_export(&api, Some(&api.writer), &body, Some("exp-1")).await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    assert_eq!(
        receipt,
        json!({"resource_id":installation_id,"row_version":3,"event_id":"42"})
    );
    let (replay_status, replay) = post_export(&api, Some(&api.writer), &body, Some("exp-1")).await;
    assert_eq!((replay_status, replay), (StatusCode::CREATED, receipt));
    assert_eq!(
        api.exporter.calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let changed = json!({
        "adapter_installation_id": installation_id,
        "destination_path": "C:/exports/other.zip"
    });
    let (status, error) = post_export(&api, Some(&api.writer), &changed, Some("exp-1")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(reason(&error), "idempotency_body_mismatch");
    assert_eq!(
        api.exporter.calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn catalogo_paginado_detalle_y_cursores_ligados_a_filtros() {
    let api = setup().await;
    assert_eq!(
        get(&api, "/api/v1/adapters", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&api, "/api/v1/adapters", Some(&api.reader)).await.1["items"],
        json!([])
    );
    let (_, a) = post(
        &api,
        &api.writer,
        &document(api.dir.path(), "1.0"),
        Some("v1"),
    )
    .await;
    let (_, b) = post(
        &api,
        &api.writer,
        &document(api.dir.path(), "2.0"),
        Some("v2"),
    )
    .await;
    let (status, first) = get(&api, "/api/v1/adapters?limit=1", Some(&api.reader)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["items"], json!([a.clone()]));
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, second) = get(
        &api,
        &format!("/api/v1/adapters?limit=1&cursor={cursor}"),
        Some(&api.reader),
    )
    .await;
    assert_eq!(second["items"], json!([b]));
    assert!(second["next_cursor"].is_null());
    let (status, _) = get(
        &api,
        &format!("/api/v1/adapters?limit=1&state=INSTALLED&cursor={cursor}"),
        Some(&api.reader),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, detail) = get(
        &api,
        &format!("/api/v1/adapters/{}", a["id"].as_str().unwrap()),
        Some(&api.reader),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail, a);
    for path in [
        "/api/v1/adapters?limit=0",
        "/api/v1/adapters?extra=1",
        "/api/v1/adapters?state=ENABLED",
        "/api/v1/adapters/not-uuid",
    ] {
        assert_eq!(
            get(&api, path, Some(&api.reader)).await.0,
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    assert_eq!(
        get(
            &api,
            &format!("/api/v1/adapters/{}", uuid::Uuid::now_v7()),
            Some(&api.reader)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn alta_idempotente_y_paquete_en_disco() {
    let api = setup().await;
    let doc = document(api.dir.path(), "1.0.0");
    let (s, created) = post(&api, &api.writer, &doc, Some("k1")).await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    // Forma exacta de adapter_installation.
    let keys: Vec<&str> = created
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = vec![
        "id",
        "created_at",
        "updated_at",
        "row_version",
        "metadata",
        "adapter_id",
        "version",
        "family",
        "package_hash",
        "publisher",
        "trust",
        "manifest",
        "state",
    ];
    expected.sort_unstable();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(keys, expected);
    assert!(larm_domain::bridge::is_uuid_v7(
        created["id"].as_str().unwrap()
    ));
    assert_eq!(
        created["trust"], "UNTRUSTED",
        "un declarativo nunca nace confiable"
    );
    assert_eq!(created["state"], "INSTALLED");
    assert_eq!(created["family"], "runtime");
    assert_eq!(created["metadata"]["signature_state"], "CUSTOM");
    assert_eq!(created["metadata"]["execution_mode"], "TRUSTED_ONLY");
    assert_eq!(
        created["manifest"]["package_sha256"],
        created["package_hash"]
    );
    let pkg = api
        .dir
        .path()
        .join("adapters")
        .join("acme.fake-runtime")
        .join("1.0.0");
    assert_eq!(
        larm_adapter_sdk_hash(&pkg),
        created["package_hash"].as_str().unwrap(),
        "el hash es el de los ficheros reales"
    );
    assert_eq!(api.memory.rows.lock().len(), 1);
    assert_ne!(api.memory.rows.lock()[0].1, "", "el actor queda registrado");

    // Repetir con la misma clave devuelve lo mismo sin otra alta.
    let (s2, replay) = post(&api, &api.writer, &doc, Some("k1")).await;
    assert_eq!((s2, &replay), (StatusCode::CREATED, &created));
    assert_eq!(api.memory.rows.lock().len(), 1);
    // Misma clave con otro cuerpo.
    let (s, e) = post(
        &api,
        &api.writer,
        &document(api.dir.path(), "9.9.9"),
        Some("k1"),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "IDEMPOTENCY_CONFLICT");
    // Otra clave, misma versión: conflicto y el paquete existente intacto.
    let (s, e) = post(&api, &api.writer, &doc, Some("k2")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(reason(&e), "adapter_version_exists");
    assert_eq!(
        larm_adapter_sdk_hash(&pkg),
        created["package_hash"].as_str().unwrap()
    );
    // Aunque la carpeta desaparezca, la fila manda: no hay una segunda alta
    // de la misma versión.
    std::fs::remove_dir_all(&pkg).unwrap();
    let (s, e) = post(&api, &api.writer, &doc, Some("k3")).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(reason(&e), "adapter_version_exists");
    assert_eq!(api.memory.rows.lock().len(), 1);
    assert!(!pkg.exists(), "un alta rechazada no recrea el paquete");
    // Sin Idempotency-Key.
    let (s, e) = post(&api, &api.writer, &document(api.dir.path(), "2.0.0"), None).await;
    assert!(s.is_client_error(), "{s} {e}");
}

/// Hash del paquete con la misma regla que el host (P022).
fn larm_adapter_sdk_hash(dir: &Path) -> String {
    let mut files: Vec<(String, String)> = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase();
                if rel != "manifest.json" {
                    use sha2::Digest;
                    out.push((
                        rel,
                        hex::encode(sha2::Sha256::digest(std::fs::read(&p).unwrap())),
                    ));
                }
            }
        }
    }
    walk(dir, dir, &mut files);
    files.sort();
    let text: String = files.iter().map(|(r, h)| format!("{r}\0{h}\n")).collect();
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

#[tokio::test]
async fn autorizacion_y_validacion_antes_de_io() {
    let api = setup().await;
    let doc = document(api.dir.path(), "1.0.0");
    let (s, e) = post(&api, &api.reader, &doc, Some("r1")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    let (s, _) = post(&api, "token-falso", &doc, Some("r2")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let root = api.dir.path();
    let mut cases: Vec<(Value, &str)> = Vec::new();
    let mut d = doc.clone();
    d["environment"]["variables"] = json!([{"name": "NO_PERMITIDA", "value": "x"}]);
    cases.push((d, "env_not_allowed"));
    let mut d = doc.clone();
    d["environment"]["variables"] = json!([{"name": "HF_TOKEN", "value": "hf_secreto"}]);
    cases.push((d, "secret_value_not_allowed"));
    let mut d = doc.clone();
    d["environment"]["variables"] =
        json!([{"name": "HF_TOKEN", "credential_reference": "ai-broker/key"}]);
    cases.push((d, "credential_reference_invalid"));
    let mut d = doc.clone();
    d["start"]["args"] = json!(["{SHELL}"]);
    cases.push((d, "placeholder_unknown"));
    let mut d = doc.clone();
    d["start"]["executable"] = json!(r"C:\Windows\System32\cmd.exe");
    cases.push((d, "path_outside_roots"));
    let mut d = doc.clone();
    d["start"]["executable"] = json!(root.join("rt").join("no-existe.exe").display().to_string());
    cases.push((d, "path_missing"));
    let mut d = doc.clone();
    d["unknown"] = json!(true);
    cases.push((d, "schema_violation"));
    for (i, (d, expected)) in cases.into_iter().enumerate() {
        let (s, e) = post(&api, &api.writer, &d, Some(&format!("v{i}"))).await;
        assert!(s.is_client_error(), "{expected}: {s} {e}");
        if expected == "credential_reference_invalid" {
            assert!(
                ["credential_reference_invalid", "schema_violation"].contains(&reason(&e)),
                "{e}"
            );
        } else {
            assert_eq!(reason(&e), expected, "{e}");
        }
    }
    // Nada se creó: ni filas ni carpetas.
    assert!(api.memory.rows.lock().is_empty());
    assert_eq!(std::fs::read_dir(root.join("adapters")).unwrap().count(), 0);
    let (s, e) = post(
        &api,
        &api.writer,
        &json!({"schema_version": "1.0"}),
        Some("x"),
    )
    .await;
    assert_eq!(
        (s, reason(&e)),
        (StatusCode::BAD_REQUEST, "schema_violation"),
        "{e}"
    );
}

#[tokio::test]
async fn exportar_e_importar_sin_secretos_ni_confianza() {
    let api = setup().await;
    let (s, created) = post(
        &api,
        &api.writer,
        &document(api.dir.path(), "1.0.0"),
        Some("e1"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let pkg = api
        .dir
        .path()
        .join("adapters")
        .join("acme.fake-runtime")
        .join("1.0.0");
    // Exportar desde el paquete guardado.
    let (stored, _) = DeclarativeAdapter::load_package(&pkg).unwrap();
    let exported = stored.export().unwrap();
    let text = exported.to_string();
    assert!(text.contains("\"LARM_TEST\"") && text.contains("\"value\":\"ok\""));
    assert!(text.contains("larm/test/key"), "la referencia se conserva");
    assert!(!text.to_lowercase().contains("hf_secreto"));
    for b in exported["environment"]["variables"].as_array().unwrap() {
        if b["name"] == "HF_TOKEN" {
            assert!(
                b.get("value").is_none(),
                "un secreto solo viaja como referencia"
            );
        }
    }
    // Importar como otra versión: alta nueva, sin heredar confianza.
    let mut imported = exported.clone();
    imported["manifest"]["version"] = json!("1.0.1");
    let (s, again) = post(&api, &api.writer, &imported, Some("e2")).await;
    assert_eq!(s, StatusCode::CREATED, "{again}");
    assert_eq!(again["trust"], "UNTRUSTED");
    assert_ne!(again["id"], created["id"]);
    let (reloaded, _) = DeclarativeAdapter::load_package(
        &api.dir
            .path()
            .join("adapters")
            .join("acme.fake-runtime")
            .join("1.0.1"),
    )
    .unwrap();
    assert_eq!(
        reloaded.environment, stored.environment,
        "el entorno sobrevive al viaje"
    );
}
