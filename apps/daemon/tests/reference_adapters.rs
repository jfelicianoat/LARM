//! P025/P026: las cinco referencias por P021/P022 reales, sin inferencia simulada.
//! Ledger source privado por SQL real; protocol sólo cualifica transporte echo.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use larm_adapter_host::policy::{GuardedWorker, Policy, TrustedOnlyGrant, WhoAmI};
use larm_adapter_host::source::SourceWorker;
use larm_adapter_host::worker::{HostConfig, TreeKill, WorkerSpec};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{
    adapter_resource, execution_digest, Access, RootMap, TrustStore,
};
use larm_adapter_sdk::source::{
    DownloadPlanRequest, DownloadPriority, ExecutePlan, SearchSupport, SourceReply, SourceRequest,
    SourceSearch,
};
use larm_domain::adapters::{AdapterFilter, AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::approval::ApprovalRequest;
use larm_domain::bridge::timestamp;
use larm_domain::ports::{
    AdapterCatalogRepository, AdapterInstallationRepository, MutationContext,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[path = "support/daemon.rs"]
mod daemon_process;
#[path = "support/download_restart.rs"]
mod download_restart;
#[path = "support/p048_download.rs"]
mod p048_download;
#[path = "support/settings_restart.rs"]
mod settings_restart;
#[path = "support/source_acquisition.rs"]
mod source_acquisition;
#[path = "support/source_http.rs"]
mod source_http_product;
#[path = "support/source_search_http.rs"]
mod source_search_http;
#[path = "support/source_watch_tcp.rs"]
mod source_watch_tcp;

#[path = "../src/model_search.rs"]
mod model_search;
#[path = "../src/probes.rs"]
#[allow(dead_code)]
mod native_probes;
#[path = "../src/probe_profiles.rs"]
#[allow(dead_code)]
mod probe_profiles;
#[path = "../src/download_execution.rs"]
#[allow(dead_code)]
mod qualified_download_execution;
#[cfg(windows)]
#[path = "support/qualified_guardian.rs"]
mod qualified_guardian;
#[path = "../src/source_search.rs"]
#[allow(dead_code)]
mod qualified_source_search;
#[path = "support/source_first_download.rs"]
mod source_first_download;
#[path = "support/source_first_download_tcp.rs"]
mod source_first_download_tcp;
#[path = "support/source_models_gui.rs"]
mod source_models_gui;
#[path = "support/transformation_lifecycle.rs"]
mod transformation_lifecycle;
#[path = "support/transformation_publication.rs"]
mod transformation_publication;
#[path = "support/transformation_worker.rs"]
mod transformation_worker;

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit native Windows keyring/HTTP P026/qualified guardian acceptance; build and freeze inputs"]
async fn transformation_guardian_cualificado_rpc_y_recuperacion_de_propietario() {
    qualified_guardian::exercise(false, false, false, false, false, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public P093 HTTP/native consumer qualification and restart acceptance"]
async fn transformation_publica_plan_confirmacion_worker_y_publicacion() {
    qualified_guardian::exercise(true, false, false, false, false, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public P093 cooperative cancellation acceptance with real qualified native worker"]
async fn transformation_publica_cancelacion_colaborativa_y_cierre_de_recursos() {
    qualified_guardian::exercise(true, true, false, false, false, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public H07 partial-bytes acceptance: daemon crash with committed worker blocks, reused worker job namespace and same-job restart to CAS publication"]
async fn transformation_publica_h07_caida_con_bytes_parciales_y_reinicio_hasta_publicacion() {
    qualified_guardian::exercise(true, false, true, false, false, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit definitive witness-loss acceptance: system restart retires the PREPARING intent witness-lessly and the job re-dispatches to CAS publication"]
async fn transformation_publica_perdida_definitiva_del_testigo_y_redespacho_hasta_publicacion() {
    qualified_guardian::exercise(true, false, false, true, false, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public GGUF product-converter acceptance: real P026 qualification with the host GGUF suite and CAS publication verified by the Rust GGUF oracle"]
async fn transformation_publica_gguf_conversor_cualificado_hasta_publicacion() {
    qualified_guardian::exercise(true, false, false, false, true, false, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public ONNX product-converter acceptance: real P026 qualification with the host ONNX suite and CAS publication verified by the Rust ONNX oracle"]
async fn transformation_publica_onnx_conversor_cualificado_hasta_publicacion() {
    qualified_guardian::exercise(true, false, false, false, false, true, false).await;
}
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit public GGUF Q8_0 quantization acceptance: real P026 qualification with the host GGUF suite, plan with quantization=Q8_0 and CAS publication of the quantized container verified byte-exact by the Rust GGUF oracle"]
async fn transformation_publica_gguf_cuantizacion_q8_0_hasta_publicacion() {
    qualified_guardian::exercise(true, false, false, false, false, false, true).await;
}
async fn native_actor() -> (
    MutationContext,
    Arc<larm_core::security::Principals>,
    String,
) {
    use larm_core::security::{
        memory::{MemoryCredentials, MemorySecrets},
        Principals,
    };
    use larm_domain::auth::{ApiScope, PrincipalKind};
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let (principal, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "native-probe-test",
            vec![ApiScope::Read, ApiScope::AdaptersWrite],
            vec![],
            None,
            &MutationContext {
                actor_id: "native-fixture".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
        .unwrap();
    (
        MutationContext {
            actor_id: principal.id,
            correlation_id: uuid::Uuid::now_v7().to_string(),
        },
        principals,
        token,
    )
}
async fn native_public_probe(
    db: &larm_db::pool::Db,
    runner: Arc<larm_adapter_host::conformance::NativeConformanceRunner>,
    id: &str,
    ctx: &MutationContext,
    auth: Arc<larm_core::security::Principals>,
    token: &str,
) -> (
    Result<larm_domain::conformance::Receipt, larm_domain::error::LarmError>,
    Value,
) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let service = native_probes::service(db, runner);
    service.start().await;
    let authenticator: Arc<dyn larm_domain::ports::PrincipalAuthenticator> = auth;
    let app = larm_api::adapters::probe_routes(service.clone())
        .merge(larm_api::jobs::routes(Arc::new(vec![service.clone()])))
        .layer(axum::middleware::from_fn_with_state(
            authenticator,
            larm_api::security::guard,
        ));
    let path = format!("/api/v1/adapters/{id}/probe");
    let key = uuid::Uuid::now_v7().to_string();
    let body = json!({"resource_id":null,"profile_id":null}).to_string();
    let send = |method: &str, path: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("x-correlation-id", &ctx.correlation_id)
            .header("content-type", "application/json")
            .header("idempotency-key", &key)
            .body(Body::from(if method == "POST" {
                body.clone()
            } else {
                String::new()
            }))
            .unwrap()
    };
    let response = app.clone().oneshot(send("POST", &path)).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    let accepted: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();

    let status = accepted["status_url"].as_str().unwrap();
    let final_state = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let response = app.clone().oneshot(send("GET", status)).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let value: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let state = &value;
            if state["state"] == "COMPLETED" || state["state"] == "FAILED" {
                break state.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let replay = app.clone().oneshot(send("POST", &path)).await.unwrap();
    assert_eq!(replay.status(), axum::http::StatusCode::ACCEPTED);
    let replay: Value = serde_json::from_slice(
        &axum::body::to_bytes(replay.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(replay, accepted);
    service.finish_shutdown().await;
    let result = if final_state["state"] == "COMPLETED" {
        Ok(serde_json::from_value(final_state["result"]["conformance"].clone()).unwrap())
    } else {
        Err(serde_json::from_value(final_state["error"].clone()).unwrap())
    };
    use larm_domain::ports::{ApprovalRepository, ConformanceProfileRepository};
    let profile = larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .conformance_profile(id)
    .await
    .unwrap()
    .unwrap();
    let consumed = larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .get(&profile.approval.id)
    .await
    .unwrap()
    .unwrap();
    let run_id = final_state["result"]["conformance"]["run_id"]
        .as_str()
        .unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM operation_journal WHERE id=? AND kind='adapter.conformance'",
    )
    .bind(run_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let journal: Value = serde_json::from_str(&payload).unwrap();
    let report = json!({"status":"PASS","scope":"real guard/ephemeral credential store/HTTP router, persistent compiled profile, Core/SQLite/native workers; no network HTTP listener in this fixture","accepted":accepted,"final":final_state,"same_key_replay_unchanged":true,"request_sha256":hex::encode(Sha256::digest(body.as_bytes())),"registered_profile":{"record":profile,"sha256":larm_domain::conformance::hash(&profile).unwrap(),"consumed_approval":consumed},"core_journal":journal});
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/reports/P026_runs/native-probe-http");
    std::fs::create_dir_all(&root).unwrap();
    let job_id = report["accepted"]["job_id"].as_str().unwrap();
    assert!(larm_domain::bridge::is_uuid_v7(job_id));
    std::fs::write(
        root.join(format!("{job_id}.json")),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    (result, report)
}

struct Fixture {
    _root: tempfile::TempDir,
    helper: Child,
    data: Value,
    package: PathBuf,
    work: PathBuf,
    manifest: AdapterManifest,
}
impl Fixture {
    /// Origen INSTALLED explícito de fixture. La actualización 1.0.1 sí recorre
    /// el coordinador genérico real, profiler, reservas y publicación P024.
    async fn install_source_candidate(&mut self) {
        let data = self._root.path().join("data");
        let db = larm_db::pool::Db::open(&data.join("state/larm.sqlite"))
            .await
            .unwrap();
        larm_db::migrate::migrate(&db.pool).await.unwrap();
        let repo = larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone(),
        };
        let now = timestamp(chrono::Utc::now());
        let origin = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: json!({"fixture":"P025 controlled initial installation, not P028 import"})
                .as_object()
                .unwrap()
                .clone(),
            adapter_id: self.manifest.id.clone(),
            version: self.manifest.version.clone(),
            family: "source".into(),
            package_hash: self.manifest.package_sha256.clone(),
            publisher: self.manifest.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            manifest: serde_json::to_value(&self.manifest).unwrap(),
            state: AdapterState::Installed,
        };
        repo.insert(
            &origin,
            &MutationContext {
                actor_id: "p025-fixture".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_larm-daemon"));
        for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("LARM_")) {
            command.env_remove(name);
        }
        command
            .arg("--data-root")
            .arg(&data)
            .args(["update-adapter", "--installation", &origin.id, "--request"])
            .arg(self.data["update_request"].as_str().unwrap())
            .args(["--idempotency-key", "p025-source-install"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["job"]["state"], "COMPLETED");
        let candidates = repo
            .page(None, 20, &AdapterFilter::default())
            .await
            .unwrap();
        let mut source_versions: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.adapter_id == self.manifest.id)
            .map(|candidate| candidate.version.as_str())
            .collect();
        source_versions.sort_unstable();
        // La migración P098 registra además la fuente local integrada.
        assert_eq!(source_versions, ["1.0.0", "1.0.1"]);
        let candidate = repo
            .find(&origin.adapter_id, "1.0.1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(candidate.state, AdapterState::Installed);
        assert_eq!(candidate.trust, AdapterTrust::Untrusted);
        let held: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM volume_reservation)+(SELECT count(*) FROM resource_lock)+(SELECT count(*) FROM adapter_selection)").fetch_one(&db.pool).await.unwrap();
        assert_eq!(held, 0);
        self.package = data.join("adapters/reference.catalog/1.0.1");
        self.manifest = AdapterManifest::parse(
            &std::fs::read_to_string(self.package.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(self.manifest.package_sha256, candidate.package_hash);
        db.pool.close().await;
    }

    fn new(kind: &str) -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let runs = repo.join("docs/reports/P025_runs");
        std::fs::create_dir_all(&runs).unwrap();
        let root = tempfile::tempdir_in(&runs).unwrap();
        assert!(root
            .path()
            .canonicalize()
            .unwrap()
            .starts_with(runs.canonicalize().unwrap()));
        let mut command = Command::new(repo.join(".venv-tools/Scripts/python.exe"));
        command
            .args(["tools/p025_host_fixture.py", kind])
            .arg(root.path())
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut helper = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(helper.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let data: Value = serde_json::from_str(&line).unwrap();
        let package = PathBuf::from(data["package"].as_str().unwrap());
        let work = PathBuf::from(data["work"].as_str().unwrap());
        let manifest = AdapterManifest::parse(
            &std::fs::read_to_string(package.join("manifest.json")).unwrap(),
        )
        .unwrap();
        Self {
            _root: root,
            helper,
            data,
            package,
            work,
            manifest,
        }
    }
    fn policy(&self) -> Policy {
        Policy {
            trust: TrustStore::default(),
            roots: RootMap(vec![
                ("reference-package".into(), self.package.clone()),
                ("reference-work".into(), self.work.clone()),
                ("reference-protocol-work".into(), self.work.clone()),
                ("reference-catalog".into(), self.work.join("catalog")),
                (
                    "reference-benchmark-profile".into(),
                    self.work.join("profile"),
                ),
                ("reference-benchmark-state".into(), self.work.join("state")),
                (
                    "reference-benchmark-output".into(),
                    self.work.join("output"),
                ),
                ("reference-transform-input".into(), self.work.join("input")),
                (
                    "reference-transform-profile".into(),
                    self.work.join("profile"),
                ),
                ("reference-transform-state".into(), self.work.join("state")),
                (
                    "reference-transform-output".into(),
                    self.work.join("output"),
                ),
            ]),
            elevation: Arc::new(WhoAmI),
        }
    }
    async fn start(&self) -> Arc<GuardedWorker> {
        let policy = self.policy();
        let now = chrono::Utc::now();
        let denied = policy
            .authorize(&self.package, &self.manifest, false, None, now)
            .unwrap_err();
        assert_eq!(denied.code.as_str(), "APPROVAL_REQUIRED");
        let actor = "p025-controlled-fixture";
        let permissions = json!({"publisher":self.manifest.publisher,"permissions":self.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone();
        let record = ApprovalRequest {
            operation_digest: execution_digest(&self.manifest).unwrap(),
            resource_ids: vec![adapter_resource(&self.manifest)],
            code_hash: Some(self.manifest.package_sha256.clone()),
            permissions,
            approved: true,
        }
        .grant(
            uuid::Uuid::now_v7().to_string(),
            actor,
            now,
            chrono::Duration::minutes(10),
        )
        .unwrap();
        let grant = TrustedOnlyGrant::from_approval(&record, &self.manifest, actor, now).unwrap();
        let authorization = policy
            .authorize(&self.package, &self.manifest, false, Some(&grant), now)
            .unwrap();
        assert!(authorization.grants.secret_refs.is_empty());
        Arc::new(
            GuardedWorker::start(
                WorkerSpec {
                    package_dir: self.package.clone(),
                    manifest: self.manifest.clone(),
                    environment: vec![],
                    work_dir: self.work.clone(),
                },
                &authorization,
                HostConfig {
                    handshake_timeout: Duration::from_secs(15),
                    rpc_timeout: Duration::from_secs(15),
                    ..HostConfig::default()
                },
                1,
                Arc::new(TreeKill),
            )
            .await
            .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.helper.stdin.take();
        for _ in 0..100 {
            if self.helper.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.helper.kill();
        let _ = self.helper.wait();
    }
}

// Oráculos reales del paquete original, todavía sólo nueve checks comunes.
// El motor debe rechazar la cobertura parcial: no hay VALIDATED ni promoción.
struct OriginalWireSuite {
    probe: larm_adapter_host::conformance::wire::WireProbe,
    family: String,
    capabilities: Vec<String>,
    report: std::sync::Mutex<Option<larm_adapter_host::conformance::wire::WireReport>>,
}
#[async_trait::async_trait]
impl larm_adapter_host::conformance::NativeSuite for OriginalWireSuite {
    fn descriptor(&self) -> Value {
        json!({"family":self.family,"capabilities":self.capabilities,"suite":"original-package-wire.v1",
            "scope":"nine common checks only; no family qualification","probe":self.probe.descriptor()})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &larm_domain::conformance::Run,
        session: &mut larm_adapter_host::conformance::NativeSession,
    ) -> Result<Vec<larm_domain::conformance::Observation>, larm_domain::error::LarmError> {
        let report =
            larm_adapter_host::conformance::wire::run_common(run, session, &self.probe).await;
        let observed = report.observations.clone();
        *self.report.lock().unwrap() = Some(report);
        observed
    }
}
struct NativeReferenceTarget {
    spec: WorkerSpec,
    grant: TrustedOnlyGrant,
    inputs: Vec<PathBuf>,
    suite: Arc<dyn larm_adapter_host::conformance::NativeSuite>,
    #[allow(dead_code)]
    capability_profile: Option<PathBuf>,
}
#[async_trait::async_trait]
impl larm_adapter_host::conformance::NativeTargets for NativeReferenceTarget {
    async fn target(
        &self,
        _: &AdapterInstallation,
        _: &MutationContext,
    ) -> Result<larm_adapter_host::conformance::NativeTarget, larm_domain::error::LarmError> {
        Ok(larm_adapter_host::conformance::NativeTarget {
            spec: self.spec.clone(),
            roots: None,
            custom: false,
            grant: Some(self.grant.clone()),
            capability_profile: self.capability_profile.clone(),
            immutable_inputs: self.inputs.clone(),
            suite: self.suite.clone(),
            timeout: Duration::from_secs(120),
        })
    }
}
async fn persistent_recipe(
    db: &larm_db::pool::Db,
    ctx: &MutationContext,
    installation: &AdapterInstallation,
    approval: &larm_domain::approval::ApprovalRecord,
    target: NativeReferenceTarget,
    recipe: larm_adapter_host::conformance::profiles::Recipe,
    roots: Vec<(String, PathBuf)>,
) -> Arc<probe_profiles::PersistentTargets> {
    use larm_adapter_host::conformance::profiles::LocalProfile;
    use larm_domain::conformance_profiles::ConformanceProfile;
    use larm_domain::ports::{ApprovalRepository, ConformanceProfileRepository};
    assert_eq!(target.grant.approval_id, approval.id);
    let repo = larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    let previous = repo.conformance_profile(&installation.id).await.unwrap();
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .insert(approval, ctx)
    .await
    .unwrap();
    let capability_profile = match &recipe {
        larm_adapter_host::conformance::profiles::Recipe::Gguf(_)
        | larm_adapter_host::conformance::profiles::Recipe::Onnx(_) => "product-capabilities.json",
        _ => "reference-capabilities.json",
    };
    let config = LocalProfile {
        schema_version: "1.0".into(),
        package_dir: target.spec.package_dir,
        work_dir: target.spec.work_dir,
        roots,
        custom: false,
        capability_profile: Some(capability_profile.into()),
        immutable_inputs: target.inputs,
        // Los conversores de producto reales (dos formatos K en Python
        // stdlib superan los 20 minutos de suite completa): el timeout del
        // perfil es un detector de cuelgue con techo de 1800 s.
        timeout_seconds: 2400,
        suite: recipe,
    };
    let profile = ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: installation.id.clone(),
        actor_id: ctx.actor_id.clone(),
        manifest_sha256: larm_domain::conformance::hash(&installation.manifest).unwrap(),
        configuration: serde_json::to_value(config).unwrap(),
        approval: approval.clone(),
        created_at: timestamp(chrono::Utc::now()),
    };
    repo.store_conformance_profile(&profile, previous.as_ref().map(|v| v.id.as_str()), ctx)
        .await
        .unwrap();
    // Nueva instancia sin catálogo en memoria; reconstruye todo desde SQLite.
    let resolver = Arc::new(probe_profiles::PersistentTargets {
        repo: larm_db::conformance_profiles::SqlConformanceProfiles {
            pool: db.pool.clone(),
        },
    });
    let rebound =
        larm_adapter_host::conformance::NativeTargets::target(resolver.as_ref(), installation, ctx)
            .await
            .unwrap();
    assert!(rebound.roots.is_some());
    assert_eq!(
        rebound.suite.descriptor()["registered_profile_sha256"],
        json!(larm_domain::conformance::hash(&profile).unwrap())
    );
    resolver
}
fn collect_input_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_input_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// Gate real de las cinco recetas: reusa el Run confirmado en SQLite y altera
/// sólo bytes propios del harness. Ningún veredicto externo se acepta.
async fn physical_selection_probe(
    fixture: &Fixture,
    selections: &larm_db::adapter_selection::SqlAdapterSelections,
    stored: &AdapterInstallation,
    ctx: &MutationContext,
) -> Value {
    use larm_domain::adapters::AdapterSelectionChange as Change;
    use larm_domain::ports::AdapterSelectionRepository;
    let mut cases = Vec::new();
    let mut record = |id: &str, expected: Value, observed: Value| {
        assert_eq!(expected, observed, "physical {id}");
        cases.push(json!({"id":id,"expected":expected,"observed":observed,"passed":true}));
    };
    let selection = selections
        .selection(&stored.adapter_id)
        .await
        .unwrap()
        .unwrap();
    let before:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM operation_journal)").fetch_one(&selections.pool).await.unwrap();
    let path = fixture._root.path().join("outside-grant.bin");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"changed physical oracle input after conformance").unwrap();
    let error = selections
        .change_selection(
            &stored.adapter_id,
            selection.row_version,
            &Change::SetEnabled { enabled: true },
            ctx,
        )
        .await
        .unwrap_err();
    record(
        "changed-input-rejected",
        json!("conformance_context_changed"),
        json!(error.details.reason),
    );
    std::fs::write(&path, &bytes).unwrap();
    let extra = fixture.package.join("qualification-owned.bin");
    std::fs::write(&extra, b"changed package inventory after conformance").unwrap();
    let error = selections
        .change_selection(
            &stored.adapter_id,
            selection.row_version,
            &Change::SetEnabled { enabled: true },
            ctx,
        )
        .await
        .unwrap_err();
    record(
        "changed-package-rejected",
        json!("package_hash_mismatch"),
        json!(error.details.reason),
    );
    std::fs::remove_file(&extra).unwrap();
    let after:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM operation_journal)").fetch_one(&selections.pool).await.unwrap();
    record(
        "failures-no-event-journal-outbox",
        json!(before),
        json!(after),
    );
    record(
        "failures-no-selection-change",
        json!(selection),
        json!(selections
            .selection(&stored.adapter_id)
            .await
            .unwrap()
            .unwrap()),
    );
    let raw:String=sqlx::query_scalar("SELECT json_extract(j.payload,'$.run') FROM operation_journal j JOIN adapter_installation a ON j.id=json_extract(a.metadata,'$.larm_conformance_v1.run_id') WHERE a.id=?").bind(&stored.id).fetch_one(&selections.pool).await.unwrap();
    let run: larm_domain::conformance::Run = serde_json::from_str(&raw).unwrap();
    let guard = selections
        .qualification
        .as_ref()
        .unwrap()
        .acquire(stored, &run, ctx)
        .await
        .unwrap();
    guard.recheck().await.unwrap();
    record(
        "windows-package-write-denied",
        json!(true),
        json!(std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.package.join("manifest.json"))
            .is_err()),
    );
    record(
        "windows-input-write-denied",
        json!(true),
        json!(std::fs::OpenOptions::new().write(true).open(&path).is_err()),
    );
    std::fs::write(&extra, b"added after physical acquisition").unwrap();
    record(
        "final-inventory-recheck",
        json!("package_hash_mismatch"),
        json!(guard.recheck().await.unwrap_err().details.reason),
    );
    drop(guard);
    std::fs::remove_file(&extra).unwrap();
    let enabled = selections
        .change_selection(
            &stored.adapter_id,
            selection.row_version,
            &Change::SetEnabled { enabled: true },
            ctx,
        )
        .await
        .unwrap();
    record(
        "restored-inputs-enable",
        json!(true),
        json!(enabled.enabled),
    );
    let disabled = selections
        .change_selection(
            &stored.adapter_id,
            enabled.row_version,
            &Change::SetEnabled { enabled: false },
            ctx,
        )
        .await
        .unwrap();
    record("final-disabled", json!(false), json!(disabled.enabled));
    json!({"status":"PASS","run_sha256":run.digest().unwrap(),"cases":cases,"scope":"actual native physical gate and SQLite selection, owned reference files only"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cinco_paquetes_originales_por_probes_nativos_p022_antes_del_handshake() {
    use larm_adapter_host::conformance::{wire::WireProbe, NativeConformanceRunner};
    use larm_domain::ports::AdapterConformanceRunner;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = repo
        .join("docs/reports/P026_runs/native-wire-original")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&output).unwrap();
    let mut summaries = Vec::new();
    for kind in [
        "runtime",
        "source",
        "protocol",
        "transformation",
        "benchmark",
    ] {
        let f = Fixture::new(kind);
        let ctx = MutationContext {
            actor_id: "p026-original-wire-fixture".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let now = chrono::Utc::now();
        let probe = match kind {
            "runtime" => WireProbe {
                method: "runtime.list_versions".into(),
                params: json!({"runtime_id":uuid::Uuid::now_v7().to_string(),"channel":"custom","cursor":null}),
                access: Access::default(),
            },
            "source" => {
                let profile: Value =
                    serde_json::from_slice(&std::fs::read(f.work.join("profile.json")).unwrap())
                        .unwrap();
                WireProbe {
                    method: "source.auth_probe".into(),
                    params: json!({"source_id":profile["source_id"],"auth_mode":"NONE","account_id":null,"credential_reference_id":null}),
                    access: Access::default(),
                }
            }
            "protocol" => WireProbe {
                method: "protocol.describe".into(),
                params: json!({"endpoint_id":f.data["profile"]["endpoint_id"]}),
                access: Access::default(),
            },
            "transformation" => WireProbe {
                method: "transformation.describe".into(),
                params: json!({}),
                access: Access::default(),
            },
            _ => WireProbe {
                method: "benchmark.describe".into(),
                params: json!({}),
                access: Access::default(),
            },
        };
        let suite = Arc::new(OriginalWireSuite {
            probe,
            family: kind.into(),
            capabilities: f.manifest.capabilities.clone(),
            report: std::sync::Mutex::new(None),
        });
        let record = ApprovalRequest {
            operation_digest: execution_digest(&f.manifest).unwrap(),
            resource_ids: vec![adapter_resource(&f.manifest)],
            code_hash: Some(f.manifest.package_sha256.clone()),
            approved: true,
            permissions:
                json!({"publisher":f.manifest.publisher,"permissions":f.manifest.permissions,
                "expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]})
                .as_object()
                .unwrap()
                .clone(),
        }
        .grant(
            uuid::Uuid::now_v7().to_string(),
            &ctx.actor_id,
            now,
            chrono::Duration::minutes(10),
        )
        .unwrap();
        let grant =
            TrustedOnlyGrant::from_approval(&record, &f.manifest, &ctx.actor_id, now).unwrap();
        let mut inputs = Vec::new();
        collect_input_files(&f.work, &mut inputs);
        let target = NativeReferenceTarget {
            spec: WorkerSpec {
                package_dir: f.package.clone(),
                manifest: f.manifest.clone(),
                environment: vec![],
                work_dir: f.work.clone(),
            },
            grant,
            inputs,
            suite: suite.clone(),
            // Los cinco paquetes originales son de referencia: declaran
            // capabilities en su manifest y exigen su profile.
            capability_profile: Some("reference-capabilities.json".into()),
        };
        let runner = NativeConformanceRunner::new(
            Arc::new(f.policy()),
            Arc::new(target),
            HostConfig {
                handshake_timeout: Duration::from_secs(15),
                rpc_timeout: Duration::from_secs(15),
                ..HostConfig::default()
            },
        );
        let installation = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: timestamp(now),
            updated_at: timestamp(now),
            row_version: 1,
            metadata: serde_json::Map::new(),
            adapter_id: f.manifest.id.clone(),
            version: f.manifest.version.clone(),
            family: kind.into(),
            package_hash: f.manifest.package_sha256.clone(),
            publisher: f.manifest.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            state: AdapterState::Installed,
            manifest: serde_json::to_value(&f.manifest).unwrap(),
        };
        let execution = runner.execute_conformance(&installation, &ctx).await;
        runner.shutdown().await.unwrap();
        let report = suite.report.lock().unwrap().take();
        let case_path = output.join(format!("{kind}.json"));
        if let Some(report) = &report {
            std::fs::write(&case_path, serde_json::to_vec_pretty(report).unwrap()).unwrap();
        }
        let execution = execution.unwrap();
        let report = report.expect("suite reached original package probes");
        let observations = report
            .observations
            .as_ref()
            .unwrap_or_else(|error| panic!("{kind}: {error:?}"));
        assert_eq!(observations.len(), 9);
        assert_eq!(report.cases.len(), 16);
        assert_eq!(report.before_work_sha256, report.rejected_work_sha256);
        assert!(report.exit.as_ref().unwrap().success);
        assert_eq!(report.exit.as_ref().unwrap().extra_frames, 0);
        assert_eq!(
            execution.proof.err().unwrap().details.reason,
            "conformance_coverage_incomplete",
            "nueve checks no cualifican familia/capacidades ni inventan los seis restantes"
        );
        summaries.push(json!({"family":kind,"case_file":case_path.strip_prefix(repo).unwrap(),
            "case_sha256":hex::encode(Sha256::digest(std::fs::read(&case_path).unwrap())),
            "package_sha256":installation.package_hash,"run":execution.run,"protocol_cases":report.cases.len(),
            "real_common_checks":observations.len(),"full_qualification":"NOT_COMPLETED"}));
    }
    std::fs::write(repo.join("docs/reports/P026_runs/native-wire-original.json"),serde_json::to_vec_pretty(&json!({
        "status":"PASS","scope":"native P022 probes against original packages; nine actual common checks, not full qualification",
        "families":summaries,"generated_at":timestamp(chrono::Utc::now())})).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_python_por_host_protegido_y_plan_tipado() {
    let mut fixture = Fixture::new("source");
    fixture.install_source_candidate().await;
    let guard = fixture.start().await;
    let source_id = fixture.data["config"]["source_id"]
        .as_str()
        .unwrap()
        .to_string();
    let support: SearchSupport =
        serde_json::from_value(fixture.data["config"]["search_support"].clone()).unwrap();
    let source = SourceWorker::new(guard.clone(), source_id.clone(), support).unwrap();
    let check: Result<(), Box<dyn std::error::Error>> = async {
        let query = SourceRequest::Search(SourceSearch {
            source_id: source_id.clone(),
            q: "Reference".into(),
            capabilities: vec![],
            cursor: None,
            limit: 10,
        });
        let read = Access {
            reads: vec![fixture.work.join("catalog/catalog.json")],
            ..Access::default()
        };
        let SourceReply::Search(page) = source.call(query.clone(), &read, None).await? else {
            return Err(std::io::Error::other("ModelPage esperado").into());
        };
        assert_eq!(page.items.len(), 1);
        let catalog_reference = larm_domain::acquisition::catalog_reference(&page.items[0])?
            .ok_or_else(|| std::io::Error::other("SourceCatalogReference esperado"))?;
        assert_eq!(catalog_reference.source_id, source_id);
        let revision_reference = larm_adapter_sdk::source::RevisionRef {
            source_id: catalog_reference.source_id.clone(),
            upstream_namespace: catalog_reference.upstream_namespace.clone(),
            upstream_model_id: catalog_reference.upstream_model_id.clone(),
            revision_key: catalog_reference.revision_key.clone(),
        };
        let SourceReply::GetManifest(remote_manifest) = source
            .call(SourceRequest::GetManifest(revision_reference), &read, None).await?
        else { return Err(std::io::Error::other("ArtifactManifest esperado").into()); };
        assert_eq!(remote_manifest.revision_key, catalog_reference.revision_key);
        assert!(!remote_manifest.artifacts.is_empty());
        let denied = source
            .call(
                query,
                &Access {
                    urls: vec!["http://127.0.0.1:1/foreign".into()],
                    ..Access::default()
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.details.reason, "network_denied");
        let denied = guard
            .call(
                "source.auth_probe",
                json!({"source_id":source_id,"auth_mode":"NONE","account_id":null,"credential_reference_id":null}),
                &Access {
                    secrets: vec!["larm/ungranted".into()],
                    ..Access::default()
                },
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.details.reason, "secret_denied");
        let request = DownloadPlanRequest {
            acquisition: None,
            variant_id: fixture.data["model"]["id"].as_str().unwrap().into(),
            storage_location_id: fixture.data["config"]["storage_location_id"]
                .as_str()
                .unwrap()
                .into(),
            source_id: Some(source_id.clone()),
            hard_budget_bytes: None,
            priority: DownloadPriority::Normal,
            optional_artifact_ids: Some(vec![]),
        };
        let all = Access {
            reads: vec![fixture.work.clone()],
            writes: vec![fixture.work.clone()],
            urls: vec![
                format!("{}/probe", fixture.data["base_url"].as_str().unwrap()),
                format!("{}/artifact/0", fixture.data["base_url"].as_str().unwrap()),
            ],
            ..Access::default()
        };
        let SourceReply::DownloadPlan(plan) = source
            .call(SourceRequest::DownloadPlan(request.clone()), &all, None)
            .await?
        else {
            return Err(std::io::Error::other("Plan esperado").into());
        };
        let SourceReply::Download(receipt) = source
            .call(
                SourceRequest::Download(ExecutePlan {
                    plan_id: plan.plan_id,
                    plan_hash: plan.plan_hash,
                    approval_id: None,
                }),
                &all,
                None,
            )
            .await?
        else {
            return Err(std::io::Error::other("JobReceipt esperado").into());
        };
        assert_eq!(serde_json::to_value(&receipt)?["state"], "COMPLETED");
        assert!(receipt.status_url.starts_with("file:///"));
        let status: Value = serde_json::from_str(&std::fs::read_to_string(
            fixture
                .work
                .join("job-status")
                .join(format!("{}.json", receipt.job_id)),
        )?)?;
        let hash = status["result"]["manifest_hash"].as_str().unwrap();
        let bytes = std::fs::read(fixture.work.join("storage").join(hash).join("data.bin"))?;
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            fixture.data["sha256"].as_str().unwrap()
        );
        let entry = &fixture.data["catalog"]["entries"][0];
        let SourceReply::EstimateSize(cached) = source.call(
            SourceRequest::EstimateSize(larm_adapter_sdk::source::RevisionRef {
                source_id,
                upstream_namespace: entry["upstream_namespace"].as_str().unwrap().into(),
                upstream_model_id: entry["upstream_model_id"].as_str().unwrap().into(),
                revision_key: entry["revision"]["revision_key"].as_str().unwrap().into(),
            }), &all, None,
        ).await? else { return Err(std::io::Error::other("StorageEstimate esperado").into()); };
        assert_eq!(cached.download_bytes, larm_domain::units::DecimalU64::new(0));
        let SourceReply::DownloadPlan(cached_plan) = source.call(
            SourceRequest::DownloadPlan(request), &all, None,
        ).await? else { return Err(std::io::Error::other("Plan completo esperado con caché privada").into()); };
        assert_eq!(cached_plan.storage_estimate.download_bytes, larm_domain::units::DecimalU64::new(bytes.len().try_into().unwrap()));
        assert_eq!(cached_plan.storage_estimate.existing_bytes, larm_domain::units::DecimalU64::new(0));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("reference-source.sqlite"))
            .read_only(true);
        let pool = sqlx::SqlitePool::connect_with(options).await?;
        let held: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM ref_lock)+(SELECT count(*) FROM ref_reservation)",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(held, 0);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_outbox")
            .fetch_one(&pool)
            .await?;
        assert_eq!(events, 3);
        pool.close().await;
        Ok(())
    }
    .await;
    source.stop().await;
    check.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_rust_por_host_protegido_arranca_y_cierra_echo() {
    let fixture = Fixture::new("runtime");
    let guard = fixture.start().await;
    let check:Result<(),Box<dyn std::error::Error>>=async {
        let listener=std::net::TcpListener::bind(("127.0.0.1",0))?;
        let port=listener.local_addr()?.port(); drop(listener);
        let id=uuid::Uuid::now_v7().to_string();
        let parameters=larm_domain::launch::LaunchParameters::builtin(&id,2,Some(2048),512);
        let mut parameters=serde_json::to_value(parameters)?;
        parameters["adapter_options"]=json!({"port":port,"model_path":fixture.data["model_path"]});
        let access=Access { reads:vec![PathBuf::from(fixture.data["model_path"].as_str().unwrap())], ..Access::default() };
        let plan=guard.call("runtime.launch_plan",json!({"installation_id":id,"variant_id":id,"launch_profile":parameters,"constraints":{"runtime_version_id":null,"backend_id":null,"execution_fingerprint":null,"allow_backend_fallback":false,"max_memory_bytes":null},"resource_requirements":null}),&access,None,None).await?;
        let started=guard.call("runtime.start",json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"approval_id":null}),&access,None,None).await?;
        let reference=json!({"instance_id":started["instance_id"],"generation":started["generation"]});
        let mut ready=false;
        for _ in 0..100 {
            let health=guard.call("runtime.health",reference.clone(),&access,None,None).await?;
            if health["state"]=="READY" { ready=true;break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ready);
        let stopped=guard.call("runtime.stop",reference,&access,None,None).await?;
        assert_eq!(stopped,json!({"stopped":true}));
        assert!(std::net::TcpStream::connect(("127.0.0.1",port)).is_err());
        Ok(())
    }.await;
    guard.stop().await;
    check.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn benchmark_suite_nativa_completa_muestras_estadisticas_fallos_y_core_sqlite() {
    use larm_adapter_host::conformance::{
        benchmark::{BenchmarkConfig, ReferenceBenchmarkSuite},
        NativeConformanceRunner,
    };
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::{AdapterConformanceRepository, AdapterSelectionRepository};
    let fixture = Fixture::new("benchmark-conformance");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let forbidden = fixture._root.path().join("outside-grant.bin");
    std::fs::write(&forbidden, b"owned benchmark sentinel outside grants").unwrap();
    let binary = PathBuf::from(fixture.data["backend_binary"].as_str().unwrap());
    let config = BenchmarkConfig {
        profile: fixture.data["profile"].clone(),
        base_url: fixture.data["base_url"].as_str().unwrap().into(),
        control: fixture.data["control"].as_str().unwrap().into(),
        requests: fixture.data["requests"].as_str().unwrap().into(),
        backend_state: fixture.data["backend_state"].as_str().unwrap().into(),
        backend_binary: binary.clone(),
        forbidden: forbidden.clone(),
        evidence_root: repo
            .join("docs/reports/P026_runs/native-benchmark")
            .join(uuid::Uuid::now_v7().to_string()),
    };
    let suite = Arc::new(ReferenceBenchmarkSuite::new(config.clone()).unwrap());
    let (_db_root, db) = larm_db::testing::temp_migrated().await.unwrap();
    let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let receipts = Arc::new(larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    });
    let (ctx, auth, token) = native_actor().await;
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "benchmark".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    installations.insert(&installation, &ctx).await.unwrap();
    let record=ApprovalRequest{operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone()}.grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(10)).unwrap();
    let grant =
        TrustedOnlyGrant::from_approval(&record, &fixture.manifest, &ctx.actor_id, now).unwrap();
    let target = NativeReferenceTarget {
        spec: WorkerSpec {
            package_dir: fixture.package.clone(),
            manifest: fixture.manifest.clone(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        grant,
        inputs: vec![
            forbidden,
            binary,
            fixture.work.join("profile/profile.json"),
            repo.join("tools/p025_host_fixture.py"),
        ],
        suite: suite.clone(),
        capability_profile: None,
    };
    let catalog = persistent_recipe(
        &db,
        &ctx,
        &installation,
        &record,
        target,
        larm_adapter_host::conformance::profiles::Recipe::Benchmark(config.clone()),
        fixture.policy().roots.0,
    )
    .await;
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(fixture.policy()),
        catalog,
        // Los timeouts son detectores de cuelgue, no presupuestos de
        // rendimiento: bajo carga en paralelo el workspace satura el
        // fixture y los 30 s de rpc ya no dan margen al worker de benchmark.
        HostConfig {
            rpc_timeout: Duration::from_secs(60),
            handshake_timeout: Duration::from_secs(30),
            ..HostConfig::default()
        },
    ));
    let (result, probe_job) = native_public_probe(
        &db,
        runner.clone(),
        &installation.id,
        &ctx,
        auth.clone(),
        &token,
    )
    .await;
    let stored = installations.get(&installation.id).await.unwrap().unwrap();
    let receipt: larm_domain::conformance::Receipt =
        serde_json::from_value(stored.metadata[larm_domain::conformance::METADATA_KEY].clone())
            .unwrap_or_else(|_| {
                panic!(
                    "benchmark validation did not commit: {:?}",
                    result.as_ref().err()
                )
            });
    let report_path = suite.report_path(&receipt.run_id);
    result.unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(receipt.passed && receipt.matches_committed(&stored).unwrap());
    assert_eq!(stored.state, AdapterState::Validated);
    assert_eq!(stored.trust, AdapterTrust::Untrusted);
    assert_eq!(stored.row_version, 2);
    let observations = report["observations"]["Ok"].as_array().unwrap();
    assert_eq!(observations.len(), 17);
    assert_eq!(report["run_sha256"], json!(receipt.run_sha256));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["passed"] == true));
    let material = json!({"run_sha256":report["run_sha256"],"scope":report["scope"],"wire":report["wire"],"cases":report["cases"],"methods":report["methods"],"checkpoints":report["checkpoints"]});
    for observation in observations.iter().skip(9) {
        assert_eq!(
            observation["evidence_sha256"],
            json!(larm_domain::conformance::hash(
                &json!({"check":observation["check_id"],"evidence":material})
            )
            .unwrap())
        );
    }
    assert_eq!(
        report["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m.as_str().unwrap().starts_with("benchmark."))
            .count(),
        3
    );
    assert_eq!(
        receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt.clone())
    );
    let selections = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    selections
        .change_selection(
            &stored.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: stored.id.clone(),
            },
            &ctx,
        )
        .await
        .unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, counts.2);
    let physical_gate = physical_selection_probe(&fixture, &selections, &stored, &ctx).await;
    std::fs::write(repo.join("docs/reports/P026_runs/native-benchmark.json"),serde_json::to_vec_pretty(&json!({"status":"PASS","physical_gate":physical_gate,"probe_job":probe_job,"scope":"native benchmark full HTTP/Rust echo/private samples/ledger recipe, 17 real observations, Core/SQLite receipt and disabled-selection promotion gate; no inference/Core benchmark admission/public daemon validation","report":report_path.strip_prefix(repo).unwrap(),"sha256":hex::encode(Sha256::digest(std::fs::read(&report_path).unwrap())),"receipt":receipt,"row_version":stored.row_version,"generated_at":timestamp(chrono::Utc::now())})).unwrap()).unwrap();
    runner.shutdown().await.unwrap();
    db.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transformation_suite_nativa_completa_archivos_intentos_fallos_y_core_sqlite() {
    use larm_adapter_host::conformance::{
        transformation::{ReferenceTransformationSuite, TransformationConfig},
        NativeConformanceRunner,
    };
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::{AdapterConformanceRepository, AdapterSelectionRepository};
    let fixture = Fixture::new("transformation-conformance");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let forbidden = fixture._root.path().join("outside-grant.bin");
    std::fs::write(&forbidden, b"owned transformation sentinel outside grants").unwrap();
    let expected = PathBuf::from(fixture.data["expected_input"].as_str().unwrap());
    let config = TransformationConfig {
        profile: fixture.data["profile"].clone(),
        expected_input: expected.clone(),
        forbidden: forbidden.clone(),
        evidence_root: repo
            .join("docs/reports/P026_runs/native-transformation")
            .join(uuid::Uuid::now_v7().to_string()),
    };
    let suite = Arc::new(ReferenceTransformationSuite::new(config.clone()).unwrap());
    let (_db_root, db) = larm_db::testing::temp_migrated().await.unwrap();
    let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let receipts = Arc::new(larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    });
    let (ctx, auth, token) = native_actor().await;
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "transformation".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    installations.insert(&installation, &ctx).await.unwrap();
    let record=ApprovalRequest{operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(10)).unwrap();
    let grant =
        TrustedOnlyGrant::from_approval(&record, &fixture.manifest, &ctx.actor_id, now).unwrap();
    let target = NativeReferenceTarget {
        spec: WorkerSpec {
            package_dir: fixture.package.clone(),
            manifest: fixture.manifest.clone(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        grant,
        inputs: vec![
            forbidden,
            expected,
            fixture.work.join("input/fixture.bin"),
            fixture.work.join("profile/profile.json"),
            repo.join("tools/p025_host_fixture.py"),
        ],
        suite: suite.clone(),
        capability_profile: None,
    };
    let catalog = persistent_recipe(
        &db,
        &ctx,
        &installation,
        &record,
        target,
        larm_adapter_host::conformance::profiles::Recipe::Transformation(config.clone()),
        fixture.policy().roots.0,
    )
    .await;
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(fixture.policy()),
        catalog,
        HostConfig {
            rpc_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(15),
            ..HostConfig::default()
        },
    ));
    let (result, probe_job) = native_public_probe(
        &db,
        runner.clone(),
        &installation.id,
        &ctx,
        auth.clone(),
        &token,
    )
    .await;
    let stored = installations.get(&installation.id).await.unwrap().unwrap();
    let receipt: larm_domain::conformance::Receipt =
        serde_json::from_value(stored.metadata[larm_domain::conformance::METADATA_KEY].clone())
            .unwrap_or_else(|_| {
                panic!(
                    "transformation validation did not commit: {:?}",
                    result.as_ref().err()
                )
            });
    let report_path = suite.report_path(&receipt.run_id);
    result.unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(receipt.passed && receipt.matches_committed(&stored).unwrap());
    assert_eq!(stored.state, AdapterState::Validated);
    assert_eq!(stored.trust, AdapterTrust::Untrusted);
    assert_eq!(stored.row_version, 2);
    let observations = report["observations"]["Ok"].as_array().unwrap();
    assert_eq!(observations.len(), 17);
    assert_eq!(report["run_sha256"], json!(receipt.run_sha256));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["passed"] == true));
    let material = json!({"run_sha256":report["run_sha256"],"scope":report["scope"],"wire":report["wire"],"cases":report["cases"],"methods":report["methods"],"checkpoints":report["checkpoints"]});
    for observation in observations.iter().skip(9) {
        assert_eq!(
            observation["evidence_sha256"],
            json!(larm_domain::conformance::hash(
                &json!({"check":observation["check_id"],"evidence":material})
            )
            .unwrap())
        );
    }
    assert_eq!(
        report["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m.as_str().unwrap().starts_with("transformation."))
            .count(),
        4
    );
    assert_eq!(
        receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt.clone())
    );
    let typed_consumer = transformation_worker::exercise(
        &db,
        &runner,
        &stored,
        &receipts
            .current_for_consumer(&stored.id)
            .await
            .unwrap()
            .unwrap()
            .1,
        &ctx,
        &fixture,
        &config,
    )
    .await;
    let selections = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    selections
        .change_selection(
            &stored.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: stored.id.clone(),
            },
            &ctx,
        )
        .await
        .unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, counts.2);
    let physical_gate = physical_selection_probe(&fixture, &selections, &stored, &ctx).await;
    std::fs::write(repo.join("docs/reports/P026_runs/native-transformation.json"),serde_json::to_vec_pretty(&json!({"status":"PASS","typed_consumer":typed_consumer,"physical_gate":physical_gate,"probe_job":probe_job,"scope":"original native P026, typed consumer and direct Core admission/restart/closure, managed CAS and atomic catalogue publication with live process and storage; seeded catalogue/fixture preview/reference namespace, no public daemon pipeline, actual daemon crash/restart or GGUF acceptance",
        "report":report_path.strip_prefix(repo).unwrap(),"sha256":hex::encode(Sha256::digest(std::fs::read(&report_path).unwrap())),"receipt":receipt,"row_version":stored.row_version,"generated_at":timestamp(chrono::Utc::now())})).unwrap()).unwrap();
    runner.shutdown().await.unwrap();
    db.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_suite_nativa_completa_http_cancel_crash_y_core_sqlite() {
    use larm_adapter_host::conformance::{
        protocol::{ProtocolConfig, ReferenceProtocolSuite},
        NativeConformanceRunner,
    };
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::{AdapterConformanceRepository, AdapterSelectionRepository};
    let fixture = Fixture::new("protocol-conformance");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let forbidden = fixture._root.path().join("outside-grant.bin");
    std::fs::write(&forbidden, b"owned protocol sentinel outside grants").unwrap();
    let binary = PathBuf::from(fixture.data["backend_binary"].as_str().unwrap());
    let config = ProtocolConfig {
        profile: fixture.data["profile"].clone(),
        base_url: fixture.data["base_url"].as_str().unwrap().into(),
        control: fixture.data["control"].as_str().unwrap().into(),
        requests: fixture.data["requests"].as_str().unwrap().into(),
        backend_state: fixture.data["backend_state"].as_str().unwrap().into(),
        backend_binary: binary.clone(),
        forbidden: forbidden.clone(),
        evidence_root: repo
            .join("docs/reports/P026_runs/native-protocol")
            .join(uuid::Uuid::now_v7().to_string()),
    };
    let suite = Arc::new(ReferenceProtocolSuite::new(config.clone()).unwrap());
    let (_db_root, db) = larm_db::testing::temp_migrated().await.unwrap();
    let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let receipts = Arc::new(larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    });
    let (ctx, auth, token) = native_actor().await;
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "protocol".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    installations.insert(&installation, &ctx).await.unwrap();
    let record=ApprovalRequest{operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(10)).unwrap();
    let grant =
        TrustedOnlyGrant::from_approval(&record, &fixture.manifest, &ctx.actor_id, now).unwrap();
    let target = NativeReferenceTarget {
        spec: WorkerSpec {
            package_dir: fixture.package.clone(),
            manifest: fixture.manifest.clone(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        grant,
        inputs: vec![
            forbidden,
            binary,
            fixture.work.join("profile.json"),
            repo.join("tools/p025_host_fixture.py"),
            repo.join("tools/p026_protocol_real.py"),
        ],
        suite: suite.clone(),
        capability_profile: None,
    };
    let catalog = persistent_recipe(
        &db,
        &ctx,
        &installation,
        &record,
        target,
        larm_adapter_host::conformance::profiles::Recipe::Protocol(config.clone()),
        fixture.policy().roots.0,
    )
    .await;
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(fixture.policy()),
        catalog,
        HostConfig {
            rpc_timeout: Duration::from_secs(15),
            handshake_timeout: Duration::from_secs(15),
            ..HostConfig::default()
        },
    ));
    let (result, probe_job) = native_public_probe(
        &db,
        runner.clone(),
        &installation.id,
        &ctx,
        auth.clone(),
        &token,
    )
    .await;
    let stored = installations.get(&installation.id).await.unwrap().unwrap();
    let receipt: larm_domain::conformance::Receipt =
        serde_json::from_value(stored.metadata[larm_domain::conformance::METADATA_KEY].clone())
            .unwrap_or_else(|_| {
                panic!(
                    "protocol validation did not commit: {:?}",
                    result.as_ref().err()
                )
            });
    let report_path = suite.report_path(&receipt.run_id);
    result.unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(receipt.passed && receipt.matches_committed(&stored).unwrap());
    assert_eq!(stored.state, AdapterState::Validated);
    assert_eq!(stored.trust, AdapterTrust::Untrusted);
    assert_eq!(stored.row_version, 2);
    let observations = report["observations"]["Ok"].as_array().unwrap();
    assert_eq!(observations.len(), 17);
    assert_eq!(report["run_sha256"], json!(receipt.run_sha256));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["passed"] == true));
    let material = json!({"run_sha256":report["run_sha256"],"scope":report["scope"],"wire":report["wire"],"cases":report["cases"],"methods":report["methods"],"checkpoints":report["checkpoints"]});
    for observation in observations.iter().skip(9) {
        assert_eq!(
            observation["evidence_sha256"],
            json!(larm_domain::conformance::hash(
                &json!({"check":observation["check_id"],"evidence":material})
            )
            .unwrap())
        );
    }
    assert_eq!(
        report["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m.as_str().unwrap().starts_with("protocol."))
            .count(),
        4
    );
    assert_eq!(
        receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt.clone())
    );
    let selections = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    selections
        .change_selection(
            &stored.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: stored.id.clone(),
            },
            &ctx,
        )
        .await
        .unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, counts.2);
    let physical_gate = physical_selection_probe(&fixture, &selections, &stored, &ctx).await;
    std::fs::write(repo.join("docs/reports/P026_runs/native-protocol.json"),serde_json::to_vec_pretty(&json!({"status":"PASS","physical_gate":physical_gate,"probe_job":probe_job,"scope":"native protocol full HTTP/Rust echo recipe, 17 real observations, Core/SQLite receipt and disabled-selection promotion gate; no inference/Core lease admission/public daemon validation",
        "report":report_path.strip_prefix(repo).unwrap(),"sha256":hex::encode(Sha256::digest(std::fs::read(&report_path).unwrap())),"receipt":receipt,"row_version":stored.row_version,"generated_at":timestamp(chrono::Utc::now())})).unwrap()).unwrap();
    runner.shutdown().await.unwrap();
    db.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_suite_nativa_completa_descarga_recupera_y_confirma_core_sqlite() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::Download,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_suite_nativa_http416_verifica_parciales_y_reinicia_rango() {
    source_suite_nativa(download_restart::Flow::Ranges, SourcePurpose::Download).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_catalogo_cualificado_resuelve_p041_y_persiste_sin_bundle_fisico() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::Ingest,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_busqueda_http_cualificada_pagina_ingiere_y_revoca_cache() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::Search,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "native performance acceptance: run explicitly with --release --ignored"]
async fn source_busqueda_http_defaults_optimizada_pagina_ingiere_y_revoca_cache() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::SearchDefaults,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit optimized source profiling with phase timing and extended harness deadline"]
async fn source_busqueda_http_optimizada_perfil_de_fases() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::SearchProfile,
    )
    .await;
}

#[derive(Clone, Copy, PartialEq)]
// This acceptance is run explicitly after building/freezing the native inputs.
enum SourcePurpose {
    Download,
    FirstDownload,
    FirstDownloadTcp,
    ModelsGui,
    Ingest,
    Search,
    SearchDefaults,
    SearchProfile,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit native first-acquisition acceptance: build/freeze then run with --release --ignored"]
async fn source_primera_descarga_core_cualificada_concilia_bundle_fisico() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::FirstDownload,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit TCP first-acquisition/pause/cancel/restart acceptance; build and freeze native inputs"]
async fn source_primera_adquisicion_tcp_pausa_cancela_y_recupera_sin_bundle_previo() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::FirstDownloadTcp,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "interactive native desktop acceptance: no other daemon/desktop may own the user's bootstrap pipe; explicit build/freeze and bounded GUI control"]
async fn source_catalogo_cualificado_y_primera_descarga_desde_escritorio_nativo() {
    source_suite_nativa(
        download_restart::Flow::IntegrityAndRestart,
        SourcePurpose::ModelsGui,
    )
    .await;
}

async fn source_suite_nativa(flow: download_restart::Flow, purpose: SourcePurpose) {
    use larm_adapter_host::conformance::{
        source::{ReferenceSourceSuite, SourceConfig},
        NativeConformanceRunner,
    };
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::{AdapterConformanceRepository, AdapterSelectionRepository};
    let mut fixture = Fixture::new("source-conformance");
    let (_db_root, db) = larm_db::testing::temp_migrated().await.unwrap();
    let (ctx, auth, token) = native_actor().await;
    let download_location =
        download_restart::create_location(&db, &_db_root.path().join("download-models"), &ctx)
            .await;
    fixture.data["config"]["storage_location_id"] = json!(download_location.id);
    std::fs::write(
        fixture.work.join("profile.json"),
        json!({"source_id":fixture.data["config"]["source_id"],"storage_location_id":download_location.id}).to_string(),
    )
    .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let evidence = repo
        .join("docs/reports/P026_runs/native-source")
        .join(uuid::Uuid::now_v7().to_string());
    let forbidden = fixture._root.path().join("outside-grant.bin");
    std::fs::write(&forbidden, b"owned sentinel outside source grants").unwrap();
    let expected_files: Vec<PathBuf> = fixture.data["expected_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| PathBuf::from(v.as_str().unwrap()))
        .collect();
    let mut config = SourceConfig {
        source_id: fixture.data["config"]["source_id"].as_str().unwrap().into(),
        storage_id: fixture.data["config"]["storage_location_id"]
            .as_str()
            .unwrap()
            .into(),
        base_url: fixture.data["base_url"].as_str().unwrap().into(),
        catalog: fixture.data["catalog"].clone(),
        expected_files: expected_files.clone(),
        control: fixture.data["control"].as_str().unwrap().into(),
        requests: fixture.data["requests"].as_str().unwrap().into(),
        forbidden: forbidden.clone(),
        evidence_root: evidence,
    };
    let mut suite = Arc::new(ReferenceSourceSuite::new(config.clone()).unwrap());
    let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let receipts = Arc::new(larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    });
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "source".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    installations.insert(&installation, &ctx).await.unwrap();
    let mut configured_source = None;
    if purpose != SourcePurpose::Download {
        // Create the Core source first. Bind the explicit fixture profile and
        // catalog to its generated UUID BEFORE qualification, never after it.
        let sources = larm_core::sources::SourceCatalog {
            repo: Arc::new(larm_db::sources::SqlSources {
                pool: db.pool.clone(),
                qualification: None,
            }),
            installations: installations.clone(),
        };
        let adapter = sources
            .register_adapter(
                &installation.id,
                "HTTP",
                fixture.data["config"]["search_support"].clone(),
                &ctx,
            )
            .await
            .unwrap();
        let request = larm_domain::sources::ModelSourceCreate {
            name: "Controlled qualified catalog".into(),
            adapter_id: adapter.id,
            base_url: config.base_url.clone(),
            priority: 0,
            enabled: false,
            auth_mode: larm_domain::sources::AuthMode::None,
            account_id: None,
            policy: Default::default(),
        };
        let source = sources
            .create(
                &request,
                &uuid::Uuid::now_v7().to_string(),
                &larm_domain::conformance::hash(&request).unwrap(),
                &ctx,
            )
            .await
            .unwrap();
        config.source_id = source.id.clone();
        config.catalog["source_id"] = json!(source.id);
        fixture.data["config"]["source_id"] = json!(source.id);
        fixture.data["catalog"] = config.catalog.clone();
        std::fs::write(
            fixture.work.join("profile.json"),
            json!({"source_id":source.id,"storage_location_id":download_location.id}).to_string(),
        )
        .unwrap();
        std::fs::write(
            fixture.work.join("catalog/catalog.json"),
            serde_json::to_vec_pretty(&config.catalog).unwrap(),
        )
        .unwrap();
        suite = Arc::new(ReferenceSourceSuite::new(config.clone()).unwrap());
        configured_source = Some(source);
    }
    let record=ApprovalRequest{operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],
        code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,
            "expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(10)).unwrap();
    let grant =
        TrustedOnlyGrant::from_approval(&record, &fixture.manifest, &ctx.actor_id, now).unwrap();
    let mut inputs = expected_files;
    inputs.extend([
        forbidden.clone(),
        fixture.work.join("profile.json"),
        fixture.work.join("catalog/catalog.json"),
        repo.join("tools/p025_host_fixture.py"),
        repo.join("tools/p025_source_real.py"),
    ]);
    let target = NativeReferenceTarget {
        spec: WorkerSpec {
            package_dir: fixture.package.clone(),
            manifest: fixture.manifest.clone(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        grant,
        inputs,
        suite: suite.clone(),
        capability_profile: None,
    };
    let catalog = persistent_recipe(
        &db,
        &ctx,
        &installation,
        &record,
        target,
        larm_adapter_host::conformance::profiles::Recipe::Source(config.clone()),
        fixture.policy().roots.0,
    )
    .await;
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(fixture.policy()),
        catalog,
        HostConfig {
            rpc_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(15),
            ..HostConfig::default()
        },
    ));
    let pre_probe_version = installations
        .get(&installation.id)
        .await
        .unwrap()
        .unwrap()
        .row_version;
    let (result, probe_job) = native_public_probe(
        &db,
        runner.clone(),
        &installation.id,
        &ctx,
        auth.clone(),
        &token,
    )
    .await;
    let stored = installations.get(&installation.id).await.unwrap().unwrap();
    let receipt: larm_domain::conformance::Receipt =
        serde_json::from_value(stored.metadata[larm_domain::conformance::METADATA_KEY].clone())
            .unwrap_or_else(|_| {
                panic!(
                    "source validation did not commit: {:?}",
                    result.as_ref().err()
                )
            });
    let report_path = suite.report_path(&receipt.run_id);
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    result.unwrap();
    assert!(receipt.passed && receipt.matches_committed(&stored).unwrap());
    assert_eq!(stored.state, AdapterState::Validated);
    assert_eq!(stored.trust, AdapterTrust::Untrusted);
    assert_eq!(stored.row_version, pre_probe_version + 1);
    let observations = report["observations"]["Ok"].as_array().unwrap();
    assert_eq!(observations.len(), 21);
    assert_eq!(report["run_sha256"], json!(receipt.run_sha256));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["passed"] == true));
    let material = json!({"run_sha256":report["run_sha256"],"scope":report["scope"],"wire":report["wire"],"cases":report["cases"],"methods":report["methods"],"checkpoints":report["checkpoints"]});
    for observation in observations.iter().skip(9) {
        assert_eq!(
            observation["evidence_sha256"],
            json!(larm_domain::conformance::hash(
                &json!({"check":observation["check_id"],"evidence":material})
            )
            .unwrap())
        );
    }
    assert_eq!(
        report["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m.as_str().unwrap().starts_with("source."))
            .count(),
        9
    );
    assert_eq!(
        receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt.clone())
    );
    let (_, current_run) = receipts
        .current_for_consumer(&stored.id)
        .await
        .unwrap()
        .unwrap();
    let owner = larm_domain::ports::AdapterQualification::acquire(
        runner.as_ref(),
        &stored,
        &current_run,
        &ctx,
    )
    .await
    .unwrap();
    let request_log_before = std::fs::read(&config.requests).unwrap();
    runner
        .verify_qualification_snapshot(&stored, &current_run, &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&config.requests).unwrap(),
        request_log_before,
        "la admisión física no ejecuta RPC ni IO de fuente"
    );
    let second_owner = larm_domain::ports::AdapterQualification::acquire(
        runner.as_ref(),
        &stored,
        &current_run,
        &ctx,
    )
    .await;
    assert_eq!(
        second_owner.err().unwrap().details.reason,
        "conformance_already_running",
        "las sesiones de ejecución conservan exclusión"
    );
    drop(owner);
    if purpose != SourcePurpose::Download {
        let source = configured_source.unwrap();
        let sources = larm_core::sources::SourceCatalog {
            repo: Arc::new(larm_db::sources::SqlSources {
                pool: db.pool.clone(),
                qualification: Some(runner.clone()),
            }),
            installations: installations.clone(),
        };
        let mut request = source.configuration();
        request.enabled = true;
        let enabled = sources
            .update(&source.id, source.row_version, &request, &ctx)
            .await
            .unwrap();
        if matches!(
            purpose,
            SourcePurpose::Search | SourcePurpose::SearchDefaults | SourcePurpose::SearchProfile
        ) {
            let defaults = purpose == SourcePurpose::SearchDefaults;
            let evidence_folder = match purpose {
                SourcePurpose::SearchDefaults => "native-qualified-search-defaults",
                SourcePurpose::SearchProfile => "native-qualified-search-optimized-profile",
                _ => "native-qualified-search",
            };
            let search_config = if defaults {
                larm_models::search::SearchConfig::default()
            } else {
                larm_models::search::SearchConfig {
                    source_timeout: Duration::from_secs(300),
                    cache_ttl: Duration::from_secs(600),
                    ..Default::default()
                }
            };
            let search = source_search_http::exercise(
                &db,
                &fixture,
                runner.clone(),
                &ctx,
                auth.clone(),
                &token,
                &enabled,
                &forbidden,
                search_config,
                defaults,
                evidence_folder,
            )
            .await;
            let summary = json!({"status":"PASS","scope":"complete original native P026, actual authenticated HTTP router -> qualified product search -> P041 atomic ingestion, pagination/idempotency/context and source revocation; in-process router, no daemon TCP/GUI/first Core download acceptance",
                "source":enabled,"probe_job":probe_job,"receipt":receipt,"qualification_observations":observations.len(),
                "qualification_report":report_path,"search":search});
            let summary_name = format!("{evidence_folder}.json");
            std::fs::write(
                repo.join("docs/reports/P044_runs").join(summary_name),
                serde_json::to_vec_pretty(&summary).unwrap(),
            )
            .unwrap();
            runner.shutdown().await.unwrap();
            db.pool.close().await;
            return;
        }
        if purpose == SourcePurpose::ModelsGui {
            let gui = source_models_gui::exercise(
                &db,
                _db_root.path(),
                &fixture,
                &stored.id,
                &enabled.id,
                &ctx,
            )
            .await;
            let summary = json!({"status":"PASS", "scope":"original P026 plus actual daemon qualification and API ingestion; native desktop comparison/explicit first download/verification, controlled own artifacts; Discover UI and upstream providers not accepted", "parent_receipt":receipt,"parent_qualification_report":report_path,"gui":gui});
            std::fs::write(
                repo.join("docs/reports/P044_runs/native-models-gui.json"),
                serde_json::to_vec_pretty(&summary).unwrap(),
            )
            .unwrap();
            runner.shutdown().await.unwrap();
            db.pool.close().await;
            return;
        }
        if purpose == SourcePurpose::FirstDownloadTcp {
            let transfer = source_first_download_tcp::exercise(
                &db,
                _db_root.path(),
                &fixture,
                &stored.id,
                &enabled.id,
                &ctx,
            )
            .await;
            let summary = json!({"status":"PASS","scope":"original P026 then independent daemon qualification; actual TCP search/P041/first remote acquisition, pause/cancel/restart/Range/CAS/P042/reobservation; no GUI/upstream/private-auth/runtime acceptance", "parent_receipt":receipt,"parent_qualification_report":report_path,"transfer":transfer});
            std::fs::write(
                repo.join("docs/reports/P044_runs/native-first-core-download-tcp.json"),
                serde_json::to_vec_pretty(&summary).unwrap(),
            )
            .unwrap();
            runner.shutdown().await.unwrap();
            db.pool.close().await;
            return;
        }
        let ingestion = source_acquisition::qualified_catalog_ingestion(
            &db,
            &fixture,
            runner.clone(),
            &stored,
            &current_run,
            &ctx,
        )
        .await
        .unwrap();
        if purpose == SourcePurpose::FirstDownload {
            let transfer = source_first_download::exercise(
                &db,
                &fixture,
                runner.clone(),
                &ctx,
                ingestion["central_variant_ids"][0].as_str().unwrap(),
            )
            .await;
            let summary = json!({"status":"PASS","scope":"complete original P026 plus qualified P041 ingestion and product Core first download with native source worker and observed Windows storage; direct services, no HTTP/GUI/load acceptance", "source":enabled,"receipt":receipt,"qualification_report":report_path,"ingestion":ingestion,"transfer":transfer});
            std::fs::write(
                repo.join("docs/reports/P044_runs/native-first-core-download.json"),
                serde_json::to_vec_pretty(&summary).unwrap(),
            )
            .unwrap();
            runner.shutdown().await.unwrap();
            db.pool.close().await;
            return;
        }
        let summary = json!({"status":"PASS","scope":"original source with complete native P026 recipe, Core-generated source ID bound in fixture before qualification, qualified public RPC observations and real P041/atomic SQLite ingestion; no daemon search HTTP, first Core download or GUI acceptance",
            "source":enabled,"probe_job":probe_job,"receipt":receipt,"qualification_observations":observations.len(),
            "qualification_report":report_path,"ingestion":ingestion});
        std::fs::write(
            repo.join("docs/reports/P044_runs/native-qualified-ingestion.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();
        runner.shutdown().await.unwrap();
        db.pool.close().await;
        return;
    }
    let selections = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    selections
        .change_selection(
            &stored.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: stored.id.clone(),
            },
            &ctx,
        )
        .await
        .unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts.0, 1);
    assert_eq!(counts.1, counts.2);
    let physical_gate = physical_selection_probe(&fixture, &selections, &stored, &ctx).await;
    let daemon_download_restart = download_restart::exercise(
        &db,
        _db_root.path(),
        &fixture.data,
        &stored.id,
        &download_location,
        flow,
    )
    .await;
    let (summary_name, result_key) = match flow {
        download_restart::Flow::IntegrityAndRestart => {
            ("native-source.json", "daemon_download_restart")
        }
        download_restart::Flow::Ranges => ("native-source-http416.json", "daemon_download_ranges"),
    };
    let mut summary = json!({"status":"PASS","physical_gate":physical_gate,"probe_job":probe_job,
        "scope":"native source full recipe, Core/SQLite qualification and physical selection gate; controlled daemon acceptance is recorded separately and does not qualify an upstream",
        "report":report_path.strip_prefix(repo).unwrap(),"sha256":hex::encode(Sha256::digest(std::fs::read(&report_path).unwrap())),
        "receipt":receipt,"row_version":stored.row_version,"generated_at":timestamp(chrono::Utc::now())});
    summary[result_key] = daemon_download_restart;
    std::fs::write(
        repo.join("docs/reports/P026_runs").join(summary_name),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    runner.shutdown().await.unwrap();
    db.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_suite_nativa_completa_confirma_core_sqlite_y_fallo_retira_elegibilidad() {
    use larm_adapter_host::conformance::{
        runtime::{ReferenceRuntimeSuite, RuntimeConfig},
        NativeConformanceRunner,
    };
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::{AdapterConformanceRepository, AdapterSelectionRepository};
    let fixture = Fixture::new("runtime-conformance");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let evidence_root = repo
        .join("docs/reports/P026_runs/native-runtime")
        .join(uuid::Uuid::now_v7().to_string());
    let (_db_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let receipts = Arc::new(larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    });
    let (ctx, auth, token) = native_actor().await;
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "runtime".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    installations.insert(&installation, &ctx).await.unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let id = uuid::Uuid::now_v7().to_string();
    let mut parameters = serde_json::to_value(larm_domain::launch::LaunchParameters::builtin(
        &id,
        2,
        Some(2048),
        512,
    ))
    .unwrap();
    parameters["adapter_options"] = json!({"port":port,"model_path":fixture.data["model_path"]});
    let launch_request = json!({"installation_id":id,"variant_id":id,"launch_profile":parameters,
        "constraints":{"runtime_version_id":null,"backend_id":null,"execution_fingerprint":null,"allow_backend_fallback":false,"max_memory_bytes":null},"resource_requirements":null});
    let forbidden = fixture._root.path().join("outside-grant.bin");
    std::fs::write(&forbidden, b"owned sentinel outside package grant").unwrap();
    let mut outcomes = Vec::new();
    for positive in [true, false] {
        let current = installations.get(&installation.id).await.unwrap().unwrap();
        let expected_version = if positive {
            fixture.data["observed_version"]
                .as_str()
                .unwrap()
                .to_owned()
        } else {
            "wrong expected version to test revalidation failure".into()
        };
        let config = RuntimeConfig {
            runtime_id: id.clone(),
            launch_request: launch_request.clone(),
            expected_version,
            forbidden_model: forbidden.clone(),
            evidence_root: evidence_root.clone(),
        };
        let suite = Arc::new(ReferenceRuntimeSuite::new(config.clone()).unwrap());
        let now = chrono::Utc::now();
        let record=ApprovalRequest {operation_digest:execution_digest(&fixture.manifest).unwrap(),
            resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
            permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,
                "expires_at":timestamp(now+chrono::Duration::minutes(10)),"secret_refs":[]}).as_object().unwrap().clone()}
            .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(10)).unwrap();
        let grant = TrustedOnlyGrant::from_approval(&record, &fixture.manifest, &ctx.actor_id, now)
            .unwrap();
        let target = NativeReferenceTarget {
            spec: WorkerSpec {
                package_dir: fixture.package.clone(),
                manifest: fixture.manifest.clone(),
                environment: vec![],
                work_dir: fixture.work.clone(),
            },
            grant,
            inputs: vec![forbidden.clone()],
            suite: suite.clone(),
            capability_profile: None,
        };
        let catalog = persistent_recipe(
            &db,
            &ctx,
            &current,
            &record,
            target,
            larm_adapter_host::conformance::profiles::Recipe::Runtime(config.clone()),
            fixture.policy().roots.0,
        )
        .await;
        let runner = Arc::new(NativeConformanceRunner::new(
            Arc::new(fixture.policy()),
            catalog,
            HostConfig {
                rpc_timeout: Duration::from_secs(15),
                handshake_timeout: Duration::from_secs(15),
                ..HostConfig::default()
            },
        ));
        let (result, probe_job) = native_public_probe(
            &db,
            runner.clone(),
            &installation.id,
            &ctx,
            auth.clone(),
            &token,
        )
        .await;
        let stored = installations.get(&installation.id).await.unwrap().unwrap();
        let receipt: larm_domain::conformance::Receipt =
            serde_json::from_value(stored.metadata[larm_domain::conformance::METADATA_KEY].clone())
                .unwrap_or_else(|_| {
                    panic!(
                        "runtime validation did not reach metadata commit: {:?}",
                        result.as_ref().err()
                    )
                });
        let report_path = suite.report_path(&receipt.run_id);
        let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
        let selections = larm_db::adapter_selection::SqlAdapterSelections {
            pool: db.pool.clone(),
            qualification: Some(runner.clone()),
        };
        let mut physical_gate = Value::Null;
        if positive {
            result.unwrap();
            assert!(receipt.passed && receipt.matches_committed(&stored).unwrap());
            assert_eq!(stored.state, AdapterState::Validated);
            assert_eq!(report["observations"]["Ok"].as_array().unwrap().len(), 17);
            assert_eq!(report["run_sha256"], json!(receipt.run_sha256));
            // El informe durable conserva todo el material de los hashes; puede
            // auditarse sin el estado volátil de la suite ni su proceso original.
            let material = json!({"run_sha256":report["run_sha256"],"wire":report["wire"],
                "cases":report["cases"],"checkpoints":report["checkpoints"],"methods":report["methods"],
                "scope":"reference echo; volatile plans/instances, no inference or Core job ledger"});
            for observation in report["observations"]["Ok"]
                .as_array()
                .unwrap()
                .iter()
                .skip(9)
            {
                assert_eq!(
                    observation["evidence_sha256"],
                    json!(larm_domain::conformance::hash(
                        &json!({"check":observation["check_id"],"evidence":material})
                    )
                    .unwrap())
                );
            }
            assert!(report["cases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|case| case["passed"] == true));
            assert_eq!(
                report["methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m.as_str().unwrap().starts_with("runtime."))
                    .count(),
                16
            );
            selections
                .change_selection(
                    &stored.adapter_id,
                    0,
                    &AdapterSelectionChange::Promote {
                        installation_id: stored.id.clone(),
                    },
                    &ctx,
                )
                .await
                .unwrap();
            physical_gate = physical_selection_probe(&fixture, &selections, &stored, &ctx).await;
        } else {
            assert_eq!(result.unwrap_err().details.reason, "adapter_probe_failed");
            assert!(!receipt.passed);
            assert_eq!(stored.state, AdapterState::Installed);
            assert!(selections
                .change_selection(
                    &stored.adapter_id,
                    selections
                        .selection(&stored.adapter_id)
                        .await
                        .unwrap()
                        .unwrap()
                        .row_version,
                    &AdapterSelectionChange::SetEnabled { enabled: true },
                    &ctx
                )
                .await
                .is_err());
        }
        assert_eq!(stored.trust, AdapterTrust::Untrusted);
        assert_eq!(
            receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(stored.row_version, current.row_version + 1);
        let event_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM event")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let outbox_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(event_rows, outbox_rows);
        runner.shutdown().await.unwrap();
        outcomes.push(json!({"passed":positive,"physical_gate":physical_gate,"probe_job":probe_job,"receipt":receipt,"report":report_path.strip_prefix(repo).unwrap(),
            "sha256":hex::encode(Sha256::digest(std::fs::read(&report_path).unwrap())),"row_version":stored.row_version,
            "scope":"actual runtime recipe plus Core/SQLite; not global P026 completion or daemon validation route"}));
    }
    let journals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(journals, 2);
    std::fs::write(repo.join("docs/reports/P026_runs/native-runtime.json"),serde_json::to_vec_pretty(&json!({"status":"PASS",
        "scope":"full native reference runtime recipe, 17 real observations; actual Core/SQLite success, promotion gate and failed revalidation; public composition/execution admission pending",
        "outcomes":outcomes,"generated_at":timestamp(chrono::Utc::now())})).unwrap()).unwrap();
    db.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_python_por_host_protegido_invoca_y_cualifica_transporte_real() {
    let fixture = Fixture::new("protocol");
    let guard = fixture.start().await;
    let check:Result<(),Box<dyn std::error::Error>>=async {
        let ensure=|condition:bool| if condition { Ok(()) } else { Err(std::io::Error::other("oráculo protocol no cumplido")) };
        let endpoint=fixture.data["profile"]["endpoint_id"].clone();
        let all=Access { reads:vec![fixture.package.clone(),fixture.work.clone()], urls:vec![format!("{}/health",fixture.data["base_url"].as_str().unwrap()),format!("{}/echo",fixture.data["base_url"].as_str().unwrap())], ..Access::default() };
        let description=guard.call("protocol.describe",json!({"endpoint_id":endpoint}),&all,None,None).await?;
        ensure(description["capabilities"]==json!(["vendor.reference.echo"]) && description["streaming"]==false)?;
        let request=json!({"lease_id":fixture.data["profile"]["lease_id"],"request_id":uuid::Uuid::now_v7().to_string(),"capability":"vendor.reference.echo","input":{"text":"protected actual echo"},"generation":null,"stream":false});
        let denied=guard.call("protocol.invoke",request.clone(),&Access { urls:vec!["http://127.0.0.1:1/foreign".into()], ..Access::default() },None,None).await.err().ok_or_else(|| std::io::Error::other("origen ajeno no rechazado"))?;
        ensure(denied.details.reason=="network_denied")?;
        let denied=guard.call("protocol.invoke",request.clone(),&Access { secrets:vec!["larm/ungranted".into()], ..Access::default() },None,None).await.err().ok_or_else(|| std::io::Error::other("secreto no concedido no rechazado"))?;
        ensure(denied.details.reason=="secret_denied")?;
        let answer=guard.call("protocol.invoke",request,&all,None,None).await?;
        ensure(answer["output"]==json!({"echo":{"text":"protected actual echo"}}))?;
        ensure(answer["usage"]["tokens_input"].is_null() && answer["fingerprint"]["components"]["backend_identity"]["status"]=="UNAVAILABLE")?;
        let qualified=guard.call("protocol.qualify",json!({"endpoint_id":endpoint,"variant_id":fixture.data["profile"]["variant_id"],"suite_id":"reference.http.echo.v1","fixture_hash":fixture.data["entry"]["fixture_sha256"]}),&all,None,None).await?;
        ensure(qualified["qualified"]==true && qualified["fingerprint"]["components"]["scope"]["value"]=="controlled_reference_http_transport")?;
        Ok(())
    }.await;
    guard.stop().await;
    check.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transformation_python_por_host_protegido_convierte_y_verifica_bundle_canonico() {
    let fixture = Fixture::new("transformation");
    let guard = fixture.start().await;
    let check:Result<(),Box<dyn std::error::Error>>=async {
        let ensure=|condition:bool| if condition { Ok(()) } else { Err(std::io::Error::other("oráculo transformation no cumplido")) };
        let input=fixture.work.join("input/fixture.bin");
        let original=std::fs::read(&input)?;
        let access=Access { reads:vec![fixture.package.clone(),fixture.work.join("input"),fixture.work.join("profile"),fixture.work.join("state"),fixture.work.join("output")], writes:vec![fixture.work.join("state"),fixture.work.join("output")], ..Access::default() };
        let description=guard.call("transformation.describe",json!({}),&access,None,None).await?;
        ensure(description["formats_output"]==json!(["BASE64"]) && description["pause_supported"]==false)?;
        let request=json!({"input_variant_id":fixture.data["profile"]["input_variant_id"],"adapter_id":fixture.manifest.id,"tool_version":"1.0.0","target_format":"BASE64","parameters":{},"storage_location_id":fixture.data["profile"]["storage_location_id"],"hard_budget_bytes":null});
        let denied=guard.call("transformation.plan",request.clone(),&Access { writes:vec![input.clone()], ..Access::default() },None,None).await.err().ok_or_else(|| std::io::Error::other("escritura sobre input no rechazada"))?;
        ensure(denied.details.reason=="write_denied")?;
        let denied=guard.call("transformation.plan",request.clone(),&Access { urls:vec!["http://127.0.0.1:1/foreign".into()], ..Access::default() },None,None).await.err().ok_or_else(|| std::io::Error::other("red no rechazada"))?;
        ensure(denied.details.reason=="network_denied")?;
        let plan=guard.call("transformation.plan",request,&access,None,None).await?;
        ensure(plan["storage_estimate"]["per_volume"][0]["eligible"]==false)?;
        let receipt=guard.call("transformation.execute",json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"approval_id":null}),&access,None,None).await?;
        ensure(receipt["state"]=="COMPLETED" && receipt["status_url"].as_str().is_some_and(|s| s.starts_with("file:///")))?;
        let artifacts=guard.call("transformation.verify",json!({"job_id":receipt["job_id"]}),&access,None,None).await?;
        let entries:Vec<larm_domain::model::BundleEntry>=artifacts["artifacts"].as_array().ok_or_else(|| std::io::Error::other("artefactos ausentes"))?.iter().map(|item| -> Result<_,Box<dyn std::error::Error>> {
            let relative=item["relative_path"].as_str().ok_or_else(|| std::io::Error::other("path ausente"))?;
            let bytes=std::fs::read(fixture.work.join("output").join(relative))?;
            let expected=item["sha256"].as_str().ok_or_else(|| std::io::Error::other("SHA ausente"))?;
            ensure(hex::encode(Sha256::digest(&bytes))==expected)?;
            ensure(item["size_bytes"].as_str().unwrap().parse::<u64>()?==bytes.len() as u64)?;
            if item["required"]==true {
                use base64::Engine;
                ensure(base64::engine::general_purpose::STANDARD.decode(&bytes)?==original && bytes!=original)?;
            }
            Ok(larm_domain::model::BundleEntry { relative_path:relative.into(),sha256:expected.into(),size_bytes:bytes.len() as u64,required:item["required"].as_bool().unwrap() })
        }).collect::<Result<_,_>>()?;
        let expected=larm_domain::model::bundle_hash(&entries)?;
        ensure(artifacts["manifest_hash"]==expected && artifacts["revision_key"]==format!("content:{expected}"))?;
        ensure(std::fs::read(input)?==original)?;
        let options=sqlx::sqlite::SqliteConnectOptions::new().filename(fixture.work.join("state/transform.sqlite")).read_only(true);
        let pool=sqlx::SqlitePool::connect_with(options).await?;
        let held:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM ref_lock)+(SELECT count(*) FROM ref_reservation)").fetch_one(&pool).await?;
        let consistent:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM ref_event)=(SELECT count(*) FROM ref_outbox)").fetch_one(&pool).await?;
        pool.close().await;
        ensure(held==0 && consistent==1)?;
        Ok(())
    }.await;
    guard.stop().await;
    check.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn benchmark_python_por_host_protegido_mide_y_valida_estadisticas_reales() {
    let fixture = Fixture::new("benchmark");
    let guard = fixture.start().await;
    let check: Result<(), Box<dyn std::error::Error>> = async {
        let ensure = |condition: bool| {
            if condition {
                Ok(())
            } else {
                Err(std::io::Error::other("oráculo benchmark no cumplido"))
            }
        };
        let access = Access {
            reads: vec![
                fixture.package.clone(),
                fixture.work.join("profile"),
                fixture.work.join("state"),
                fixture.work.join("output"),
            ],
            writes: vec![fixture.work.join("state"), fixture.work.join("output")],
            urls: vec![format!(
                "{}/echo",
                fixture.data["base_url"].as_str().unwrap()
            )],
            ..Access::default()
        };
        let description = guard
            .call("benchmark.describe", json!({}), &access, None, None)
            .await?;
        ensure(description["capabilities"] == json!(["vendor.reference.echo"]))?;
        let request = fixture.data["request"].clone();
        let denied = guard
            .call(
                "benchmark.run",
                request.clone(),
                &Access {
                    writes: vec![fixture.package.clone()],
                    ..Access::default()
                },
                None,
                None,
            )
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("escritura en paquete no rechazada"))?;
        ensure(denied.details.reason == "write_denied")?;
        let denied = guard
            .call(
                "benchmark.run",
                request.clone(),
                &Access {
                    urls: vec!["http://127.0.0.1:1/foreign".into()],
                    ..Access::default()
                },
                None,
                None,
            )
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("origen ajeno no rechazado"))?;
        ensure(denied.details.reason == "network_denied")?;
        let report = guard
            .call("benchmark.run", request, &access, None, None)
            .await?;
        ensure(
            report["status"] == "PASS"
                && report["metrics"]["inference_qualification_eligible"] == false,
        )?;
        let raw = report["raw_samples"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("muestras ausentes"))?;
        ensure(
            raw.len() == 6
                && raw[0]["warmup"] == true
                && raw[1..].iter().all(|s| {
                    s["warmup"] == false
                        && s["success"] == true
                        && s["ttft_ms"].is_null()
                        && s["output_tokens"].is_null()
                }),
        )?;
        let mut timings = Vec::new();
        for sample in &raw[1..] {
            let start = sample["start_monotonic_ns"]
                .as_str()
                .unwrap()
                .parse::<u64>()?;
            let end = sample["end_monotonic_ns"]
                .as_str()
                .unwrap()
                .parse::<u64>()?;
            let elapsed = end
                .checked_sub(start)
                .filter(|n| *n > 0)
                .ok_or_else(|| std::io::Error::other("duración no positiva"))?;
            timings.push(elapsed as f64 / 1_000_000.);
        }
        let mean = timings.iter().sum::<f64>() / 5.;
        let deviation = (timings.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 4.).sqrt();
        let interval = 2.776 * deviation / 5_f64.sqrt();
        timings.sort_by(f64::total_cmp);
        let metrics = &report["metrics"]["latency_ms"];
        let near = |key: &str, value: f64| {
            metrics[key]
                .as_f64()
                .is_some_and(|v| (v - value).abs() <= 1e-9 * value.abs().max(1.))
        };
        ensure(
            near("mean", mean)
                && near("median", timings[2])
                && near("p95", timings[4])
                && near("sample_stddev", deviation),
        )?;
        ensure(
            (metrics["ci95_mean"]["lower"].as_f64().unwrap() - (mean - interval).max(0.)).abs()
                < 1e-8
                && (metrics["ci95_mean"]["upper"].as_f64().unwrap() - (mean + interval)).abs()
                    < 1e-8,
        )?;
        let components = report["fingerprint"]["components"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, item)| {
                (
                    name.clone(),
                    json!({"value":item["value"],"status":item["status"]}),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
        let expected = larm_domain::canonical::sha256_hex(&Value::Object(components))
            .map_err(std::io::Error::other)?;
        ensure(
            report["fingerprint"]["hash"] == expected
                && report["fingerprint"]["components"]["hardware_fingerprint"]["status"]
                    == "UNAVAILABLE",
        )?;
        let artifact = fixture
            .work
            .join("output")
            .join(report["benchmark_id"].as_str().unwrap())
            .join("observations.json");
        ensure(
            hex::encode(Sha256::digest(std::fs::read(artifact)?))
                == report["metrics"]["observation_artifact_sha256"]
                    .as_str()
                    .unwrap(),
        )?;
        let validation = guard
            .call("benchmark.validate", report, &access, None, None)
            .await?;
        ensure(validation == json!({"valid":true,"errors":[]}))?;
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("state/benchmark.sqlite"))
            .read_only(true);
        let pool = sqlx::SqlitePool::connect_with(options).await?;
        let held: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM ref_lock)+(SELECT count(*) FROM ref_reservation)",
        )
        .fetch_one(&pool)
        .await?;
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_sample")
            .fetch_one(&pool)
            .await?;
        let consistent: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM ref_event)=(SELECT count(*) FROM ref_outbox)",
        )
        .fetch_one(&pool)
        .await?;
        pool.close().await;
        ensure(held == 0 && rows == 6 && consistent == 1)?;
        Ok(())
    }
    .await;
    guard.stop().await;
    check.unwrap();
}
