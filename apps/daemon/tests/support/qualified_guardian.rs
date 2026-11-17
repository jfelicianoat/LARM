//! Actual daemon P026 -> persisted qualification -> detached typed worker RPC.
//! Includes real Core registered-owner recovery under an explicit test driver;
//! public transformation producer/consumer and complete H07 remain unaccepted.
use super::*;
use base64::Engine;
use larm_adapter_host::{
    conformance::{
        gguf::{GgufConfig, GgufTransformationSuite},
        onnx::{OnnxConfig, OnnxTransformationSuite},
        onnx_quantize::{OnnxQuantizeConfig, OnnxQuantizeSuite},
        profiles::Recipe,
        transformation::{ReferenceTransformationSuite, TransformationConfig},
        NativeSuite,
    },
    guardian::{GuardianBootstrap, GuardianTransformationClient, NativeGuardian},
    transformation::SessionIdentity,
};
use larm_adapter_sdk::permissions::RootMap;
use larm_domain::{
    conformance::Run,
    conformance_profiles::ConformanceProfile,
    ports::ConformanceProfileRepository,
    transformation_execution::TransformationAttemptFence,
    transformations::{ExecuteTransformationPlan, TransformationRequest},
    units::DecimalU64,
};
use std::{io::Write, os::windows::process::CommandExt, time::Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
#[path = "public_cancel.rs"]
mod public_cancel;
#[path = "public_transformations.rs"]
mod public_case;
#[path = "public_gguf.rs"]
mod public_gguf;
#[path = "public_gguf_quant.rs"]
mod public_gguf_quant;
#[path = "public_h07.rs"]
mod public_h07;
#[path = "public_onnx.rs"]
mod public_onnx;
#[path = "public_onnxq.rs"]
mod public_onnxq;
#[path = "public_witness_lost.rs"]
mod public_witness_lost;
#[path = "registered_guardian.rs"]
mod registered_case;

struct Parent {
    child: Child,
    helper: System,
    pid: Pid,
}
impl Drop for Parent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(process) = self.helper.process(self.pid) {
            let _ = process.kill();
        }
    }
}
impl Parent {
    async fn start(bootstrap: &GuardianBootstrap, repo: &Path, output: &Path) -> Self {
        let log = std::fs::File::create(output.join("guardian.log")).unwrap();
        let mut child = Command::new(repo.join(".venv-tools/Scripts/python.exe"))
            .args(["-X", "utf8"])
            .arg(repo.join("apps/daemon/tests/support/guardian_owner.py"))
            .arg(env!("CARGO_BIN_EXE_larm-daemon"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let mut bytes = Vec::new();
        bootstrap.write_to(&mut bytes).await.unwrap();
        child.stdin.as_mut().unwrap().write_all(&bytes).unwrap();
        child.stdin.as_mut().unwrap().flush().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        let pid = Pid::from_u32(reply["helper_pid"].as_u64().unwrap() as u32);
        let mut helper = System::new();
        helper.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        assert!(helper.process(pid).is_some());
        Self { child, helper, pid }
    }
    fn crash(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    async fn client(
        &mut self,
        bootstrap: &GuardianBootstrap,
        output: &Path,
    ) -> GuardianTransformationClient {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(client) = bootstrap.transformation_client().await {
                return client;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "guardian owner failed: {}",
                std::fs::read_to_string(output.join("guardian.log")).unwrap_or_default()
            );
            assert!(
                Instant::now() < deadline,
                "qualified guardian did not attach"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM model_variant),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)")
        .fetch_one(&db.pool).await.unwrap()
}

async fn raw_rpc(
    descriptor: &larm_adapter_host::guardian::GuardianDescriptor,
    bytes: &[u8],
) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(descriptor.pipe_name())
            .unwrap();
        pipe.write_u32_le(bytes.len() as u32).await.unwrap();
        pipe.write_all(bytes).await.unwrap();
        let size = pipe.read_u32_le().await.unwrap() as usize;
        assert!((1..=512 * 1024).contains(&size));
        let mut reply = vec![0; size];
        pipe.read_exact(&mut reply).await.unwrap();
        serde_json::from_slice(&reply).unwrap()
    })
    .await
    .unwrap()
}

/// Raíces del conversor GGUF de producto: los nombres declarados en su
/// manifest (product-package/product-gguf-*) mapeados a los directorios del
/// fixture. La fixture de referencia aporta los suyos vía fixture.policy().
fn gguf_roots(fixture: &super::Fixture) -> RootMap {
    RootMap(vec![
        ("product-package".into(), fixture.package.clone()),
        ("product-gguf-input".into(), fixture.work.join("input")),
        ("product-gguf-profile".into(), fixture.work.join("profile")),
        ("product-gguf-state".into(), fixture.work.join("state")),
        ("product-gguf-output".into(), fixture.work.join("output")),
    ])
}

/// Raíces del conversor ONNX de producto: los nombres declarados en su
/// manifest (product-onnx-package/product-onnx-*) mapeados a los directorios
/// del fixture.
fn onnx_roots(fixture: &super::Fixture) -> RootMap {
    RootMap(vec![
        ("product-onnx-package".into(), fixture.package.clone()),
        ("product-onnx-input".into(), fixture.work.join("input")),
        ("product-onnx-profile".into(), fixture.work.join("profile")),
        ("product-onnx-state".into(), fixture.work.join("state")),
        ("product-onnx-output".into(), fixture.work.join("output")),
    ])
}

/// Raíces del cuantizador ONNX de producto: su manifest declara los nombres
/// reference-* (derivado del patrón de referencia) mapeados a los directorios
/// del fixture.
fn onnxq_roots(fixture: &super::Fixture) -> RootMap {
    RootMap(vec![
        ("product-package".into(), fixture.package.clone()),
        ("product-onnxq-input".into(), fixture.work.join("input")),
        ("product-onnxq-profile".into(), fixture.work.join("profile")),
        ("product-onnxq-state".into(), fixture.work.join("state")),
        ("product-onnxq-output".into(), fixture.work.join("output")),
    ])
}

#[allow(clippy::too_many_arguments)] // selector de escenario de aceptación
pub(super) async fn exercise(
    public: bool,
    cancel: bool,
    h07: bool,
    witness_lost: bool,
    gguf: bool,
    onnx: bool,
    quant: bool,
    onnxq: bool,
) {
    let fixture = Fixture::new(if onnx {
        "onnx-conformance"
    } else if onnxq {
        "onnxq-conformance"
    } else if gguf || quant {
        "gguf-conformance"
    } else {
        "transformation-conformance"
    });
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let output = repo
        .join("docs/reports/P093_runs/qualified-guardian")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&output).unwrap();
    let data = fixture._root.path().join("guardian-data");
    let dbpath = data.join("larm.sqlite");
    let db = larm_db::pool::Db::open(&dbpath).await.unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let settings = data.join("config.json");
    download_restart::live_config(&settings);
    if public {
        // La composición síncrona del plan lanza el worker de preview y
        // revalida la guardia física viva antes y después de cada RPC; esa
        // cadena supera la ventana de descargas del fixture compartido, así
        // que el operador de esta prueba configura una mayor para esta ruta.
        let mut value: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        // La cuantización K del conversor real supera los 180 s en el plan
        // preview síncrono (Python stdlib): el timeout es un detector de
        // cuelgue, no un presupuesto de rendimiento.
        value["api"]["request_timeout_seconds"] = json!(300);
        std::fs::write(&settings, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut principal = if cancel {
        download_restart::TemporaryPrincipal::enroll_canceller(&data, &settings)
    } else {
        download_restart::TemporaryPrincipal::enroll(&data, &settings)
    };
    // Public-case job principal: enrolled before the daemon serves its first
    // authenticated request so the initial principal cache load already
    // contains it (5s TTL without mid-flight reload).
    let job_principal = public.then(|| {
        if cancel || gguf || onnx || quant || onnxq {
            download_restart::TemporaryPrincipal::enroll_canceller(&data, &settings)
        } else {
            download_restart::TemporaryPrincipal::enroll(&data, &settings)
        }
    });
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let forbidden = fixture._root.path().join("forbidden.bin");
    std::fs::write(&forbidden, b"outside guardian grant").unwrap();
    // El suite/config de cualificación dependen del conversor: las recetas
    // de producto (GGUF/ONNX) usan su suite con oráculo en Rust.
    let gguf_config = (gguf || quant).then(|| GgufConfig {
        profile: fixture.data["profile"].clone(),
        expected_input: fixture.data["expected_input"].as_str().unwrap().into(),
        forbidden: forbidden.clone(),
        evidence_root: output.join("p026"),
    });
    let onnx_config = onnx.then(|| OnnxConfig {
        profile: fixture.data["profile"].clone(),
        expected_input: fixture.data["expected_input"].as_str().unwrap().into(),
        forbidden: forbidden.clone(),
        evidence_root: output.join("p026"),
    });
    let onnxq_config = onnxq.then(|| OnnxQuantizeConfig {
        profile: fixture.data["profile"].clone(),
        expected_input: fixture.data["expected_input"].as_str().unwrap().into(),
        forbidden: forbidden.clone(),
        evidence_root: output.join("p026"),
        olive_runtime: fixture.data["olive_runtime"].as_str().unwrap().into(),
    });
    let reference_config = (!gguf && !onnx && !quant && !onnxq).then(|| TransformationConfig {
        profile: fixture.data["profile"].clone(),
        expected_input: fixture.data["expected_input"].as_str().unwrap().into(),
        forbidden: forbidden.clone(),
        evidence_root: output.join("p026"),
    });
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: fixture.manifest.id.clone(),
        version: fixture.manifest.version.clone(),
        family: "transformation".into(),
        package_hash: fixture.manifest.package_sha256.clone(),
        publisher: fixture.manifest.publisher.clone(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: serde_json::to_value(&fixture.manifest).unwrap(),
    };
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&installation, &ctx)
    .await
    .unwrap();
    let approval=ApprovalRequest {operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        // La sesión completa (seis formatos de cuantización, tres de ellos
        // K, en Python stdlib sobre una máquina degradada) supera la hora:
        // el TTL de la aprobación de prueba debe cubrir toda la sesión.
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(120)),"secret_refs":[]}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(120)).unwrap();
    let suite: Arc<dyn NativeSuite> = if let Some(config) = &onnxq_config {
        Arc::new(OnnxQuantizeSuite::new(config.clone()).unwrap())
    } else if let Some(config) = &onnx_config {
        Arc::new(OnnxTransformationSuite::new(config.clone()).unwrap())
    } else if let Some(config) = &gguf_config {
        Arc::new(GgufTransformationSuite::new(config.clone()).unwrap())
    } else {
        Arc::new(ReferenceTransformationSuite::new(reference_config.clone().unwrap()).unwrap())
    };
    let recipe = if let Some(config) = &onnxq_config {
        Recipe::OnnxQuantize(config.clone())
    } else if let Some(config) = &onnx_config {
        Recipe::Onnx(config.clone())
    } else if let Some(config) = &gguf_config {
        Recipe::Gguf(config.clone())
    } else {
        Recipe::Transformation(reference_config.clone().unwrap())
    };
    let roots = if onnx {
        onnx_roots(&fixture)
    } else if onnxq {
        onnxq_roots(&fixture)
    } else if gguf || quant {
        gguf_roots(&fixture)
    } else {
        fixture.policy().roots
    };
    let target = NativeReferenceTarget {
        spec: WorkerSpec {
            package_dir: fixture.package.clone(),
            manifest: fixture.manifest.clone(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        grant: TrustedOnlyGrant::from_approval(&approval, &fixture.manifest, &ctx.actor_id, now)
            .unwrap(),
        inputs: vec![repo.join("tools/p025_host_fixture.py")],
        suite,
        capability_profile: if gguf || onnx || quant || onnxq {
            Some("product-capabilities.json".into())
        } else {
            Some("reference-capabilities.json".into())
        },
    };
    persistent_recipe(&db, &ctx, &installation, &approval, target, recipe, roots.0).await;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut daemon =
        daemon_process::RunningDaemon::start(&data, &settings, port, output.join("daemon.log"));
    daemon.wait_live(port, Duration::from_secs(45)).unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let (status, receipt) = download_restart::post_json(
        &http,
        port,
        &principal,
        &format!("/api/v1/adapters/{}/probe", installation.id),
        &uuid::Uuid::now_v7().to_string(),
        &json!({"resource_id":null,"profile_id":null}),
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "probe: {receipt}");
    // La cualificación con los conversores de producto reales (cinco
    // formatos de cuantización, dos de ellos K, en Python stdlib) supera los
    // 20 minutos de suite completa: el deadline debe cubrir el perfil.
    let deadline = Instant::now() + Duration::from_secs(2400);
    let qualified = loop {
        let reply = http
            .get(format!(
                "http://127.0.0.1:{port}{}",
                receipt["status_url"].as_str().unwrap()
            ))
            .bearer_auth(&principal.token)
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), reqwest::StatusCode::OK);
        let job: Value = reply.json().await.unwrap();
        if job["state"] == "COMPLETED" {
            break job;
        }
        if job["state"] == "FAILED" {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT id,payload FROM operation_journal WHERE kind='adapter.probe' ORDER BY rowid DESC LIMIT 2",
            )
            .fetch_all(&db.pool)
            .await
            .unwrap_or_default();
            std::fs::write(
                output.join("probe-failure-journal.txt"),
                format!(
                    "{rows:?}

job={job}"
                ),
            )
            .unwrap();
            panic!("native P026 failed; ver probe-failure-journal.txt");
        }
        assert!(Instant::now() < deadline, "native P026 timeout");
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let (stored, run) = larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    }
    .current_for_consumer(&installation.id)
    .await
    .unwrap()
    .unwrap();
    let profile = larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .conformance_profile(&installation.id)
    .await
    .unwrap()
    .unwrap();
    let identity = SessionIdentity {
        adapter_id: stored.adapter_id.clone(),
        installation_id: stored.id.clone(),
        run_id: run.id.clone(),
        run_sha256: run.digest().unwrap(),
        profile_sha256: larm_domain::conformance::hash(&profile).unwrap(),
    };
    if public {
        let inputs = public_case::Inputs {
            backend_principal: principal,
            daemon,
            db,
            context: ctx,
            repo: repo.clone(),
            output: output.clone(),
            data,
            settings,
            port,
            installation: stored,
            run,
            profile,
            fixture,
        };
        let job_principal = job_principal.expect("public case job principal");
        let result = if quant {
            public_gguf_quant::start(inputs, job_principal)
        } else if onnx {
            public_onnx::start(inputs, job_principal)
        } else if onnxq {
            public_onnxq::start(inputs, job_principal)
        } else if gguf {
            public_gguf::start(inputs, job_principal)
        } else if witness_lost {
            public_witness_lost::start(inputs, job_principal)
        } else if h07 {
            public_h07::start(inputs, job_principal)
        } else if cancel {
            public_cancel::start(inputs, job_principal)
        } else {
            public_case::start(inputs, job_principal)
        }
        .await
        .unwrap();
        let synopsis_path = if quant {
            "docs/reports/P093_runs/native-public-gguf-quant.json"
        } else if onnx {
            "docs/reports/P093_runs/native-public-onnx.json"
        } else if onnxq {
            "docs/reports/P093_runs/native-public-onnxq.json"
        } else if gguf {
            "docs/reports/P093_runs/native-public-gguf.json"
        } else if witness_lost {
            "docs/reports/P093_runs/native-public-witness-lost.json"
        } else if h07 {
            "docs/reports/P093_runs/native-public-h07.json"
        } else if cancel {
            "docs/reports/P093_runs/native-public-cancel.json"
        } else {
            "docs/reports/P093_runs/native-public-transformation.json"
        };
        let synopsis = json!({"status":"PASS","generated_at":timestamp(chrono::Utc::now()),"scope":"public Core HTTP plan/admission/restart/native consumer/publication with actual P026 qualification; seeded input catalogue only, controlled reference conversion, no GGUF/runtime/UI acceptance","evidence":output.strip_prefix(&repo).unwrap(),"report":result});
        std::fs::write(
            repo.join(synopsis_path),
            serde_json::to_vec_pretty(&synopsis).unwrap(),
        )
        .unwrap();
        return;
    }
    let fence = TransformationAttemptFence {
        job_id: uuid::Uuid::now_v7().to_string(),
        claim_id: uuid::Uuid::now_v7().to_string(),
        generation: 0,
        attempt: 1,
        plan_hash: "c".repeat(64),
    };
    let bootstrap =
        GuardianBootstrap::transformation(dbpath, identity.clone(), ctx.clone(), fence).unwrap();
    std::fs::write(
        output.join("descriptor.json"),
        serde_json::to_vec_pretty(bootstrap.descriptor()).unwrap(),
    )
    .unwrap();
    let mut parent = Parent::start(&bootstrap, &repo, &output).await;
    let client = Arc::new(parent.client(&bootstrap, &output).await);
    // The valid worker already holds its native private-state lock. If the
    // wrong identity reached handshake it would fail on that lock instead.
    let mut wrong = identity.clone();
    wrong.run_sha256 = "0".repeat(64);
    let rejected = GuardianBootstrap::transformation(
        data.join("larm.sqlite"),
        wrong,
        ctx.clone(),
        bootstrap.descriptor().fence.clone(),
    )
    .unwrap();
    let mut invalid = tokio::process::Command::new(env!("CARGO_BIN_EXE_larm-daemon"));
    invalid
        .arg("worker-guardian")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000)
        .kill_on_drop(true);
    let mut invalid = invalid.spawn().unwrap();
    rejected
        .write_to(invalid.stdin.as_mut().unwrap())
        .await
        .unwrap();
    drop(invalid.stdin.take());
    let rejected = tokio::time::timeout(Duration::from_secs(60), invalid.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("transformation_guardian_identity_changed"));
    std::fs::write(output.join("wrong-identity.log"), &rejected.stderr).unwrap();
    let original = std::fs::read(fixture.work.join("input/fixture.bin")).unwrap();
    std::fs::write(output.join("input.bin"), &original).unwrap();
    assert!(std::fs::write(fixture.work.join("input/fixture.bin"), b"pinned").is_err());
    let before = counts(&db).await;
    let access = Access {
        reads: ["input", "profile", "state", "output"]
            .into_iter()
            .map(|p| fixture.work.join(p))
            .collect(),
        writes: ["state", "output"]
            .into_iter()
            .map(|p| fixture.work.join(p))
            .collect(),
        ..Access::default()
    };
    let id = || uuid::Uuid::now_v7().to_string();
    let description = client.describe(&id(), &access, None).await.unwrap();
    let request = TransformationRequest {
        input_variant_id: fixture.data["profile"]["input_variant_id"]
            .as_str()
            .unwrap()
            .into(),
        adapter_id: stored.adapter_id.clone(),
        tool_version: description.tool_version,
        target_format: "BASE64".into(),
        parameters: json!({}),
        storage_location_id: fixture.data["profile"]["storage_location_id"]
            .as_str()
            .unwrap()
            .into(),
        hard_budget_bytes: DecimalU64::new(1 << 30),
    };
    // A public descriptor can reconnect for recovery, but cannot create even
    // a private worker plan. Send a well-formed, unsigned RPC and measure the
    // actual private ledger before/after; no execution key is put on disk.
    use sqlx::Connection;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut ledger = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("state/transform.sqlite"))
            .read_only(true),
    )
    .await
    .unwrap();
    let plans_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_plan")
        .fetch_one(&mut ledger)
        .await
        .unwrap();
    let raw_rpc = json!({"id":id(),"operation":{"method":"Plan","params":request},
        "access":{"reads":access.reads,"writes":access.writes,"urls":[],"processes":[],"secrets":[]},"timeout_ms":null});
    serde_json::from_value::<larm_adapter_host::guardian::TransformationRpc>(raw_rpc.clone())
        .unwrap();
    let raw = json!({"descriptor":bootstrap.descriptor(),"nonce":uuid::Uuid::new_v4().to_string(),"action":{"RPC":raw_rpc}});
    let bytes = serde_json::to_vec(&raw).unwrap();
    let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(bootstrap.descriptor().pipe_name())
        .unwrap();
    pipe.write_u32_le(bytes.len() as u32).await.unwrap();
    pipe.write_all(&bytes).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pipe.read_u32_le())
            .await
            .unwrap()
            .is_err()
    );
    drop(pipe);
    let plans_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_plan")
        .fetch_one(&mut ledger)
        .await
        .unwrap();
    assert_eq!(plans_before, plans_after);
    // Build an independent wire request in memory. A bad signature must not
    // consume its nonce; the valid frame then creates exactly one plan, and
    // replaying its identical signed bytes must not create another.
    #[derive(serde::Serialize)]
    enum WireAction {
        #[serde(rename = "RPC")]
        Rpc(larm_adapter_host::guardian::TransformationRpc),
    }
    let rpc =
        serde_json::from_value::<larm_adapter_host::guardian::TransformationRpc>(raw_rpc).unwrap();
    let action = WireAction::Rpc(rpc);
    let nonce = uuid::Uuid::new_v4().to_string();
    let secret: [u8; 32] =
        serde_json::from_value(serde_json::to_value(&bootstrap).unwrap()["signing_seed"].clone())
            .unwrap();
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&secret);
    let mut control = b"larm-native-guardian-control-v1\0".to_vec();
    control.extend(serde_json::to_vec(&(bootstrap.descriptor(), &nonce, &action)).unwrap());
    let signature = hex::encode(key.sign(&control).to_bytes());
    let signed = json!({"descriptor":bootstrap.descriptor(),"nonce":nonce,"action":action,"control_signature":signature});
    let signed_bytes = serde_json::to_vec(&signed).unwrap();
    let mut bad = signed.clone();
    bad["control_signature"] = json!("0".repeat(128));
    let bad_bytes = serde_json::to_vec(&bad).unwrap();
    let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(bootstrap.descriptor().pipe_name())
        .unwrap();
    pipe.write_u32_le(bad_bytes.len() as u32).await.unwrap();
    pipe.write_all(&bad_bytes).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pipe.read_u32_le())
            .await
            .unwrap()
            .is_err()
    );
    drop(pipe);
    let accepted = self::raw_rpc(bootstrap.descriptor(), &signed_bytes).await;
    assert!(accepted["observation"]["result"]["Ok"]["plan_id"].is_string());
    let plans_once: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_plan")
        .fetch_one(&mut ledger)
        .await
        .unwrap();
    assert_eq!(plans_once, plans_after + 1);
    let replayed = self::raw_rpc(bootstrap.descriptor(), &signed_bytes).await;
    assert_eq!(
        replayed["observation"]["result"]["Err"]["details"]["reason"],
        "guardian_control_replayed"
    );
    let plans_replayed: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_plan")
        .fetch_one(&mut ledger)
        .await
        .unwrap();
    assert_eq!(plans_replayed, plans_once);
    let denied = client
        .plan(
            &id(),
            &request,
            &Access {
                reads: vec![forbidden],
                ..Access::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.details.reason, "read_denied");
    let plan = client.plan(&id(), &request, &access, None).await.unwrap();
    let binding = client.execution_binding(&plan).unwrap();
    assert_eq!(binding.profile_sha256, identity.profile_sha256);
    let execute = ExecuteTransformationPlan {
        plan_id: plan.plan_id,
        plan_hash: plan.plan_hash,
        approval_id: None,
    };
    let job = client
        .execute(&id(), &execute, &access, None)
        .await
        .unwrap();
    let manifest = client
        .verify(&id(), &job.job_id, &access, None)
        .await
        .unwrap();
    let mut artifacts = Vec::new();
    for artifact in &manifest.artifacts {
        let bytes =
            std::fs::read(fixture.work.join("output").join(&artifact.relative_path)).unwrap();
        assert_eq!(
            artifact.sha256.as_deref(),
            Some(hex::encode(Sha256::digest(&bytes)).as_str())
        );
        if artifact.role == "DATA" {
            assert_eq!(
                bytes,
                base64::engine::general_purpose::STANDARD
                    .encode(&original)
                    .as_bytes()
            );
        }
        let path = output.join(format!("artifact-{}", artifacts.len()));
        std::fs::write(&path, &bytes).unwrap();
        artifacts.push(json!({"file":path.strip_prefix(&repo).unwrap().to_string_lossy().replace('\\',"/"),"size_bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes)),"descriptor":artifact}));
    }
    assert_eq!(artifacts.len(), 2);
    assert_eq!(before, counts(&db).await);
    // A persisted profile change is checked afresh, even while all physical
    // input/package pins retained by the old qualification are still valid.
    let mut drift = profile.clone();
    drift.configuration["timeout_seconds"] = json!(119);
    sqlx::query("UPDATE operation_journal SET payload=?,plan_hash=? WHERE id=?")
        .bind(serde_json::to_string(&drift).unwrap())
        .bind(larm_domain::conformance::hash(&drift).unwrap())
        .bind(&profile.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(client.describe(&id(), &access, None).await.is_err());
    sqlx::query("UPDATE operation_journal SET payload=?,plan_hash=? WHERE id=?")
        .bind(serde_json::to_string(&profile).unwrap())
        .bind(larm_domain::conformance::hash(&profile).unwrap())
        .bind(&profile.id)
        .execute(&db.pool)
        .await
        .unwrap();
    client.describe(&id(), &access, None).await.unwrap();
    // Hold the owned private ledger writer. The helper accepts an actual RPC
    // while another connection remains able to inspect/stop the physical owner.
    let mut lock = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("state/transform.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut lock)
        .await
        .unwrap();
    let running = client.clone();
    let a = access.clone();
    let pending = tokio::spawn(async move {
        running
            .execute(&uuid::Uuid::now_v7().to_string(), &execute, &a, None)
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while client.owner().active_rpcs().await.unwrap() == 0 {
        assert!(Instant::now() < deadline && !pending.is_finished());
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    parent.crash();
    let recovered = NativeGuardian::attach(
        serde_json::from_slice(&std::fs::read(output.join("descriptor.json")).unwrap()).unwrap(),
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while recovered.require_closed().await.is_err() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(recovered.active_rpcs().await.unwrap(), 0);
    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(client.describe(&id(), &access, None).await.is_err());
    assert_eq!(before, counts(&db).await);
    assert!(std::fs::write(
        fixture.work.join("input/fixture.bin"),
        b"pins held until release"
    )
    .is_err());
    drop(client);
    drop(bootstrap);
    recovered.release().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::write(fixture.work.join("input/fixture.bin"), &original).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let private_counts_after = counts(&db).await;
    let registered_case = registered_case::exercise(registered_case::Inputs {
        db: &db,
        fixture: &fixture,
        request: &request,
        identity: &identity,
        context: &ctx,
        repo: &repo,
        output: &output,
        data: &data,
        settings: &settings,
        port,
        principal: &principal,
        daemon: &mut daemon,
    })
    .await;
    daemon.stop();
    assert!(principal.revoke());
    let report = json!({"status":"PASS","scope":"actual daemon HTTP P026, qualified detached typed RPC/test-owner crash, then durable Core preparation and automatic daemon recovery of registered and pre-registration helpers, with separate actors; test-driver plan mapping; public transformation producer/consumer and full H07 pending",
        "qualified_probe":qualified,"run":run,"profile":profile,"installation":stored,"identity":identity,"execution_binding":binding,"manifest":manifest,"artifacts":artifacts,
        "core_counts_before":before,"core_counts_after":private_counts_after,"input_sha256":hex::encode(Sha256::digest(&original)),
        "registered_owner_case":registered_case,
        "unsigned_rpc_denied_without_plan":true,"private_plan_counts_around_unsigned_rpc":[plans_before,plans_after],
        "wrong_identity_rejected_before_handshake":true,"bad_signature_does_not_consume_nonce":true,
        "signed_rpc_replay_denied":true,"private_plan_counts_around_signed_replay":[plans_after,plans_once,plans_replayed],
        "declared_access_denied":true,"persisted_profile_drift_denied":true,"inflight_rpc_observed_before_owner_crash":true,
        "recovered_native_closure":true,"rpc_after_closure_denied":true,"input_pins_retained_until_release":true,
        "rpc_quiescent_before_closed":true,
        "principal_revoked":true,"private_case_created_core_resources":false,"generated_at":timestamp(chrono::Utc::now())});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(repo.join("docs/reports/P093_runs/native-qualified-guardian.json"),serde_json::to_vec_pretty(&json!({"status":"PASS","generated_at":timestamp(chrono::Utc::now()),"evidence":output.strip_prefix(&repo).unwrap().to_string_lossy().replace('\\',"/"),"public_p093_accepted":false})).unwrap()).unwrap();
}
