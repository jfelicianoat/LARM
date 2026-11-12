//! P051: descarga real del daemon con paquete cualificado y reinicio abrupto.
//! El catálogo es propio de la fixture; no cualifica un proveedor de producto.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use larm_domain::download::{DownloadPlanRequest, DownloadPlanResponse, ExecutePlan};
use larm_domain::locations::{StorageLocation, StorageLocationCreate};
use larm_domain::ports::{
    AdapterInstallationRepository, ApprovalRepository, ConformanceProfileRepository,
    LocationService, MutationContext, StorageProber,
};
use larm_domain::units::DecimalU64;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::daemon_process::{daemon, RunningDaemon};

#[path = "download_integrity.rs"]
mod integrity;

#[derive(Clone, Copy)]
pub(super) enum Flow {
    IntegrityAndRestart,
    Ranges,
}

/// Principal exclusivo de este caso: token sólo en memoria, revocado incluso
/// durante el unwinding de una aserción. No se usan credenciales del operador.
pub(super) struct TemporaryPrincipal {
    data_root: PathBuf,
    config_path: PathBuf,
    name: String,
    pub(super) actor_id: String,
    pub(super) token: String,
    revoked: bool,
}

fn local_command(data_root: &Path, config_path: &Path) -> Command {
    let mut command = daemon();
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("LARM_")) {
        command.env_remove(name);
    }
    command
        .arg("--data-root")
        .arg(data_root)
        .arg("--config")
        .arg(config_path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

impl TemporaryPrincipal {
    pub(super) fn enroll(data_root: &Path, config_path: &Path) -> Self {
        let name = format!("p051-restart-{}", uuid::Uuid::now_v7());
        Self::enroll_named(
            data_root,
            config_path,
            name,
            "OPERATOR",
            "read,adapters:write,jobs:write,settings:write,models:write",
        )
    }

    /// Principal del caso de cancelación: POST /api/v1/jobs/{id}/cancel
    /// exige `runtimes:write` (P031) para cualquier clase de job.
    pub(super) fn enroll_canceller(data_root: &Path, config_path: &Path) -> Self {
        Self::enroll_named(
            data_root,
            config_path,
            format!("p093-cancel-{}", uuid::Uuid::now_v7()),
            "OPERATOR",
            "read,adapters:write,jobs:write,settings:write,models:write,runtimes:write",
        )
    }

    pub(super) fn enroll_desktop(data_root: &Path, config_path: &Path) -> Self {
        // Only called with the migrated, owned temporary data root. Native
        // bootstrap rotates this token but preserves the qualified actor UUID.
        Self::enroll_named(data_root, config_path, "desktop".into(), "DESKTOP", "admin")
    }

    fn enroll_named(
        data_root: &Path,
        config_path: &Path,
        name: String,
        kind: &str,
        scopes: &str,
    ) -> Self {
        let output = local_command(data_root, config_path)
            .args([
                "enroll-principal",
                "--name",
                &name,
                "--kind",
                kind,
                "--scopes",
                scopes,
            ])
            .output()
            .expect("crear principal temporal del caso");
        assert!(
            output.status.success(),
            "no se pudo crear el principal temporal"
        );
        let value: Value =
            serde_json::from_slice(&output.stdout).expect("recibo de principal temporal");
        Self {
            data_root: data_root.into(),
            config_path: config_path.into(),
            name,
            actor_id: value["principal"]["id"].as_str().unwrap().into(),
            token: value["token"].as_str().unwrap().into(),
            revoked: false,
        }
    }

    pub(super) fn revoke(&mut self) -> bool {
        if self.revoked {
            return true;
        }
        self.revoked = local_command(&self.data_root, &self.config_path)
            .args(["revoke-principal", "--name", &self.name])
            .output()
            .is_ok_and(|output| output.status.success());
        self.revoked
    }
}

impl Drop for TemporaryPrincipal {
    fn drop(&mut self) {
        if !self.revoke() {
            eprintln!("no se pudo revocar el principal temporal {}", self.name);
        }
    }
}

pub(super) async fn qualify_from_daemon(
    db: &larm_db::pool::Db,
    port: u16,
    installation_id: &str,
    principal: &TemporaryPrincipal,
    ctx: &MutationContext,
) -> Value {
    let installation = larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .get(installation_id)
    .await
    .unwrap()
    .unwrap();
    let manifest =
        larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())
            .unwrap();
    let profiles = larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    let previous = profiles
        .conformance_profile(installation_id)
        .await
        .unwrap()
        .unwrap();
    // Una segunda cualificación usa un workspace nuevo: la receta anterior
    // ya descargó artefactos en su almacenamiento privado y no son entradas
    // válidas para el caso que espera medir una descarga íntegra desde cero.
    let mut configuration: larm_adapter_host::conformance::profiles::LocalProfile =
        serde_json::from_value(previous.configuration.clone()).unwrap();
    let old_work = configuration.work_dir.clone();
    let fresh_work = principal.data_root.join("daemon-source-work");
    std::fs::create_dir_all(fresh_work.join("catalog")).unwrap();
    std::fs::create_dir_all(fresh_work.join("storage")).unwrap();
    for relative in ["profile.json", "catalog/catalog.json"] {
        std::fs::copy(old_work.join(relative), fresh_work.join(relative)).unwrap();
    }
    configuration.work_dir = fresh_work.clone();
    for (_, path) in &mut configuration.roots {
        if let Ok(relative) = path.strip_prefix(&old_work) {
            *path = fresh_work.join(relative);
        }
    }
    for path in &mut configuration.immutable_inputs {
        if let Ok(relative) = path.strip_prefix(&old_work) {
            *path = fresh_work.join(relative);
        }
    }
    let now = chrono::Utc::now();
    let approval=larm_domain::approval::ApprovalRequest {
        operation_digest:larm_adapter_sdk::permissions::execution_digest(&manifest).unwrap(),
        resource_ids:vec![larm_adapter_sdk::permissions::adapter_resource(&manifest)],
        code_hash:Some(manifest.package_sha256.clone()), approved:true,
        permissions:json!({"publisher":manifest.publisher,"permissions":manifest.permissions,"expires_at":larm_domain::bridge::timestamp(now+chrono::Duration::minutes(15)),"secret_refs":[]}).as_object().unwrap().clone(),
    }.grant(uuid::Uuid::now_v7().to_string(), &principal.actor_id, now, chrono::Duration::minutes(15)).unwrap();
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .insert(&approval, ctx)
    .await
    .unwrap();
    let profile = larm_domain::conformance_profiles::ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: installation_id.into(),
        actor_id: principal.actor_id.clone(),
        manifest_sha256: previous.manifest_sha256,
        configuration: serde_json::to_value(configuration).unwrap(),
        approval,
        created_at: larm_domain::bridge::timestamp(now),
    };
    profiles
        .store_conformance_profile(&profile, Some(&previous.id), ctx)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let response = client
        .post(format!("{base}/api/v1/adapters/{installation_id}/probe"))
        .bearer_auth(&principal.token)
        .header("idempotency-key", uuid::Uuid::now_v7().to_string())
        .header("x-correlation-id", &ctx.correlation_id)
        .json(&json!({"resource_id":null,"profile_id":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let receipt: Value = response.json().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let response = client
            .get(format!("{base}{}", receipt["status_url"].as_str().unwrap()))
            .bearer_auth(&principal.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let job: Value = response.json().await.unwrap();
        assert_ne!(
            job["state"], "FAILED",
            "cualificación del daemon falló: {}",
            job["error"]
        );
        if job["state"] == "COMPLETED" {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "cualificación del daemon sin completar"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn native_storage() -> Arc<dyn StorageProber> {
    Arc::new(larm_hardware::storage::NativeStorageProber {
        command: larm_hardware::devices::NativeCommand {
            program: env!("CARGO_BIN_EXE_larm-daemon").into(),
            args: vec!["probe-storage".into()],
            timeout: Duration::from_secs(60),
        },
    })
}

pub async fn create_location(
    db: &larm_db::pool::Db,
    root: &Path,
    ctx: &MutationContext,
) -> StorageLocation {
    std::fs::create_dir_all(root).unwrap();
    let location = larm_storage::locations::LocationServiceImpl {
        repo: Arc::new(larm_db::locations::SqlStorageLocations {
            pool: db.pool.clone(),
        }),
        prober: native_storage(),
        thresholds: None,
    }
    .create(
        &StorageLocationCreate {
            name: "P051 daemon restart destination".into(),
            root_path: larm_storage::locations::plain_path(root),
            reserved_bytes: DecimalU64::new(0).unwrap(),
            priority: 0,
            auto_placement: false,
            performance_class: larm_domain::storage::PerformanceClass::Unknown,
        },
        ctx,
    )
    .await
    .unwrap();
    assert!(!location.volume_key.starts_with("unknown:"));
    location
}

async fn seed_catalog(db: &larm_db::pool::Db, data: &Value, installation_id: &str) -> Vec<String> {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let source_id = data["config"]["source_id"].as_str().unwrap();
    let source_adapter_id = uuid::Uuid::now_v7().to_string();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) SELECT ?,?,?,1,?,id,'HTTP',json_extract(manifest,'$.capabilities') FROM adapter_installation WHERE id=? AND state='VALIDATED'")
        .bind(&source_adapter_id).bind(&now).bind(&now)
        .bind(json!({"source_protocol_v1":data["config"]["search_support"]}).to_string())
        .bind(installation_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES(?,?,?,1,'{}','P051 controlled HTTP source',?,?,0,1,'NONE','{}')")
        .bind(source_id).bind(&now).bind(&now).bind(&source_adapter_id).bind(data["base_url"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
    let mut artifact_ids = Vec::new();
    for entry in data["catalog"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .take(6)
    {
        let model = &entry["model"];
        let revision = &entry["revision"];
        let artifact = &entry["inspection"]["artifacts"][0];
        let variant_id = model["id"].as_str().unwrap();
        let revision_id = model["revision_id"].as_str().unwrap();
        let distribution_id = revision["distribution_id"].as_str().unwrap();
        let canonical_id = uuid::Uuid::now_v7().to_string();
        let artifact_id = uuid::Uuid::now_v7().to_string();
        let size = artifact["size_bytes"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let estimate = json!({"confidence":"EXACT", "bytes":size.to_string(),
            "lower_bound_bytes":size.to_string(), "upper_bound_bytes":size.to_string(),
            "source":"MANIFEST", "evidence_refs":["fixture:self-authored-artifact"], "reason":null});
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES(?,?,?,1,'{}','P051 owned restart fixture',?,'[]')")
            .bind(&canonical_id).bind(&now).bind(&now).bind(model["capabilities"].to_string()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES(?,?,?,1,'{}',?,?,?,?)")
            .bind(distribution_id).bind(&now).bind(&now).bind(&canonical_id).bind(source_id)
            .bind(entry["upstream_namespace"].as_str().unwrap()).bind(entry["upstream_model_id"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,'CURRENT')")
            .bind(revision_id).bind(&now).bind(&now).bind(distribution_id).bind(revision["revision_key"].as_str().unwrap()).bind(revision["manifest_hash"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,format,capabilities,model_limits,update_available) VALUES(?,?,?,1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','BINARY',?,?,0)")
            .bind(variant_id).bind(&now).bind(&now).bind(revision_id).bind(model["capabilities"].to_string()).bind(model["model_limits"].to_string()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,format,expected_size,sha256,required,size_estimate) VALUES(?,?,?,1,'{}',?,?,'BINARY',?,?,1,?)")
            .bind(&artifact_id).bind(&now).bind(&now).bind(variant_id).bind(artifact["relative_path"].as_str().unwrap()).bind(size).bind(artifact["sha256"].as_str().unwrap()).bind(estimate.to_string()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
            .bind(uuid::Uuid::now_v7().to_string()).bind(&now).bind(&now).bind(variant_id).bind(json!([artifact_id]).to_string()).bind(revision["manifest_hash"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
        artifact_ids.push(artifact_id);
    }
    tx.commit().await.unwrap();
    artifact_ids
}

pub(super) fn config(path: &Path, limit: Option<u64>) {
    let thresholds = larm_core::config::LowDiskThresholds::Bytes {
        warning_bytes: DecimalU64::new(2).unwrap(),
        critical_bytes: DecimalU64::new(1).unwrap(),
    };
    std::fs::write(
        path,
        json!({
            "paths":{"database_relative_path":"larm.sqlite"},
            "api":{"request_timeout_seconds":30},
            "storage":{
                "download_bandwidth_limit_bytes_per_second":limit.map(|bytes| bytes.to_string()),
                "download_global_concurrency":1,
                "low_disk_thresholds":thresholds,
                "minimum_margin_bytes":"1048576",
                "minimum_free_bytes":"0",
            },
        })
        .to_string(),
    )
    .unwrap();
}

pub(super) async fn set_bandwidth(
    port: u16,
    principal: &TemporaryPrincipal,
    limit: Option<u64>,
) -> Value {
    integrity::set_bandwidth(port, principal, limit).await
}

/// Deja H08 bajo el control de Settings persistido, sin fijarlo por fichero.
pub(super) fn live_config(path: &Path) {
    config(path, None);
    let mut value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["storage"]
        .as_object_mut()
        .unwrap()
        .remove("download_bandwidth_limit_bytes_per_second");
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

pub(super) async fn job_snapshot(
    db: &larm_db::pool::Db,
    id: &str,
) -> (String, i64, Value, Option<String>) {
    let (state, attempt, progress, error): (String, i64, String, Option<String>) =
        sqlx::query_as("SELECT state,attempt,progress,error FROM download_job WHERE id=?")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    (
        state,
        attempt,
        serde_json::from_str(&progress).unwrap(),
        error,
    )
}

pub(super) async fn post_json(
    client: &reqwest::Client,
    port: u16,
    principal: &TemporaryPrincipal,
    path: &str,
    key: &str,
    body: &Value,
    ctx: &MutationContext,
) -> (reqwest::StatusCode, Value) {
    let response = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(&principal.token)
        .header("idempotency-key", key)
        .header("x-correlation-id", &ctx.correlation_id)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

pub(super) async fn wait_state(
    db: &larm_db::pool::Db,
    id: &str,
    expected: &str,
) -> (String, i64, Value, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        let snapshot = job_snapshot(db, id).await;
        if snapshot.0 == expected {
            return snapshot;
        }
        assert_ne!(
            snapshot.0, "FAILED",
            "estado fallido al esperar {expected}: {snapshot:?}"
        );
        assert!(
            Instant::now() < deadline,
            "sin alcanzar {expected}: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Clone, Debug, serde::Serialize)]
struct OwnedWorkerProcess {
    pid: u32,
    started_at: u64,
    executable: PathBuf,
}

/// Sólo se identifican ejecutables descendientes del daemon aislado del caso.
/// La genealogía usa sólo IDs; rutas sólo de los descendientes. No lee
/// argumentos ni entorno.
fn source_processes(daemon_pid: u32, executable: &Path) -> Vec<OwnedWorkerProcess> {
    use std::collections::BTreeSet;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = Pid::from_u32(daemon_pid);
    let mut descendants = BTreeSet::from([root]);
    loop {
        let children = system
            .processes()
            .iter()
            .filter(|(_, p)| {
                p.parent()
                    .is_some_and(|parent| descendants.contains(&parent))
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let previous = descendants.len();
        descendants.extend(children);
        if descendants.len() == previous {
            break;
        }
    }
    descendants.remove(&root);
    let ids = descendants.into_iter().collect::<Vec<_>>();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    // El servicio puede compartir una consola con conhost.exe o ejecutar
    // sondeos independientes. Sólo el ejecutable del paquete identifica la
    // raíz SourceWorker; después se incluyen todos sus descendientes.
    let executable = std::fs::canonicalize(executable).unwrap();
    let mut workers = ids
        .iter()
        .copied()
        .filter(|pid| {
            system
                .process(*pid)
                .and_then(|p| p.exe())
                .and_then(|path| std::fs::canonicalize(path).ok())
                .is_some_and(|path| path == executable)
        })
        .collect::<BTreeSet<_>>();
    loop {
        let children = ids
            .iter()
            .copied()
            .filter(|pid| {
                system
                    .process(*pid)
                    .and_then(|p| p.parent())
                    .is_some_and(|parent| workers.contains(&parent))
            })
            .collect::<Vec<_>>();
        let previous = workers.len();
        workers.extend(children);
        if workers.len() == previous {
            break;
        }
    }
    workers
        .into_iter()
        .map(|pid| {
            let process = system.process(pid).expect("worker propio observado");
            OwnedWorkerProcess {
                pid: pid.as_u32(),
                started_at: process.start_time(),
                executable: std::fs::canonicalize(
                    process.exe().expect("ejecutable del worker propio"),
                )
                .expect("ruta real del worker propio"),
            }
        })
        .collect()
}

fn worker_still_present(worker: &OwnedWorkerProcess) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let ids = [Pid::from_u32(worker.pid)];
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    system.process(ids[0]).is_some_and(|process| {
        process.start_time() == worker.started_at
            && process
                .exe()
                .map(|path| std::fs::canonicalize(path).ok() == Some(worker.executable.clone()))
                .unwrap_or(true)
    })
}

struct ActiveCancellationCase<'a> {
    db: &'a larm_db::pool::Db,
    data: &'a Value,
    daemon: &'a RunningDaemon,
    port: u16,
    principal: &'a TemporaryPrincipal,
    request: &'a DownloadPlanRequest,
    ctx: &'a MutationContext,
}

async fn active_cancellation(
    case: ActiveCancellationCase<'_>,
    paused_job_id: &str,
    paused_file: &Path,
) -> Value {
    let ActiveCancellationCase {
        db,
        data,
        daemon,
        port,
        principal,
        request,
        ctx,
    } = case;
    let client = reqwest::Client::new();
    let (status, plan) = post_json(
        &client,
        port,
        principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(request).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let execute =
        json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"operator_confirmed":true});
    let (status, created) = post_json(
        &client,
        port,
        principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &execute,
        ctx,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "aceptar descarga para cancelación activa: {created}"
    );
    let id = created["job_id"].as_str().unwrap();
    let artifact = plan["artifact_estimates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["selected"] == true)
        .unwrap()["artifact_id"]
        .as_str()
        .unwrap();
    let partial = paused_file
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(id)
        .join(format!("{artifact}.partial"));
    let deadline = Instant::now() + Duration::from_secs(75);
    let before = loop {
        let current = job_snapshot(db, id).await;
        let offset = current.2["artifacts"][artifact]["durable_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        assert_ne!(
            current.0, "FAILED",
            "descarga antes de cancelar: {current:?}"
        );
        if current.0 == "RUNNING" && offset > 0 {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "sin bytes durables antes de cancelar: {current:?}; log={}",
            daemon.log_text()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(partial.is_file());
    let initial_len = std::fs::metadata(&partial).unwrap().len();
    let total = std::fs::metadata(data["expected_files"][0].as_str().unwrap())
        .unwrap()
        .len();
    assert!(initial_len > 0 && initial_len < total);
    let package = PathBuf::from(data["package"].as_str().unwrap());
    let manifest = larm_adapter_sdk::manifest::AdapterManifest::parse(
        &std::fs::read_to_string(package.join("manifest.json")).unwrap(),
    )
    .unwrap();
    let workers = source_processes(
        daemon.child.id(),
        &package.join(manifest.executable.relative_path),
    );
    assert!(
        !workers.is_empty(),
        "la transferencia activa tiene procesos propios vivos"
    );
    assert!(workers.iter().all(worker_still_present));
    let preserved = std::fs::read(paused_file).unwrap();
    let path = format!("/api/v1/downloads/{id}/cancel");
    let key = uuid::Uuid::now_v7().to_string();
    let body = json!({"reason":"operator_request","approval_id":null});
    let started = Instant::now();
    let (status, receipt) = post_json(&client, port, principal, &path, &key, &body, ctx).await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    let cancelled = wait_state(db, id, "CANCELLED").await;
    let elapsed = started.elapsed();
    assert!(
        workers.iter().all(|worker| !worker_still_present(worker)),
        "CANCELLED no deja procesos propios presentes: {:?}",
        workers
            .iter()
            .filter(|worker| worker_still_present(worker))
            .collect::<Vec<_>>()
    );
    assert!(
        !partial.exists(),
        "se retiró el parcial de la transferencia cancelada"
    );
    assert_eq!(std::fs::read(paused_file).unwrap(), preserved);
    assert_eq!(job_snapshot(db, paused_job_id).await.0, "PAUSED");
    let (cancelled_reservations,paused_reservations):(i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM volume_reservation WHERE job_id=?),(SELECT count(*) FROM volume_reservation WHERE job_id=?)")
        .bind(id).bind(paused_job_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!((cancelled_reservations, paused_reservations), (0, 1));
    let phase: String = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE id=? AND kind='download.plan'",
    )
    .bind(execute["plan_id"].as_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(phase, "CANCELLED");
    let published: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact_location WHERE artifact_id=?")
            .bind(artifact)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        published, 0,
        "el archivo incompleto no se publica en la biblioteca"
    );
    let unpaired:i64 = sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND o.event_record_id IS NULL").bind(id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(unpaired, 0);
    let requests = std::fs::read(data["requests"].as_str().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        std::fs::read(data["requests"].as_str().unwrap()).unwrap(),
        requests,
        "no hay nuevas lecturas después de cancelar"
    );
    let (replay_status, replay) =
        post_json(&client, port, principal, &path, &key, &body, ctx).await;
    assert_eq!(replay_status, reqwest::StatusCode::ACCEPTED);
    assert_eq!(replay, receipt);
    json!({"job_id":id,"state_before_request":before.0,"durable_checkpoint_before_request":before.2["artifacts"][artifact],"partial_bytes_before_request":initial_len.to_string(),"native_processes":workers,"native_processes_exited_at_terminal":true,"final_state":cancelled.0,"elapsed_milliseconds":elapsed.as_millis().to_string(),"partial_removed":true,"no_new_source_reads_after_terminal":true,"no_artifact_location_published":true,"paused_partial_unchanged":true,"paused_reservations":paused_reservations,"cancelled_reservations":cancelled_reservations,"journal_phase":phase,"unpaired_outbox_events":unpaired,"replay_unchanged":replay==receipt})
}

pub async fn exercise(
    db: &larm_db::pool::Db,
    data_root: &Path,
    data: &Value,
    installation_id: &str,
    location: &StorageLocation,
    flow: Flow,
) -> Value {
    let config_path = data_root.join("download-restart-config.json");
    live_config(&config_path);
    let mut principal = TemporaryPrincipal::enroll(data_root, &config_path);
    let mutation = MutationContext {
        actor_id: principal.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let ctx = &mutation;
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first = RunningDaemon::start(
        data_root,
        &config_path,
        port,
        data_root.join("download-first.log"),
    );
    first.wait_live(port, Duration::from_secs(45)).unwrap();
    set_bandwidth(port, &principal, Some(64 << 10)).await;
    let artifact_ids = seed_catalog(db, data, installation_id).await;
    let artifact_id = artifact_ids[0].clone();
    let expected = std::fs::read(data["expected_files"][0].as_str().unwrap()).unwrap();
    let expected_hash = hex::encode(Sha256::digest(&expected));
    let source_id = data["config"]["source_id"].as_str().unwrap();
    let request = DownloadPlanRequest {
        variant_id: data["catalog"]["entries"][0]["model"]["id"]
            .as_str()
            .unwrap()
            .into(),
        storage_location_id: Some(location.id.clone()),
        hard_budget_bytes: DecimalU64::new(expected.len().try_into().unwrap()),
        source_id: Some(source_id.into()),
        priority: larm_domain::download::DownloadPriority::Normal,
        optional_artifact_ids: vec![],
    };
    let client = reqwest::Client::new();
    let (_, preliminary) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(&request).unwrap(),
        ctx,
    )
    .await;
    let preliminary: DownloadPlanResponse =
        serde_json::from_value(preliminary).expect("preflight por HTTP");
    let preliminary_execution = ExecutePlan {
        plan_id: preliminary.plan_id.unwrap(),
        plan_hash: preliminary.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let (unqualified_status, unqualified) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(&preliminary_execution).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(
        unqualified_status,
        reqwest::StatusCode::CONFLICT,
        "gate debe rechazar el recibo de otro ejecutable: {unqualified}"
    );
    assert_eq!(
        unqualified["details"]["reason"],
        "conformance_context_changed"
    );
    let before_confirmation: i64 = sqlx::query_scalar("SELECT count(*) FROM download_job")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before_confirmation, 0);
    let conformance_job = qualify_from_daemon(db, port, installation_id, &principal, ctx).await;
    let (preview_status, response) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(&request).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(
        preview_status,
        reqwest::StatusCode::CREATED,
        "preflight HTTP: {response}"
    );
    let response: DownloadPlanResponse = serde_json::from_value(response).unwrap();
    let execute = ExecutePlan {
        plan_id: response.plan_id.unwrap(),
        plan_hash: response.plan_hash.unwrap(),
        operator_confirmed: true,
    };
    let rejected_body =
        json!({"plan_id":execute.plan_id,"plan_hash":execute.plan_hash,"operator_confirmed":false});
    let (unconfirmed_status, _) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &rejected_body,
        ctx,
    )
    .await;
    assert_eq!(unconfirmed_status, reqwest::StatusCode::BAD_REQUEST);
    let not_created: i64 = sqlx::query_scalar("SELECT count(*) FROM download_job")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let not_reserved: i64 = sqlx::query_scalar("SELECT count(*) FROM volume_reservation")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!((not_created, not_reserved), (0, 0));
    let key = uuid::Uuid::now_v7().to_string();
    let body = serde_json::to_value(&execute).unwrap();
    let (accepted_status, accepted) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &key,
        &body,
        ctx,
    )
    .await;
    assert_eq!(
        accepted_status,
        reqwest::StatusCode::ACCEPTED,
        "admisión HTTP: {accepted}"
    );
    let receipt: larm_domain::bridge::JobReceipt =
        serde_json::from_value(accepted.clone()).unwrap();
    let (replay_status, replay) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &key,
        &body,
        ctx,
    )
    .await;
    assert_eq!(replay_status, reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        replay, accepted,
        "mismo recibo aunque el consumer ya esté ejecutando el worker"
    );
    let created: i64 = sqlx::query_scalar("SELECT count(*) FROM download_job")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(created, 1);

    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let (state, _, progress, error) = job_snapshot(db, &receipt.job_id).await;
        let offset = progress["artifacts"][&artifact_id]["durable_offset"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        assert_ne!(
            state,
            "FAILED",
            "descarga falló antes de interrumpir: {error:?}; log={}",
            first.log_text()
        );
        if offset > 0 && offset < expected.len().try_into().unwrap() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sin checkpoint parcial; state={state}, progress={progress}; log={}",
            first.log_text()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stop_body = json!({"reason":"operator_request","approval_id":null});
    let pause_path = format!("/api/v1/downloads/{}/pause", receipt.job_id);
    let pause_key = uuid::Uuid::now_v7().to_string();
    let (pause_status, pause_receipt) = post_json(
        &client,
        port,
        &principal,
        &pause_path,
        &pause_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(
        pause_status,
        reqwest::StatusCode::ACCEPTED,
        "pausa: {pause_receipt}"
    );
    let (replay_status, pause_replay) = post_json(
        &client,
        port,
        &principal,
        &pause_path,
        &pause_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(replay_status, reqwest::StatusCode::ACCEPTED);
    assert_eq!(pause_replay, pause_receipt);
    let paused = wait_state(db, &receipt.job_id, "PAUSED").await;
    let paused_offset = paused.2["artifacts"][&artifact_id]["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let paused_file = Path::new(&location.canonical_path)
        .join("staging")
        .join(&receipt.job_id)
        .join(format!("{artifact_id}.partial"));
    assert_eq!(
        std::fs::metadata(&paused_file).unwrap().len(),
        paused_offset
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        job_snapshot(db, &receipt.job_id).await.2,
        paused.2,
        "la pausa conserva el checkpoint"
    );
    let paused_reservations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(paused_reservations, 1);

    let case = integrity::Case {
        db,
        data,
        daemon: &first,
        port,
        principal: &principal,
        request: &request,
        ctx,
        location,
        artifact_ids: &artifact_ids,
    };
    if matches!(flow, Flow::Ranges) {
        let proof = integrity::exercise_ranges(case, &receipt.job_id, &paused_file).await;
        let (status, _) = post_json(
            &client,
            port,
            &principal,
            &format!("/api/v1/downloads/{}/cancel", receipt.job_id),
            &uuid::Uuid::now_v7().to_string(),
            &stop_body,
            ctx,
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::ACCEPTED);
        wait_state(db, &receipt.job_id, "CANCELLED").await;
        assert!(!paused_file.exists());
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM volume_reservation")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        first.stop();
        assert!(principal.revoke());
        let report = json!({"scope":"qualified controlled reference source, real daemon API and HTTP 416; full-file verification, corrupt partial rejection and restart from zero; independent grant within 900s; no upstream qualification", "observed_at":larm_domain::bridge::timestamp(chrono::Utc::now()),"daemon_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),"conformance":conformance_job["result"]["conformance"],"job_id":receipt.job_id,"range_cases":proof,"paused_job_cancelled_after_checks":true,"reservations":remaining,"temporary_principal_revoked":true});
        let output = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("docs/reports/P051_runs/http416")
            .join(&receipt.job_id);
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        std::fs::copy(
            data["requests"].as_str().unwrap(),
            output.join("source-requests.ndjson"),
        )
        .unwrap();
        std::fs::copy(
            data_root.join("download-first.log"),
            output.join("download-first.log"),
        )
        .unwrap();
        return report;
    }
    let integrity_cases = integrity::exercise(case, &receipt.job_id, &paused_file).await;

    let active_cancellation = active_cancellation(
        ActiveCancellationCase {
            db,
            data,
            daemon: &first,
            port,
            principal: &principal,
            request: &request,
            ctx,
        },
        &receipt.job_id,
        &paused_file,
    )
    .await;

    let resume_path = format!("/api/v1/downloads/{}/resume", receipt.job_id);
    let resume_key = uuid::Uuid::now_v7().to_string();
    let (resume_status, resume_receipt) = post_json(
        &client,
        port,
        &principal,
        &resume_path,
        &resume_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(resume_status, reqwest::StatusCode::ACCEPTED);
    let (_, resume_replay) = post_json(
        &client,
        port,
        &principal,
        &resume_path,
        &resume_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(resume_replay, resume_receipt);
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        let current = job_snapshot(db, &receipt.job_id).await;
        let offset = current.2["artifacts"][&artifact_id]["durable_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        assert_ne!(current.0, "FAILED", "reanudación fallida: {current:?}");
        if offset > paused_offset && offset < expected.len() as u64 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sin avanzar tras reanudar: {current:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Un segundo job del mismo artefacto debe poder cancelarse sin tocar el
    // parcial ni la reserva de la descarga reanudada, con el único slot ocupado.
    let (cancel_preview_status, cancel_preview) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/download-plans",
        &uuid::Uuid::now_v7().to_string(),
        &serde_json::to_value(&request).unwrap(),
        ctx,
    )
    .await;
    assert_eq!(cancel_preview_status, reqwest::StatusCode::CREATED);
    let cancel_execute = json!({"plan_id":cancel_preview["plan_id"],"plan_hash":cancel_preview["plan_hash"],"operator_confirmed":true});
    let admission_started = Instant::now();
    let (cancel_create_status, cancel_created) = post_json(
        &client,
        port,
        &principal,
        "/api/v1/downloads",
        &uuid::Uuid::now_v7().to_string(),
        &cancel_execute,
        ctx,
    )
    .await;
    assert_eq!(
        cancel_create_status,
        reqwest::StatusCode::ACCEPTED,
        "crear job cancelable: {cancel_created}"
    );
    let admission_elapsed = admission_started.elapsed();
    assert!(admission_elapsed < Duration::from_secs(30));
    let cancel_id = cancel_created["job_id"].as_str().unwrap();
    assert_eq!(
        job_snapshot(db, cancel_id).await.0,
        "QUEUED",
        "el único worker de transferencia sigue ocupado"
    );
    let cancel_path = format!("/api/v1/downloads/{cancel_id}/cancel");
    let cancel_key = uuid::Uuid::now_v7().to_string();
    let cancellation_started = Instant::now();
    let (cancel_status, cancel_receipt) = post_json(
        &client,
        port,
        &principal,
        &cancel_path,
        &cancel_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(cancel_status, reqwest::StatusCode::ACCEPTED);
    let cancelled = wait_state(db, cancel_id, "CANCELLED").await;
    let cancellation_elapsed = cancellation_started.elapsed();
    let (_, cancel_replay) = post_json(
        &client,
        port,
        &principal,
        &cancel_path,
        &cancel_key,
        &stop_body,
        ctx,
    )
    .await;
    assert_eq!(cancel_replay, cancel_receipt);
    let cancelled_partial = Path::new(&location.canonical_path)
        .join("staging")
        .join(cancel_id)
        .join(format!("{artifact_id}.partial"));
    assert!(!cancelled_partial.exists());
    assert!(std::fs::metadata(&paused_file).unwrap().len() >= paused_offset);
    assert_eq!(job_snapshot(db, &receipt.job_id).await.0, "RUNNING");
    let cancelled_reservations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM volume_reservation WHERE job_id=?")
            .bind(cancel_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(cancelled_reservations, 0);
    let controls = json!({"pause_status":pause_status.as_u16(),"pause_replay_unchanged":pause_receipt==pause_replay,"paused_offset":paused_offset.to_string(),"paused_reservations":paused_reservations,"cancelled_job_id":cancel_id,"cancel_status":cancel_status.as_u16(),"cancelled_state":cancelled.0,"cancel_replay_unchanged":cancel_receipt==cancel_replay,"cancelled_reservations":cancelled_reservations,"cancelled_partial_removed":!cancelled_partial.exists(),"other_running_partial_preserved":true,"single_transfer_worker_occupied":true,"cancelled_from_queue":true,"api_request_timeout_seconds":30,"admission_elapsed_milliseconds":admission_elapsed.as_millis().to_string(),"cancellation_elapsed_milliseconds":cancellation_elapsed.as_millis().to_string(),"resume_status":resume_status.as_u16(),"resume_replay_unchanged":resume_receipt==resume_replay});
    let restart_bandwidth = integrity::set_bandwidth(port, &principal, None).await;
    first.stop();
    let (state, attempt, interrupted, _) = job_snapshot(db, &receipt.job_id).await;
    assert_eq!((state.as_str(), attempt), ("RUNNING", 2));
    let checkpoint = &interrupted["artifacts"][&artifact_id];
    let offset = checkpoint["durable_offset"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(offset > 0 && offset < expected.len().try_into().unwrap());
    assert_eq!(checkpoint["source_id"], source_id);
    let partial = Path::new(&location.canonical_path)
        .join("staging")
        .join(&receipt.job_id)
        .join(format!("{artifact_id}.partial"));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&partial)
        .unwrap();
    file.write_all(b"tail-not-acknowledged-in-sqlite").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let requests_path = Path::new(data["requests"].as_str().unwrap());
    let previous_requests = std::fs::read_to_string(requests_path)
        .unwrap()
        .lines()
        .count();
    config(&config_path, None);
    let mut restarted = RunningDaemon::start(
        data_root,
        &config_path,
        port,
        data_root.join("download-restarted.log"),
    );
    restarted.wait_live(port, Duration::from_secs(45)).unwrap();
    // Cada RPC conserva la revalidación física del paquete Python completo.
    // En debug puede tardar varios segundos por bloque aunque haya progreso.
    let deadline = Instant::now() + Duration::from_secs(180);
    let completed = loop {
        let snapshot = job_snapshot(db, &receipt.job_id).await;
        assert_ne!(
            snapshot.0,
            "FAILED",
            "descarga recuperada falló: {:?}; log={}",
            snapshot.3,
            restarted.log_text()
        );
        if snapshot.0 == "COMPLETED" {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "recuperación sin completar: {:?}; log={}",
            snapshot,
            restarted.log_text()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(completed.1, 3);
    assert_eq!(completed.2["recovered_after_restart"], true);
    assert!(completed.3.is_none());
    let requests = std::fs::read_to_string(requests_path)
        .unwrap()
        .lines()
        .skip(previous_requests)
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let resumed = requests
        .iter()
        .find(|request| request["path"] == "/artifact/0")
        .expect("petición HTTP real tras el reinicio");
    assert_eq!(resumed["range"], format!("bytes={offset}-"));
    assert_eq!(resumed["if_range"], format!("\"{expected_hash}\""));
    let relative: String =
        sqlx::query_scalar("SELECT relative_path FROM artifact_location WHERE artifact_id=?")
            .bind(&artifact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let actual = std::fs::read(Path::new(&location.canonical_path).join(&relative)).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(hex::encode(Sha256::digest(&actual)), expected_hash);
    let provenance: (String, String) = sqlx::query_as(
        "SELECT source_id,revision_key FROM artifact_provenance WHERE artifact_id=?",
    )
    .bind(&artifact_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(provenance.0, source_id);
    assert_eq!(
        provenance.1,
        data["catalog"]["entries"][0]["revision"]["revision_key"]
            .as_str()
            .unwrap()
    );
    let phase: String = sqlx::query_scalar(
        "SELECT phase FROM operation_journal WHERE id=? AND kind='download.plan'",
    )
    .bind(&execute.plan_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(phase, "COMPLETED");
    let reservations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(reservations, 0);
    let recovery_events:i64=sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=? AND event_type='download.state_changed' AND json_extract(payload,'$.action')='recovered_after_restart'").bind(&receipt.job_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(recovery_events, 1);
    let unpaired:i64=sqlx::query_scalar("SELECT count(*) FROM event e LEFT JOIN event_outbox o ON o.event_id=e.event_id WHERE e.resource_id=? AND o.event_id IS NULL").bind(&receipt.job_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(unpaired, 0);
    assert!(!partial.exists());
    restarted.stop();
    assert!(
        principal.revoke(),
        "revocar y eliminar la credencial temporal de la fixture"
    );
    let mut report = json!({"scope":"controlled source qualified through the daemon HTTP API/P022/P026, sealed preflight, transactional acceptance, two real daemon processes, HTTP Range, SQLite/staging/CAS/catalog; no upstream qualification", "observed_at":larm_domain::bridge::timestamp(chrono::Utc::now()),"host":std::env::var("COMPUTERNAME").unwrap_or_else(|_|std::env::consts::OS.into()),"daemon_sha256":hex::encode(Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_larm-daemon")).unwrap())),"conformance":conformance_job["result"]["conformance"],"job_id":receipt.job_id,"plan_id":execute.plan_id,"source_id":source_id,"artifact_id":artifact_id,"interrupted_checkpoint":checkpoint,"resumed_request":resumed,"final_state":completed.0,"attempt":completed.1,"sha256":expected_hash,"size_bytes":actual.len().to_string(),"provenance":{"source_id":provenance.0,"revision_key":provenance.1},"journal_phase":phase,"reservations":reservations,"recovery_events":recovery_events,"unpaired_outbox_events":unpaired,"temporary_principal_revoked":true});
    report["api_admission"] = json!({"preview_status":preview_status.as_u16(),"unqualified_status":unqualified_status.as_u16(),"unqualified_reason":unqualified["details"]["reason"],"unconfirmed_status":unconfirmed_status.as_u16(),"jobs_before_confirmation":not_created,"reservations_before_confirmation":not_reserved,"accepted_status":accepted_status.as_u16(),"same_key_replay_unchanged":replay==accepted,"jobs_created":created});
    report["controls"] = controls;
    report["active_cancellation"] = active_cancellation;
    report["integrity_cases"] = integrity_cases;
    report["restart_bandwidth"] = restart_bandwidth;
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/reports/P051_runs/daemon-restart")
        .join(&receipt.job_id);
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        output.join("resumed-requests.json"),
        serde_json::to_vec_pretty(&requests).unwrap(),
    )
    .unwrap();
    for name in ["download-first.log", "download-restarted.log"] {
        std::fs::copy(data_root.join(name), output.join(name)).unwrap();
    }
    report
}
