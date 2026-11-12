//! Ciclo de vida del binario: arranque y rechazo de configuración ejecutando el proceso real.
//! El cierre limpio por Ctrl+C/CTRL_BREAK lo prueba tools/daemon_lifecycle.py (necesita
//! enviar la señal de consola a un grupo de procesos, algo que este test no puede hacer
//! sin `unsafe`).
#![forbid(unsafe_code)]
#![allow(clippy::expect_used)] // en tests un fallo de entorno debe abortar el caso

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "support/daemon.rs"]
mod daemon_process;
use daemon_process::{daemon, RunningDaemon};

async fn seed_interrupted_download(
    data_root: &Path,
    with_journal: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let database = data_root.join("larm.sqlite");
    let db = larm_db::pool::Db::open(&database).await?;
    larm_db::migrate::migrate(&db.pool).await?;
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let installation_id = uuid::Uuid::now_v7().to_string();
    let source_adapter_id = uuid::Uuid::now_v7().to_string();
    let canonical_id = uuid::Uuid::now_v7().to_string();
    let source_id = uuid::Uuid::now_v7().to_string();
    let distribution_id = uuid::Uuid::now_v7().to_string();
    let revision_id = uuid::Uuid::now_v7().to_string();
    let variant_id = uuid::Uuid::now_v7().to_string();
    let location_id = uuid::Uuid::now_v7().to_string();
    let job_id = uuid::Uuid::now_v7().to_string();
    let plan_id = uuid::Uuid::now_v7().to_string();
    let actor_id = uuid::Uuid::now_v7().to_string();
    let correlation_id = uuid::Uuid::now_v7().to_string();
    let root = data_root.join("models");
    std::fs::create_dir_all(&root)?;
    let pool = &db.pool;
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{}','restart.fixture.source','1.0','source',?,'fixture','UNTRUSTED','{}','INSTALLED')")
        .bind(&installation_id).bind(&now).bind(&now).bind("a".repeat(64)).execute(pool).await?;
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES(?,?,?,1,'{}',?,'HTTP','[]')")
        .bind(&source_adapter_id).bind(&now).bind(&now).bind(&installation_id).execute(pool).await?;
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES(?,?,?,1,'{}','Restart fixture','[]','[]')")
        .bind(&canonical_id).bind(&now).bind(&now).execute(pool).await?;
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,?,?,1,'{}','Restart fixture source',?,'https://example.invalid',0,1,'NONE','{}')")
        .bind(&source_id).bind(&now).bind(&now).bind(&source_adapter_id).execute(pool).await?;
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES(?,?,?,1,'{}',?,?,'fixture','restart')")
        .bind(&distribution_id).bind(&now).bind(&now).bind(&canonical_id).bind(&source_id).execute(pool).await?;
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,'CURRENT')")
        .bind(&revision_id).bind(&now).bind(&now).bind(&distribution_id).bind("restart-fixture-v1").bind("a".repeat(64)).execute(pool).await?;
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,?,?,1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
        .bind(&variant_id).bind(&now).bind(&now).bind(&revision_id).execute(pool).await?;
    sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,?,?,1,'{}','Restart fixture location',?,?, 'NTFS','LOCAL',1000,900,0,0,0,'ONLINE','restart-fixture-volume','SSD')")
        .bind(&location_id).bind(&now).bind(&now).bind(root.to_string_lossy().as_ref()).bind(root.to_string_lossy().as_ref()).execute(pool).await?;
    let metadata = serde_json::json!({
        "plan_id": plan_id,
        "source_id": source_id,
        "actor_id": actor_id,
        "correlation_id": correlation_id,
        "hard_budget_bytes": null,
        "artifacts": [],
    });
    let progress = serde_json::json!({
        "phase": "RUNNING",
        "completed_bytes": "4",
        "artifacts": {
            "0190f5c0-7a00-7000-8000-000000000035": {
                "durable_offset": "4",
                "source_id": source_id,
                "revision_key": "restart-fixture-v1",
                "resume_validator": {"type": "STRONG_ETAG", "value": "\"restart-v1\""}
            }
        }
    });
    sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,2,?,'NORMAL',?,?,?,'RUNNING',1,?,NULL)")
        .bind(&job_id).bind(&now).bind(&now).bind(metadata.to_string()).bind(&variant_id).bind(&location_id).bind("a".repeat(64)).bind(progress.to_string()).execute(pool).await?;
    if with_journal {
        // Preserve the accepted operation binding but deliberately omit its
        // plan record: recovery must fail safely and close the journal too.
        let payload = serde_json::json!({"execution":{"job_id":job_id,"phase":"QUEUED"}});
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan',?,'QUEUED',?,?,?)")
            .bind(&plan_id).bind(serde_json::json!([variant_id]).to_string())
            .bind("a".repeat(64)).bind(payload.to_string()).bind(&now).execute(pool).await?;
    }
    sqlx::query("INSERT INTO volume_reservation(id,volume_key,job_id,total_bytes,remaining_bytes,expires_at) VALUES(?,?,?,100,96,NULL)")
        .bind(uuid::Uuid::now_v7().to_string()).bind("restart-fixture-volume").bind(&job_id).execute(pool).await?;
    db.pool.close().await;
    Ok(job_id)
}

#[test]
fn help_termina_con_codigo_cero() {
    let out = daemon().arg("--help").output().expect("lanzar larm-daemon");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("larm-daemon"));
}

#[test]
fn config_json_invalida_sale_con_codigo_dos_sin_arrancar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join("config.json");
    std::fs::write(&cfg, "{ no es json").expect("escribir config");
    let out = daemon()
        .arg("--config")
        .arg(&cfg)
        .arg("--data-root")
        .arg(dir.path())
        .output()
        .expect("lanzar larm-daemon");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no es JSON válido"));
    // No llegó a inicializar logging: no hay carpeta de logs.
    assert!(!dir.path().join("logs").exists());
}

#[test]
fn config_inexistente_sale_con_codigo_dos() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = daemon()
        .arg("--config")
        .arg(dir.path().join("no-existe.json"))
        .arg("--data-root")
        .arg(dir.path())
        .output()
        .expect("lanzar larm-daemon");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no se pudo leer config"));
}

#[test]
fn proceso_real_reinicia_api_y_recupera_descarga_running_con_checkpoint() {
    assert_download_restart(true);
}

#[test]
fn reinicio_sin_journal_conserva_checkpoint_y_reserva_sin_publicar_un_cierre_parcial() {
    assert_download_restart(false);
}

fn assert_download_restart(with_journal: bool) {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("carpeta apps")
        .parent()
        .expect("raíz del workspace")
        .to_path_buf();
    let test_temp = repo.join("target/test-tmp");
    std::fs::create_dir_all(&test_temp).expect("crear temporales gestionados");
    let workspace = tempfile::Builder::new()
        .prefix("daemon-download-restart-")
        .tempdir_in(&test_temp)
        .expect("crear raíz temporal del daemon");
    let data_root = workspace.path().join("data");
    std::fs::create_dir_all(&data_root).expect("crear raíz de datos");
    let config = workspace.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::json!({"paths":{"database_relative_path":"larm.sqlite"}}).to_string(),
    )
    .expect("escribir configuración aislada");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reservar puerto loopback");
    let port = listener.local_addr().expect("leer puerto loopback").port();
    drop(listener);

    let mut first = RunningDaemon::start(
        &data_root,
        &config,
        port,
        workspace.path().join("first.log"),
    );
    first
        .wait_live(port, Duration::from_secs(30))
        .expect("primer arranque del daemon");
    first.stop();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("crear runtime de fixture");
    let job_id = runtime
        .block_on(seed_interrupted_download(&data_root, with_journal))
        .expect("sembrar una descarga durable interrumpida");

    let mut restarted = RunningDaemon::start_desktop_fixture(
        &data_root,
        &config,
        port,
        workspace.path().join("restarted.log"),
    );
    restarted
        .wait_live(port, Duration::from_secs(30))
        .expect("reinicio del daemon");

    let database = data_root.join("larm.sqlite");
    runtime.block_on(async {
        let db = larm_db::pool::Db::open(&database).await.expect("reabrir SQLite");
        let deadline = Instant::now() + Duration::from_secs(10);
        let (state, attempt, progress_text, error_text): (String, i64, String, Option<String>);
        loop {
            let row: Option<(String, i64, String, Option<String>)> = sqlx::query_as(
                "SELECT state,attempt,progress,error FROM download_job WHERE id=?",
            )
            .bind(&job_id)
            .fetch_optional(&db.pool)
            .await
            .expect("leer job recuperado");
            if let Some(row) = row.as_ref() {
                let ready = if with_journal {
                    row.0 == "FAILED"
                } else {
                    row.0 == "RUNNING"
                        && row.1 == 2
                        && restarted.log_text().contains("download_failure_journal_conflict")
                };
                if row.2.contains("\"recovered_after_restart\":true") && ready {
                    (state, attempt, progress_text, error_text) = row.clone();
                    break;
                }
            }
            assert!(Instant::now() < deadline, "el daemon no recuperó el job; último estado={row:?}; log={}", restarted.log_text());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(state, if with_journal { "FAILED" } else { "RUNNING" });
        assert_eq!(attempt, 2, "el consumer debe reclamar el job tras la recuperación");
        let progress: serde_json::Value = serde_json::from_str(&progress_text).expect("JSON de progreso");
        assert_eq!(progress["recovered_after_restart"], true);
        assert_eq!(progress["artifacts"]["0190f5c0-7a00-7000-8000-000000000035"]["durable_offset"], "4");
        if with_journal {
            let error: serde_json::Value = serde_json::from_str(error_text.as_deref().expect("error del job")).expect("JSON de error");
            assert_eq!(error["details"]["reason"], "download_plan_invalid");
            let phases: (String, String) = sqlx::query_as("SELECT phase,json_extract(payload,'$.execution.phase') FROM operation_journal WHERE id=(SELECT json_extract(metadata,'$.plan_id') FROM download_job WHERE id=?)")
                .bind(&job_id).fetch_one(&db.pool).await.expect("leer journal cerrado");
            assert_eq!(phases, ("FAILED".into(), "FAILED".into()));
        } else {
            assert!(error_text.is_none(), "sin journal no se publica un fallo parcial");
            let failed_events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=? AND event_type='download.state_changed' AND json_extract(payload,'$.action')='failed'")
                .bind(&job_id).fetch_one(&db.pool).await.expect("contar fallos publicados");
            assert_eq!(failed_events, 0);
        }
        let reservation_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
            .bind(&job_id)
            .fetch_one(&db.pool)
            .await
            .expect("contar reservas recuperadas");
        assert_eq!(reservation_count, i64::from(!with_journal), "sólo el cierre atómico libera la reserva");
        let recovered_event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND e.event_type='download.state_changed' AND json_extract(e.payload,'$.action')='recovered_after_restart'",
        )
        .bind(&job_id)
        .fetch_one(&db.pool)
        .await
        .expect("contar eventos de recuperación");
        assert_eq!(recovered_event_count, 1, "la recuperación emite un evento durable");
        db.pool.close().await;
    });
    restarted.stop();
}

fn isolated_owner_root(prefix: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("apps")
        .parent()
        .expect("repo");
    let parent = repo.join("target/test-tmp");
    std::fs::create_dir_all(&parent).expect("test root");
    let owned = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .expect("owned root");
    let data = owned.path().join("datos 模型");
    std::fs::create_dir(&data).expect("data root");
    let config = owned.path().join("config.json");
    std::fs::write(
        &config,
        r#"{"paths":{"database_relative_path":"larm.sqlite"}}"#,
    )
    .expect("isolated config");
    (owned, data, config)
}
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("loopback")
        .local_addr()
        .expect("port")
        .port()
}
fn wait_exit(daemon: &mut RunningDaemon) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.child.try_wait().expect("observe child") {
            return status.code().expect("exit code");
        }
        assert!(
            Instant::now() < deadline,
            "owned child did not exit: {}",
            daemon.log_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
async fn owner_job_snapshot(data: &Path, job: &str) -> serde_json::Value {
    let db = larm_db::pool::Db::open(&data.join("larm.sqlite"))
        .await
        .expect("read DB");
    let state: (String, i64, i64, String, Option<String>) = sqlx::query_as(
        "SELECT state,attempt,row_version,progress,error FROM download_job WHERE id=?",
    )
    .bind(job)
    .fetch_one(&db.pool)
    .await
    .expect("read fixture job");
    let reserves:Vec<(String,i64,i64)>=sqlx::query_as("SELECT volume_key,total_bytes,remaining_bytes FROM volume_reservation WHERE job_id=? ORDER BY volume_key")
        .bind(job).fetch_all(&db.pool).await.expect("read reserves");
    let events:Vec<(String,i64,String)>=sqlx::query_as("SELECT event_type,resource_version,payload FROM event WHERE resource_id=? ORDER BY event_id")
        .bind(job).fetch_all(&db.pool).await.expect("read events");
    db.pool.close().await;
    serde_json::json!({"job":state,"reserves":reserves,"events":events})
}

#[test]
fn live_daemon_owns_data_before_recovery_and_crash_releases_only_host_lease() {
    let (owned, data, config) = isolated_owner_root("daemon-native-owner-");
    let port_a = free_port();
    let mut port_b = free_port();
    while port_b == port_a {
        port_b = free_port();
    }
    let mut first = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        port_a,
        owned.path().join("first.log"),
    );
    first
        .wait_live(port_a, Duration::from_secs(45))
        .expect("first live daemon");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    // Deliberate recovery sentinel, not an actual running download worker.
    let job = runtime
        .block_on(seed_interrupted_download(&data, true))
        .expect("seed sentinel");
    let before = runtime.block_on(owner_job_snapshot(&data, &job));
    let first_pid = first.child.id();
    let mut duplicate = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        port_b,
        owned.path().join("duplicate.log"),
    );
    assert_eq!(wait_exit(&mut duplicate), 1);
    assert!(duplicate.log_text().contains("daemon_data_root_in_use"));
    let duplicate_after = runtime.block_on(owner_job_snapshot(&data, &job));
    assert_eq!(
        duplicate_after, before,
        "a duplicate must not migrate/recover the live host's jobs"
    );
    // A different data root can select the same absolute database.
    let other_data = owned.path().join("other-data");
    let other_config = owned.path().join("other-config.json");
    std::fs::write(&other_config,serde_json::json!({"paths":{"database_relative_path":data.join("larm.sqlite").to_str().expect("UTF8 fixture")}}).to_string()).expect("external database config");
    let mut shared = RunningDaemon::start_desktop_fixture(
        &other_data,
        &other_config,
        port_b,
        owned.path().join("shared-database.log"),
    );
    assert_eq!(wait_exit(&mut shared), 1);
    assert!(shared.log_text().contains("daemon_data_root_in_use"));
    let shared_after = runtime.block_on(owner_job_snapshot(&data, &job));
    assert_eq!(shared_after, before);
    first
        .wait_live(port_a, Duration::from_secs(5))
        .expect("owner remains live");
    first.stop();
    // Native process kill/wait, without deleting the persistent lock anchor.
    assert!(data
        .join(larm_adapter_host::daemon_owner::OWNER_FILE)
        .is_file());
    let mut restarted = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        port_b,
        owned.path().join("restarted.log"),
    );
    restarted
        .wait_live(port_b, Duration::from_secs(45))
        .expect("reacquire after host crash");
    let deadline = Instant::now() + Duration::from_secs(15);
    let after = loop {
        let value = runtime.block_on(owner_job_snapshot(&data, &job));
        if value["job"][0] == "FAILED" {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "restart did not recover sentinel: {value}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(after["job"][1], 2);
    assert_eq!(after["reserves"], serde_json::json!([]));
    let recovered = after["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|e| {
            e[0] == "download.state_changed"
                && e[2]
                    .as_str()
                    .is_some_and(|s| s.contains("recovered_after_restart"))
        })
        .count();
    assert_eq!(recovered, 1);
    let restarted_pid = restarted.child.id();
    restarted.stop();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("apps")
        .parent()
        .expect("repo");
    let evidence = repo
        .join("docs/reports/P093_runs/native-daemon-owner")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&evidence).expect("retained evidence");
    for name in [
        "first.log",
        "duplicate.log",
        "shared-database.log",
        "restarted.log",
    ] {
        std::fs::copy(owned.path().join(name), evidence.join(name)).expect("retain log");
    }
    let report = serde_json::json!({"status":"PASS","scope":"actual daemon process ownership, duplicate rejection before recovery and host crash/reacquisition; seeded download sentinel, no proof of P093 worker tree closure",
        "first_pid":first_pid,"restarted_pid":restarted_pid,"port_a":port_a,"port_b":port_b,"duplicate_exit_code":1,
        "duplicate_before":before,"duplicate_after":duplicate_after,"shared_database_after":shared_after,
        "external_database_duplicate_rejected":true,"after_host_restart":after,"owner_anchor_not_deleted":true,
        "worker_closure_accepted":false,"generated_at":larm_domain::bridge::timestamp(chrono::Utc::now()),"evidence":evidence.strip_prefix(repo).expect("repo evidence")});
    std::fs::write(
        evidence.join("report.json"),
        serde_json::to_vec_pretty(&report).expect("report JSON"),
    )
    .expect("retain report");
    std::fs::write(
        repo.join("docs/reports/P093_runs/native-daemon-owner.json"),
        serde_json::to_vec_pretty(&report).expect("report JSON"),
    )
    .expect("latest report");
}

#[test]
fn startup_failure_releases_the_host_lease_for_the_next_real_server() {
    let (owned, data, config) = isolated_owner_root("daemon-owner-failure-");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let blocked_port = listener.local_addr().expect("occupied port").port();
    let mut failed = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        blocked_port,
        owned.path().join("failed.log"),
    );
    assert_eq!(wait_exit(&mut failed), 1);
    assert!(!failed.log_text().contains("daemon_data_root_in_use"));
    drop(listener);
    let mut next = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        blocked_port,
        owned.path().join("next.log"),
    );
    next.wait_live(blocked_port, Duration::from_secs(45))
        .expect("ownership released after startup failure");
    next.stop();
}

#[test]
fn restoring_a_backup_requires_exclusive_database_ownership() {
    let (owned, data, config) = isolated_owner_root("daemon-owner-restore-");
    let port = free_port();
    let mut server =
        RunningDaemon::start_desktop_fixture(&data, &config, port, owned.path().join("live.log"));
    server
        .wait_live(port, Duration::from_secs(45))
        .expect("live daemon");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    let backup = owned.path().join("backup.sqlite");
    runtime.block_on(async {
        let db = larm_db::pool::Db::open(&data.join("larm.sqlite"))
            .await
            .expect("read live DB");
        larm_db::backup::backup(&db.pool, &backup)
            .await
            .expect("online backup");
        db.pool.close().await;
    });
    let restore = || {
        let mut command = daemon();
        command
            .arg("--data-root")
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .arg("db-restore")
            .arg("--from")
            .arg(&backup);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command.output().expect("owned restore process")
    };
    let rejected = restore();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("daemon_data_root_in_use"));
    assert!(!data.join("larm.sqlite.restoring").exists());
    server
        .wait_live(port, Duration::from_secs(5))
        .expect("live database preserved");
    server.stop();
    let restored = restore();
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&restored.stdout).expect("restore report");
    assert_eq!(result["restore"], "ok");
    let mut restarted = RunningDaemon::start_desktop_fixture(
        &data,
        &config,
        port,
        owned.path().join("restored.log"),
    );
    restarted
        .wait_live(port, Duration::from_secs(45))
        .expect("restored daemon owns DB");
    restarted.stop();
}
