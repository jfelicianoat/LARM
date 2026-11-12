//! P048 original por TCP: cualificación del daemon, plan sellado y CAS real.
use super::daemon_process::RunningDaemon;
use super::download_restart::{
    create_location, job_snapshot, live_config, post_json, set_bandwidth, wait_state,
    TemporaryPrincipal,
};
use super::*;
use larm_adapter_host::conformance::{
    profiles::{LocalProfile, Recipe},
    source_http::HttpSourceConfig,
};
use larm_domain::download::{DownloadPlanRequest, DownloadPlanResponse, ExecutePlan};
use larm_domain::ports::{ApprovalRepository, ConformanceProfileRepository};
use larm_domain::units::DecimalU64;
use std::time::Instant;

struct TraceArchive {
    output: PathBuf,
    data_root: PathBuf,
    requests: PathBuf,
}
impl Drop for TraceArchive {
    fn drop(&mut self) {
        for name in [
            "p048-first.log",
            "p048-paused-restart.log",
            "p048-running-restart.log",
        ] {
            let path = self.data_root.join(name);
            if path.is_file() {
                let _ = std::fs::copy(path, self.output.join(name));
            }
        }
        if self.requests.is_file() {
            let _ = std::fs::copy(&self.requests, self.output.join("requests.ndjson"));
        }
    }
}

async fn wait_terminal(
    db: &larm_db::pool::Db,
    job: &str,
    expected: &str,
    output: &Path,
) -> (String, i64, Value, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let snapshot = job_snapshot(db, job).await;
        if snapshot.0 == expected {
            return snapshot;
        }
        assert!(
            !matches!(snapshot.0.as_str(), "FAILED" | "COMPLETED" | "CANCELLED"),
            "estado terminal inesperado: {snapshot:?}"
        );
        if Instant::now() >= deadline {
            std::fs::write(output.join("wait-timeout.json"),serde_json::to_vec_pretty(&json!({"job_id":job,"expected":expected,"state":snapshot.0,"attempt":snapshot.1,"progress":snapshot.2,"error":snapshot.3})).unwrap()).unwrap();
            panic!(
                "sin terminar la descarga en 180 s; diagnóstico {}",
                output.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn register_profile(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    cfg: &HttpSourceConfig,
    ctx: &MutationContext,
    repo: &Path,
) -> String {
    let now = chrono::Utc::now();
    let installation = AdapterInstallation {id:uuid::Uuid::now_v7().to_string(),created_at:timestamp(now),updated_at:timestamp(now),row_version:1,
        metadata:json!({"fixture":"original P048 HTTP package; initial candidate prepared independently of P028 import"}).as_object().unwrap().clone(),
        adapter_id:fixture.manifest.id.clone(),version:fixture.manifest.version.clone(),family:"source".into(),package_hash:fixture.manifest.package_sha256.clone(),publisher:fixture.manifest.publisher.clone(),trust:AdapterTrust::Untrusted,state:AdapterState::Installed,manifest:serde_json::to_value(&fixture.manifest).unwrap()};
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&installation, ctx)
    .await
    .unwrap();
    let approval = ApprovalRequest {operation_digest:execution_digest(&fixture.manifest).unwrap(),resource_ids:vec![adapter_resource(&fixture.manifest)],code_hash:Some(fixture.manifest.package_sha256.clone()),approved:true,
        permissions:json!({"publisher":fixture.manifest.publisher,"permissions":fixture.manifest.permissions,"expires_at":timestamp(now+chrono::Duration::minutes(15)),"secret_refs":fixture.manifest.permissions.secret_refs}).as_object().unwrap().clone()}
        .grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(15)).unwrap();
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .insert(&approval, ctx)
    .await
    .unwrap();
    let profile = larm_domain::conformance_profiles::ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: installation.id.clone(),
        actor_id: ctx.actor_id.clone(),
        manifest_sha256: larm_domain::conformance::hash(&installation.manifest).unwrap(),
        configuration: serde_json::to_value(LocalProfile {
            schema_version: "1.0".into(),
            package_dir: fixture.package.clone(),
            work_dir: fixture.work.clone(),
            roots: vec![("p048-work".into(), fixture.work.clone())],
            custom: false,
            capability_profile: None,
            immutable_inputs: vec![repo.join("tools/p048_http_fixture.py")],
            timeout_seconds: 150,
            suite: Recipe::SourceHttp(cfg.clone()),
        })
        .unwrap(),
        approval,
        created_at: timestamp(now),
    };
    larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .store_conformance_profile(&profile, None, ctx)
    .await
    .unwrap();
    installation.id
}

pub(super) async fn qualify(
    port: u16,
    principal: &TemporaryPrincipal,
    installation: &str,
    ctx: &MutationContext,
    fixture: &Fixture,
    cfg: &HttpSourceConfig,
) -> Value {
    let client = reqwest::Client::new();
    let (status, receipt) = post_json(
        &client,
        port,
        principal,
        &format!("/api/v1/adapters/{installation}/probe"),
        &uuid::Uuid::now_v7().to_string(),
        &json!({"resource_id":null,"profile_id":null}),
        ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "probe: {receipt}");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let response = client
            .get(format!(
                "http://127.0.0.1:{port}{}",
                receipt["status_url"].as_str().unwrap()
            ))
            .bearer_auth(&principal.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let job: Value = response.json().await.unwrap();
        if job["state"] == "FAILED" {
            let output = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/reports/P048_runs/daemon-tcp-failures")
                .join(uuid::Uuid::now_v7().to_string());
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(
                output.join("probe-job.json"),
                serde_json::to_vec_pretty(&job).unwrap(),
            )
            .unwrap();
            for (name, path) in [
                (
                    "daemon.log",
                    fixture._root.path().join("data/p048-first.log"),
                ),
                ("requests.ndjson", cfg.requests.clone()),
            ] {
                if path.is_file() {
                    std::fs::copy(path, output.join(name)).unwrap();
                }
            }
            let paths = json!({"package_exists":fixture.package.is_dir(),"work_exists":fixture.work.is_dir(),"profile_exists":fixture.work.join("profile.json").is_file(),"oracle_exists":cfg.expected_file.is_file(),"control_exists":cfg.control.is_file(),"evidence_exists":cfg.evidence_root.is_dir()});
            std::fs::write(
                output.join("paths.json"),
                serde_json::to_vec_pretty(&paths).unwrap(),
            )
            .unwrap();
            for entry in std::fs::read_dir(&cfg.evidence_root).unwrap() {
                let path = entry.unwrap().path().join("source-http.json");
                if path.is_file() {
                    std::fs::copy(path, output.join("source-http.json")).unwrap();
                }
            }
            panic!(
                "cualificación P048 del daemon falló; diagnóstico {}",
                output.display()
            );
        }
        assert_ne!(
            job["state"], "FAILED",
            "cualificación P048 del daemon: {}",
            job["error"]
        );
        if job["state"] == "COMPLETED" {
            return job;
        }
        assert!(Instant::now() < deadline, "probe P048 sin completar");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// Preparación del catálogo P043/P052 desde respuestas RPC observadas. No lee
// identificadores privados del plugin; los IDs centrales son UUIDv7 nuevos.
async fn seed_catalog(
    db: &larm_db::pool::Db,
    cfg: &HttpSourceConfig,
    installation: &str,
    metadata: &Value,
) -> (String, String) {
    let now = timestamp(chrono::Utc::now());
    let adapter = uuid::Uuid::now_v7().to_string();
    let canonical = uuid::Uuid::now_v7().to_string();
    let distribution = uuid::Uuid::now_v7().to_string();
    let revision = uuid::Uuid::now_v7().to_string();
    let variant = uuid::Uuid::now_v7().to_string();
    let artifact = uuid::Uuid::now_v7().to_string();
    let manifest = &metadata["manifest"];
    let a = &manifest["artifacts"][0];
    let size = a["size_bytes"].as_str().unwrap().parse::<i64>().unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    let inserted=sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) SELECT ?,?,?,1,?,id,'HTTP',json_extract(manifest,'$.capabilities') FROM adapter_installation WHERE id=? AND state='VALIDATED'")
        .bind(&adapter).bind(&now).bind(&now).bind(json!({"source_protocol_v1":{"search_applicable":false,"supported_filters":[],"limitations":["explicit_url_import_only"]}}).to_string()).bind(installation).execute(&mut *tx).await.unwrap();
    assert_eq!(inserted.rows_affected(), 1);
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,?,?,1,'{}','P048 owned HTTP download',?,?,0,1,'NONE','{}')")
        .bind(&cfg.source_id).bind(&now).bind(&now).bind(&adapter).bind(&cfg.url).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES(?,?,?,1,'{}','P048 self-authored bytes','[]','[]')")
        .bind(&canonical).bind(&now).bind(&now).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES(?,?,?,1,'{}',?,?,'direct-http','explicit-url')")
        .bind(&distribution).bind(&now).bind(&now).bind(&canonical).bind(&cfg.source_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,'CURRENT')")
        .bind(&revision).bind(&now).bind(&now).bind(&distribution).bind(manifest["revision_key"].as_str().unwrap()).bind(manifest["manifest_hash"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,format,capabilities,model_limits,update_available) VALUES(?,?,?,1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','BINARY','[]',?,0)")
        .bind(&variant).bind(&now).bind(&now).bind(&revision).bind(serde_json::to_string(&larm_domain::model::ModelLimits::default()).unwrap()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,expected_size,sha256,required,size_estimate) VALUES(?,?,?,1,'{}',?,?,'BINARY',?,?,1,?)")
        .bind(&artifact).bind(&now).bind(&now).bind(&variant).bind(a["relative_path"].as_str().unwrap()).bind(size).bind(a["sha256"].as_str().unwrap()).bind(a["size_estimate"].to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
        .bind(uuid::Uuid::now_v7().to_string()).bind(&now).bind(&now).bind(&variant).bind(json!([artifact]).to_string()).bind(manifest["manifest_hash"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    (variant, artifact)
}

async fn preview(
    port: u16,
    principal: &TemporaryPrincipal,
    request: &DownloadPlanRequest,
    ctx: &MutationContext,
) -> DownloadPlanResponse {
    let (status, body) = post_json(
        &reqwest::Client::new(),
        port,
        principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(request).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "preview: {body}");
    serde_json::from_value(body).unwrap()
}

async fn accept(
    port: u16,
    principal: &TemporaryPrincipal,
    plan: &DownloadPlanResponse,
    ctx: &MutationContext,
) -> (String, String, Value) {
    let execute = ExecutePlan {
        plan_id: plan.plan_id.clone().unwrap(),
        plan_hash: plan.plan_hash.clone().unwrap(),
        operator_confirmed: true,
    };
    let key = uuid::Uuid::now_v7().to_string();
    let client = reqwest::Client::new();
    let (status, receipt) = post_json(
        &client,
        port,
        principal,
        "/api/v1/downloads",
        &key,
        &serde_json::to_value(&execute).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "aceptación P048: {receipt}"
    );
    let (status, replay) = post_json(
        &client,
        port,
        principal,
        "/api/v1/downloads",
        &key,
        &serde_json::to_value(&execute).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    assert_eq!(replay, receipt);
    (
        receipt["job_id"].as_str().unwrap().into(),
        execute.plan_id,
        receipt,
    )
}

async fn closure(db: &larm_db::pool::Db, job: &str, plan: &str, phase: &str) -> Value {
    let stored: String = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE id=? AND kind='download.plan'",
    )
    .bind(plan)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let reservations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(job)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let unpaired: i64=sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE e.resource_id=? AND o.event_id IS NULL").bind(job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(stored, phase);
    assert_eq!((reservations, unpaired), (0, 0));
    json!({"journal_phase":stored,"reservations":reservations,"unpaired_outbox_events":unpaired})
}

fn source_mode(cfg: &HttpSourceConfig, mode: &str) {
    let temp = cfg.control.with_extension("tmp");
    std::fs::write(&temp, json!({"mode":mode}).to_string()).unwrap();
    std::fs::rename(temp, &cfg.control).unwrap();
}

async fn wait_offset(
    db: &larm_db::pool::Db,
    job: &str,
    artifact: &str,
    after: u64,
    total: u64,
    daemon: &RunningDaemon,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let (state, _, progress, error) = job_snapshot(db, job).await;
        let offset = progress["artifacts"][artifact]["durable_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        assert_ne!(
            state,
            "FAILED",
            "descarga P048: {error:?}; {}",
            daemon.log_text()
        );
        if state == "RUNNING" && offset > after && offset < total {
            return progress["artifacts"][artifact].clone();
        }
        assert!(
            Instant::now() < deadline,
            "no hay checkpoint intermedio: {state}/{progress}; {}",
            daemon.log_text()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn p048_daemon_tcp_rechaza_corrupcion_y_recupera_descarga_con_cas() {
    let (repo, fixture) = super::source_http_product::http_fixture();
    let cfg: HttpSourceConfig = serde_json::from_value(fixture.data["config"].clone()).unwrap();
    let data_root = fixture._root.path().join("data");
    let config_path = data_root.join("p048-download-config.json");
    live_config(&config_path);
    let db = larm_db::pool::Db::open(&data_root.join("larm.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let mut principal = TemporaryPrincipal::enroll(&data_root, &config_path);
    let ctx = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let installation = register_profile(&db, &fixture, &cfg, &ctx, &repo).await;
    let location = create_location(&db, &fixture._root.path().join("managed-download"), &ctx).await;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first = RunningDaemon::start(
        &data_root,
        &config_path,
        port,
        data_root.join("p048-first.log"),
    );
    first.wait_live(port, Duration::from_secs(45)).unwrap();
    let qualified = qualify(port, &principal, &installation, &ctx, &fixture, &cfg).await;
    let run_id = qualified["result"]["conformance"]["run_id"]
        .as_str()
        .unwrap();
    let qualification_path = cfg.evidence_root.join(run_id).join("source-http.json");
    let qualification: Value =
        serde_json::from_slice(&std::fs::read(&qualification_path).unwrap()).unwrap();
    assert_eq!(
        qualification["observations"]["Ok"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
    let preparation = repo
        .join("docs/reports/P048_runs/daemon-tcp-preparation")
        .join(run_id);
    std::fs::create_dir_all(&preparation).unwrap();
    std::fs::copy(&qualification_path, preparation.join("source-http.json")).unwrap();
    std::fs::write(
        preparation.join("probe-job.json"),
        serde_json::to_vec_pretty(&qualified).unwrap(),
    )
    .unwrap();
    let _trace_archive = TraceArchive {
        output: preparation.clone(),
        data_root: data_root.clone(),
        requests: cfg.requests.clone(),
    };
    let metadata = &qualification["checkpoints"]["source_metadata"];
    assert!(metadata["manifest"].is_object());
    let (variant, artifact) = seed_catalog(&db, &cfg, &installation, metadata).await;
    let expected = std::fs::read(&cfg.expected_file).unwrap();
    let hash = hex::encode(Sha256::digest(&expected));
    let size = expected.len() as u64;
    let request = DownloadPlanRequest {
        variant_id: variant.clone(),
        storage_location_id: Some(location.id.clone()),
        hard_budget_bytes: DecimalU64::new(size),
        source_id: Some(cfg.source_id.clone()),
        priority: larm_domain::download::DownloadPriority::Normal,
        optional_artifact_ids: vec![],
    };
    let before = std::fs::read(&cfg.requests).unwrap();
    let initial = preview(port, &principal, &request, &ctx).await;
    let (status,_)=post_json(&reqwest::Client::new(),port,&principal,"/api/v1/downloads",&uuid::Uuid::now_v7().to_string(),&json!({"plan_id":initial.plan_id,"plan_hash":initial.plan_hash,"operator_confirmed":false}),&ctx).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read(&cfg.requests).unwrap(),
        before,
        "preview y rechazo sin confirmación no leen la fuente"
    );
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(counts, (0, 0));

    source_mode(&cfg, "corrupt");
    let negative = preview(port, &principal, &request, &ctx).await;
    let (failed_job, failed_plan, _) = accept(port, &principal, &negative, &ctx).await;
    let failed = wait_terminal(&db, &failed_job, "FAILED", &preparation).await;
    let failure: Value = serde_json::from_str(failed.3.as_ref().unwrap()).unwrap();
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert_eq!(failure["retryable"], false);
    let failed_closure = closure(&db, &failed_job, &failed_plan, "FAILED").await;
    let published:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM artifact_location WHERE artifact_id=?),(SELECT count(*) FROM artifact_provenance WHERE artifact_id=?)").bind(&artifact).bind(&artifact).fetch_one(&db.pool).await.unwrap();
    assert_eq!(published, (0, 0));
    let target = Path::new(&location.canonical_path);
    let bad_partial = target
        .join("staging")
        .join(&failed_job)
        .join(format!("{artifact}.partial"));
    assert!(!bad_partial.exists());
    let mut corrupt = expected.clone();
    corrupt[0] ^= 255;
    let corrupt_hash = hex::encode(Sha256::digest(&corrupt));
    assert!(!target
        .join(larm_storage::cas::blob_relative(&hash).unwrap())
        .exists());
    assert!(!target
        .join(larm_storage::cas::blob_relative(&corrupt_hash).unwrap())
        .exists());
    eprintln!("P048 TCP: cuerpo corrupto rechazado, sin CAS/procedencia y con reserva cerrada");

    source_mode(&cfg, "normal");
    let bandwidth_update = set_bandwidth(port, &principal, Some(64 << 10)).await;
    let (_, bandwidth_before_pause) = super::settings_restart::read(port, &principal).await;
    assert_eq!(
        bandwidth_before_pause["storage"]["download_bandwidth_limit_bytes_per_second"],
        "65536"
    );
    let plan = preview(port, &principal, &request, &ctx).await;
    let (job, plan_id, accepted) = accept(port, &principal, &plan, &ctx).await;
    let checkpoint = wait_offset(&db, &job, &artifact, 0, size, &first).await;
    let control = json!({"reason":"operator_request","approval_id":null});
    let pause_path = format!("/api/v1/downloads/{job}/pause");
    let key = uuid::Uuid::now_v7().to_string();
    let (status, pause) = post_json(
        &reqwest::Client::new(),
        port,
        &principal,
        &pause_path,
        &key,
        &control,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    let paused = wait_state(&db, &job, "PAUSED").await;
    let offset = paused.2["artifacts"][&artifact]["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let partial = target
        .join("staging")
        .join(&job)
        .join(format!("{artifact}.partial"));
    assert_eq!(std::fs::metadata(&partial).unwrap().len(), offset);
    let paused_bytes = std::fs::read(&partial).unwrap();
    first.stop();
    let mut second = RunningDaemon::start(
        &data_root,
        &config_path,
        port,
        data_root.join("p048-paused-restart.log"),
    );
    second.wait_live(port, Duration::from_secs(45)).unwrap();
    let (_, bandwidth_after_paused_restart) = super::settings_restart::read(port, &principal).await;
    assert_eq!(
        bandwidth_after_paused_restart["storage"]["download_bandwidth_limit_bytes_per_second"],
        "65536"
    );
    assert_eq!(job_snapshot(&db, &job).await.0, "PAUSED");
    assert_eq!(std::fs::read(&partial).unwrap(), paused_bytes);
    let (status, resumed) = post_json(
        &reqwest::Client::new(),
        port,
        &principal,
        &format!("/api/v1/downloads/{job}/resume"),
        &uuid::Uuid::now_v7().to_string(),
        &control,
        &ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    wait_offset(&db, &job, &artifact, offset, size, &second).await;
    second.stop();
    // El último commit puede avanzar entre la observación y el kill. Fijar el
    // checkpoint definitivo una vez que el proceso ya no puede escribirlo.
    let stopped = job_snapshot(&db, &job).await;
    assert_eq!(stopped.0, "RUNNING");
    let interrupted = stopped.2["artifacts"][&artifact].clone();
    let interrupted_offset = interrupted["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(interrupted_offset > offset && interrupted_offset < size);
    let request_count = std::fs::read_to_string(&cfg.requests)
        .unwrap()
        .lines()
        .count();
    let mut third = RunningDaemon::start(
        &data_root,
        &config_path,
        port,
        data_root.join("p048-running-restart.log"),
    );
    third.wait_live(port, Duration::from_secs(45)).unwrap();
    let (_, bandwidth_after_running_restart) =
        super::settings_restart::read(port, &principal).await;
    assert_eq!(
        bandwidth_after_running_restart["storage"]["download_bandwidth_limit_bytes_per_second"],
        "65536"
    );
    let completed = wait_terminal(&db, &job, "COMPLETED", &preparation).await;
    assert_eq!(completed.1, 3);
    assert_eq!(completed.2["recovered_after_restart"], true);
    let completed_closure = closure(&db, &job, &plan_id, "COMPLETED").await;
    let requests = std::fs::read_to_string(&cfg.requests)
        .unwrap()
        .lines()
        .skip(request_count)
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let real_resume = requests.iter().find(|r| r["method"] == "GET").unwrap();
    assert!(real_resume["range"]
        .as_str()
        .unwrap()
        .starts_with(&format!("bytes={interrupted_offset}-")));
    assert_eq!(real_resume["if_range"], format!("\"{hash}\""));
    for request in requests.iter().filter(|r| r["method"] == "GET") {
        let (start, end) = request["range"]
            .as_str()
            .unwrap()
            .strip_prefix("bytes=")
            .unwrap()
            .split_once('-')
            .unwrap();
        let (start, end) = (start.parse::<u64>().unwrap(), end.parse::<u64>().unwrap());
        assert!(
            end >= start && end - start < 65536,
            "el límite persistido debe acotar el bloque tras reiniciar"
        );
    }
    let relative: String =
        sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
            .bind(&artifact)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let actual = std::fs::read(target.join(&relative)).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(hex::encode(Sha256::digest(&actual)), hash);
    assert_eq!(relative, larm_storage::cas::blob_relative(&hash).unwrap());
    let provenance: (String, String) = sqlx::query_as(
        "SELECT source_id,revision_key FROM artifact_provenance WHERE artifact_id=?",
    )
    .bind(&artifact)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(provenance.0, cfg.source_id);
    assert_eq!(
        provenance.1,
        metadata["manifest"]["revision_key"].as_str().unwrap()
    );
    assert!(!partial.exists());
    let recoveries:i64=sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=? AND event_type='download.state_changed' AND json_extract(payload,'$.action')='recovered_after_restart'").bind(&job).fetch_one(&db.pool).await.unwrap();
    assert_eq!(recoveries, 1);
    let reservations: i64 = sqlx::query_scalar("SELECT count(*) FROM volume_reservation")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(reservations, 0);
    third.stop();
    assert!(principal.revoke());
    let output = repo.join("docs/reports/P048_runs/daemon-tcp").join(&job);
    std::fs::create_dir_all(&output).unwrap();
    std::fs::copy(&qualification_path, output.join("source-http.json")).unwrap();
    std::fs::copy(&cfg.requests, output.join("requests.ndjson")).unwrap();
    for name in [
        "p048-first.log",
        "p048-paused-restart.log",
        "p048-running-restart.log",
    ] {
        std::fs::copy(data_root.join(name), output.join(name)).unwrap();
    }
    let report = json!({"scope":"original P048 HTTP executable, owned loopback bytes, actual daemon TCP/P022/P026, sealed preflight, explicit confirmation, consumer/SQLite/staging/CAS/provenance; catalog prepared from observed RPC metadata, not full P043 ingestion or GUI; no external provider qualification or global REAL_PASS",
        "observed_at":timestamp(chrono::Utc::now()),"host":std::env::var("COMPUTERNAME").unwrap_or_default(),"daemon_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),"product_sha256":hex::encode(Sha256::digest(std::fs::read(fixture.package.join(&fixture.manifest.executable.relative_path)).unwrap())),
        "conformance":qualified["result"]["conformance"],"source_id":cfg.source_id,"variant_id":variant,"artifact_id":artifact,"job_id":job,"plan_id":plan_id,"confirmation_required":true,"no_jobs_or_reservations_before_confirmation":true,"preview_did_not_read_source":true,"accepted":accepted,
        "integrity_failure":{"job_id":failed_job,"plan_id":failed_plan,"error":failure,"closure":failed_closure,"partial_removed":true,"no_publication_or_provenance_or_cas":true,"corrupt_sha256":corrupt_hash},
        "bandwidth_update_for_controlled_pause":bandwidth_update,"bandwidth_after_paused_restart":bandwidth_after_paused_restart["storage"]["download_bandwidth_limit_bytes_per_second"],"bandwidth_after_running_restart":bandwidth_after_running_restart["storage"]["download_bandwidth_limit_bytes_per_second"],"recovered_read_blocks_bounded_by_persisted_limit":true,"initial_checkpoint":checkpoint,"pause_receipt":pause,"paused_offset":offset.to_string(),"pause_preserved_after_restart":true,"resume_receipt":resumed,"interrupted_checkpoint":interrupted,"resumed_request":real_resume,"attempt":completed.1,"final_state":completed.0,"closure":completed_closure,"sha256":hash,"size_bytes":size.to_string(),"relative_path":relative,"provenance":{"source_id":provenance.0,"revision_key":provenance.1},"recovery_events":recoveries,"reservations":reservations,"temporary_principal_revoked":true});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/reports/P048_runs/daemon-tcp.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "P048 TCP: pausa y dos reinicios, archivo final verificado y procedencia/CAS confirmados"
    );
    db.pool.close().await;
}
