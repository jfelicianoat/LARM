//! Medición nativa de HTTP/echo con muestras y ledger reales. No mide inferencia
//! ni convierte datos desconocidos de backend/modelo/hardware en verificados.
use super::protocol::process;
use super::wire::{run_common, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};
use async_trait::async_trait;
use larm_adapter_sdk::{
    contracts::Contracts, manifest::MethodTable, permissions::Access, protocol::Request,
};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CAP: &str = "vendor.reference.echo";
const BUNDLE: &str = include_str!("../../../../adapters/runtime/reference/capabilities.json");
const SCOPE:&str="reference HTTP echo measurement, durable samples/private ledger; no inference quality or Core benchmark admission";
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    pub profile: Value,
    pub base_url: String,
    pub control: PathBuf,
    pub requests: PathBuf,
    pub backend_state: PathBuf,
    pub backend_binary: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct ReferenceBenchmarkSuite {
    config: BenchmarkConfig,
    bundle: Value,
}
#[derive(Serialize)]
pub struct BenchmarkReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl BenchmarkReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_benchmark_{id}"),
            ))
        }
    }
    fn material(&self) -> Value {
        json!({"run_sha256":self.run_sha256,"scope":self.scope,"wire":self.wire,"cases":self.cases,"methods":self.methods,"checkpoints":self.checkpoints})
    }
}
fn suite() -> Value {
    json!({"id":"reference.http.echo.benchmark.v1","version":"1.0","capability":CAP,"warmup":1,"repetitions":5,"metric":"end_to_end_http_ms","clock":"monotonic_ns"})
}
fn settings() -> Value {
    json!({"warmup":1,"repetitions":5,"seed":42,"temperature":0,"top_p":1,"batch":1,"benchmark_timeout_seconds":1800,"io_timeout_seconds":2,"generation_settings_applied":false})
}
fn unavailable() -> Value {
    json!({"load_ms":"NOT_MEASURED","ttft_ms":"UNSUPPORTED_NON_STREAMING","input_tokens":"NOT_EXPOSED_BY_ECHO","output_tokens":"NOT_EXPOSED_BY_ECHO","generation_duration_ms":"NOT_APPLICABLE","peak_ram_bytes":"BACKEND_PROCESS_NOT_OBSERVED","peak_vram_bytes":"BACKEND_PROCESS_NOT_OBSERVED"})
}
fn read_json(path: &Path) -> Result<Value, LarmError> {
    crate::package::check_managed_path(path)?;
    let raw = std::fs::read(path).map_err(super::io)?;
    if raw.len() > 1 << 20 {
        return Err(invalid("benchmark_oracle_document_too_large"));
    }
    serde_json::from_slice(&raw).map_err(|_| invalid("benchmark_oracle_document_invalid"))
}
fn db_error(_: sqlx::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "conformance_benchmark_ledger_io",
    )
}
struct LedgerIo(tokio::sync::Mutex<Option<SqliteConnection>>);
#[async_trait]
impl NativeIo for LedgerIo {
    async fn close_io(&self) -> Result<(), LarmError> {
        if let Some(db) = self.0.lock().await.take() {
            db.close().await.map_err(db_error)?;
        }
        Ok(())
    }
}
struct Restore<'a> {
    path: PathBuf,
    bytes: &'a [u8],
}
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, self.bytes);
    }
}
async fn snapshot(db: &mut SqliteConnection) -> Result<Value, LarmError> {
    let mut result = serde_json::Map::new();
    for(name,query)in[
        ("ref_run","SELECT json_group_array(json_array(id,phase,request,report,report_hash,error,version)) FROM (SELECT * FROM ref_run ORDER BY rowid)"),
        ("ref_sample","SELECT json_group_array(json_array(run_id,iteration,sample,observation)) FROM (SELECT * FROM ref_sample ORDER BY rowid)"),
        ("ref_journal","SELECT json_group_array(json_array(run_id,phase,path)) FROM (SELECT * FROM ref_journal ORDER BY rowid)"),
        ("ref_event","SELECT json_group_array(json_array(id,run_id,version,phase,occurred_at)) FROM (SELECT * FROM ref_event ORDER BY rowid)"),
        ("ref_outbox","SELECT json_group_array(json_array(event_id,delivered_at)) FROM (SELECT * FROM ref_outbox ORDER BY rowid)"),
        ("ref_lock","SELECT json_group_array(json_array(run_id)) FROM (SELECT * FROM ref_lock ORDER BY rowid)"),
        ("ref_reservation","SELECT json_group_array(json_array(run_id,total_bytes,remaining_bytes)) FROM (SELECT * FROM ref_reservation ORDER BY rowid)"),
    ]{let raw:String=sqlx::query_scalar(query).fetch_one(&mut *db).await.map_err(db_error)?;result.insert(name.into(),serde_json::from_str(&raw).map_err(|_|invalid("benchmark_snapshot_invalid"))?);}
    Ok(Value::Object(result))
}
async fn held(db: &mut SqliteConnection, id: &str) -> Result<i64, LarmError> {
    sqlx::query_scalar("SELECT (SELECT count(*) FROM ref_lock WHERE run_id=?)+(SELECT count(*) FROM ref_reservation WHERE run_id=?)").bind(id).bind(id).fetch_one(db).await.map_err(db_error)
}
async fn active(db: &mut SqliteConnection) -> Result<String, LarmError> {
    sqlx::query_scalar("SELECT id FROM ref_run WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1")
        .fetch_one(db)
        .await
        .map_err(db_error)
}
fn delta(sample: &Value) -> Result<u64, LarmError> {
    let start = sample["start_monotonic_ns"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| invalid("benchmark_clock_invalid"))?;
    let end = sample["end_monotonic_ns"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| invalid("benchmark_clock_invalid"))?;
    end.checked_sub(start)
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("benchmark_clock_not_positive"))
}
fn statistics(samples: &[Value]) -> Result<Value, LarmError> {
    let mut values = Vec::new();
    for s in samples {
        if s["warmup"] == false && s["success"] == true {
            values.push(delta(s)? as f64 / 1_000_000.0);
        }
    }
    if values.len() != 5 {
        return Ok(Value::Null);
    }
    let mean = values.iter().sum::<f64>() / 5.0;
    let sd = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 4.0).sqrt();
    let spread = 2.776 * sd / 5.0_f64.sqrt();
    values.sort_by(f64::total_cmp);
    Ok(
        json!({"count":5,"mean":mean,"median":values[2],"p95":values[4],"sample_stddev":sd,"ci95_mean":{"lower":(mean-spread).max(0.0),"upper":mean+spread},"unit":"ms"}),
    )
}
fn near(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(a, b)| (a - b).abs() <= 1e-10_f64.max(b.abs() * 1e-10)),
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|other| near(v, other)))
        }
        _ => a == b,
    }
}
impl ReferenceBenchmarkSuite {
    pub fn new(config: BenchmarkConfig) -> Result<Self, LarmError> {
        let keys = config
            .profile
            .as_object()
            .ok_or_else(|| invalid("benchmark_config_invalid"))?;
        if keys.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != [
                "variant_id",
                "runtime_version_id",
                "backend_id",
                "launch_profile_id",
            ]
            .into_iter()
            .collect()
            || keys
                .values()
                .any(|v| !v.as_str().is_some_and(larm_domain::bridge::is_uuid_v7))
            || !config.base_url.starts_with("http://127.0.0.1:")
        {
            return Err(invalid("benchmark_config_invalid"));
        }
        for p in [
            &config.control,
            &config.requests,
            &config.backend_state,
            &config.backend_binary,
            &config.forbidden,
            &config.evidence_root,
        ] {
            crate::package::check_managed_path(p)?;
        }
        Ok(Self {
            config,
            bundle: serde_json::from_str(BUNDLE)
                .map_err(|_| invalid("benchmark_bundle_invalid"))?,
        })
    }
    pub fn report_path(&self, id: &str) -> PathBuf {
        self.config.evidence_root.join(id).join("benchmark.json")
    }
    fn policy(&self) -> Value {
        json!({"base_url":self.config.base_url,"suite":suite(),"fixture":self.bundle["entries"][0]["fixture"],"settings":settings()})
    }
    fn request(&self) -> Result<Value, LarmError> {
        let mut v = self.config.profile.clone();
        v["suite_id"] = suite()["id"].clone();
        v["fixture_hash"] = json!(hash(&self.bundle["entries"][0]["fixture"])?);
        v["settings_hash"] = json!(hash(&settings())?);
        Ok(v)
    }
    fn access(&self, s: &NativeSession) -> Access {
        Access {
            reads: ["profile", "state", "output"]
                .into_iter()
                .map(|p| s.work_dir().join(p))
                .collect(),
            writes: ["state", "output"]
                .into_iter()
                .map(|p| s.work_dir().join(p))
                .collect(),
            urls: vec![format!("{}/echo", self.config.base_url)],
            ..Access::default()
        }
    }
    fn mode(
        &self,
        mode: &str,
        release: bool,
        running: bool,
        hold_after: Option<usize>,
    ) -> Result<(), LarmError> {
        if ![
            "normal", "auth", "rate", "redirect", "invalid", "wrong", "large", "held", "type",
            "length",
        ]
        .contains(&mode)
        {
            return Err(invalid("benchmark_mode_invalid"));
        }
        let tmp = self.config.control.with_extension("tmp");
        crate::package::check_managed_path(&tmp)?;
        std::fs::write(&tmp,serde_json::to_vec(&json!({"mode":mode,"release":release,"backend_running":running,"hold_after":hold_after})).map_err(|_|invalid("benchmark_mode_invalid"))?).map_err(super::io)?;
        std::fs::rename(tmp, &self.config.control).map_err(super::io)
    }
    fn events(&self) -> Result<Vec<Value>, LarmError> {
        crate::package::check_managed_path(&self.config.requests)?;
        let bytes = std::fs::read(&self.config.requests).map_err(super::io)?;
        if bytes.len() > 1 << 20 {
            return Err(invalid("benchmark_http_log_too_large"));
        }
        let mut result = Vec::new();
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if line.last() != Some(&b'\n') {
                break;
            }
            result.push(
                serde_json::from_slice(line).map_err(|_| invalid("benchmark_http_log_invalid"))?,
            );
        }
        Ok(result)
    }
    fn request_count(&self) -> Result<usize, LarmError> {
        Ok(self
            .events()?
            .iter()
            .filter(|v| v["kind"] == "request")
            .count())
    }
    async fn backend(&self, running: bool) -> Result<Value, LarmError> {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let v = read_json(&self.config.backend_state)?;
                if v["running"] == running {
                    return Ok(v);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| invalid("benchmark_backend_state_timeout"))?
    }
    async fn call(
        &self,
        s: &NativeSession,
        r: &mut BenchmarkReport,
        method: &str,
        params: Value,
        access: &Access,
    ) -> Result<Value, LarmError> {
        Contracts::shared()?.request(method, &params)?;
        r.methods.insert(method.into());
        s.call(
            Request::new(method, params),
            access,
            Some(Duration::from_secs(30)),
            None,
        )
        .await
    }
    async fn refused(
        &self,
        s: &NativeSession,
        r: &mut BenchmarkReport,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<(), LarmError> {
        let observed = match self.call(s, r, "benchmark.run", params, access).await {
            Err(e) => json!(e.details.reason),
            Ok(_) => json!("unexpected success"),
        };
        r.check(reason, json!(reason), observed)
    }
    async fn state(
        &self,
        db: &mut SqliteConnection,
        r: &mut BenchmarkReport,
        id: &str,
        phase: &str,
        resources: i64,
    ) -> Result<Value, LarmError> {
        let row = sqlx::query("SELECT phase,error,report_hash,version FROM ref_run WHERE id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let error: Option<String> = row.get(1);
        let value = json!({"phase":row.get::<String,_>(0),"error":error.map(|v|serde_json::from_str::<Value>(&v)).transpose().map_err(|_|invalid("benchmark_error_invalid"))?,"report_hash":row.get::<Option<String>,_>(2),"version":row.get::<i64,_>(3)});
        r.check(
            "ledger-phase-resources",
            json!({"phase":phase,"held":resources}),
            json!({"phase":value["phase"],"held":held(db,id).await?}),
        )?;
        let journal: String = sqlx::query_scalar("SELECT phase FROM ref_journal WHERE run_id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        r.check("journal-matches-phase", json!(phase), json!(journal))?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM ref_event),(SELECT count(*) FROM ref_outbox)",
        )
        .fetch_one(&mut *db)
        .await
        .map_err(db_error)?;
        r.check("event-outbox-pairing", json!(counts.0), json!(counts.1))?;
        r.checkpoints
            .insert(format!("state-{id}-{phase}"), value.clone());
        Ok(value)
    }
    async fn validate(
        &self,
        s: &NativeSession,
        r: &mut BenchmarkReport,
        value: Value,
        valid: bool,
    ) -> Result<Value, LarmError> {
        let result = self
            .call(s, r, "benchmark.validate", value, &self.access(s))
            .await?;
        r.check("validation-verdict", json!(valid), result["valid"].clone())?;
        Ok(result)
    }
    async fn observe(
        &self,
        s: &NativeSession,
        db: &mut SqliteConnection,
        r: &mut BenchmarkReport,
        report: &Value,
        before: usize,
    ) -> Result<(), LarmError> {
        let id = report["benchmark_id"]
            .as_str()
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .ok_or_else(|| invalid("benchmark_id_invalid"))?;
        self.state(db, r, id, "COMPLETED", 0).await?;
        let folder = s.work_dir().join("output").join(id);
        let path = folder.join("observations.json");
        let raw = read_json(&path)?;
        r.check(
            "published-report-equals-returned",
            json!(hash(report)?),
            json!(hash(&read_json(&folder.join("report.json"))?)?),
        )?;
        let plain = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
        let uri = url::Url::from_file_path(plain)
            .map_err(|_| invalid("benchmark_artifact_uri_invalid"))?;
        r.check(
            "owned-observation-uri",
            json!([uri.as_str()]),
            report["artifact_refs"].clone(),
        )?;
        r.check(
            "observation-file-sha",
            json!(file_digest(&path)?),
            report["metrics"]["observation_artifact_sha256"].clone(),
        )?;
        r.check(
            "raw-samples-match-artifact",
            report["raw_samples"].clone(),
            raw["samples"].clone(),
        )?;
        r.check("raw-binding-suite-settings",json!({"id":id,"binding":self.request()?,"suite":suite(),"settings":settings(),"fixture_hash":hash(&self.bundle["entries"][0]["fixture"])?}),json!({"id":raw["benchmark_id"],"binding":raw["binding"],"suite":raw["suite"],"settings":raw["settings"],"fixture_hash":raw["fixture_hash"]}))?;
        let samples = report["raw_samples"]
            .as_array()
            .ok_or_else(|| invalid("benchmark_samples_invalid"))?;
        r.check("warmup-and-five-measured",json!({"iterations":[0,1,2,3,4,5],"warmup":[true,false,false,false,false,false]}),json!({"iterations":samples.iter().map(|s|s["iteration"].clone()).collect::<Vec<_>>(),"warmup":samples.iter().map(|s|s["warmup"].clone()).collect::<Vec<_>>()}))?;
        let events = self.events()?;
        let requests = events
            .iter()
            .filter(|v| {
                v["kind"] == "request" && v["seq"].as_u64().is_some_and(|n| n >= before as u64)
            })
            .collect::<Vec<_>>();
        r.check("six-actual-http-requests", json!(6), json!(requests.len()))?;
        let observations = raw["observations"]
            .as_array()
            .ok_or_else(|| invalid("benchmark_observations_invalid"))?;
        let mut last = 0u64;
        for (index, sample) in samples.iter().enumerate() {
            let duration = delta(sample)?;
            let start = sample["start_monotonic_ns"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| invalid("benchmark_clock_invalid"))?;
            r.check("sample-monotonic-order", json!(true), json!(start >= last))?;
            last = sample["end_monotonic_ns"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| invalid("benchmark_clock_invalid"))?;
            r.check(
                "unmeasured-stays-null",
                json!(true),
                json!(unavailable()
                    .as_object()
                    .ok_or_else(|| invalid("benchmark_unavailable_invalid"))?
                    .keys()
                    .all(|key| sample[key].is_null())),
            )?;
            r.check(
                "sample-backend-scope",
                json!("REFERENCE_HTTP_ECHO"),
                sample["backend_effective"].clone(),
            )?;
            let observation = observations
                .get(index)
                .ok_or_else(|| invalid("benchmark_observation_missing"))?;
            let request = requests
                .get(index)
                .ok_or_else(|| invalid("benchmark_request_missing"))?;
            r.check("request-path-body-no-auth",json!({"method":"POST","path":"/echo","body":hash(&self.bundle["entries"][0]["fixture"]["input"])? ,"auth":false}),json!({"method":request["method"],"path":request["path"],"body":request["body_sha256"],"auth":request["authorization_present"]}))?;
            let response = events
                .iter()
                .find(|v| v["kind"] == "response" && v["seq"] == request["seq"]);
            if !observation["wire_sha256"].is_null() {
                let response =
                    response.ok_or_else(|| invalid("benchmark_response_not_observed"))?;
                r.check(
                    "wire-sha-independent-proxy",
                    response["wire_sha256"].clone(),
                    observation["wire_sha256"].clone(),
                )?;
            }
            if sample["success"] == true {
                let response =
                    response.ok_or_else(|| invalid("benchmark_response_not_observed"))?;
                r.check(
                    "success-observed-fixture",
                    self.bundle["entries"][0]["fixture"]["output"].clone(),
                    observation["response"].clone(),
                )?;
                let proxy_duration = response["duration_ns"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or_else(|| invalid("benchmark_proxy_clock_invalid"))?;
                r.check(
                    "sample-encloses-server-processing",
                    json!(true),
                    json!(duration >= proxy_duration),
                )?;
            } else {
                r.check(
                    "private-failed-body-not-persisted",
                    Value::Null,
                    observation["response"].clone(),
                )?;
            }
            let pair: (String, String) = sqlx::query_as(
                "SELECT sample,observation FROM ref_sample WHERE run_id=? AND iteration=?",
            )
            .bind(id)
            .bind(index as i64)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
            r.check(
                "sample-ledger-and-artifact",
                json!([sample, observation]),
                json!([
                    serde_json::from_str::<Value>(&pair.0)
                        .map_err(|_| invalid("benchmark_sample_invalid"))?,
                    serde_json::from_str::<Value>(&pair.1)
                        .map_err(|_| invalid("benchmark_sample_invalid"))?
                ]),
            )?;
        }
        let stats = statistics(samples)?;
        r.check(
            "independent-rust-statistics",
            json!(true),
            json!(near(&report["metrics"]["latency_ms"], &stats)),
        )?;
        let failed = samples.iter().filter(|v| v["success"] != true).count();
        let measured = samples
            .iter()
            .filter(|v| v["success"] == true && v["warmup"] == false)
            .count();
        r.check("metrics-unknown-not-inference",json!({"success":measured,"failed":failed,"unavailable":unavailable(),"generation":false,"eligible":false}),json!({"success":report["metrics"]["successful_repetitions"],"failed":report["metrics"]["failed_samples"],"unavailable":report["metrics"]["unavailable"],"generation":report["metrics"]["generation_settings_applicable"],"eligible":report["metrics"]["inference_qualification_eligible"]}))?;
        r.check(
            "suite-status-from-observations",
            json!(if failed > 0 {
                "FAIL"
            } else if stats.is_null() {
                "INCONCLUSIVE"
            } else {
                "PASS"
            }),
            report["status"].clone(),
        )?;
        let components = report["fingerprint"]["components"]
            .as_object()
            .ok_or_else(|| invalid("benchmark_fingerprint_invalid"))?;
        let identity = components
            .iter()
            .map(|(k, v)| (k.clone(), json!({"value":v["value"],"status":v["status"]})))
            .collect::<BTreeMap<_, _>>();
        r.check(
            "fingerprint-identity-jcs",
            json!(hash(&identity)?),
            report["fingerprint"]["hash"].clone(),
        )?;
        for name in [
            "artifact_bundle_hash",
            "revision",
            "runtime_id",
            "runtime_version",
            "installation_hash",
            "backend",
            "driver_versions",
            "hardware_fingerprint",
            "launch_profile_hash",
            "tokenizer_hash",
            "chat_template_hash",
            "effective_generation",
            "artifact_storage_volume_keys",
        ] {
            r.check("fingerprint-unavailable-preserved",json!({"value":null,"status":"UNAVAILABLE"}),json!({"value":report["fingerprint"]["components"][name]["value"],"status":report["fingerprint"]["components"][name]["status"]}))?;
        }
        let owner_hash = hash(
            &json!({"profile":self.config.profile,"policy":self.policy(),"registry":hash(&self.bundle)?,"package":s.manifest().package_sha256}),
        )?;
        r.check("fingerprint-measurement-scope",json!({"scope":"controlled_reference_http_measurement","declared":"DECLARED","observer":owner_hash,"protocol":if samples.iter().any(|v|v["success"]==true){"VERIFIED"}else{"DECLARED"}}),json!({"scope":report["fingerprint"]["components"]["scope"]["value"],"declared":report["fingerprint"]["components"]["scope"]["status"],"observer":report["fingerprint"]["components"]["observer_profile_hash"]["value"],"protocol":report["fingerprint"]["components"]["protocol"]["status"]}))?;
        r.checkpoints.insert(
            format!("measurement-{id}"),
            json!({"report":report,"artifact":raw,"rust_statistics":stats,"first_http_seq":before}),
        );
        Ok(())
    }
    async fn capture(
        &self,
        s: &NativeSession,
        db: &mut SqliteConnection,
        r: &mut BenchmarkReport,
    ) -> Result<Value, LarmError> {
        let before = self.request_count()?;
        let result = self
            .call(s, r, "benchmark.run", self.request()?, &self.access(s))
            .await?;
        self.observe(s, db, r, &result, before).await?;
        Ok(result)
    }
    async fn interrupted(
        &self,
        s: &mut NativeSession,
        db: &mut SqliteConnection,
        r: &mut BenchmarkReport,
        how: &str,
    ) -> Result<String, LarmError> {
        let before = self.request_count()?;
        self.mode("normal", false, true, Some(before + 1))?;
        let mut progress = s.progress()?;
        let request = Request::new("benchmark.run", self.request()?);
        let rpc = request.id.clone();
        let access = self.access(s);
        r.methods.insert("benchmark.run".into());
        let mut pending = Box::pin(s.call(request, &access, Some(Duration::from_secs(30)), None));
        let event = loop {
            tokio::select! {event=progress.recv()=>{let e=event.map_err(|_|invalid("benchmark_progress_missing"))?;if e.phase=="BENCHMARK_CAPTURE"{break e;}},_=&mut pending=>return Err(invalid("benchmark_finished_before_durable_sample"))}
        };
        r.check(
            "durable-capture-progress-id",
            json!(rpc),
            json!(event.request_id),
        )?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.events()?.iter().any(|v| {
                    v["kind"] == "request"
                        && v["mode"] == "held"
                        && v["seq"].as_u64().is_some_and(|n| n > before as u64)
                }) {
                    return Ok::<_, LarmError>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| invalid("benchmark_held_http_not_observed"))??;
        let id = active(db).await?;
        let samples: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_sample WHERE run_id=?")
            .bind(&id)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        r.check(
            "one-durable-sample-before-interrupt",
            json!(1),
            json!(samples),
        )?;
        let heartbeat = self
            .call(s, r, "core.heartbeat", json!({}), &Access::default())
            .await?;
        r.check(
            "capture-heartbeat",
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|v| v.contains(&json!(rpc)))),
        )?;
        if how == "eof" {
            drop(pending);
            s.restart().await?;
            self.mode("normal", true, true, None)?;
        } else {
            if how == "cancel" {
                r.check("cancel-ack", json!(true), json!(s.cancel(&rpc).await?))?;
                self.mode("normal", true, true, None)?;
            } else if how == "crash" {
                s.crash()?;
                self.mode("normal", true, true, None)?;
            } else if how == "collision" {
                let folder = s.work_dir().join("output").join(&id);
                crate::package::check_managed_path(&folder)?;
                std::fs::create_dir(&folder).map_err(super::io)?;
                std::fs::write(folder.join("sentinel.txt"), b"owned collision sentinel")
                    .map_err(super::io)?;
                self.mode("normal", true, true, None)?;
            } else {
                return Err(invalid("benchmark_interrupt_invalid"));
            }
            let failure = pending
                .await
                .err()
                .ok_or_else(|| invalid("benchmark_interrupted_capture_succeeded"))?;
            r.check(
                if how == "crash" {
                    "worker-crash"
                } else if how == "collision" {
                    "publication-collision"
                } else {
                    "cancel-result"
                },
                if how == "crash" {
                    json!("ADAPTER_CRASHED")
                } else if how == "collision" {
                    json!("reference_benchmark_io_failed")
                } else {
                    json!("operation_cancelled")
                },
                if how == "crash" {
                    json!(failure.code.as_str())
                } else {
                    json!(failure.details.reason)
                },
            )?;
        }
        r.checkpoints.insert(how.into(),json!({"rpc_id":rpc,"benchmark_id":id,"durable_samples":samples,"first_http_seq":before}));
        Ok(id)
    }
    async fn functional(
        &self,
        s: &mut NativeSession,
        db: &mut SqliteConnection,
        r: &mut BenchmarkReport,
    ) -> Result<(), LarmError> {
        let access = self.access(s);
        let description = self
            .call(s, r, "benchmark.describe", json!({}), &Access::default())
            .await?;
        r.check("description-bound-suite",json!({"suite":suite()["id"],"sha":hash(&suite())?,"capabilities":[CAP]}),json!({"suite":description["suite_id"],"sha":description["suite_hash"],"capabilities":description["capabilities"]}))?;
        let before = snapshot(db).await?;
        let requests = self.request_count()?;
        let sentinel = file_digest(&self.config.forbidden)?;
        for (key, value, reason) in [
            (
                "variant_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "reference_benchmark_resource_not_bound",
            ),
            (
                "backend_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "reference_benchmark_resource_not_bound",
            ),
            (
                "suite_id",
                json!("LLM_TEXT"),
                "reference_benchmark_suite_not_supported",
            ),
            (
                "fixture_hash",
                json!("a".repeat(64)),
                "reference_benchmark_hash_not_bound",
            ),
            (
                "settings_hash",
                json!("b".repeat(64)),
                "reference_benchmark_hash_not_bound",
            ),
        ] {
            let mut request = self.request()?;
            request[key] = value;
            self.refused(s, r, request, &access, reason).await?;
        }
        self.refused(
            s,
            r,
            self.request()?,
            &Access {
                reads: vec![self.config.forbidden.clone()],
                ..Access::default()
            },
            "read_denied",
        )
        .await?;
        self.refused(
            s,
            r,
            self.request()?,
            &Access {
                writes: vec![s.work_dir().join("profile/profile.json")],
                ..Access::default()
            },
            "write_denied",
        )
        .await?;
        self.refused(
            s,
            r,
            self.request()?,
            &Access {
                urls: vec!["http://127.0.0.1:1/foreign".into()],
                ..Access::default()
            },
            "network_denied",
        )
        .await?;
        self.refused(
            s,
            r,
            self.request()?,
            &Access {
                secrets: vec!["larm/ungranted".into()],
                ..Access::default()
            },
            "secret_denied",
        )
        .await?;
        r.check("rejections-before-ledger", before, snapshot(db).await?)?;
        r.check(
            "rejections-before-http",
            json!(requests),
            json!(self.request_count()?),
        )?;
        let first = self.capture(s, db, r).await?;
        let count = self.request_count()?;
        self.validate(s, r, first.clone(), true).await?;
        r.check(
            "validate-no-extra-http",
            json!(count),
            json!(self.request_count()?),
        )?;
        for case in 0..5 {
            let mut bad = first.clone();
            match case {
                0 => bad["metrics"]["latency_ms"]["mean"] = json!(999),
                1 => bad["benchmark_id"] = json!(uuid::Uuid::now_v7().to_string()),
                2 => bad["settings_hash"] = json!("f".repeat(64)),
                3 => {
                    bad["fingerprint"]["components"]["hardware_fingerprint"] =
                        json!({"value":"invented","status":"VERIFIED","evidence_refs":[]})
                }
                _ => bad["metrics"]["inference_qualification_eligible"] = json!(true),
            };
            self.validate(s, r, bad, false).await?;
        }
        let artifact = s
            .work_dir()
            .join("output")
            .join(
                first["benchmark_id"]
                    .as_str()
                    .ok_or_else(|| invalid("benchmark_id_invalid"))?,
            )
            .join("observations.json");
        let original = std::fs::read(&artifact).map_err(super::io)?;
        {
            let restore = Restore {
                path: artifact.clone(),
                bytes: &original,
            };
            let mut changed = original.clone();
            changed.push(b' ');
            std::fs::write(&artifact, changed).map_err(super::io)?;
            self.validate(s, r, first.clone(), false).await?;
            drop(restore);
        }
        s.restart().await?;
        self.validate(s, r, first.clone(), true).await?;
        let second = self.capture(s, db, r).await?;
        r.check("new-measurement-same-execution-identity",json!({"new_id":true,"new_artifact":true,"identity":first["fingerprint"]["hash"]}),json!({"new_id":first["benchmark_id"]!=second["benchmark_id"],"new_artifact":first["artifact_refs"]!=second["artifact_refs"],"identity":second["fingerprint"]["hash"]}))?;
        for mode in [
            "auth", "rate", "redirect", "invalid", "wrong", "large", "type", "length",
        ] {
            self.mode(mode, true, true, None)?;
            let failed = self.capture(s, db, r).await?;
            r.check("failed-http-no-statistics",json!({"status":"FAIL","failed":6,"latency":null}),json!({"status":failed["status"],"failed":failed["metrics"]["failed_samples"],"latency":failed["metrics"]["latency_ms"]}))?;
            self.validate(s, r, failed, false).await?;
        }
        self.mode("normal", true, true, None)?;
        sqlx::query("CREATE TRIGGER p026_benchmark_accept BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'owned_accept_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let before = snapshot(db).await?;
        let count = self.request_count()?;
        let rejected = self
            .refused(
                s,
                r,
                self.request()?,
                &access,
                "reference_benchmark_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_benchmark_accept")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        rejected?;
        r.check("accept-transaction-rollback", before, snapshot(db).await?)?;
        r.check(
            "accept-fault-no-http",
            json!(count),
            json!(self.request_count()?),
        )?;
        let collision = self.interrupted(s, db, r, "collision").await?;
        self.state(db, r, &collision, "FAILED", 0).await?;
        r.check(
            "publication-preserves-foreign-sentinel",
            json!(true),
            json!(
                std::fs::read(
                    s.work_dir()
                        .join("output")
                        .join(&collision)
                        .join("sentinel.txt")
                )
                .map_err(super::io)?
                    == b"owned collision sentinel"
            ),
        )?;
        let cancelled = self.interrupted(s, db, r, "cancel").await?;
        self.state(db, r, &cancelled, "CANCELLED", 0).await?;
        r.check(
            "cancel-no-published-report",
            json!(false),
            json!(s.work_dir().join("output").join(&cancelled).exists()),
        )?;
        let crashed = self.interrupted(s, db, r, "crash").await?;
        self.state(db, r, &crashed, "RUNNING", 2).await?;
        s.restart().await?;
        let recovered = self.state(db, r, &crashed, "FAILED", 0).await?;
        r.check(
            "crash-cause-preserved",
            json!("ADAPTER_CRASHED"),
            recovered["error"]["code"].clone(),
        )?;
        sqlx::query("CREATE TRIGGER p026_benchmark_sample BEFORE INSERT ON ref_sample BEGIN SELECT RAISE(ABORT,'owned_sample_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let result = self
            .refused(
                s,
                r,
                self.request()?,
                &access,
                "reference_benchmark_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_benchmark_sample")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        result?;
        let uncertain = active(db).await?;
        self.state(db, r, &uncertain, "RUNNING", 2).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ref_sample WHERE run_id=?")
            .bind(&uncertain)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        r.check("sample-commit-atomic", json!(0), json!(count))?;
        self.refused(
            s,
            r,
            self.request()?,
            &access,
            "reference_benchmark_recovery_required",
        )
        .await?;
        s.restart().await?;
        let recovered = self.state(db, r, &uncertain, "FAILED", 0).await?;
        r.check(
            "sample-fault-cause-survives",
            json!("IntegrityError"),
            recovered["error"]["details"]["original_error"]["code"].clone(),
        )?;
        sqlx::query("CREATE TRIGGER p026_benchmark_final BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned_final_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let result = self
            .refused(
                s,
                r,
                self.request()?,
                &access,
                "reference_benchmark_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_benchmark_final")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        result?;
        let uncertain: String =
            sqlx::query_scalar("SELECT id FROM ref_run WHERE phase='VERIFYING'")
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        self.state(db, r, &uncertain, "VERIFYING", 2).await?;
        let folder = s.work_dir().join("output").join(&uncertain);
        let uncommitted = read_json(&folder.join("report.json"))?;
        let old_hash = file_digest(&folder.join("observations.json"))?;
        self.validate(s, r, uncommitted.clone(), false).await?;
        s.restart().await?;
        self.state(db, r, &uncertain, "FAILED", 0).await?;
        self.validate(s, r, uncommitted, false).await?;
        let fresh = self.capture(s, db, r).await?;
        self.validate(s, r, fresh.clone(), true).await?;
        r.check(
            "uncertain-run-not-reused",
            json!(true),
            json!(fresh["benchmark_id"] != uncertain),
        )?;
        r.check(
            "uncertain-artifact-retained",
            json!(old_hash),
            json!(file_digest(&folder.join("observations.json"))?),
        )?;
        let before = self.backend(true).await?;
        let pid = before["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid("benchmark_backend_pid_invalid"))?;
        self.mode("normal", true, false, None)?;
        let stopped = self.backend(false).await?;
        r.check(
            "actual-backend-ended",
            json!(true),
            json!(process(pid, &self.config.backend_binary)?.is_none()),
        )?;
        let offline = self.capture(s, db, r).await?;
        r.check(
            "offline-not-passed",
            json!("FAIL"),
            offline["status"].clone(),
        )?;
        self.mode("normal", true, true, None)?;
        let restarted = self.backend(true).await?;
        let new_pid = restarted["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid("benchmark_backend_pid_invalid"))?;
        r.check(
            "backend-restarted-owned-image",
            json!(true),
            process(new_pid, &self.config.backend_binary)?
                .ok_or_else(|| invalid("benchmark_backend_missing"))?["expected_image"]
                .clone(),
        )?;
        r.check(
            "backend-new-epoch",
            json!(true),
            json!(restarted["epoch"].as_u64() > stopped["epoch"].as_u64()),
        )?;
        r.checkpoints.insert(
            "backend-recovery".into(),
            json!({"stopped":stopped,"restarted":restarted}),
        );
        let repaired = self.capture(s, db, r).await?;
        self.validate(s, r, repaired, true).await?;
        let eof = self.interrupted(s, db, r, "eof").await?;
        self.state(db, r, &eof, "CANCELLED", 0).await?;
        let events = self.events()?;
        r.check(
            "no-auth-no-redirect",
            json!(true),
            json!(events.iter().filter(|e| e["kind"] == "request").all(|e| e
                ["authorization_present"]
                == false
                && e["method"] == "POST"
                && e["path"] == "/echo")),
        )?;
        r.check(
            "forbidden-sentinel-unchanged",
            json!(sentinel),
            json!(file_digest(&self.config.forbidden)?),
        )?;
        r.checkpoints.insert("http-events".into(), json!(events));
        r.checkpoints
            .insert("final-ledger".into(), snapshot(db).await?);
        let methods = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "benchmark")
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        r.check(
            "all-benchmark-signatures",
            json!(methods),
            json!(r
                .methods
                .iter()
                .filter(|m| m.starts_with("benchmark."))
                .collect::<BTreeSet<_>>()),
        )?;
        Ok(())
    }
    async fn run(
        &self,
        run: &Run,
        s: &mut NativeSession,
        r: &mut BenchmarkReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if s.manifest().id != "reference.benchmark.echo"
            || s.manifest().family.as_str() != "benchmark"
            || s.manifest().capabilities != [CAP]
        {
            return Err(invalid("benchmark_subject_mismatch"));
        }
        r.check(
            "exact-capability-profile",
            self.bundle.clone(),
            read_json(&s.package_dir().join("reference-capabilities.json"))?,
        )?;
        r.check(
            "exact-policy",
            self.policy(),
            read_json(&s.package_dir().join("reference-benchmark.json"))?,
        )?;
        r.check(
            "exact-instance-profile",
            self.config.profile.clone(),
            read_json(&s.work_dir().join("profile/profile.json"))?,
        )?;
        for p in [
            &self.config.control,
            &self.config.requests,
            &self.config.backend_state,
            &self.config.forbidden,
        ] {
            let p = canonical(p)?;
            if p.starts_with(canonical(s.work_dir())?) || p.starts_with(canonical(s.package_dir())?)
            {
                return Err(invalid("benchmark_oracle_resource_exposed"));
            }
        }
        self.mode("normal", true, true, None)?;
        let state = self.backend(true).await?;
        let pid = state["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid("benchmark_backend_pid_invalid"))?;
        let observed = process(pid, &self.config.backend_binary)?
            .ok_or_else(|| invalid("benchmark_backend_missing"))?;
        r.check(
            "actual-rust-backend",
            json!(true),
            observed["expected_image"].clone(),
        )?;
        r.checkpoints.insert("backend-initial".into(),json!({"state":state,"process":observed,"binary_sha256":file_digest(&self.config.backend_binary)?}));
        let wire = run_common(
            run,
            s,
            &WireProbe {
                method: "benchmark.describe".into(),
                params: json!({}),
                access: Access::default(),
            },
        )
        .await;
        let initial = wire.observations.clone();
        r.wire = Some(wire);
        let mut observations = initial?;
        s.restart().await?;
        let path = s.work_dir().join("state/benchmark.sqlite");
        crate::package::check_managed_path(&path)?;
        let db = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(2)),
        )
        .await
        .map_err(db_error)?;
        let ledger = Arc::new(LedgerIo(tokio::sync::Mutex::new(Some(db))));
        s.register_io(ledger.clone());
        let mut guard = ledger.0.lock().await;
        let db = guard
            .as_mut()
            .ok_or_else(|| invalid("benchmark_ledger_closed"))?;
        let app: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        if app != 0x4C415242 || version != 1 {
            return Err(invalid("benchmark_ledger_not_owned"));
        }
        self.functional(s, db, r).await?;
        drop(guard);
        let material = r.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:benchmark",
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
impl NativeSuite for ReferenceBenchmarkSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"benchmark","capabilities":[CAP],"suite":"reference.benchmark.echo.v1","bundle":self.bundle,"policy":self.policy(),"profile":self.config.profile,"control":self.config.control,"requests":self.config.requests,"backend_state":self.config.backend_state,"backend_binary":self.config.backend_binary,"forbidden":self.config.forbidden,"evidence_root":self.config.evidence_root})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        s: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut r = BenchmarkReport {
            run: run.clone(),
            run_sha256: run.digest()?,
            scope: SCOPE,
            wire: None,
            cases: vec![],
            methods: BTreeSet::new(),
            checkpoints: BTreeMap::new(),
            observations: Ok(vec![]),
        };
        r.observations = self.run(run, s, &mut r).await;
        let restore = self.mode("normal", true, true, None);
        if r.observations.is_ok() {
            restore?;
        }
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&r).map_err(|_| invalid("benchmark_report_invalid"))?,
        )
        .map_err(super::io)?;
        r.observations
    }
}
