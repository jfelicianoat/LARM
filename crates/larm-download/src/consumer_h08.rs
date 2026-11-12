// Real TCP bytes and monotonic time, with a test source port (no P022/P048).
use super::*;
use larm_domain::download::{DownloadCommand, DownloadControlIdempotency, DownloadStop};
use larm_domain::ports::{MutationContext, SettingsRepository};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::num::NonZeroU64;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn http(port: u16, method: &str, path: &str, body: &Value, phase: &str) -> Value {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let body = if method == "POST" {
        body.to_string()
    } else {
        String::new()
    };
    write!(socket, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-H08-Phase: {phase}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut response = Vec::new();
    socket.take(32768).read_to_end(&mut response).unwrap();
    assert!(
        response.starts_with(b"HTTP/1.0 200"),
        "controlled HTTP source must reply 200"
    );
    let split = response.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
    serde_json::from_slice(&response[split + 4..]).unwrap()
}

struct HttpSource {
    port: u16,
    phase: Arc<Mutex<String>>,
    plans: Vec<larm_domain::artifacts::Plan>,
    active_jobs: Arc<Mutex<std::collections::BTreeMap<String, tokio::task::Id>>>,
    ready: Arc<tokio::sync::Barrier>,
}

struct HttpSession {
    port: u16,
    phase: Arc<Mutex<String>>,
    plan: larm_domain::artifacts::Plan,
    ready: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl DownloadSourceResolver for HttpSource {
    async fn open(
        &self,
        claim: &DownloadClaim,
        _record: &DownloadPlanRecord,
        candidate: &DownloadSourceCandidate,
    ) -> Result<Box<dyn DownloadSourceSession>, LarmError> {
        self.active_jobs
            .lock()
            .unwrap()
            .insert(claim.job.id.clone(), tokio::task::id());
        let plan = self
            .plans
            .iter()
            .find(|p| download_spec(p).unwrap().variant_id == candidate.variant_id)
            .unwrap()
            .clone();
        Ok(Box::new(HttpSession {
            port: self.port,
            phase: self.phase.clone(),
            plan,
            ready: self.ready.clone(),
        }))
    }
}

#[async_trait]
impl DownloadSourceSession for HttpSession {
    async fn download_plan(
        &self,
        _request: SourcePlanRequest,
    ) -> Result<larm_domain::artifacts::Plan, LarmError> {
        self.ready.wait().await;
        Ok(self.plan.clone())
    }

    async fn download_chunk(
        &self,
        request: SourceDownloadChunkRequest,
    ) -> Result<SourceDownloadChunk, LarmError> {
        let port = self.port;
        let phase = self.phase.lock().unwrap().clone();
        let request = serde_json::to_value(request).unwrap();
        let value =
            tokio::task::spawn_blocking(move || http(port, "POST", "/chunk", &request, &phase))
                .await
                .unwrap();
        Ok(serde_json::from_value(value).unwrap())
    }

    async fn close(&self) -> Result<(), LarmError> {
        Ok(())
    }
}

struct Server {
    child: Child,
    port: u16,
    trace: PathBuf,
}

impl Server {
    fn start(workspace: &std::path::Path, trace: PathBuf) -> Self {
        let mut command = Command::new("python");
        command
            .args(["-X", "utf8"])
            .arg(workspace.join("tools/p051_h08_http_fixture.py"))
            .arg(&trace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let mut announcement = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut announcement)
            .unwrap();
        let value: Value = serde_json::from_str(&announcement).unwrap();
        assert_eq!(value["payload"], "0123456789");
        Self {
            child,
            port: value["port"].as_u64().unwrap().try_into().unwrap(),
            trace,
        }
    }

    fn mark(&self, phase: &str) {
        http(
            self.port,
            "GET",
            &format!("/mark/{phase}"),
            &Value::Null,
            phase,
        );
    }

    fn rows(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.trace)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn prepare_pair() -> (SeededDownloadFixture, DownloadJob, DownloadPlanRecord) {
    let mut fixture = seeded_download_fixture().await;
    fixture.record.response.source_candidates.clear();
    sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
        .bind(
            json!({"record":fixture.record,"execution":{"job_id":fixture.job.id,"phase":"QUEUED"}})
                .to_string(),
        )
        .bind(PLAN)
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    let mut peer_record = fixture.record.clone();
    let mut peer = fixture.job.clone();
    peer.id = uuid::Uuid::now_v7().to_string();
    peer.variant_id = uuid::Uuid::now_v7().to_string();
    let artifact = uuid::Uuid::now_v7().to_string();
    let plan = uuid::Uuid::now_v7().to_string();
    peer_record.request.variant_id = peer.variant_id.clone();
    peer_record.response.variant_id = peer.variant_id.clone();
    peer_record.response.preview_id = uuid::Uuid::now_v7().to_string();
    peer_record.response.plan_id = Some(plan.clone());
    peer_record.response.artifact_estimates[0].artifact_id = artifact.clone();
    peer.metadata.insert("plan_id".into(), json!(plan));
    peer.metadata.insert(
        "artifacts".into(),
        json!(peer_record.response.artifact_estimates),
    );
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) SELECT ?,'t','t',1,'{}',revision_id,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0 FROM model_variant WHERE id=?")
        .bind(&peer.variant_id).bind(VARIANT).execute(&fixture.db.pool).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,expected_size,sha256,required,size_estimate) VALUES(?,'t','t',1,'{}',?,'weights.gguf','GGUF',10,?,1,?)")
        .bind(artifact).bind(&peer.variant_id).bind(&fixture.sha256).bind(json!(exact_size(10)).to_string()).execute(&fixture.db.pool).await.unwrap();
    sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','QUEUED',?,?,'t')")
        .bind(plan).bind(&peer.plan_hash).bind(json!({"record":peer_record,"execution":{"job_id":peer.id,"phase":"QUEUED"}}).to_string()).execute(&fixture.db.pool).await.unwrap();
    sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,?,?,'NORMAL',?,?,?,'QUEUED',0,'{}',NULL)")
        .bind(&peer.id).bind(&peer.created_at).bind(&peer.updated_at).bind(peer.row_version).bind(json!(peer.metadata).to_string()).bind(&peer.variant_id).bind(LOCATION).bind(&peer.plan_hash).execute(&fixture.db.pool).await.unwrap();
    sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES(?,'volume-test',?,100,100,NULL)")
        .bind(uuid::Uuid::now_v7().to_string()).bind(&peer.id).execute(&fixture.db.pool).await.unwrap();
    (fixture, peer, peer_record)
}

fn consumers(
    fixture: &SeededDownloadFixture,
    peer: &DownloadPlanRecord,
    server: &Server,
    phase: Arc<Mutex<String>>,
    limiter: Arc<crate::policy::BandwidthLimiter>,
    active_jobs: Arc<Mutex<std::collections::BTreeMap<String, tokio::task::Id>>>,
) -> Vec<tokio::task::JoinHandle<DownloadConsumerOutcome>> {
    let source = Arc::new(HttpSource {
        active_jobs,
        ready: Arc::new(tokio::sync::Barrier::new(2)),
        port: server.port,
        phase,
        plans: [&fixture.record, peer]
            .map(|r| {
                source_plan_for(
                    SOURCE,
                    &r.request.variant_id,
                    &r.revision_key,
                    &fixture.sha256,
                )
            })
            .to_vec(),
    });
    (0..2)
        .map(|_| {
            let mut consumer = consumer_for_fixture(
                fixture.into(),
                None,
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                Arc::new(Mutex::new(Vec::new())),
            );
            consumer.sources = source.clone();
            consumer.bandwidth_limiter = limiter.clone();
            tokio::spawn(async move { consumer.run_once().await.unwrap() })
        })
        .collect()
}

async fn completed(
    handles: Vec<tokio::task::JoinHandle<DownloadConsumerOutcome>>,
    fixture: &SeededDownloadFixture,
) {
    for handle in handles {
        let outcome = tokio::time::timeout(Duration::from_secs(30), handle)
            .await
            .unwrap()
            .unwrap();
        if !matches!(&outcome, DownloadConsumerOutcome::Completed { .. }) {
            let rows: Vec<(String, String, Option<String>)> =
                sqlx::query_as("SELECT id,state,error FROM download_job ORDER BY id")
                    .fetch_all(&fixture.db.pool)
                    .await
                    .unwrap();
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let path = workspace
                .join("docs/reports/P051_runs/h08-core-20261027")
                .join(format!("failure-{}.json", uuid::Uuid::now_v7()));
            std::fs::write(
                &path,
                json!({"outcome":format!("{outcome:?}"),"jobs":rows}).to_string(),
            )
            .unwrap();
            assert!(
                matches!(&outcome, DownloadConsumerOutcome::Completed { .. }),
                "Core TCP did not complete: {outcome:?}; jobs={rows:?}; diagnostic={}",
                path.display()
            );
        }
    }
}

async fn snapshot(fixture: &SeededDownloadFixture, job: &str) -> (String, Value) {
    let (state, progress): (String, String) =
        sqlx::query_as("SELECT state,progress FROM download_job WHERE id=?")
            .bind(job)
            .fetch_one(&fixture.db.pool)
            .await
            .unwrap();
    (state, serde_json::from_str(&progress).unwrap())
}

fn epoch(rows: &[Value], phase: &str, rate: u64, require_cap: bool) -> Value {
    let start = rows
        .iter()
        .find(|r| r["kind"] == "mark" && r["phase"] == phase)
        .unwrap()["monotonic_ns"]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap();
    let mut reads = rows
        .iter()
        .filter(|r| r["kind"] == "read" && r["phase"] == phase)
        .collect::<Vec<_>>();
    reads.sort_by_key(|r| r["monotonic_ns"].as_str().unwrap().parse::<u128>().unwrap());
    assert!(
        !reads.is_empty(),
        "the phase must actually transfer TCP bytes"
    );
    let mut total = 0_u128;
    let mut violates = false;
    for row in &reads {
        let now = row["monotonic_ns"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap();
        let bytes = row["bytes"].as_str().unwrap().parse::<u128>().unwrap();
        total += bytes;
        // One-second shared burst + elapsed budget + ONE byte IO tolerance.
        let budget = u128::from(rate) + u128::from(rate) * (now - start) / 1_000_000_000 + 1;
        violates |= total > budget;
        if require_cap {
            assert!(
                total <= budget,
                "shared TCP budget exceeded: {phase} {total}>{budget}"
            );
        }
    }
    let finish = reads.last().unwrap()["monotonic_ns"]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap();
    // Cada ventana también limita la ráfaga; el tiempo ocioso anterior no
    // puede convertirse en un saldo sin cota dentro de la fase.
    for first in 0..reads.len() {
        let low = reads[first]["monotonic_ns"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap();
        let mut window_bytes = 0_u128;
        for row in &reads[first..] {
            let high = row["monotonic_ns"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap();
            window_bytes += row["bytes"].as_str().unwrap().parse::<u128>().unwrap();
            let budget = u128::from(rate) + u128::from(rate) * (high - low) / 1_000_000_000 + 1;
            violates |= window_bytes > budget;
            if require_cap {
                assert!(
                    window_bytes <= budget,
                    "shared burst window exceeded in {phase}"
                );
            }
        }
    }
    json!({"phase":phase,"rate":rate.to_string(),"start_ns":start.to_string(),"finish_ns":finish.to_string(),"bytes":total.to_string(),"requests":reads.len(),"violates_limited_envelope":violates,"tolerance_bytes":"1","clock":"server time.monotonic_ns"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "H08 stress: eight pairs publishing identical content; sensitive to parallel network suites on the same host — run explicitly"]
async fn h08_publicacion_concurrente_del_mismo_blob_ocho_veces() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = workspace
        .join("docs/reports/P051_runs/h08-core-http")
        .join(format!("cas-stress-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&output).unwrap();
    let server = Server::start(workspace, output.join("requests.ndjson"));
    for index in 0..8 {
        let (fixture, _, peer) = prepare_pair().await;
        let phase = format!("concurrent-{index}");
        server.mark(&phase);
        completed(
            consumers(
                &fixture,
                &peer,
                &server,
                Arc::new(Mutex::new(phase)),
                Arc::new(crate::policy::BandwidthLimiter::new(None)),
                Arc::default(),
            ),
            &fixture,
        )
        .await;
        fixture.db.pool.close().await;
    }
    std::fs::write(output.join("result.json"), json!({"scope":"Core TCP source fixture, eight concurrent pairs publishing identical content; no qualification or rate claim","pairs":8,"completed_jobs":16}).to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h08_dos_consumidores_tcp_comparten_caudal_cambian_limite_y_recuperan_pausa() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let id = uuid::Uuid::now_v7().to_string();
    let output = workspace
        .join("docs/reports/P051_runs/h08-core-http")
        .join(&id);
    std::fs::create_dir_all(&output).unwrap();
    let server = Server::start(workspace, output.join("requests.ndjson"));
    let phase = Arc::new(Mutex::new("baseline".to_string()));
    let (baseline, _, baseline_peer) = prepare_pair().await;
    server.mark("baseline");
    completed(
        consumers(
            &baseline,
            &baseline_peer,
            &server,
            phase.clone(),
            Arc::new(crate::policy::BandwidthLimiter::new(None)),
            Arc::default(),
        ),
        &baseline,
    )
    .await;
    let baseline_measurement = epoch(&server.rows(), "baseline", 1, false);
    assert_eq!(baseline_measurement["bytes"], "20");
    assert_eq!(
        baseline_measurement["violates_limited_envelope"], true,
        "the unthrottled source must be faster than the tested budget"
    );
    baseline.db.pool.close().await;

    let (mut fixture, peer, peer_record) = prepare_pair().await;
    let settings = larm_db::settings::SqlSettings {
        pool: fixture.db.pool.clone(),
    };
    let mut payload: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.join("docs/contracts/defaults.json")).unwrap(),
    )
    .unwrap();
    payload["storage"]["download_bandwidth_limit_bytes_per_second"] = json!("1");
    settings.get_or_initialize(payload.clone()).await.unwrap();
    *phase.lock().unwrap() = "limited-1".into();
    server.mark("limited-1");
    let limiter = Arc::new(crate::policy::BandwidthLimiter::new(NonZeroU64::new(1)));
    let active_jobs = Arc::default();
    let handles = consumers(
        &fixture,
        &peer_record,
        &server,
        phase.clone(),
        limiter.clone(),
        Arc::clone(&active_jobs),
    );
    tokio::time::sleep(Duration::from_millis(3300)).await;
    assert_eq!(snapshot(&fixture, &fixture.job.id).await.0, "RUNNING");
    assert_eq!(snapshot(&fixture, &peer.id).await.0, "RUNNING");
    let ctx = MutationContext {
        actor_id: fixture.job.metadata["actor_id"].as_str().unwrap().into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    payload["storage"]["download_bandwidth_limit_bytes_per_second"] = json!("2");
    let saved = settings.update(1, payload, &ctx).await.unwrap();
    assert_eq!(saved.revision, 2);
    server.mark("changed-2");
    *phase.lock().unwrap() = "changed-2".into();
    limiter.set_limit(NonZeroU64::new(2));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let jobs = SqlDownloadJobs {
        pool: fixture.db.pool.clone(),
    };
    let candidates = [&fixture.job.id, &peer.id];
    let mut paused_job = None;
    for job in candidates {
        let (state, progress) = snapshot(&fixture, job).await;
        assert_eq!(state, "RUNNING");
        if progress["artifacts"].as_object().is_some_and(|a| {
            a.values().any(|a| {
                a["durable_offset"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
                    > 0
            })
        }) {
            paused_job = Some(job.clone());
            break;
        }
    }
    let paused_job = paused_job.unwrap();
    let stop = DownloadStop {
        reason: "operator_request".into(),
        approval_id: None,
    };
    jobs.request_control(
        &paused_job,
        DownloadCommand::Pause,
        &stop,
        &ctx,
        &DownloadControlIdempotency {
            key: uuid::Uuid::now_v7().to_string(),
            body_hash: "a".repeat(64),
        },
    )
    .await
    .unwrap();
    // Congelar el otro consumidor mientras sigue RUNNING, antes de esperar
    // la pausa. Con presupuestos bajos podría completar mientras el elegido
    // espera su siguiente byte; su tiempo no forma parte del oráculo de pausa.
    let other_job = if paused_job == fixture.job.id {
        &peer.id
    } else {
        &fixture.job.id
    };
    assert_eq!(snapshot(&fixture, other_job).await.0, "RUNNING");
    let other_task = active_jobs.lock().unwrap()[other_job];
    for handle in &handles {
        if handle.id() == other_task {
            handle.abort();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let paused = loop {
        let row = snapshot(&fixture, &paused_job).await;
        if row.0 == "PAUSED" {
            break row.1;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
    let artifact = paused["artifacts"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let partial = fixture
        .root
        .join("staging")
        .join(&paused_job)
        .join(format!("{artifact}.partial"));
    let preserved = std::fs::read(&partial).unwrap();
    assert!(!preserved.is_empty() && preserved.len() < 10);
    drop(jobs);
    drop(settings);
    fixture.db.pool.close().await;
    fixture.db = larm_db::pool::Db::open(&fixture.db_path).await.unwrap();
    let jobs = SqlDownloadJobs {
        pool: fixture.db.pool.clone(),
    };
    assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
    assert_eq!(snapshot(&fixture, &paused_job).await.0, "PAUSED");
    assert_eq!(std::fs::read(&partial).unwrap(), preserved);
    let settings = larm_db::settings::SqlSettings {
        pool: fixture.db.pool.clone(),
    }
    .get_or_initialize(Value::Null)
    .await
    .unwrap();
    assert_eq!(settings.revision, 2);
    let rate = settings.payload["storage"]["download_bandwidth_limit_bytes_per_second"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(rate, 2);
    jobs.request_control(
        &paused_job,
        DownloadCommand::Resume,
        &stop,
        &ctx,
        &DownloadControlIdempotency {
            key: uuid::Uuid::now_v7().to_string(),
            body_hash: "b".repeat(64),
        },
    )
    .await
    .unwrap();
    server.mark("restarted-2");
    *phase.lock().unwrap() = "restarted-2".into();
    completed(
        consumers(
            &fixture,
            &peer_record,
            &server,
            phase,
            Arc::new(crate::policy::BandwidthLimiter::new(NonZeroU64::new(rate))),
            Arc::default(),
        ),
        &fixture,
    )
    .await;
    let rows = server.rows();
    let measures = [
        epoch(&rows, "limited-1", 1, true),
        epoch(&rows, "changed-2", 2, true),
        epoch(&rows, "restarted-2", 2, true),
    ];
    let (locations, provenance, reservations, unpaired): (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM artifact_location),(SELECT count(*) FROM artifact_provenance),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE o.event_id IS NULL)").fetch_one(&fixture.db.pool).await.unwrap();
    assert_eq!(
        (locations, provenance, reservations, unpaired),
        (2, 2, 0, 0)
    );
    let phases: Vec<String> =
        sqlx::query_scalar("SELECT phase FROM operation_journal WHERE kind='download.plan'")
            .fetch_all(&fixture.db.pool)
            .await
            .unwrap();
    assert!(phases.iter().all(|phase| phase == "COMPLETED"));
    let artifact = std::fs::read(
        fixture
            .root
            .join(larm_storage::cas::blob_relative(&fixture.sha256).unwrap()),
    )
    .unwrap();
    assert_eq!(artifact, fixture.bytes);
    for job in [&fixture.job.id, &peer.id] {
        assert_eq!(snapshot(&fixture, job).await.0, "COMPLETED");
    }
    let receipt = json!({"id":id,"observed_at":larm_domain::bridge::timestamp(chrono::Utc::now()),"scope":"Core DownloadConsumer with real TCP SourceDownloadChunk fixture, real SQLite/staging/CAS/provenance; test-prepared plans/catalog and source port, no P022/P048 qualification, daemon Settings API, process kill or GUI acceptance","baseline":baseline_measurement,"limited_epochs":measures,"settings_revision":settings.revision,"persisted_limit":rate.to_string(),"paused_job":paused_job,"paused_checkpoint":paused,"paused_partial_bytes_preserved":preserved.len().to_string(),"recovered_running_jobs":1,"final_jobs":[fixture.job.id,peer.id],"final_sha256":fixture.sha256,"artifact_bytes":"10","locations":locations,"provenance":provenance,"reservations":reservations,"unpaired_outbox_events":unpaired,"journal_phases":phases});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    std::fs::write(
        workspace.join("docs/reports/P051_runs/h08-core-http.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    fixture.db.pool.close().await;
    eprintln!("H08 Core TCP: baseline faster than budget, two consumers limited, live change and SQLite recovery verified; receipt {}", output.display());
}
