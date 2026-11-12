//! Worker fixture de protocolo P031, no motor ni instalador. Sólo datos de prueba.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::RpcError;
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use larm_domain::bridge::{timestamp, JobPhase};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::instances::{InstanceRef, LaunchPlan, LaunchRequest};
use larm_domain::launch::EnvironmentBinding;
use larm_domain::runtime_install::{
    ExecutePlan, InstallRequest, InstallStatusRequest, VersionQuery,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

enum RuntimeInstanceState {
    Synthetic { generation: u64 },
    Process(Box<RuntimeProcess>),
}

struct RuntimeProcess {
    generation: u64,
    child: Child,
    stdin: ChildStdin,
}

struct Fixture {
    manifest: AdapterManifest,
    plans: tokio::sync::Mutex<BTreeMap<String, String>>,
    launch_plans: tokio::sync::Mutex<BTreeMap<String, LaunchPlan>>,
    jobs: tokio::sync::Mutex<BTreeMap<String, (String, JobPhase)>>,
    instances: tokio::sync::Mutex<BTreeMap<String, RuntimeInstanceState>>,
}
fn failure(code: ErrorCode, reason: &str) -> RpcError {
    RpcError::domain(LarmError::new(code, "Fixture P031", reason))
}

async fn launch_fixture_process(
    plan: &LaunchPlan,
    instance_id: &str,
) -> Result<(RuntimeProcess, u32, String), RpcError> {
    let observation = Path::new(&plan.cwd).join(format!("p033-runtime-{instance_id}.json"));
    let mut command = Command::new(&plan.executable);
    command
        .arg("--runtime-child")
        .arg("--observation-file")
        .arg(&observation)
        .arg("--observed-env-keys")
        .arg(plan.env_allowlist.join(","))
        .arg("--")
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    for binding in &plan.environment.variables {
        match binding {
            EnvironmentBinding::Value { name, value } => {
                command.env(name, value);
            }
            EnvironmentBinding::Credential { name, .. } => {
                // Proveedor de secreto exclusivo de esta fixture. El valor no
                // aparece en el plan, en el protocolo ni en requests.jsonl.
                let value = std::env::var("LARM_FIXTURE_SECRET_VALUE")
                    .map_err(|_| failure(ErrorCode::Forbidden, "fixture_secret_unavailable"))?;
                command.env(name, value);
            }
        }
    }

    let mut child = command.spawn().map_err(|_| {
        failure(
            ErrorCode::ResourceUnavailable,
            "fixture_runtime_spawn_failed",
        )
    })?;
    let pid = child
        .id()
        .ok_or_else(|| failure(ErrorCode::IntegrityFailed, "fixture_runtime_pid_missing"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| failure(ErrorCode::IntegrityFailed, "fixture_runtime_stdin_missing"))?;
    let started_at = process_started_at(pid, Path::new(&plan.executable)).await?;
    Ok((
        RuntimeProcess {
            generation: 0,
            child,
            stdin,
        },
        pid,
        started_at,
    ))
}

async fn process_started_at(pid: u32, expected_exe: &Path) -> Result<String, RpcError> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let ids = [Pid::from_u32(pid)];
    let expected_exe = std::fs::canonicalize(expected_exe).map_err(|_| {
        failure(
            ErrorCode::IntegrityFailed,
            "fixture_runtime_exe_unavailable",
        )
    })?;
    let mut system = System::new();
    for _ in 0..100 {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        if let Some(process) = system.process(ids[0]) {
            let actual_exe = process
                .exe()
                .and_then(|path| std::fs::canonicalize(path).ok());
            if actual_exe.as_ref() != Some(&expected_exe) {
                return Err(failure(
                    ErrorCode::IntegrityFailed,
                    "fixture_runtime_exe_identity_mismatch",
                ));
            }
            let started = chrono::DateTime::<chrono::Utc>::from_timestamp(
                i64::try_from(process.start_time()).unwrap_or(i64::MAX),
                0,
            )
            .ok_or_else(|| failure(ErrorCode::IntegrityFailed, "fixture_runtime_start_invalid"))?;
            return Ok(timestamp(started));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(failure(
        ErrorCode::Timeout,
        "fixture_runtime_process_not_observed",
    ))
}

async fn runtime_child(args: &[String]) {
    let Some(observation) = args
        .iter()
        .position(|arg| arg == "--observation-file")
        .and_then(|index| args.get(index + 1))
        .map(Path::new)
    else {
        return;
    };
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        return;
    };
    let env_keys = args
        .iter()
        .position(|arg| arg == "--observed-env-keys")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_default();
    let environment: BTreeMap<_, _> = env_keys
        .split(',')
        .filter(|name| !name.is_empty())
        .filter_map(|name| std::env::var(name).ok().map(|value| (name, value)))
        .collect();
    let observed = json!({
        "args": &args[separator + 1..],
        "environment": environment,
        "memory_limit_bytes": null,
    });
    if let Ok(bytes) = serde_json::to_vec(&observed) {
        let _ = tokio::fs::write(observation, bytes).await;
    }

    let mut input = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    while input.read_line(&mut line).await.unwrap_or(0) > 0 {
        if line.trim() == "STOP" {
            break;
        }
        line.clear();
    }
}
#[async_trait]
impl AdapterHandler for Fixture {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        let mut trace = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("requests.jsonl")
            .unwrap();
        writeln!(trace, "{}", json!({"method":method,"params":params})).unwrap();
        match method {
            "runtime.launch_plan" => {
                let request: LaunchRequest = serde_json::from_value(params).unwrap();
                if request.launch_profile.memory_limit_bytes.is_some() {
                    return Err(failure(
                        ErrorCode::UnsupportedCapability,
                        "fixture_memory_limit_unsupported",
                    ));
                }
                let id = uuid::Uuid::now_v7().to_string();
                let effective_parameters = request.launch_profile;
                let environment = effective_parameters.environment.clone();
                let env_allowlist = environment.allowlist.clone();
                let executable = std::env::current_exe().unwrap();
                let cwd = std::env::current_dir().unwrap();
                let mut plan = json!({
                    "plan_id": id,
                    "executable": executable,
                    "args": effective_parameters.runtime_arguments,
                    "cwd": cwd,
                    "env_allowlist": env_allowlist,
                    "port": 24000,
                    "memory_estimate": {
                        "weights_bytes": null, "kv_bytes": null, "activation_bytes": null,
                        "workspace_bytes": null, "overhead_bytes": null, "total_bytes": null,
                        "per_pool": [], "confidence": "UNKNOWN", "evidence_refs": [],
                        "multimodal_bytes": null, "temporary_bytes": null, "safety_margin_bytes": null
                    },
                    "artifact_ids": [],
                    "expires_at": timestamp(chrono::Utc::now() + chrono::Duration::minutes(5)),
                    "effective_parameters": effective_parameters,
                    "environment": environment,
                    "effective_memory_limit_bytes": null
                });
                let hash = larm_domain::canonical::sha256_hex(&plan).unwrap();
                plan["plan_hash"] = json!(hash);
                self.plans.lock().await.insert(id.clone(), hash);
                let typed: LaunchPlan = serde_json::from_value(plan.clone()).unwrap();
                self.launch_plans.lock().await.insert(id, typed);
                Ok(plan)
            }
            "runtime.start" => {
                let request: ExecutePlan = serde_json::from_value(params).unwrap();
                if self.plans.lock().await.remove(&request.plan_id).as_deref()
                    != Some(&request.plan_hash)
                {
                    return Err(failure(ErrorCode::Conflict, "fixture_plan_not_found"));
                }
                let instance_id = uuid::Uuid::now_v7().to_string();
                let launch_plan = self
                    .launch_plans
                    .lock()
                    .await
                    .remove(&request.plan_id)
                    .ok_or_else(|| failure(ErrorCode::Conflict, "fixture_plan_not_found"))?;
                if std::env::var("LARM_FIXTURE_START_PROCESS").as_deref() == Ok("1") {
                    let (process, pid, process_started_at) =
                        launch_fixture_process(&launch_plan, &instance_id).await?;
                    self.instances.lock().await.insert(
                        instance_id.clone(),
                        RuntimeInstanceState::Process(Box::new(process)),
                    );
                    return Ok(
                        json!({"instance_id":instance_id,"generation":0,"pid":pid,"process_started_at":process_started_at}),
                    );
                }
                self.instances.lock().await.insert(
                    instance_id.clone(),
                    RuntimeInstanceState::Synthetic { generation: 0 },
                );
                Ok(
                    json!({"instance_id":instance_id,"generation":0,"pid":null,"process_started_at":null}),
                )
            }
            "runtime.load" => {
                let _: LaunchRequest = serde_json::from_value(params).unwrap();
                let instance_id = uuid::Uuid::now_v7().to_string();
                self.instances.lock().await.insert(
                    instance_id.clone(),
                    RuntimeInstanceState::Synthetic { generation: 0 },
                );
                Ok(
                    json!({"instance_id":instance_id,"generation":0,"pid":null,"process_started_at":null}),
                )
            }
            "runtime.health" => {
                let request: InstanceRef = serde_json::from_value(params).unwrap();
                let mut instances = self.instances.lock().await;
                match instances.get_mut(&request.instance_id) {
                    Some(RuntimeInstanceState::Synthetic { generation })
                        if *generation == request.generation =>
                    {
                        Ok(json!({
                            "state":"READY","backend_effective":null,"reason":"fixture_ready","evidence_refs":[]
                        }))
                    }
                    Some(RuntimeInstanceState::Process(process))
                        if process.generation == request.generation =>
                    {
                        match process.child.try_wait() {
                            Ok(None) => Ok(json!({
                                "state":"READY","backend_effective":null,"reason":"fixture_process_ready","evidence_refs":[]
                            })),
                            Ok(Some(_)) => Ok(json!({
                                "state":"FAILED","backend_effective":null,"reason":"fixture_process_exited","evidence_refs":[]
                            })),
                            Err(_) => Err(failure(
                                ErrorCode::ResourceUnavailable,
                                "fixture_process_probe_failed",
                            )),
                        }
                    }
                    Some(_) => Err(failure(ErrorCode::Conflict, "generation_mismatch")),
                    None => Err(failure(ErrorCode::NotFound, "instance_not_found")),
                }
            }
            "runtime.stop" => {
                let request: InstanceRef = serde_json::from_value(params).unwrap();
                let mut instances = self.instances.lock().await;
                match instances.get(&request.instance_id) {
                    Some(RuntimeInstanceState::Synthetic { generation })
                        if *generation == request.generation =>
                    {
                        instances.remove(&request.instance_id);
                        Ok(json!({"stopped":true}))
                    }
                    Some(RuntimeInstanceState::Process(process))
                        if process.generation == request.generation =>
                    {
                        let Some(RuntimeInstanceState::Process(mut process)) =
                            instances.remove(&request.instance_id)
                        else {
                            return Err(failure(
                                ErrorCode::IntegrityFailed,
                                "fixture_process_state_changed",
                            ));
                        };
                        drop(instances);
                        process.stdin.write_all(b"STOP\n").await.map_err(|_| {
                            failure(ErrorCode::ResourceUnavailable, "fixture_stop_write_failed")
                        })?;
                        process.stdin.flush().await.map_err(|_| {
                            failure(ErrorCode::ResourceUnavailable, "fixture_stop_flush_failed")
                        })?;
                        let status = process.child.wait().await.map_err(|_| {
                            failure(ErrorCode::ResourceUnavailable, "fixture_stop_wait_failed")
                        })?;
                        Ok(json!({"stopped":status.success()}))
                    }
                    Some(_) => Err(failure(ErrorCode::Conflict, "generation_mismatch")),
                    None => Err(failure(ErrorCode::NotFound, "instance_not_found")),
                }
            }
            "runtime.list_versions" => {
                let query: VersionQuery = serde_json::from_value(params).unwrap();
                // Control de fault injection sólo de esta fixture, dentro de work.
                let mode = std::fs::read_to_string("fixture-discovery-mode").unwrap_or_default();
                if mode == "fail-nightly"
                    && query.channel == larm_domain::runtime::RuntimeChannel::Nightly
                {
                    return Err(failure(
                        ErrorCode::Other("VENDOR_P031".into()),
                        "fixture_vendor_failure",
                    ));
                }
                if mode == "delay"
                    || matches!(query.cursor.as_deref(), Some("delay" | "vendor-delay"))
                {
                    ctx.progress("VERSION_QUERY", None, None, Some("fixture_wait"));
                    let rounds = if query.cursor.as_deref() == Some("vendor-delay") {
                        15
                    } else {
                        300
                    };
                    for _ in 0..rounds {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        if ctx.is_cancelled() {
                            return Err(failure(ErrorCode::Conflict, "fixture_runtime_cancelled"));
                        }
                    }
                    if query.cursor.as_deref() == Some("vendor-delay") {
                        return Err(failure(
                            ErrorCode::Other("VENDOR_P031".into()),
                            "fixture_vendor_failure",
                        ));
                    }
                }
                let at = timestamp(chrono::Utc::now());
                let metadata = if query.channel == larm_domain::runtime::RuntimeChannel::Custom {
                    json!({"fixture_only":true,"upstream_channel":{"value":"fixture-preview/42","reason":"UNKNOWN"}})
                } else {
                    json!({"fixture_only":true})
                };
                let runtime = if query.cursor.as_deref() == Some("wrong-runtime") {
                    uuid::Uuid::now_v7().to_string()
                } else {
                    query.runtime_id
                };
                Ok(
                    json!({"items":[{"id":uuid::Uuid::now_v7().to_string(),"created_at":at,"updated_at":at,"row_version":1,"metadata":metadata,"runtime_id":runtime,"version_string":"b7142-fixture","commit":null,"build":null,"channel":query.channel,"released_at":null,"artifact_hash":null,"promotion":"NONE"}],"next_cursor":null,"snapshot_at":at}),
                )
            }
            "runtime.install_plan" => {
                let request: InstallRequest = serde_json::from_value(params).unwrap();
                self.manifest
                    .require_strategy(request.strategy)
                    .map_err(RpcError::domain)?;
                let id = uuid::Uuid::now_v7().to_string();
                let mut plan = json!({"plan_id":id,"operation":"runtime.install","resource_ids":[request.runtime_id,request.storage_location_id],"steps":[{"phase":"FIXTURE_PLAN_ONLY","adapter_id":self.manifest.id,"request":{"strategy":request.strategy,"profile_lock_hash":request.profile_lock_hash}}],"expires_at":timestamp(chrono::Utc::now()+chrono::Duration::minutes(15)),"approval_required":false,"storage_estimate":{"download_bytes":null,"final_artifact_bytes":null,"installed_bytes":null,"derived_cache_bytes":null,"temporary_peak_bytes":null,"existing_bytes":null,"peak_extra_bytes":null,"required_free_bytes":null,"upper_bound_bytes":null,"confidence":"UNKNOWN","reason":"fixture_plan_only_no_installation","evidence_refs":[],"per_volume":[]}});
                let hash = larm_domain::canonical::sha256_hex(&plan).unwrap();
                plan["plan_hash"] = json!(hash);
                let mode = std::fs::read_to_string("fixture-install-plan-mode").unwrap_or_default();
                if mode == "bad-hash" {
                    plan["plan_hash"] = json!("0".repeat(64));
                } else if mode == "wrong-runtime" {
                    plan["resource_ids"][0] = json!(uuid::Uuid::now_v7().to_string());
                }
                self.plans.lock().await.insert(id, hash);
                Ok(plan)
            }
            "runtime.install" => {
                let request: ExecutePlan = serde_json::from_value(params).unwrap();
                if self.plans.lock().await.remove(&request.plan_id).as_deref()
                    != Some(&request.plan_hash)
                {
                    return Err(failure(ErrorCode::Conflict, "fixture_plan_not_found"));
                }
                // El fixture escribe un artefacto real en la zona derivada del
                // plan_id; el host mantiene el cwd P026 como raíz autorizada.
                let output = std::path::Path::new("runtime-installs")
                    .join(&request.plan_id)
                    .join("engine.bin");
                std::fs::create_dir_all(output.parent().unwrap()).unwrap();
                std::fs::write(&output, b"fake-managed-runtime").unwrap();
                let mode = std::fs::read_to_string("fixture-install-mode").unwrap_or_default();
                if mode == "crash" {
                    ctx.progress("INSTALLING", Some(1), Some(2), Some("fixture_crashing"));
                    std::process::exit(86);
                }
                if mode == "delay" {
                    ctx.progress("INSTALLING", Some(1), Some(2), Some("fixture_wait"));
                    for _ in 0..300 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        if ctx.is_cancelled() {
                            return Err(failure(
                                ErrorCode::Conflict,
                                "fixture_runtime_install_cancelled",
                            ));
                        }
                    }
                }
                let job_id = match mode.as_str() {
                    "invalid-job-id" => "not-a-uuidv7".into(),
                    "different-job-id" => uuid::Uuid::now_v7().to_string(),
                    _ => request.plan_id.clone(),
                };
                let status_url = if mode == "empty-status-url" {
                    String::new()
                } else if mode == "foreign-status-url" {
                    "https://worker.invalid/api/v1/jobs/other".into()
                } else {
                    format!("/api/v1/jobs/{}", request.plan_id)
                };
                let state = match mode.as_str() {
                    "completed" => "COMPLETED",
                    "failed" => "FAILED",
                    _ => "QUEUED",
                };
                let phase = match state {
                    "COMPLETED" => JobPhase::Completed,
                    "FAILED" => JobPhase::Failed,
                    "QUEUED" => JobPhase::Queued,
                    _ => {
                        return Err(failure(
                            ErrorCode::IntegrityFailed,
                            "fixture_job_state_invalid",
                        ));
                    }
                };
                self.jobs
                    .lock()
                    .await
                    .insert(request.plan_id.clone(), (request.plan_hash, phase));
                // Sólo recibo de protocolo: no instala y no se compone en el daemon.
                Ok(json!({"job_id":job_id,"status_url":status_url,"state":state}))
            }
            "runtime.install_status" => {
                let request: InstallStatusRequest = serde_json::from_value(params).unwrap();
                let jobs = self.jobs.lock().await;
                let Some((hash, state)) = jobs.get(&request.plan_id) else {
                    return Err(failure(
                        ErrorCode::NotFound,
                        "fixture_install_job_not_found",
                    ));
                };
                if hash != &request.plan_hash {
                    return Err(failure(
                        ErrorCode::Conflict,
                        "fixture_install_plan_hash_mismatch",
                    ));
                }
                Ok(
                    json!({"job_id":request.plan_id,"status_url":format!("/api/v1/jobs/{}", request.plan_id),"state":state}),
                )
            }
            _ => Err(failure(
                ErrorCode::UnsupportedCapability,
                "fixture_runtime_method_unavailable",
            )),
        }
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--runtime-child") {
        runtime_child(&args).await;
        return;
    }
    let manifest = args
        .iter()
        .position(|arg| arg == "--manifest")
        .and_then(|n| args.get(n + 1))
        .unwrap();
    let manifest = AdapterManifest::parse(&std::fs::read_to_string(manifest).unwrap()).unwrap();
    serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        Arc::new(Fixture {
            manifest,
            plans: Default::default(),
            launch_plans: Default::default(),
            jobs: Default::default(),
            instances: Default::default(),
        }),
    )
    .await
    .unwrap();
}
