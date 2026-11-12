//! Receta nativa protocol/echo. Prueba transporte HTTP, no inferencia ni leases
//! del Core. El proxy sólo inyecta fallos; se observa un backend Rust real.
use super::wire::{run_common, work_snapshot, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeSession, NativeSuite};
use async_trait::async_trait;
use larm_adapter_sdk::{
    contracts::Contracts, manifest::MethodTable, permissions::Access, protocol::Request,
};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CAP: &str = "vendor.reference.echo";
const MAPPING: &str = "reference.http.echo.v1";
const PROFILE: &str = include_str!("../../../../adapters/runtime/reference/capabilities.json");
const SCOPE: &str =
    "reference protocol HTTP/Rust echo transport; no inference or Core lease admission";
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolConfig {
    pub profile: Value,
    pub base_url: String,
    pub control: PathBuf,
    pub requests: PathBuf,
    pub backend_state: PathBuf,
    pub backend_binary: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct ReferenceProtocolSuite {
    config: ProtocolConfig,
    bundle: Value,
}
#[derive(Serialize)]
pub struct ProtocolReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl ProtocolReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_protocol_{id}"),
            ))
        }
    }
    fn material(&self) -> Value {
        json!({"run_sha256":self.run_sha256,"scope":self.scope,"wire":self.wire,"cases":self.cases,"methods":self.methods,"checkpoints":self.checkpoints})
    }
}
fn read_json(path: &Path) -> Result<Value, LarmError> {
    crate::package::check_managed_path(path)?;
    let bytes = std::fs::read(path).map_err(super::io)?;
    if bytes.len() > 1 << 20 {
        return Err(invalid("protocol_oracle_document_too_large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid("protocol_oracle_document_invalid"))
}
pub(super) fn process(pid: u32, binary: &Path) -> Result<Option<Value>, LarmError> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let ids = [Pid::from_u32(pid)];
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let Some(p) = system.process(ids[0]) else {
        return Ok(None);
    };
    let expected = canonical(binary)?;
    let normalize = |p: &Path| {
        p.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_uppercase()
    };
    let same = p
        .exe()
        .is_some_and(|image| normalize(image) == normalize(&expected))
        && p.exe().and_then(|p| std::fs::canonicalize(p).ok()).as_ref() == Some(&expected);
    Ok(Some(
        json!({"pid":pid,"start_time":p.start_time(),"expected_image":same}),
    ))
}
impl ReferenceProtocolSuite {
    pub fn new(config: ProtocolConfig) -> Result<Self, LarmError> {
        let names = config
            .profile
            .as_object()
            .ok_or_else(|| invalid("protocol_config_invalid"))?;
        if names.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != ["endpoint_id", "variant_id", "lease_id"]
                .into_iter()
                .collect()
            || names
                .values()
                .any(|v| !v.as_str().is_some_and(larm_domain::bridge::is_uuid_v7))
            || !config.base_url.starts_with("http://127.0.0.1:")
        {
            return Err(invalid("protocol_config_invalid"));
        }
        for path in [
            &config.control,
            &config.requests,
            &config.backend_state,
            &config.backend_binary,
            &config.forbidden,
            &config.evidence_root,
        ] {
            crate::package::check_managed_path(path)?;
        }
        Ok(Self {
            config,
            bundle: serde_json::from_str(PROFILE)
                .map_err(|_| invalid("protocol_profile_invalid"))?,
        })
    }
    pub fn report_path(&self, id: &str) -> PathBuf {
        self.config.evidence_root.join(id).join("protocol.json")
    }
    fn policy(&self) -> Value {
        json!({"base_url":self.config.base_url,"mapping":MAPPING,"max_request_bytes":16<<10,"max_response_bytes":32<<10,"timeout_seconds":2})
    }
    fn describe(&self) -> Value {
        json!({"endpoint_id":self.config.profile["endpoint_id"]})
    }
    fn qualify(&self) -> Value {
        json!({"endpoint_id":self.config.profile["endpoint_id"],"variant_id":self.config.profile["variant_id"],"suite_id":MAPPING,"fixture_hash":self.bundle["entries"][0]["fixture_sha256"]})
    }
    fn invoke(&self) -> Value {
        json!({"lease_id":self.config.profile["lease_id"],"request_id":uuid::Uuid::now_v7().to_string(),"capability":CAP,"input":self.bundle["entries"][0]["fixture"]["input"],"generation":null,"stream":false})
    }
    fn access(&self) -> Access {
        Access {
            urls: vec![
                format!("{}/health", self.config.base_url),
                format!("{}/echo", self.config.base_url),
            ],
            ..Access::default()
        }
    }
    fn mode(&self, mode: &str, release: bool, running: bool) -> Result<(), LarmError> {
        if ![
            "normal", "auth", "rate", "redirect", "invalid", "wrong", "large", "held", "type",
            "length",
        ]
        .contains(&mode)
        {
            return Err(invalid("protocol_mode_invalid"));
        }
        let tmp = self.config.control.with_extension("tmp");
        crate::package::check_managed_path(&tmp)?;
        std::fs::write(
            &tmp,
            serde_json::to_vec(&json!({"mode":mode,"release":release,"backend_running":running}))
                .map_err(|_| invalid("protocol_mode_invalid"))?,
        )
        .map_err(super::io)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match std::fs::rename(&tmp, &self.config.control) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(super::io(error)),
            }
        }
    }
    fn requests(&self) -> Result<Vec<Value>, LarmError> {
        crate::package::check_managed_path(&self.config.requests)?;
        let bytes = std::fs::read(&self.config.requests).map_err(super::io)?;
        if bytes.len() > 1 << 20 {
            return Err(invalid("protocol_requests_too_large"));
        }
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).map_err(|_| invalid("protocol_requests_invalid")))
            .collect()
    }
    async fn backend(&self, running: bool) -> Result<Value, LarmError> {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let state = read_json(&self.config.backend_state)?;
                if state["running"] == running {
                    return Ok(state);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| invalid("protocol_backend_state_timeout"))?
    }
    async fn call(
        &self,
        session: &NativeSession,
        report: &mut ProtocolReport,
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
        report: &mut ProtocolReport,
        method: &str,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<LarmError, LarmError> {
        match self.call(session, report, method, params, access).await {
            Err(e) => {
                report.check(reason, json!(reason), json!(e.details.reason))?;
                Ok(e)
            }
            Ok(_) => {
                report.check(reason, json!(reason), json!("unexpected success"))?;
                Err(invalid("protocol_unexpected_success"))
            }
        }
    }
    fn fingerprint(&self, report: &mut ProtocolReport, fp: &Value) -> Result<(), LarmError> {
        report
            .checkpoints
            .insert(format!("fingerprint-{}", report.cases.len()), fp.clone());
        report.check(
            "fingerprint-jcs",
            json!(hash(&fp["components"])?),
            fp["hash"].clone(),
        )?;
        report.check("backend-unknown-preserved",json!({"status":"UNAVAILABLE","value":null}),json!({"status":fp["components"]["backend_identity"]["status"],"value":fp["components"]["backend_identity"]["value"]}))?;
        report.check("transport-scope-declared",json!({"value":"controlled_reference_http_transport","status":"DECLARED"}),json!({"value":fp["components"]["scope"]["value"],"status":fp["components"]["scope"]["status"]}))
    }
    async fn echo(
        &self,
        session: &NativeSession,
        report: &mut ProtocolReport,
        params: Value,
    ) -> Result<(), LarmError> {
        let answer = self
            .call(
                session,
                report,
                "protocol.invoke",
                params.clone(),
                &self.access(),
            )
            .await?;
        report.check("actual-echo-and-semantic-id",json!({"request_id":params["request_id"],"output":{"echo":params["input"]},"capability":CAP}),json!({"request_id":answer["request_id"],"output":answer["output"],"capability":answer["capability"]}))?;
        report.check("unknown-tokens-real-latency",json!({"input":null,"output":null,"nonnegative":true}),json!({"input":answer["usage"]["tokens_input"],"output":answer["usage"]["tokens_output"],"nonnegative":answer["usage"]["latency_ms"].as_f64().is_some_and(|v|v>=0.0)}))?;
        self.fingerprint(report, &answer["fingerprint"])?;
        report.checkpoints.insert(
            format!(
                "invoke-{}",
                params["request_id"].as_str().unwrap_or("invalid")
            ),
            answer,
        );
        Ok(())
    }
    async fn interrupted(
        &self,
        session: &NativeSession,
        report: &mut ProtocolReport,
        how: &str,
    ) -> Result<Value, LarmError> {
        self.mode("held", false, true)?;
        let mut progress = session.progress()?;
        let params = self.invoke();
        let semantic = params["request_id"].clone();
        let request = Request::new("protocol.invoke", params.clone());
        let rpc = request.id.clone();
        let access = self.access();
        let before = self.requests()?.len();
        let start = tokio::time::Instant::now();
        let pending = session.call(request, &access, Some(Duration::from_secs(15)), None);
        tokio::pin!(pending);
        report.methods.insert("protocol.invoke".into());
        let event = tokio::select! {event=progress.recv()=>event.map_err(|_|invalid("protocol_progress_missing"))?,_=&mut pending=>return Err(invalid("protocol_finished_before_progress"))};
        report.check(
            "real-http-progress",
            json!({"id":rpc,"phase":"PROTOCOL_HTTP"}),
            json!({"id":event.request_id,"phase":event.phase}),
        )?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.requests()?.len() > before {
                    return Ok::<_, LarmError>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| invalid("protocol_http_not_observed"))??;
        let heartbeat = self
            .call(
                session,
                report,
                "core.heartbeat",
                json!({}),
                &Access::default(),
            )
            .await?;
        report.check(
            "inflight-heartbeat",
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&json!(rpc)))),
        )?;
        if how == "semantic" {
            let count = self.requests()?.len();
            self.refused(
                session,
                report,
                "protocol.invoke",
                params.clone(),
                &access,
                "reference_protocol_request_active",
            )
            .await?;
            report.check(
                "duplicate-no-extra-http",
                json!(count),
                json!(self.requests()?.len()),
            )?;
            let cancelled = self
                .call(
                    session,
                    report,
                    "protocol.cancel",
                    json!({"request_id":semantic}),
                    &Access::default(),
                )
                .await?;
            report.check("semantic-cancel", json!({"cancelled":true}), cancelled)?;
            self.mode("normal", true, true)?;
        } else if how == "core" {
            report.check(
                "core-cancel",
                json!(true),
                json!(session.cancel(&rpc).await?),
            )?;
            self.mode("normal", true, true)?;
        } else if how == "crash" {
            session.crash()?;
            self.mode("normal", true, true)?;
        }
        let failure = pending
            .await
            .err()
            .ok_or_else(|| invalid("protocol_interrupted_call_succeeded"))?;
        let elapsed = start.elapsed().as_millis() as u64;
        if how == "crash" {
            report.check(
                "worker-crash",
                json!("ADAPTER_CRASHED"),
                json!(failure.code.as_str()),
            )?;
        } else if how == "timeout" {
            report.check("actual-http-timeout",json!({"code":"UPSTREAM_UNAVAILABLE","reason":"reference_protocol_network_failed","retryable":true,"delayed":true}),json!({"code":failure.code.as_str(),"reason":failure.details.reason,"retryable":failure.retryable,"delayed":elapsed>=1000}))?;
            self.mode("normal", true, true)?;
        } else {
            report.check(
                "cancel-result",
                json!("operation_cancelled"),
                json!(failure.details.reason),
            )?;
            let cancelled = self
                .call(
                    session,
                    report,
                    "protocol.cancel",
                    json!({"request_id":semantic}),
                    &Access::default(),
                )
                .await?;
            report.check(
                "completed-semantic-cancel",
                json!({"cancelled":false}),
                cancelled,
            )?;
            report.check(
                "completed-core-cancel",
                json!(false),
                json!(session.cancel(&rpc).await?),
            )?;
        }
        report.checkpoints.insert(how.into(),json!({"rpc_id":rpc,"semantic_id":semantic,"elapsed_ms":elapsed,"http_requests_observed":self.requests()?.len()}));
        Ok(params)
    }
    async fn run(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut ProtocolReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "reference.protocol.echo"
            || session.manifest().family.as_str() != "protocol"
            || session.manifest().capabilities != [CAP]
        {
            return Err(invalid("protocol_subject_mismatch"));
        }
        report.check(
            "exact-capability-profile",
            self.bundle.clone(),
            read_json(&session.package_dir().join("reference-capabilities.json"))?,
        )?;
        report.check(
            "exact-policy",
            self.policy(),
            read_json(&session.package_dir().join("reference-protocol.json"))?,
        )?;
        report.check(
            "exact-instance-profile",
            self.config.profile.clone(),
            read_json(&session.work_dir().join("profile.json"))?,
        )?;
        for path in [
            &self.config.control,
            &self.config.requests,
            &self.config.backend_state,
            &self.config.forbidden,
        ] {
            let path = canonical(path)?;
            if path.starts_with(canonical(session.work_dir())?)
                || path.starts_with(canonical(session.package_dir())?)
            {
                return Err(invalid("protocol_oracle_resource_exposed"));
            }
        }
        let before_work = work_snapshot(session.work_dir())?;
        let sentinel = file_digest(&self.config.forbidden)?;
        self.mode("normal", true, true)?;
        let backend = self.backend(true).await?;
        let pid = backend["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| invalid("protocol_backend_pid_invalid"))?;
        let observed = process(pid, &self.config.backend_binary)?
            .ok_or_else(|| invalid("protocol_backend_missing"))?;
        report.check(
            "actual-rust-backend",
            json!(true),
            observed["expected_image"].clone(),
        )?;
        report.checkpoints.insert("backend-initial".into(),json!({"state":backend,"process":observed,"binary_sha256":file_digest(&self.config.backend_binary)?}));
        let wire = run_common(
            run,
            session,
            &WireProbe {
                method: "protocol.describe".into(),
                params: self.describe(),
                access: Access::default(),
            },
        )
        .await;
        let initial = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = initial?;
        session.restart().await?;
        let access = self.access();
        let description = self
            .call(
                session,
                report,
                "protocol.describe",
                self.describe(),
                &Access::default(),
            )
            .await?;
        let profile_hash = hash(
            &json!({"policy":self.policy(),"profile":self.config.profile,"registry_hash":hash(&self.bundle)?}),
        )?;
        let entry = &self.bundle["entries"][0];
        report.check("description-exact-bindings",json!({"protocol":MAPPING,"profile_hash":profile_hash,"capabilities":[CAP],"streaming":false,"tools":false,
            "request_schema_id":format!("urn:larm:capability:{CAP}:1.0:input:sha256:{}",entry["input_sha256"].as_str().unwrap_or("invalid")),
            "response_schema_id":format!("urn:larm:capability:{CAP}:1.0:output:sha256:{}",entry["output_sha256"].as_str().unwrap_or("invalid"))}),description)?;
        let before = self.requests()?.len();
        self.refused(
            session,
            report,
            "protocol.describe",
            json!({"endpoint_id":uuid::Uuid::now_v7().to_string()}),
            &Access::default(),
            "reference_protocol_endpoint_not_bound",
        )
        .await?;
        for (field, value, reason) in [
            (
                "lease_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "reference_protocol_lease_not_bound",
            ),
            (
                "capability",
                json!("vendor.not_registered"),
                "capability_descriptor_missing",
            ),
            ("input", json!({"text":4}), "capability_input_invalid"),
            (
                "stream",
                json!(true),
                "reference_protocol_options_not_supported",
            ),
            (
                "generation",
                json!({"temperature":null,"top_p":null,"seed":null,"max_output_tokens":null}),
                "reference_protocol_options_not_supported",
            ),
        ] {
            let mut params = self.invoke();
            params[field] = value;
            self.refused(session, report, "protocol.invoke", params, &access, reason)
                .await?;
        }
        self.refused(
            session,
            report,
            "protocol.invoke",
            self.invoke(),
            &Access {
                urls: vec!["http://127.0.0.1:1/foreign".into()],
                ..Access::default()
            },
            "network_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "protocol.describe",
            self.describe(),
            &Access {
                reads: vec![self.config.forbidden.clone()],
                ..Access::default()
            },
            "read_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "protocol.invoke",
            self.invoke(),
            &Access {
                secrets: vec!["larm/ungranted".into()],
                ..Access::default()
            },
            "secret_denied",
        )
        .await?;
        report.check(
            "rejections-before-http",
            json!(before),
            json!(self.requests()?.len()),
        )?;
        self.echo(session, report, self.invoke()).await?;
        let qualified = self
            .call(session, report, "protocol.qualify", self.qualify(), &access)
            .await?;
        report.check("qualify-actual-pinned-fixture",json!({"qualified":true,"tests":2,"passed":true}),json!({"qualified":qualified["qualified"],"tests":qualified["tests"].as_array().map(Vec::len),"passed":qualified["tests"].as_array().is_some_and(|v|v.iter().all(|t|t["status"]=="PASS"))}))?;
        self.fingerprint(report, &qualified["fingerprint"])?;
        let mut wrong = self.qualify();
        wrong["fixture_hash"] = json!("b".repeat(64));
        let before = self.requests()?.len();
        self.refused(
            session,
            report,
            "protocol.qualify",
            wrong,
            &access,
            "reference_protocol_fixture_not_bound",
        )
        .await?;
        report.check(
            "fixture-mismatch-before-http",
            json!(before),
            json!(self.requests()?.len()),
        )?;
        for (mode, reason) in [
            ("auth", "reference_protocol_auth_required"),
            ("rate", "reference_protocol_rate_limited"),
            ("redirect", "reference_protocol_redirect_denied"),
            ("invalid", "reference_protocol_json_invalid"),
            ("wrong", "capability_output_invalid"),
            ("large", "reference_protocol_response_budget_exceeded"),
            ("type", "reference_protocol_http_reply_invalid"),
            ("length", "reference_protocol_reply_length_invalid"),
        ] {
            self.mode(mode, true, true)?;
            let failure = self
                .refused(
                    session,
                    report,
                    "protocol.invoke",
                    self.invoke(),
                    &access,
                    reason,
                )
                .await?;
            report.check(
                "private-upstream-body-not-leaked",
                json!(false),
                json!(serde_json::to_string(&failure)
                    .map_err(|_| invalid("protocol_error_invalid"))?
                    .contains("private-upstream-fixture")),
            )?;
            if mode == "rate" {
                let value = serde_json::to_value(&failure)
                    .map_err(|_| invalid("protocol_error_invalid"))?;
                let original = value["details"]["original_error"]["message"]
                    .as_str()
                    .ok_or_else(|| invalid("protocol_retry_after_missing"))?;
                let original: Value = serde_json::from_str(original)
                    .map_err(|_| invalid("protocol_retry_after_invalid"))?;
                report.check("rate-limit-preserved",json!({"retryable":true,"seconds":"900"}),json!({"retryable":failure.retryable,"seconds":original["retry_after_seconds"]}))?;
            }
        }
        self.mode("wrong", true, true)?;
        let bad = self
            .call(session, report, "protocol.qualify", self.qualify(), &access)
            .await?;
        report.check("wrong-output-not-qualified",json!({"qualified":false,"has_fail":true}),json!({"qualified":bad["qualified"],"has_fail":bad["tests"].as_array().is_some_and(|v|v.iter().any(|t|t["status"]=="FAIL"))}))?;
        for how in ["semantic", "core", "timeout"] {
            let params = self.interrupted(session, report, how).await?;
            self.echo(session, report, params).await?;
        }
        let crashed = self.interrupted(session, report, "crash").await?;
        session.restart().await?;
        let cancelled = self
            .call(
                session,
                report,
                "protocol.cancel",
                json!({"request_id":crashed["request_id"]}),
                &Access::default(),
            )
            .await?;
        report.check(
            "crash-volatile-map-cleared",
            json!({"cancelled":false}),
            cancelled,
        )?;
        self.echo(session, report, crashed).await?;
        self.mode("normal", true, false)?;
        let stopped = self.backend(false).await?;
        report.check(
            "backend-process-ended",
            json!(true),
            json!(process(pid, &self.config.backend_binary)?.is_none()),
        )?;
        let unavailable = self
            .call(session, report, "protocol.invoke", self.invoke(), &access)
            .await
            .err()
            .ok_or_else(|| invalid("protocol_stopped_backend_succeeded"))?;
        report.check("stopped-backend-unavailable",json!({"code":"UPSTREAM_UNAVAILABLE","retryable":true,"reason_valid":true}),
            json!({"code":unavailable.code.as_str(),"retryable":unavailable.retryable,
                "reason_valid":(["reference_protocol_http_error","reference_protocol_network_failed"].contains(&unavailable.details.reason.as_str()))}))?;
        report.checkpoints.insert(
            "stopped-backend-error".into(),
            serde_json::to_value(unavailable).map_err(|_| invalid("protocol_error_invalid"))?,
        );
        self.mode("normal", true, true)?;
        let recovered = self.backend(true).await?;
        let new_pid = recovered["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid("protocol_backend_pid_invalid"))?;
        let new_process = process(new_pid, &self.config.backend_binary)?
            .ok_or_else(|| invalid("protocol_backend_missing"))?;
        report.check(
            "backend-restarted-image",
            json!(true),
            new_process["expected_image"].clone(),
        )?;
        report.check(
            "backend-new-instance",
            json!(true),
            json!(recovered["epoch"].as_u64() > stopped["epoch"].as_u64()),
        )?;
        report.checkpoints.insert(
            "backend-recovery".into(),
            json!({"stopped":stopped,"recovered":recovered,"process":new_process}),
        );
        self.echo(session, report, self.invoke()).await?;
        let requests = self.requests()?;
        report.check(
            "requests-only-expected-paths",
            json!(true),
            json!(requests
                .iter()
                .all(|r| r["path"] == "/echo" || r["path"] == "/health")),
        )?;
        report.check(
            "no-auth-header",
            json!(true),
            json!(requests.iter().all(|r| r["authorization_present"] == false)),
        )?;
        let body_hash = hash(&entry["fixture"]["input"])?;
        report.check(
            "fixture-request-bytes-observed",
            json!(true),
            json!(requests
                .iter()
                .any(|r| r["path"] == "/echo" && r["body_sha256"] == body_hash)),
        )?;
        report
            .checkpoints
            .insert("http-requests".into(), json!(requests));
        report.check(
            "work-state-unchanged",
            json!(before_work),
            json!(work_snapshot(session.work_dir())?),
        )?;
        report.check(
            "forbidden-sentinel-unchanged",
            json!(sentinel),
            json!(file_digest(&self.config.forbidden)?),
        )?;
        let methods = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "protocol")
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        report.check(
            "all-protocol-signatures",
            json!(methods),
            json!(report
                .methods
                .iter()
                .filter(|m| m.starts_with("protocol."))
                .collect::<BTreeSet<_>>()),
        )?;
        let material = report.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:protocol",
            "capability:vendor.reference.echo",
        ] {
            observations.push(Observation {
                check_id: check.into(),
                passed: true,
                evidence_sha256: hash(&json!({"check":check,"evidence":material}))?,
            });
        }
        Ok(observations)
    }
}
#[async_trait]
impl NativeSuite for ReferenceProtocolSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"protocol","capabilities":[CAP],"suite":"reference.protocol.echo.v1","profile":self.config.profile,"bundle":self.bundle,"policy":self.policy(),
        "control":self.config.control,"requests":self.config.requests,"backend_state":self.config.backend_state,"backend_binary":self.config.backend_binary,"forbidden":self.config.forbidden,"evidence_root":self.config.evidence_root})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut report = ProtocolReport {
            run: run.clone(),
            run_sha256: run.digest()?,
            scope: SCOPE,
            wire: None,
            cases: vec![],
            methods: BTreeSet::new(),
            checkpoints: BTreeMap::new(),
            observations: Ok(vec![]),
        };
        report.observations = self.run(run, session, &mut report).await;
        let restored = self.mode("normal", true, true);
        if report.observations.is_ok() {
            restored?;
        }
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&report).map_err(|_| invalid("protocol_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}
