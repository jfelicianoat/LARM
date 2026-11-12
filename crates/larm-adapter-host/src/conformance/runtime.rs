//! Suite funcional de la referencia runtime/echo. No cualifica inferencia ni
//! promete métodos no aplicables al launcher. Sólo acepta su perfil explícito.
use super::wire::{run_common, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeSession, NativeSuite};
use async_trait::async_trait;
use larm_adapter_sdk::contracts::Contracts;
use larm_adapter_sdk::manifest::MethodTable;
use larm_adapter_sdk::permissions::Access;
use larm_adapter_sdk::protocol::Request;
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const POSITIVE: [&str; 5] = [
    "runtime.list_versions",
    "runtime.launch_plan",
    "runtime.start",
    "runtime.health",
    "runtime.stop",
];
const CAPABILITY: &str = "vendor.reference.echo";
const PROFILE: &str = include_str!("../../../../adapters/runtime/reference/capabilities.json");
const REQUESTS: &str = include_str!("fixtures/reference-runtime.requests.json");

/// Configuración de una fixture controlada y ligada por el descriptor del host.
/// No es un DTO ni acepta pruebas/PASS del plugin o del cliente.
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub runtime_id: String,
    pub launch_request: Value,
    pub expected_version: String,
    pub forbidden_model: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct ReferenceRuntimeSuite {
    config: RuntimeConfig,
    unsupported: BTreeMap<String, Value>,
    profile: Value,
}
#[derive(Clone, Serialize)]
pub struct RuntimeCase {
    pub id: String,
    pub expected: Value,
    pub observed: Value,
    pub passed: bool,
}
#[derive(Serialize)]
pub struct RuntimeReport {
    pub run_id: String,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<RuntimeCase>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl RuntimeReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases.push(RuntimeCase {
            id: id.into(),
            expected,
            observed,
            passed,
        });
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_runtime_{id}"),
            ))
        }
    }
}
impl ReferenceRuntimeSuite {
    pub fn new(config: RuntimeConfig) -> Result<Self, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(&config.runtime_id)
            || config.expected_version.is_empty()
        {
            return Err(invalid("conformance_runtime_config_invalid"));
        }
        Contracts::shared()?.request("runtime.launch_plan", &config.launch_request)?;
        crate::package::check_managed_path(&config.forbidden_model)?;
        crate::package::check_managed_path(&config.evidence_root)?;
        let unsupported: BTreeMap<String, Value> = serde_json::from_str(REQUESTS)
            .map_err(|_| invalid("conformance_runtime_fixtures_invalid"))?;
        for (method, value) in &unsupported {
            Contracts::shared()?.request(method, value)?;
        }
        let expected: BTreeSet<_> = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "runtime" && !POSITIVE.contains(&m.method.as_str()))
            .map(|m| m.method.clone())
            .collect();
        if unsupported.keys().cloned().collect::<BTreeSet<_>>() != expected {
            return Err(invalid("conformance_runtime_fixtures_invalid"));
        }
        let profile = serde_json::from_str(PROFILE)
            .map_err(|_| invalid("conformance_runtime_profile_invalid"))?;
        Ok(Self {
            config,
            unsupported,
            profile,
        })
    }
    fn versions(&self) -> Value {
        json!({"runtime_id":self.config.runtime_id,"channel":"custom","cursor":null})
    }
    pub fn report_path(&self, run_id: &str) -> PathBuf {
        self.config.evidence_root.join(run_id).join("runtime.json")
    }
    async fn invoke(
        &self,
        session: &NativeSession,
        report: &mut RuntimeReport,
        method: &str,
        params: Value,
        access: &Access,
    ) -> Result<Value, LarmError> {
        Contracts::shared()?.request(method, &params)?;
        report.methods.insert(method.into());
        session
            .call(
                Request::new(method, params),
                access,
                Some(Duration::from_secs(15)),
                None,
            )
            .await
    }
    async fn refused(
        &self,
        session: &NativeSession,
        report: &mut RuntimeReport,
        method: &str,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<(), LarmError> {
        let result = self.invoke(session, report, method, params, access).await;
        let observed = match result {
            Err(error) => json!({"reason":error.details.reason}),
            Ok(_) => json!({"success":true}),
        };
        report.check(reason, json!({"reason":reason}), observed)
    }
    async fn interrupt(
        &self,
        session: &NativeSession,
        report: &mut RuntimeReport,
        crash: bool,
    ) -> Result<Value, LarmError> {
        let mut progress = session.progress()?;
        let request = Request::new("runtime.list_versions", self.versions());
        let id = request.id.clone();
        let access = Access::default();
        let call = session.call(request, &access, Some(Duration::from_secs(15)), None);
        tokio::pin!(call);
        let event = tokio::select! {
            event=progress.recv()=>event.map_err(|_|error(ErrorCode::ProtocolError,"runtime_probe_progress_missing"))?,
            _=&mut call=>return Err(error(ErrorCode::ValidationFailed,"runtime_probe_finished_before_checkpoint")),
        };
        report.check(
            if crash {
                "crash-progress"
            } else {
                "cancel-progress"
            },
            json!({"id":id,"phase":"VERSION_PROBE"}),
            json!({"id":event.request_id,"phase":event.phase}),
        )?;
        let children = owned_echo(session.package_dir())?;
        report.check(
            if crash {
                "crash-live-probe"
            } else {
                "cancel-live-probe"
            },
            json!(1),
            json!(children.len()),
        )?;
        let heartbeat = self
            .invoke(
                session,
                report,
                "core.heartbeat",
                json!({}),
                &Access::default(),
            )
            .await?;
        report.check(
            if crash {
                "crash-heartbeat"
            } else {
                "cancel-heartbeat"
            },
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&json!(id)))),
        )?;
        if crash {
            session.crash()?;
        } else {
            report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
        }
        let failure = call.await.err().ok_or_else(|| {
            error(
                ErrorCode::ValidationFailed,
                "runtime_interrupted_call_succeeded",
            )
        })?;
        if crash {
            report.check(
                "crash-inflight",
                json!("ADAPTER_CRASHED"),
                json!(failure.code.as_str()),
            )?;
        } else {
            report.check(
                "cancel-result",
                json!("request_cancelled"),
                json!(failure.details.reason),
            )?;
            report.check(
                "cancel-completed-id",
                json!(false),
                json!(session.cancel(&id).await?),
            )?;
        }
        no_echo(session.package_dir()).await?;
        report.check(
            if crash {
                "crash-no-child"
            } else {
                "cancel-no-child"
            },
            json!([]),
            json!(owned_echo(session.package_dir())?),
        )?;
        Ok(json!({"request_id":id,"owned_children_before":children,"owned_children_after":[]}))
    }
    async fn launch(
        &self,
        session: &NativeSession,
        report: &mut RuntimeReport,
        request: Value,
        access: &Access,
    ) -> Result<(Value, Value), LarmError> {
        let plan = self
            .invoke(
                session,
                report,
                "runtime.launch_plan",
                request.clone(),
                access,
            )
            .await?;
        let mut unsigned = plan.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("runtime_plan_invalid"))?
            .remove("plan_hash");
        report.check(
            "plan-commitment",
            json!(hash(&unsigned)?),
            plan["plan_hash"].clone(),
        )?;
        report.check(
            "effective-parameters",
            request["launch_profile"].clone(),
            plan["effective_parameters"].clone(),
        )?;
        report.check("memory-remains-unknown",json!({"total":null,"confidence":"UNKNOWN","limit":null}),
            json!({"total":plan["memory_estimate"]["total_bytes"],"confidence":plan["memory_estimate"]["confidence"],"limit":plan["effective_memory_limit_bytes"]}))?;
        let mut changed = execute(&plan);
        changed["plan_hash"] = json!("f".repeat(64));
        self.refused(
            session,
            report,
            "runtime.start",
            changed,
            access,
            "plan_hash_mismatch",
        )
        .await?;
        let started = self
            .invoke(session, report, "runtime.start", execute(&plan), access)
            .await?;
        let reference =
            json!({"instance_id":started["instance_id"],"generation":started["generation"]});
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let health = self
                .invoke(session, report, "runtime.health", reference.clone(), access)
                .await?;
            if health["state"] == "READY" {
                break;
            }
            if health["state"] == "FAILED" || tokio::time::Instant::now() >= deadline {
                return Err(error(
                    ErrorCode::RuntimeCrash,
                    "conformance_runtime_not_ready",
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = started["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| invalid("runtime_pid_missing"))?;
        report.check(
            "real-echo-image",
            json!(true),
            json!(owned_echo(session.package_dir())?
                .iter()
                .any(|p| p.pid == pid)),
        )?;
        self.refused(
            session,
            report,
            "runtime.start",
            execute(&plan),
            access,
            "plan_already_used",
        )
        .await?;
        let mut wrong = reference.clone();
        wrong["generation"] = json!(
            started["generation"]
                .as_u64()
                .ok_or_else(|| invalid("runtime_generation_invalid"))?
                + 1
        );
        self.refused(
            session,
            report,
            "runtime.health",
            wrong,
            access,
            "generation_mismatch",
        )
        .await?;
        Ok((reference, plan))
    }
    async fn functional(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut RuntimeReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "reference.echo"
            || session.manifest().capabilities != [CAPABILITY]
            || session.manifest().family.as_str() != "runtime"
        {
            return Err(invalid("conformance_runtime_subject_mismatch"));
        }
        let profile: Value = serde_json::from_slice(
            &std::fs::read(session.package_dir().join("reference-capabilities.json"))
                .map_err(super::io)?,
        )
        .map_err(|_| invalid("conformance_runtime_profile_invalid"))?;
        report.check("exact-echo-profile", self.profile.clone(), profile)?;
        let probe = WireProbe {
            method: "runtime.list_versions".into(),
            params: self.versions(),
            access: Access::default(),
        };
        let wire = run_common(run, session, &probe).await;
        let initial = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = initial?;
        session.restart().await?;
        no_echo(session.package_dir()).await?;
        let binary = session.package_dir().join("bin/echo.exe");
        let versions = self
            .invoke(
                session,
                report,
                "runtime.list_versions",
                self.versions(),
                &Access::default(),
            )
            .await?;
        report.check("observed-version-and-binary",json!({"version":self.config.expected_version,"sha256":file_digest(&binary)?,"channel":"custom","count":1}),
            json!({"version":versions["items"][0]["version_string"],"sha256":versions["items"][0]["artifact_hash"],"channel":versions["items"][0]["channel"],"count":versions["items"].as_array().map(Vec::len)}))?;
        for (method, params) in &self.unsupported {
            let response = self
                .invoke(session, report, method, params.clone(), &Access::default())
                .await;
            let observed = match response {
                Err(error) => json!({"code":error.code.as_str(),"reason":error.details.reason}),
                Ok(_) => json!({"success":true}),
            };
            report.check(method,json!({"code":"UNSUPPORTED_CAPABILITY","reason":"declarative_method_not_applicable"}),observed)?;
        }
        let cancel = self.interrupt(session, report, false).await?;
        report.checkpoints.insert("cancel".into(), cancel);
        let model = self.config.launch_request["launch_profile"]["adapter_options"]["model_path"]
            .as_str()
            .ok_or_else(|| invalid("runtime_model_missing"))?;
        let missing_model = Path::new(model).with_file_name(format!("p026-missing-{}.bin", run.id));
        let model = canonical(Path::new(model))?;
        if !model.starts_with(canonical(session.package_dir())?) {
            return Err(invalid("runtime_model_outside_reference_package"));
        }
        let access = Access {
            reads: vec![model],
            ..Access::default()
        };
        let port = self.config.launch_request["launch_profile"]["adapter_options"]["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("runtime_port_invalid"))?;
        port_closed(port).await?;
        let forbidden_hash = file_digest(&self.config.forbidden_model)?;
        let mut forbidden = self.config.launch_request.clone();
        forbidden["launch_profile"]["adapter_options"]["model_path"] =
            json!(self.config.forbidden_model);
        self.refused(
            session,
            report,
            "runtime.launch_plan",
            forbidden,
            &Access {
                reads: vec![self.config.forbidden_model.clone()],
                ..Access::default()
            },
            "read_denied",
        )
        .await?;
        report.check(
            "forbidden-model-unchanged",
            json!(forbidden_hash),
            json!(file_digest(&self.config.forbidden_model)?),
        )?;
        port_closed(port).await?;
        let mut missing = self.config.launch_request.clone();
        missing["launch_profile"]["adapter_options"]["model_path"] = json!(missing_model);
        self.refused(
            session,
            report,
            "runtime.launch_plan",
            missing,
            &access,
            "path_missing",
        )
        .await?;
        let mut limited = self.config.launch_request.clone();
        limited["launch_profile"]["memory_limit_bytes"] = json!("1024");
        self.refused(
            session,
            report,
            "runtime.launch_plan",
            limited,
            &access,
            "memory_limit_unsupported",
        )
        .await?;
        let (reference, _) = self
            .launch(session, report, self.config.launch_request.clone(), &access)
            .await?;
        let fixture = self.profile["entries"][0]["fixture"].clone();
        let echo = post_echo(port, &fixture["input"]).await?;
        report.check("actual-echo-fixture", fixture["output"].clone(), echo)?;
        let stopped = self
            .invoke(session, report, "runtime.stop", reference.clone(), &access)
            .await?;
        report.check("stop-result", json!({"stopped":true}), stopped)?;
        no_echo(session.package_dir()).await?;
        port_closed(port).await?;
        self.refused(
            session,
            report,
            "runtime.health",
            reference,
            &access,
            "instance_not_found",
        )
        .await?;
        let old_plan = self
            .invoke(
                session,
                report,
                "runtime.launch_plan",
                self.config.launch_request.clone(),
                &access,
            )
            .await?;
        let crash = self.interrupt(session, report, true).await?;
        report.checkpoints.insert("crash".into(), crash);
        session.restart().await?;
        self.refused(
            session,
            report,
            "runtime.start",
            execute(&old_plan),
            &access,
            "plan_not_found",
        )
        .await?;
        port_closed(port).await?;
        let (recovered, _) = self
            .launch(session, report, self.config.launch_request.clone(), &access)
            .await?;
        report.check(
            "recovered-echo",
            fixture["output"].clone(),
            post_echo(port, &fixture["input"]).await?,
        )?;
        self.invoke(session, report, "runtime.stop", recovered, &access)
            .await?;
        no_echo(session.package_dir()).await?;
        port_closed(port).await?;
        let expected = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "runtime")
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        report.check(
            "all-runtime-signatures",
            json!(expected),
            json!(report
                .methods
                .iter()
                .filter(|m| m.starts_with("runtime."))
                .collect::<BTreeSet<_>>()),
        )?;
        let evidence = json!({"run_sha256":report.run_sha256,"wire":report.wire,"cases":report.cases,"checkpoints":report.checkpoints,
            "scope":"reference echo; volatile plans/instances, no inference or Core job ledger","methods":report.methods});
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:runtime",
            "capability:vendor.reference.echo",
        ] {
            observations.push(Observation {
                check_id: check.into(),
                passed: true,
                evidence_sha256: hash(&json!({"check":check,"evidence":evidence}))?,
            });
        }
        Ok(observations)
    }
}
#[async_trait]
impl NativeSuite for ReferenceRuntimeSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"runtime","capabilities":[CAPABILITY],"suite":"reference.runtime.echo.v1",
            "runtime_id":self.config.runtime_id,"launch_request":self.config.launch_request,"expected_version":self.config.expected_version,
            "forbidden_model":self.config.forbidden_model,"evidence_root":self.config.evidence_root,
            "unsupported_requests":self.unsupported,"profile":self.profile})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut report = RuntimeReport {
            run_id: run.id.clone(),
            run_sha256: run.digest()?,
            scope: "reference runtime/echo full recipe; no inference/Core job ledger",
            wire: None,
            cases: vec![],
            methods: BTreeSet::new(),
            checkpoints: BTreeMap::new(),
            observations: Ok(vec![]),
        };
        report.observations = self.functional(run, session, &mut report).await;
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).map_err(|_| invalid("runtime_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}
fn execute(plan: &Value) -> Value {
    json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"approval_id":null})
}
#[derive(Serialize)]
struct OwnedProcess {
    pid: u32,
    start_time: u64,
}
fn owned_echo(package: &Path) -> Result<Vec<OwnedProcess>, LarmError> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let expected = canonical(&package.join("bin/echo.exe"))?;
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let mut found = Vec::new();
    for (pid, process) in system.processes() {
        // No accede al filesystem de ejecutables ajenos (incluidas rutas UNC).
        let Some(image) = process.exe() else {
            continue;
        };
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&path.to_string_lossy())
                .to_uppercase()
        };
        if normalize(image) != normalize(&expected) {
            continue;
        }
        if process
            .exe()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .as_ref()
            == Some(&expected)
        {
            found.push(OwnedProcess {
                pid: pid.as_u32(),
                start_time: process.start_time(),
            });
        }
    }
    found.sort_by_key(|p| p.pid);
    Ok(found)
}
async fn no_echo(package: &Path) -> Result<(), LarmError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if owned_echo(package)?.is_empty() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error(
                ErrorCode::AdapterCrashed,
                "runtime_owned_child_survived",
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn port_closed(port: u16) -> Result<(), LarmError> {
    // Un SYN descartado por el firewall no distingue cerrado de filtrado.
    // La fixture necesita poder enlazar su puerto, sin enviar tráfico ajeno.
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .map_err(|_| error(ErrorCode::Conflict, "runtime_fixture_port_busy"))?;
    drop(listener);
    Ok(())
}
async fn post_echo(port: u16, value: &Value) -> Result<Value, LarmError> {
    let body = serde_json::to_vec(value).map_err(|_| invalid("runtime_echo_fixture_invalid"))?;
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(super::io)?;
        let head=format!("POST /echo HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len());
        stream.write_all(head.as_bytes()).await.map_err(super::io)?;
        stream.write_all(&body).await.map_err(super::io)?;
        let mut raw = Vec::new();
        stream
            .take((1 << 20) + 1)
            .read_to_end(&mut raw)
            .await
            .map_err(super::io)?;
        if raw.len() > 1 << 20 {
            return Err(invalid("runtime_echo_response_too_large"));
        }
        let at = raw
            .windows(4)
            .position(|v| v == b"\r\n\r\n")
            .filter(|at| *at <= 16 << 10)
            .ok_or_else(|| invalid("runtime_echo_response_invalid"))?;
        let head = std::str::from_utf8(&raw[..at])
            .map_err(|_| invalid("runtime_echo_response_invalid"))?;
        let mut lines = head.split("\r\n");
        if lines.next().and_then(|line| line.split_whitespace().nth(1)) != Some("200") {
            return Err(invalid("runtime_echo_status_invalid"));
        }
        let mut length = None;
        let mut json_type = false;
        for line in lines {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| invalid("runtime_echo_response_invalid"))?;
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(invalid("runtime_echo_response_invalid"));
            }
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err(invalid("runtime_echo_response_invalid"));
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| invalid("runtime_echo_response_invalid"))?,
                );
            }
            if name.eq_ignore_ascii_case("content-type") {
                json_type = value.trim() == "application/json";
            }
        }
        if length != Some(raw.len() - at - 4) || !json_type {
            return Err(invalid("runtime_echo_response_invalid"));
        }
        serde_json::from_slice(&raw[at + 4..]).map_err(|_| invalid("runtime_echo_response_invalid"))
    };
    tokio::time::timeout(Duration::from_secs(3), exchange)
        .await
        .map_err(|_| error(ErrorCode::Timeout, "runtime_echo_timeout"))?
}
