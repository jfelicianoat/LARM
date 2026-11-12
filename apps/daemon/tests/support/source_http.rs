//! Producto P048 original por host protegido, perfil persistente, API y SQLite.
use super::*;
use larm_adapter_host::conformance::{
    profiles::{LocalProfile, Recipe},
    source_http::{HttpSourceConfig, HttpSourceSuite},
    NativeConformanceRunner,
};
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterQualification, ApprovalRepository,
    ConformanceProfileRepository,
};

pub(super) fn http_fixture() -> (PathBuf, Fixture) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let parent = repo.join("docs/reports/P048_runs");
    std::fs::create_dir_all(&parent).unwrap();
    let root = tempfile::Builder::new()
        .prefix(".native-http-")
        .tempdir_in(&parent)
        .unwrap();
    let mut helper = Command::new("python")
        .args(["-X", "utf8"])
        .arg(repo.join("tools/p048_http_fixture.py"))
        .arg(root.path())
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(helper.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(!line.is_empty(), "el servidor P048 no anunció sus recursos");
    let data: Value = serde_json::from_str(&line).unwrap();
    let package = PathBuf::from(data["package"].as_str().unwrap());
    let work = PathBuf::from(data["work"].as_str().unwrap());
    let manifest =
        AdapterManifest::parse(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let fixture = Fixture {
        _root: root,
        helper,
        data,
        package,
        work,
        manifest,
    };
    (repo, fixture)
}

#[tokio::test]
async fn p048_http_producto_cualifica_bloques_cancelacion_crash_y_persistencia() {
    let (repo, fixture) = http_fixture();
    let config: HttpSourceConfig = serde_json::from_value(fixture.data["config"].clone()).unwrap();
    let suite = HttpSourceSuite::new(config.clone()).unwrap();
    let (ctx, auth, token) = native_actor().await;
    let db = larm_db::pool::Db::open(&fixture._root.path().join("state.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let installations = larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    };
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {id:uuid::Uuid::now_v7().to_string(),created_at:timestamp(now),updated_at:timestamp(now),row_version:1,
        metadata:json!({"fixture":"owned HTTP server; original product P048 binary; installation prepared independently of P028 import"}).as_object().unwrap().clone(),
        adapter_id:fixture.manifest.id.clone(),version:fixture.manifest.version.clone(),family:"source".into(),package_hash:fixture.manifest.package_sha256.clone(),publisher:fixture.manifest.publisher.clone(),trust:AdapterTrust::Untrusted,state:AdapterState::Installed,manifest:serde_json::to_value(&fixture.manifest).unwrap()};
    installations.insert(&installation, &ctx).await.unwrap();
    let approval = ApprovalRequest {operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(15)),"secret_refs":fixture.manifest.permissions.secret_refs}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(15)).unwrap();
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .insert(&approval, &ctx)
    .await
    .unwrap();
    let roots = vec![("p048-work".into(), fixture.work.clone())];
    let profile = larm_domain::conformance_profiles::ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: installation.id.clone(),
        actor_id: ctx.actor_id.clone(),
        manifest_sha256: larm_domain::conformance::hash(&installation.manifest).unwrap(),
        configuration: serde_json::to_value(LocalProfile {
            schema_version: "1.0".into(),
            package_dir: fixture.package.clone(),
            work_dir: fixture.work.clone(),
            roots: roots.clone(),
            custom: false,
            capability_profile: None,
            immutable_inputs: vec![repo.join("tools/p048_http_fixture.py")],
            timeout_seconds: 150,
            suite: Recipe::SourceHttp(config.clone()),
        })
        .unwrap(),
        approval,
        created_at: timestamp(now),
    };
    larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .store_conformance_profile(&profile, None, &ctx)
    .await
    .unwrap();
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(Policy {
            trust: TrustStore::default(),
            roots: RootMap(roots),
            elevation: Arc::new(WhoAmI),
        }),
        Arc::new(probe_profiles::PersistentTargets {
            repo: larm_db::conformance_profiles::SqlConformanceProfiles {
                pool: db.pool.clone(),
            },
        }),
        HostConfig {
            rpc_timeout: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(15),
            ..HostConfig::default()
        },
    ));
    let (result, job) =
        native_public_probe(&db, runner.clone(), &installation.id, &ctx, auth, &token).await;
    // Conservar también los intentos fallidos: no reemplazar un FAIL por un PASS.
    let attempt = repo
        .join("docs/reports/P048_runs/native-http")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&attempt).unwrap();
    let reports = std::fs::read_dir(&config.evidence_root)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("source-http.json"))
        .filter(|p| p.is_file())
        .collect::<Vec<_>>();
    assert_eq!(reports.len(), 1);
    std::fs::copy(&reports[0], attempt.join("source-http.json")).unwrap();
    std::fs::copy(&config.requests, attempt.join("requests.ndjson")).unwrap();
    std::fs::write(
        attempt.join("probe-job.json"),
        serde_json::to_vec_pretty(&job).unwrap(),
    )
    .unwrap();
    let receipt = result
        .unwrap_or_else(|e| panic!("P048 no cualificado: {e:?}; informe {}", attempt.display()));
    let report_path = suite.report_path(&receipt.run_id);
    assert_eq!(report_path, reports[0]);
    assert!(report_path.is_file());
    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    let observations = report["observations"]["Ok"].as_array().unwrap();
    assert_eq!(observations.len(), 16);
    assert!(observations.iter().all(|o| o["passed"] == true));
    let stored = installations.get(&installation.id).await.unwrap().unwrap();
    let receipts = larm_db::conformance::SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    assert_eq!(
        receipts.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt.clone())
    );
    let (_, run) = receipts
        .current_for_consumer(&installation.id)
        .await
        .unwrap()
        .unwrap();
    let before = std::fs::read(&config.requests).unwrap();
    runner
        .verify_qualification_snapshot(&stored, &run, &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&config.requests).unwrap(),
        before,
        "la admisión física no ejecuta RPC"
    );
    let owner = runner.acquire(&stored, &run, &ctx).await.unwrap();
    drop(owner);
    let summary = json!({"status":"PASS","scope":"original P048 HTTP executable, controlled loopback server, actual Windows P022 host, persistent profile, authenticated Axum/Core/SQLite probe and physical gate; no daemon TCP download/CAS, external upstream, GUI or global REAL_PASS",
        "generated_at":timestamp(chrono::Utc::now()),"receipt":receipt,"installation_state":stored.state,"observations":16,"family":"source","source_bytes":std::fs::metadata(&config.expected_file).unwrap().len().to_string(),"source_sha256":hex::encode(Sha256::digest(std::fs::read(&config.expected_file).unwrap())),"report":attempt.join("source-http.json").strip_prefix(&repo).unwrap(),"report_sha256":hex::encode(Sha256::digest(std::fs::read(attempt.join("source-http.json")).unwrap())),"physical_gate":true,"admission_did_not_execute_rpc":true,
        "host":std::env::var("COMPUTERNAME").unwrap_or_else(|_|std::env::consts::OS.into()),
        "native_test_executable_sha256":hex::encode(Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())),
        "daemon_executable_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),
        "product_executable_sha256":hex::encode(Sha256::digest(std::fs::read(fixture.package.join(&fixture.manifest.executable.relative_path)).unwrap()))});
    std::fs::write(
        attempt.join("result.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/reports/P048_runs/native-http.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    runner.shutdown().await.unwrap();
    db.pool.close().await;
}
