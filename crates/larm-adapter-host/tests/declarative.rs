//! Adaptador declarativo (P023) con procesos reales: el documento se
//! materializa en un paquete con el worker genérico, la política de P022 lo
//! autoriza como CUSTOM con concesión TRUSTED_ONLY, el host lo lanza y el
//! worker ejecuta el runtime falso (sondeo, arranque, health y parada).
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as Ttl, Utc};
use larm_adapter_host::policy::{ElevationProbe, GuardedWorker, Policy, TrustedOnlyGrant};
use larm_adapter_host::worker::{Containment, HostConfig, TreeKill, WorkerSpec, WorkerState};
use larm_adapter_sdk::declarative::{DeclarativeAdapter, DOCUMENT_FILE, GENERIC_WORKER};
use larm_adapter_sdk::manifest::{AdapterManifest, MethodTable};
use larm_adapter_sdk::permissions::{
    adapter_resource, execution_digest, Access, RootMap, SignatureState, Trust, TrustStore,
};
use larm_domain::approval::ApprovalRequest;
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ACTOR: &str = "user:operador";
const RUNTIME_ID: &str = "0192aaaa-0000-7000-8000-0000000000aa";
const IDS: &str = "0192aaaa-0000-7000-8000-0000000000bb";

fn built(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(rel);
    assert!(
        p.exists(),
        "falta {} (cargo build -p larm-adapter-sdk --bins --examples)",
        p.display()
    );
    p
}

struct NotElevated;
impl ElevationProbe for NotElevated {
    fn elevated(&self) -> Result<bool, LarmError> {
        Ok(false)
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Lab {
    dir: tempfile::TempDir,
    /// Runtimes lanzados: se terminan al soltar el laboratorio, también si la
    /// prueba entra en pánico (sin Job Object, P130, sobrevivirían).
    pids: std::sync::Mutex<Vec<u32>>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        for pid in self.pids.lock().map(|p| p.clone()).unwrap_or_default() {
            TreeKill.kill_tree(pid);
        }
    }
}

impl Lab {
    fn new() -> Self {
        // Keep fixtures under the workspace: this sandbox's system temp path
        // contains braces, which the declarative path contract correctly rejects.
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .canonicalize()
            .unwrap();
        let dir = tempfile::tempdir_in(fixture_root).unwrap();
        for d in ["rt", "models", "out", "packages", "work"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::copy(
            built("examples/fake_runtime.exe"),
            dir.path().join("rt").join("fake_runtime.exe"),
        )
        .unwrap();
        std::fs::write(dir.path().join("models").join("m.gguf"), b"GGUF").unwrap();
        Self {
            dir,
            pids: std::sync::Mutex::default(),
        }
    }

    fn track(&self, handle: &Value) {
        if let Some(pid) = handle["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) {
            self.pids.lock().unwrap().push(pid);
        }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn s(&self, rel: &str) -> String {
        let path = self.p(rel).display().to_string();
        // Rust may surface TempDir paths with Windows' extended-length prefix;
        // the declarative contract intentionally accepts drive-rooted paths.
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_string()
    }

    /// Documento con el runtime falso; `extra` se añade a `start.args`.
    fn document(&self, id: &str, extra: &[&str]) -> Value {
        let exe = self.s("rt/fake_runtime.exe");
        let mut args = vec![
            "--serve".to_string(),
            "--port".into(),
            "{PORT}".into(),
            "--model".into(),
            "{MODEL_PATH}".into(),
            "--threads".into(),
            "{THREADS}".into(),
            "--dump".into(),
            self.s(&format!("out/{id}.json")),
        ];
        args.extend(extra.iter().map(ToString::to_string));
        json!({
            "schema_version": "1.0",
            "manifest": {
                "id": id,
                "version": "1.0.0",
                "family": "runtime",
                "protocol_version": "1.0",
                "publisher": "operador local",
                "executable": {"relative_path": GENERIC_WORKER, "args": []},
                "platforms": ["windows"],
                "architectures": ["x86_64"],
                "methods": MethodTable::contract().methods.iter()
                    .filter(|m| m.required && (m.family == "all" || m.family == "runtime"))
                    .map(|m| m.method.clone()).collect::<Vec<_>>(),
                "permissions": {
                    "read_roots": [self.s("rt"), self.s("models")],
                    "write_roots": [],
                    "network_origins": [],
                    "process_allowlist": [],
                    "secret_refs": ["larm/test/key"]
                },
                "capabilities": [],
                "package_sha256": "0".repeat(64),
                "signature": null,
                "installation_strategies": []
            },
            "version_probe": {"executable": exe, "args": ["--version"], "parse_mode": "EXACT_TEXT", "json_field": null},
            "start": {"executable": exe, "args": args, "cwd": self.s("rt")},
            "stop": {"mode": "SIGNAL", "endpoint": null},
            "health": {"url": "http://127.0.0.1:{PORT}/health", "expected_status": 200, "json_pointer": "/status", "expected_value": "ok"},
            "protocol_profile_id": "openai.chat-completions",
            "formats": ["GGUF"],
            "capabilities": [],
            "environment": {"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "value": "ok"}]}
        })
    }

    /// Materializa, autoriza (CUSTOM + TRUSTED_ONLY) y lanza el worker.
    async fn launch(&self, doc: &Value) -> (GuardedWorker, AdapterManifest, PathBuf) {
        let doc = DeclarativeAdapter::from_value(doc).unwrap();
        let m = doc
            .materialize(&self.p("packages"), &built("larm-generic-adapter.exe"))
            .unwrap();
        let auth = authorize(&m.dir, &m.manifest).unwrap();
        assert_eq!(
            auth.assessment.signature_state,
            Some(SignatureState::Custom)
        );
        assert_eq!(auth.assessment.trust, Trust::Untrusted);
        let spec = WorkerSpec {
            package_dir: m.dir.clone(),
            manifest: m.manifest.clone(),
            // Variable del entorno del worker: nunca debe llegar al runtime.
            environment: vec![("LARM_FUGA".into(), "del-worker".into())],
            work_dir: self.p("work"),
        };
        let w = GuardedWorker::start(
            spec,
            &auth,
            HostConfig {
                handshake_timeout: Duration::from_secs(5),
                ..HostConfig::default()
            },
            1,
            Arc::new(TreeKill),
        )
        .await
        .unwrap();
        assert_eq!(w.state(), WorkerState::Ready);
        (w, m.manifest, m.dir)
    }

    fn launch_request(&self, port: u16, model: &str, env: Value) -> Value {
        json!({
            "installation_id": IDS,
            "variant_id": IDS,
            "launch_profile": {
                "threads": 4, "context": 2048, "batch": 512, "concurrency": 1,
                "offload_layers": null, "backend_id": IDS,
                "tokenizer_hash": null, "chat_template_hash": null,
                "generation": {"temperature": null, "top_p": null, "seed": null, "max_output_tokens": null},
                "memory_limit_bytes": null, "runtime_arguments": [],
                "environment": env,
                "adapter_options": {"model_path": model, "port": port}
            },
            "constraints": {"runtime_version_id": null, "backend_id": null, "execution_fingerprint": null,
                            "allow_backend_fallback": false, "max_memory_bytes": null},
            "resource_requirements": null
        })
    }
}

fn authorize(
    dir: &Path,
    manifest: &AdapterManifest,
) -> Result<larm_adapter_host::policy::LaunchAuthorization, LarmError> {
    let now = Utc::now();
    let record = ApprovalRequest {
        operation_digest: execution_digest(manifest).unwrap(),
        resource_ids: vec![adapter_resource(manifest)],
        code_hash: Some(manifest.package_sha256.clone()),
        permissions: json!({
            "publisher": manifest.publisher,
            "permissions": serde_json::to_value(&manifest.permissions).unwrap(),
            "expires_at": (now + Ttl::days(1)).to_rfc3339(),
        })
        .as_object()
        .cloned()
        .unwrap(),
        approved: true,
    }
    .grant("appr-p023", ACTOR, now, Ttl::seconds(900))
    .unwrap();
    let grant = TrustedOnlyGrant::from_approval(&record, manifest, ACTOR, now)?;
    Policy {
        trust: TrustStore::default(),
        roots: RootMap::default(),
        elevation: Arc::new(NotElevated),
    }
    .authorize(dir, manifest, true, Some(&grant), now)
}

fn reason(e: &LarmError) -> &str {
    &e.details.reason
}

async fn call(w: &GuardedWorker, method: &str, params: Value) -> Result<Value, LarmError> {
    w.call(
        method,
        params,
        &Access::default(),
        Some(Duration::from_secs(30)),
        None,
    )
    .await
}

async fn wait_health(w: &GuardedWorker, handle: &Value, want: &str) -> Value {
    let r = json!({"instance_id": handle["instance_id"], "generation": handle["generation"]});
    let mut last = Value::Null;
    for _ in 0..100 {
        last = call(w, "runtime.health", r.clone()).await.unwrap();
        if last["state"] == want {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("health no llegó a {want}: {last}");
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
}

#[tokio::test]
async fn capacidad_sin_registro_no_llega_al_sondeo() {
    let lab = Lab::new();
    let mut value = lab.document("acme.sin-contrato", &[]);
    value["manifest"]["capabilities"] = json!(["runtime.text_generate"]);
    value["capabilities"] = json!(["runtime.text_generate"]);
    // Un sondeo autorizado escribiría el dump. La negociación debe impedirlo.
    value["version_probe"]["args"] = json!([
        "--serve",
        "--port",
        "0",
        "--dump",
        lab.s("out/acme.sin-contrato.json")
    ]);
    let document = DeclarativeAdapter::from_value(&value).unwrap();
    let package = document
        .materialize(&lab.p("packages"), &built("larm-generic-adapter.exe"))
        .unwrap();
    let auth = authorize(&package.dir, &package.manifest).unwrap();
    let result = GuardedWorker::start(
        WorkerSpec {
            package_dir: package.dir,
            manifest: package.manifest,
            environment: vec![],
            work_dir: lab.p("work"),
        },
        &auth,
        HostConfig::default(),
        1,
        Arc::new(TreeKill),
    )
    .await;
    match result {
        Ok(worker) => {
            worker.stop().await;
            panic!("capacidad sin descriptor admitida");
        }
        Err(error) => {
            assert_eq!(error.code, ErrorCode::UnsupportedCapability);
            assert_eq!(reason(&error), "capability_descriptor_missing");
        }
    }
    assert!(!lab.p("out/acme.sin-contrato.json").exists());
}

#[tokio::test]
async fn sondeo_de_version_con_ejecutable_falso() {
    let lab = Lab::new();
    let (w, _, _) = lab.launch(&lab.document("acme.sondeo", &[])).await;
    let page = call(
        &w,
        "runtime.list_versions",
        json!({"runtime_id": RUNTIME_ID, "channel": "custom", "cursor": null}),
    )
    .await
    .unwrap();
    let item = &page["items"][0];
    assert_eq!(item["version_string"], "fake-runtime 1.2.3");
    assert_eq!(item["channel"], "custom", "canal no interpretable = custom");
    assert_eq!(item["promotion"], "NONE");
    assert_eq!(item["runtime_id"], RUNTIME_ID);
    let real = hex::encode(Sha256::digest(
        std::fs::read(lab.p("rt/fake_runtime.exe")).unwrap(),
    ));
    assert_eq!(item["artifact_hash"], json!(real));
    // Pedir stable no convierte el sondeo en estable.
    let stable = call(
        &w,
        "runtime.list_versions",
        json!({"runtime_id": RUNTIME_ID, "channel": "stable", "cursor": null}),
    )
    .await
    .unwrap();
    assert_eq!(stable["items"], json!([]));
    // Un método obligatorio que no aplica no devuelve un éxito vacío.
    let e = call(
        &w,
        "runtime.detect_backends",
        json!({"installation_id":IDS}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedCapability);
    assert_eq!(reason(&e), "declarative_method_not_applicable");
    // Parámetros fuera de schema.
    let e = call(
        &w,
        "runtime.list_versions",
        json!({"runtime_id": RUNTIME_ID}),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&e), "invalid_params");
    w.stop().await;

    // JSON_FIELD con puntero.
    let mut doc = lab.document("acme.sondeo-json", &[]);
    doc["version_probe"] = json!({"executable": lab.s("rt/fake_runtime.exe"), "args": ["--json-version"],
                                  "parse_mode": "JSON_FIELD", "json_field": "/build/number"});
    let (w, _, _) = lab.launch(&doc).await;
    let page = call(
        &w,
        "runtime.list_versions",
        json!({"runtime_id": RUNTIME_ID, "channel": "custom", "cursor": null}),
    )
    .await
    .unwrap();
    assert_eq!(page["items"][0]["version_string"], "7");
    w.stop().await;

    // Sondeo que no termina: timeout a los 10 s y proceso muerto.
    let mut doc = lab.document("acme.sondeo-colgado", &[]);
    doc["version_probe"]["args"] = json!(["--hang"]);
    let (w, _, _) = lab.launch(&doc).await;
    let t = std::time::Instant::now();
    let e = call(
        &w,
        "runtime.list_versions",
        json!({"runtime_id": RUNTIME_ID, "channel": "custom", "cursor": null}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    assert_eq!(reason(&e), "version_probe_timeout");
    assert!(t.elapsed() < Duration::from_secs(20));
    w.stop().await;
}

#[tokio::test]
async fn arranque_entorno_health_y_parada_reales() {
    let lab = Lab::new();
    let (w, _, _) = lab.launch(&lab.document("acme.arranque", &[])).await;
    let port = free_port();
    let model = lab.s("models/m.gguf");
    let plan = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(
            port,
            &model,
            json!({"allowlist": ["LARM_TEST"], "variables": []}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(plan["port"], json!(port));
    assert_eq!(plan["env_allowlist"], json!(["LARM_TEST"]));
    assert_eq!(
        plan["environment"],
        json!({"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "value": "ok"}]})
    );
    assert_eq!(plan["memory_estimate"]["confidence"], "UNKNOWN");
    let exec =
        |hash: &Value| json!({"plan_id": plan["plan_id"], "plan_hash": hash, "approval_id": null});
    let e = call(&w, "runtime.start", exec(&json!("0".repeat(64))))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "plan_hash_mismatch");
    let handle = call(&w, "runtime.start", exec(&plan["plan_hash"]))
        .await
        .unwrap();
    lab.track(&handle);
    assert!(handle["pid"].as_u64().unwrap() > 0);
    let e = call(&w, "runtime.start", exec(&plan["plan_hash"]))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "plan_already_used", "un plan es de un solo uso");

    let h = wait_health(&w, &handle, "READY").await;
    assert_eq!(h["reason"], "health_ok");

    // H04: la variable no secreta llega al proceso; nada más se hereda.
    let dump: Value =
        serde_json::from_str(&std::fs::read_to_string(lab.p("out/acme.arranque.json")).unwrap())
            .unwrap();
    assert_eq!(dump["env"]["LARM_TEST"], "ok");
    assert!(
        dump["env"].get("LARM_FUGA").is_none(),
        "el runtime heredó el entorno del worker"
    );
    let allowed = [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "TEMP",
        "TMP",
        "LARM_TEST",
    ];
    for key in dump["env"].as_object().unwrap().keys() {
        assert!(
            allowed.iter().any(|a| a.eq_ignore_ascii_case(key)),
            "variable heredada: {key}"
        );
    }
    let real_model = std::fs::canonicalize(&model).unwrap();
    let real_model = real_model.display().to_string();
    let real_model = real_model.trim_start_matches(r"\\?\");
    assert_eq!(
        dump["args"],
        json!([
            "--serve",
            "--port",
            port.to_string(),
            "--model",
            real_model,
            "--threads",
            "4",
            "--dump",
            lab.s("out/acme.arranque.json")
        ])
    );
    assert_eq!(
        Path::new(dump["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        lab.p("rt").canonicalize().unwrap()
    );

    // Parada SIGNAL: árbol muerto y la instancia desaparece.
    let r = json!({"instance_id": handle["instance_id"], "generation": 0});
    let e = call(
        &w,
        "runtime.health",
        json!({"instance_id": handle["instance_id"], "generation": 9}),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&e), "generation_mismatch");
    assert_eq!(
        call(&w, "runtime.stop", r.clone()).await.unwrap(),
        json!({"stopped": true})
    );
    assert!(!port_open(port), "el runtime sigue escuchando");
    assert_eq!(
        reason(&call(&w, "runtime.health", r).await.unwrap_err()),
        "instance_not_found"
    );
    w.stop().await;
}

#[tokio::test]
async fn health_degradado_caido_y_parada_http() {
    let lab = Lab::new();
    let mut doc = lab.document("acme.http", &["--health-status", "503"]);
    doc["stop"] = json!({"mode": "HTTP", "endpoint": "http://127.0.0.1:{PORT}/shutdown"});
    let (w, _, _) = lab.launch(&doc).await;
    let port = free_port();
    let env = json!({"allowlist": ["LARM_TEST"], "variables": []});
    let plan = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(port, &lab.s("models/m.gguf"), env.clone()),
    )
    .await
    .unwrap();
    let handle = call(
        &w,
        "runtime.start",
        json!({"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": null}),
    )
    .await
    .unwrap();
    lab.track(&handle);
    let h = wait_health(&w, &handle, "DEGRADED").await;
    assert_eq!(h["reason"], "health_status_mismatch");
    // Parada HTTP: el propio runtime termina dentro de la gracia.
    let t = std::time::Instant::now();
    let r = json!({"instance_id": handle["instance_id"], "generation": 0});
    assert_eq!(
        call(&w, "runtime.stop", r).await.unwrap(),
        json!({"stopped": true})
    );
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "la parada HTTP no esperó a la gracia completa"
    );
    assert!(!port_open(port));

    // Un runtime que muere por su cuenta: FAILED.
    let port = free_port();
    let plan = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(port, &lab.s("models/m.gguf"), env),
    )
    .await
    .unwrap();
    let handle = call(
        &w,
        "runtime.start",
        json!({"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": null}),
    )
    .await
    .unwrap();
    lab.track(&handle);
    wait_health(&w, &handle, "DEGRADED").await;
    TreeKill.kill_tree(u32::try_from(handle["pid"].as_u64().unwrap()).unwrap());
    let h = wait_health(&w, &handle, "FAILED").await;
    assert_eq!(h["reason"], "process_exited");
    w.stop().await;
}

#[tokio::test]
async fn entorno_no_autorizado_inyeccion_y_credencial() {
    let lab = Lab::new();
    let mut doc = lab.document("acme.negativos", &[]);
    doc["environment"] = json!({"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": [
        {"name": "LARM_TEST", "value": "ok"},
        {"name": "HF_TOKEN", "credential_reference": "larm/test/key"}]});
    let (w, _, _) = lab.launch(&doc).await;
    let model = lab.s("models/m.gguf");
    let port = free_port();
    // Variable del perfil que el adaptador no admite: rechazo antes de IO.
    let e = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(port, &model, json!({"allowlist": ["LARM_TEST", "OTRA"], "variables": [{"name": "OTRA", "value": "x"}]})),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&e), "env_not_allowed_by_profile");
    // Unir allowlists no amplía: el perfil no autoriza HF_TOKEN.
    let e = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(
            port,
            &model,
            json!({"allowlist": ["LARM_TEST"], "variables": []}),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&e), "env_not_allowed_by_profile");
    // Inyección por la opción de modelo: el worker la rechaza aunque el host
    // no declare la lectura (defensa doble).
    for bad in [
        format!("{model}\" & calc.exe"),
        format!(
            "{}\\models\\..\\rt\\fake_runtime.exe",
            lab.dir.path().display()
        ),
        r"C:\Windows\win.ini".to_string(),
    ] {
        let result = call(
            &w,
            "runtime.launch_plan",
            lab.launch_request(
                port,
                &bad,
                json!({"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": []}),
            ),
        )
        .await;
        let e = match result {
            Err(error) => error,
            Ok(value) => panic!("{bad}: se aceptó el plan {value}"),
        };
        assert!(
            ["path_invalid", "read_denied"].contains(&reason(&e)),
            "{bad}: {}",
            reason(&e)
        );
    }
    // El host también bloquea una lectura no concedida antes del worker.
    let e = w
        .call(
            "runtime.launch_plan",
            lab.launch_request(port, &model, json!({"allowlist": [], "variables": []})),
            &Access {
                reads: vec![PathBuf::from(r"C:\Windows\win.ini")],
                ..Access::default()
            },
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "read_denied");
    // Con la credencial autorizada por ambos, el plan solo lleva la
    // referencia y el arranque se bloquea sin lanzar nada.
    let plan = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(
            port,
            &model,
            json!({"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": []}),
        ),
    )
    .await
    .unwrap();
    let text = plan.to_string();
    assert!(text.contains("larm/test/key"));
    let e = call(
        &w,
        "runtime.start",
        json!({"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": null}),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&e), "credential_not_enrolled");
    assert!(
        !lab.p("out/acme.negativos.json").exists(),
        "no se lanzó el runtime"
    );
    assert!(!port_open(port));
    // Límite de memoria que el worker no puede imponer.
    let mut req = lab.launch_request(
        port,
        &model,
        json!({"allowlist": ["LARM_TEST"], "variables": []}),
    );
    req["launch_profile"]["memory_limit_bytes"] = json!("1073741824");
    let e = call(&w, "runtime.launch_plan", req).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedCapability);
    w.stop().await;
}

#[tokio::test]
async fn paquete_manipulado_no_se_autoriza() {
    let lab = Lab::new();
    let doc = DeclarativeAdapter::from_value(&lab.document("acme.manipulado", &[])).unwrap();
    let m = doc
        .materialize(&lab.p("packages"), &built("larm-generic-adapter.exe"))
        .unwrap();
    authorize(&m.dir, &m.manifest).unwrap();
    let path = m.dir.join(DOCUMENT_FILE);
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("--threads", "--hilos");
    std::fs::write(&path, text).unwrap();
    let e = authorize(&m.dir, &m.manifest).unwrap_err();
    assert_eq!(reason(&e), "package_hash_mismatch");
}

#[tokio::test]
async fn cerrar_la_sesion_del_worker_termina_sus_runtimes() {
    let lab = Lab::new();
    let (w, _, _) = lab.launch(&lab.document("acme.sesion", &[])).await;
    let port = free_port();
    let plan = call(
        &w,
        "runtime.launch_plan",
        lab.launch_request(
            port,
            &lab.s("models/m.gguf"),
            json!({"allowlist": ["LARM_TEST"], "variables": []}),
        ),
    )
    .await
    .unwrap();
    let handle = call(
        &w,
        "runtime.start",
        json!({"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": null}),
    )
    .await
    .unwrap();
    lab.track(&handle);
    wait_health(&w, &handle, "READY").await;
    // El host cierra stdin y espera la gracia: el worker termina su runtime.
    w.stop().await;
    let mut closed = false;
    for _ in 0..50 {
        if !port_open(port) {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        closed,
        "el runtime sobrevivió al cierre de sesión de su worker"
    );
}
