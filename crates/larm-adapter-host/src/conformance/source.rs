//! Receta nativa del catálogo/transferencia de referencia. El ledger observado es
//! privado del worker: no concede admisión P050 ni acredita un upstream remoto.
use super::wire::{run_common, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};
use async_trait::async_trait;
use larm_adapter_sdk::protocol::Request;
use larm_adapter_sdk::{contracts::Contracts, manifest::MethodTable, permissions::Access};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const PROFILE: &str = include_str!("../../../../adapters/source/reference/capabilities.json");
const CAPS: [&str; 5] = [
    "source.search",
    "source.revisions",
    "source.resume",
    "source.sha256",
    "source.sync",
];
const SCOPE: &str = "reference source private ledger/HTTP fixture; no Core download admission or upstream qualification";

/// Recursos propios del harness. Control/log y bytes esperados están fuera de
/// los roots del plugin. El descriptor y NativeTarget deben fijar sus bindings.
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub source_id: String,
    pub storage_id: String,
    pub base_url: String,
    pub catalog: Value,
    pub expected_files: Vec<PathBuf>,
    pub control: PathBuf,
    pub requests: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct ReferenceSourceSuite {
    config: SourceConfig,
    profile: Value,
}
#[derive(Serialize)]
pub struct SourceReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl SourceReport {
    pub(super) fn check(
        &mut self,
        id: &str,
        expected: Value,
        observed: Value,
    ) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_source_{id}"),
            ))
        }
    }
    pub(super) fn material(&self) -> Value {
        json!({"run_sha256":self.run_sha256,"scope":self.scope,"wire":self.wire,
            "cases":self.cases,"methods":self.methods,"checkpoints":self.checkpoints})
    }
}
fn db_error(_: sqlx::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "conformance_source_ledger_io",
    )
}
struct LedgerIo(tokio::sync::Mutex<Option<SqliteConnection>>);
#[async_trait]
impl NativeIo for LedgerIo {
    async fn close_io(&self) -> Result<(), LarmError> {
        if let Some(connection) = self.0.lock().await.take() {
            connection.close().await.map_err(db_error)?;
        }
        Ok(())
    }
}
async fn snapshot(db: &mut SqliteConnection) -> Result<Value, LarmError> {
    let mut values = serde_json::Map::new();
    for (table, query) in [
        ("ref_plan", "SELECT json_group_array(json_array(id,hash,request,document,binding,job_id,outcome)) FROM (SELECT * FROM ref_plan ORDER BY rowid)"),
        ("ref_job", "SELECT json_group_array(json_array(id,identity,attempt,phase,document,version)) FROM (SELECT * FROM ref_job ORDER BY rowid)"),
        ("ref_journal", "SELECT json_group_array(json_array(job_id,plan_id,phase,payload)) FROM (SELECT * FROM ref_journal ORDER BY rowid)"),
        ("ref_event", "SELECT json_group_array(json_array(id,job_id,version,phase,occurred_at)) FROM (SELECT * FROM ref_event ORDER BY rowid)"),
        ("ref_outbox", "SELECT json_group_array(json_array(event_id,delivered_at)) FROM (SELECT * FROM ref_outbox ORDER BY rowid)"),
        ("ref_reservation", "SELECT json_group_array(json_array(job_id,total_bytes,remaining_bytes)) FROM (SELECT * FROM ref_reservation ORDER BY rowid)"),
        ("ref_lock", "SELECT json_group_array(json_array(job_id,attempt)) FROM (SELECT * FROM ref_lock ORDER BY rowid)"),
        ("ref_checkpoint", "SELECT json_group_array(json_array(job_id,path,offset)) FROM (SELECT * FROM ref_checkpoint ORDER BY rowid)"),
    ] {
        // Nombres/columnas constantes del ledger privado, nunca SQL de cliente.
        let raw: String = sqlx::query_scalar(query)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        values.insert(
            table.into(),
            serde_json::from_str(&raw).map_err(|_| invalid("source_snapshot_invalid"))?,
        );
    }
    Ok(Value::Object(values))
}
async fn resources(db: &mut SqliteConnection) -> Result<i64, LarmError> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM ref_lock)+(SELECT count(*) FROM ref_reservation)",
    )
    .fetch_one(db)
    .await
    .map_err(db_error)
}
async fn job(db: &mut SqliteConnection, id: &str) -> Result<Value, LarmError> {
    let row = sqlx::query("SELECT phase,attempt,document FROM ref_job WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(db_error)?;
    Ok(
        json!({"phase":row.get::<String,_>(0),"attempt":row.get::<i64,_>(1),
        "document":serde_json::from_str::<Value>(&row.get::<String,_>(2)).map_err(|_|invalid("source_job_invalid"))?}),
    )
}
impl ReferenceSourceSuite {
    pub fn new(config: SourceConfig) -> Result<Self, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(&config.source_id)
            || !larm_domain::bridge::is_uuid_v7(&config.storage_id)
            || !config.base_url.starts_with("http://127.0.0.1:")
            || config.catalog["source_id"] != config.source_id
            || config.catalog["entries"].as_array().map(Vec::len) != Some(6)
            || config.expected_files.len() != 6
        {
            return Err(invalid("conformance_source_config_invalid"));
        }
        for path in config.expected_files.iter().chain([
            &config.control,
            &config.requests,
            &config.forbidden,
            &config.evidence_root,
        ]) {
            crate::package::check_managed_path(path)?;
        }
        let profile =
            serde_json::from_str(PROFILE).map_err(|_| invalid("source_profile_invalid"))?;
        Ok(Self { config, profile })
    }
    pub fn report_path(&self, run_id: &str) -> PathBuf {
        self.config.evidence_root.join(run_id).join("source.json")
    }
    fn entries(&self) -> &[Value] {
        self.config.catalog["entries"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    fn search(&self) -> Value {
        json!({"source_id":self.config.source_id,"q":"Reference","capabilities":[],"cursor":null,"limit":1})
    }
    fn auth(&self) -> Value {
        json!({"source_id":self.config.source_id,"auth_mode":"NONE","account_id":null,"credential_reference_id":null})
    }
    fn reference(&self, index: usize, revision: bool) -> Value {
        let entry = &self.entries()[index];
        let mut value = json!({"source_id":self.config.source_id,"upstream_namespace":entry["upstream_namespace"],"upstream_model_id":entry["upstream_model_id"]});
        if revision {
            value["revision_key"] = entry["revision"]["revision_key"].clone();
        }
        value
    }
    fn access(&self, session: &NativeSession) -> Access {
        Access {
            reads: vec![session.work_dir().to_owned()],
            writes: vec![session.work_dir().to_owned()],
            urls: std::iter::once(format!("{}/probe", self.config.base_url))
                .chain((0..6).map(|i| format!("{}/artifact/{i}", self.config.base_url)))
                .collect(),
            ..Access::default()
        }
    }
    fn mode(&self, mode: &str, probe_mode: &str) -> Result<(), LarmError> {
        if !["normal", "slow", "cut", "auth", "rate"].contains(&mode)
            || !["normal", "auth", "rate"].contains(&probe_mode)
        {
            return Err(invalid("source_fixture_mode_invalid"));
        }
        let temporary = self.config.control.with_extension("tmp");
        crate::package::check_managed_path(&temporary)?;
        std::fs::write(
            &temporary,
            serde_json::to_vec(&json!({"mode":mode,"probe_mode":probe_mode}))
                .map_err(|_| invalid("source_fixture_mode_invalid"))?,
        )
        .map_err(super::io)?;
        std::fs::rename(temporary, &self.config.control).map_err(super::io)
    }
    fn requests(&self) -> Result<Vec<Value>, LarmError> {
        let bytes = std::fs::read(&self.config.requests).map_err(super::io)?;
        if bytes.len() > 1 << 20 {
            return Err(invalid("source_fixture_log_too_large"));
        }
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|line| {
                serde_json::from_slice(line).map_err(|_| invalid("source_fixture_log_invalid"))
            })
            .collect()
    }
    async fn call(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
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
                Some(Duration::from_secs(30)),
                None,
            )
            .await
    }
    async fn refused(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        method: &str,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<(), LarmError> {
        let observed = match self.call(session, report, method, params, access).await {
            Err(e) => json!(e.details.reason),
            Ok(_) => json!("success"),
        };
        report.check(reason, json!(reason), observed)
    }
    async fn plan(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        index: usize,
        budget: Option<&str>,
    ) -> Result<Value, LarmError> {
        let request = json!({"source_id":self.config.source_id,"variant_id":self.entries()[index]["model"]["id"],"storage_location_id":self.config.storage_id,"hard_budget_bytes":budget,"priority":"HIGH"});
        let plan = self
            .call(
                session,
                report,
                "source.download_plan",
                request,
                &self.access(session),
            )
            .await?;
        let mut unsigned = plan.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("source_plan_invalid"))?
            .remove("plan_hash");
        report.check(
            "plan-jcs",
            json!(hash(&unsigned)?),
            plan["plan_hash"].clone(),
        )?;
        Ok(plan)
    }
    async fn download(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        plan: &Value,
    ) -> Result<Value, LarmError> {
        self.call(
            session,
            report,
            "source.download",
            execute(plan),
            &self.access(session),
        )
        .await
    }
    fn publication(&self, session: &NativeSession, index: usize) -> PathBuf {
        session
            .work_dir()
            .join("storage")
            .join(
                self.entries()[index]["revision"]["manifest_hash"]
                    .as_str()
                    .unwrap_or("invalid"),
            )
            .join("data.bin")
    }
    fn verify_publication(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        index: usize,
    ) -> Result<(), LarmError> {
        let actual = self.publication(session, index);
        report.check(
            "published-sha256",
            json!(file_digest(&self.config.expected_files[index])?),
            json!(file_digest(&actual)?),
        )?;
        report.check(
            "published-size",
            json!(std::fs::metadata(&self.config.expected_files[index])
                .map_err(super::io)?
                .len()),
            json!(std::fs::metadata(actual).map_err(super::io)?.len()),
        )
    }
    async fn terminal(
        &self,
        db: &mut SqliteConnection,
        report: &mut SourceReport,
        id: &str,
        phase: &str,
        attempt: i64,
    ) -> Result<(), LarmError> {
        let state = job(db, id).await?;
        report.check(
            "terminal-ledger",
            json!({"phase":phase,"attempt":attempt,"held":0}),
            json!({"phase":state["phase"],"attempt":state["attempt"],"held":resources(db).await?}),
        )?;
        let journal: String = sqlx::query_scalar("SELECT phase FROM ref_journal WHERE job_id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        report.check("terminal-journal", json!(phase), json!(journal))?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM ref_event),(SELECT count(*) FROM ref_outbox)",
        )
        .fetch_one(db)
        .await
        .map_err(db_error)?;
        report.check("event-outbox", json!(counts.0), json!(counts.1))?;
        report
            .checkpoints
            .insert(format!("{id}-{attempt}-{phase}"), state);
        Ok(())
    }
    async fn interrupted(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut SourceReport,
        plan: &Value,
        crash: bool,
    ) -> Result<(String, i64), LarmError> {
        let mut progress = session.progress()?;
        let request = Request::new("source.download", execute(plan));
        let id = request.id.clone();
        report.methods.insert("source.download".into());
        let access = self.access(session);
        let pending = session.call(request, &access, Some(Duration::from_secs(30)), None);
        tokio::pin!(pending);
        let event = tokio::select! {
            event=progress.recv()=>event.map_err(|_|invalid("source_progress_missing"))?,
            _=&mut pending=>return Err(invalid("source_finished_before_progress")),
        };
        report.check("inflight-progress-id", json!(id), json!(event.request_id))?;
        let checkpoint=tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let row:Option<(String,i64)>=sqlx::query_as("SELECT j.id,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY c.offset DESC LIMIT 1").fetch_optional(&mut *db).await.map_err(db_error)?;
                if let Some(row)=row {return Ok::<_,LarmError>(row);}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.map_err(|_|invalid("source_durable_checkpoint_missing"))??;
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
                .is_some_and(|ids| ids.contains(&json!(id)))),
        )?;
        if crash {
            session.crash()?;
        } else {
            report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
        }
        let failure = pending
            .await
            .err()
            .ok_or_else(|| invalid("source_interrupted_call_succeeded"))?;
        report.check(
            if crash {
                "crash-result"
            } else {
                "cancel-result"
            },
            if crash {
                json!("ADAPTER_CRASHED")
            } else {
                json!("operation_cancelled")
            },
            if crash {
                json!(failure.code.as_str())
            } else {
                json!(failure.details.reason)
            },
        )?;
        if !crash {
            report.check(
                "cancel-finished-id",
                json!(false),
                json!(session.cancel(&id).await?),
            )?;
        }
        report.checkpoints.insert(if crash {"crash".into()} else {"cancel".into()},json!({"request_id":id,"job_id":checkpoint.0,"durable_offset":checkpoint.1,"progress_phase":event.phase}));
        Ok(checkpoint)
    }
    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut SourceReport,
    ) -> Result<(), LarmError> {
        let access = self.access(session);
        // Probar cada capability con conducta observada, no sólo su descriptor.
        let mut query = self.search();
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        let mut first_cursor = Value::Null;
        for page in 0..6 {
            query["cursor"] = cursor;
            let result = self
                .call(session, report, "source.search", query.clone(), &access)
                .await?;
            if page == 0 {
                first_cursor = result["next_cursor"].clone();
            }
            models.extend(
                result["items"]
                    .as_array()
                    .ok_or_else(|| invalid("source_page_invalid"))?
                    .iter()
                    .cloned(),
            );
            cursor = result["next_cursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        let expected_ids = self
            .entries()
            .iter()
            .map(|e| e["model"]["id"].as_str().unwrap_or("invalid").to_owned())
            .collect::<BTreeSet<_>>();
        let ids = models
            .iter()
            .map(|m| m["id"].as_str().unwrap_or("invalid").to_owned())
            .collect::<BTreeSet<_>>();
        report.check(
            "pagination",
            json!({"ids":expected_ids,"count":6,"cursor":null}),
            json!({"ids":ids,"count":models.len(),"cursor":cursor}),
        )?;
        report.check("metadata-unknown-redacted",json!({"token":"[REDACTED]","unknown":[1,null],"architecture":null,"availability":"UNKNOWN"}),json!({"token":models[0]["metadata"]["source_v1"]["original"]["api_token"],"unknown":models[0]["metadata"]["source_v1"]["original"]["tokenizer"]["unknown"],"architecture":models[0]["metadata"]["source_v1"]["normalized"]["architecture"],"availability":models[0]["availability"]}))?;
        let mut filtered = self.search();
        filtered["limit"] = json!(200);
        filtered["capabilities"] = json!([
            self.entries()[0]["model"]["capabilities"][0],
            self.entries()[1]["model"]["capabilities"][0]
        ]);
        let result = self
            .call(session, report, "source.search", filtered.clone(), &access)
            .await?;
        report.check(
            "capabilities-or",
            json!(2),
            json!(result["items"].as_array().map(Vec::len)),
        )?;
        filtered["q"] = json!("absent");
        let empty = self
            .call(session, report, "source.search", filtered, &access)
            .await?;
        report.check("query-and", json!([]), empty["items"].clone())?;
        let mut invalid_cursor = self.search();
        invalid_cursor["cursor"] = first_cursor.clone();
        invalid_cursor["q"] = json!("different");
        self.refused(
            session,
            report,
            "source.search",
            invalid_cursor,
            &access,
            "source_cursor_invalid",
        )
        .await?;
        let mut foreign = self.search();
        foreign["source_id"] = json!(uuid::Uuid::now_v7().to_string());
        self.refused(
            session,
            report,
            "source.search",
            foreign,
            &access,
            "source_id_mismatch",
        )
        .await?;
        let sentinel = file_digest(&self.config.forbidden)?;
        let before = snapshot(db).await?;
        self.refused(
            session,
            report,
            "source.search",
            self.search(),
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
            "source.auth_probe",
            self.auth(),
            &Access {
                secrets: vec!["larm/ungranted".into()],
                ..Access::default()
            },
            "secret_denied",
        )
        .await?;
        report.check("denied-ledger-unchanged", before, snapshot(db).await?)?;
        report.check(
            "denied-sentinel-unchanged",
            json!(sentinel),
            json!(file_digest(&self.config.forbidden)?),
        )?;
        let inspected = self
            .call(
                session,
                report,
                "source.get_model",
                self.reference(0, false),
                &access,
            )
            .await?;
        report.check(
            "inspection",
            self.entries()[0]["inspection"].clone(),
            inspected,
        )?;
        let revisions = self
            .call(
                session,
                report,
                "source.list_revisions",
                self.reference(0, false),
                &access,
            )
            .await?;
        report.check(
            "revision",
            json!([self.entries()[0]["revision"]]),
            revisions["items"].clone(),
        )?;
        let manifest = self
            .call(
                session,
                report,
                "source.get_manifest",
                self.reference(0, true),
                &access,
            )
            .await?;
        let artifact = self.entries()[0]["inspection"]["artifacts"][0].clone();
        let descriptors = json!([{"relative_path":artifact["relative_path"],"sha256":artifact["sha256"],"size_bytes":artifact["size_bytes"].as_str().and_then(|s|s.parse::<u64>().ok()),"required":true}]);
        let manifest_hash = hash(&descriptors)?;
        report.check("manifest-jcs-content",json!({"manifest_hash":manifest_hash,"revision_key":format!("content:{manifest_hash}"),"artifacts":[artifact]}),manifest)?;
        let estimate = self
            .call(
                session,
                report,
                "source.estimate_size",
                self.reference(0, true),
                &access,
            )
            .await?;
        report.check("estimate-pinned-no-core-admission",json!({"bytes":self.entries()[0]["inspection"]["artifacts"][0]["size_bytes"],"eligible":false,"confidence":"EXACT"}),json!({"bytes":estimate["download_bytes"],"eligible":estimate["per_volume"][0]["eligible"],"confidence":estimate["confidence"]}))?;
        let sync = self
            .call(
                session,
                report,
                "source.sync",
                json!({"source_id":self.config.source_id,"etag":null,"cursor":null,"query":{}}),
                &access,
            )
            .await?;
        report.check(
            "sync-changed",
            json!({"changed":true,"count":6}),
            json!({"changed":sync["changed"],"count":sync["models"].as_array().map(Vec::len)}),
        )?;
        let same=self.call(session,report,"source.sync",json!({"source_id":self.config.source_id,"etag":sync["etag"],"cursor":null,"query":{}}),&access).await?;
        report.check(
            "sync-unchanged",
            json!({"changed":false,"models":[]}),
            json!({"changed":same["changed"],"models":same["models"]}),
        )?;
        self.mode("normal", "auth")?;
        let auth = self
            .call(session, report, "source.auth_probe", self.auth(), &access)
            .await?;
        report.check(
            "real-401",
            json!({"required":true,"authenticated":false}),
            json!({"required":auth["auth_required"],"authenticated":auth["authenticated"]}),
        )?;
        let denied = self.plan(session, report, 0, None).await?;
        let before = snapshot(db).await?;
        self.refused(
            session,
            report,
            "source.download",
            execute(&denied),
            &access,
            "source_auth_required",
        )
        .await?;
        report.check("auth-before-admission", before, snapshot(db).await?)?;
        self.mode("normal", "normal")?;
        let small = self.plan(session, report, 0, Some("1")).await?;
        let before = snapshot(db).await?;
        self.refused(
            session,
            report,
            "source.download",
            execute(&small),
            &access,
            "reference_budget_exceeded",
        )
        .await?;
        report.check("budget-before-admission", before, snapshot(db).await?)?;
        let plan = self.plan(session, report, 0, None).await?;
        let mut changed = execute(&plan);
        changed["plan_hash"] = json!("f".repeat(64));
        self.refused(
            session,
            report,
            "source.download",
            changed,
            &access,
            "plan_hash_mismatch",
        )
        .await?;
        let completed = self.download(session, report, &plan).await?;
        report.check(
            "receipt-completed",
            json!("COMPLETED"),
            completed["state"].clone(),
        )?;
        let id = completed["job_id"]
            .as_str()
            .ok_or_else(|| invalid("source_job_missing"))?;
        self.verify_publication(session, report, 0)?;
        self.terminal(db, report, id, "COMPLETED", 1).await?;
        let status_path = session
            .work_dir()
            .join("job-status")
            .join(format!("{id}.json"));
        let status: Value =
            serde_json::from_slice(&std::fs::read(&status_path).map_err(super::io)?)
                .map_err(|_| invalid("source_status_invalid"))?;
        report.check(
            "status-file-matches-ledger",
            job(db, id).await?["document"].clone(),
            status,
        )?;
        let repeated = self.download(session, report, &plan).await?;
        report.check("idempotent-receipt", completed.clone(), repeated)?;
        session.restart().await?;
        let recovered = self.download(session, report, &plan).await?;
        report.check("receipt-survives-restart", completed, recovered)?;
        self.mode("cut", "normal")?;
        let cut = self.plan(session, report, 1, None).await?;
        self.refused(
            session,
            report,
            "source.download",
            execute(&cut),
            &access,
            "source_network_interrupted",
        )
        .await?;
        let row:(String,i64)=sqlx::query_as("SELECT j.id,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id WHERE j.phase='FAILED' AND c.offset>0").fetch_one(&mut *db).await.map_err(db_error)?;
        let partial = session
            .work_dir()
            .join("staging")
            .join(&row.0)
            .join("data.bin.partial");
        report.check(
            "durable-partial",
            json!(true),
            json!(
                row.1 > 0
                    && std::fs::metadata(&partial).map_err(super::io)?.len() == row.1 as u64
                    && !self.publication(session, 1).exists()
            ),
        )?;
        self.terminal(db, report, &row.0, "FAILED", 1).await?;
        self.mode("normal", "normal")?;
        let resumed_plan = self.plan(session, report, 1, None).await?;
        let resumed = self.download(session, report, &resumed_plan).await?;
        report.check("resume-same-job", json!(row.0), resumed["job_id"].clone())?;
        self.verify_publication(session, report, 1)?;
        self.terminal(db, report, &row.0, "COMPLETED", 2).await?;
        report.check(
            "real-range-if-range",
            json!(true),
            json!(self.requests()?.iter().any(|r| r["path"] == "/artifact/1"
                && r["range"] == format!("bytes={}-", row.1)
                && r["if_range"] == self.entries()[1]["downloads"][0]["etag"])),
        )?;
        self.mode("slow", "normal")?;
        let cancel = self.plan(session, report, 2, None).await?;
        let (cancelled, _) = self
            .interrupted(session, db, report, &cancel, false)
            .await?;
        self.terminal(db, report, &cancelled, "CANCELLED", 1)
            .await?;
        report.check(
            "cancel-removes-owned-partial",
            json!(false),
            json!(session
                .work_dir()
                .join("staging")
                .join(cancelled)
                .join("data.bin.partial")
                .exists()),
        )?;
        self.mode("normal", "normal")?;
        let fault = self.plan(session, report, 3, None).await?;
        sqlx::query("CREATE TRIGGER p026_fail_accept BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END").execute(&mut *db).await.map_err(db_error)?;
        let before = snapshot(db).await?;
        let rejected = self
            .refused(
                session,
                report,
                "source.download",
                execute(&fault),
                &access,
                "reference_io_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_fail_accept")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        rejected?;
        report.check("accept-transaction-rollback", before, snapshot(db).await?)?;
        sqlx::query("CREATE TRIGGER p026_fail_complete BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END").execute(&mut *db).await.map_err(db_error)?;
        let rejected = self
            .refused(
                session,
                report,
                "source.download",
                execute(&fault),
                &access,
                "reference_execution_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_fail_complete")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        rejected?;
        let uncertain: String =
            sqlx::query_scalar("SELECT id FROM ref_job WHERE phase='VERIFYING'")
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        report.check(
            "uncertain-keeps-resources",
            json!(2),
            json!(resources(db).await?),
        )?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ref_event WHERE job_id=? AND phase='COMPLETED'",
        )
        .bind(&uncertain)
        .fetch_one(&mut *db)
        .await
        .map_err(db_error)?;
        report.check("uncertain-no-completed-event", json!(0), json!(count))?;
        self.verify_publication(session, report, 3)?;
        session.restart().await?;
        self.terminal(db, report, &uncertain, "FAILED", 1).await?;
        let requests_before = self
            .requests()?
            .iter()
            .filter(|r| r["path"] == "/artifact/3")
            .count();
        let retry = self.plan(session, report, 3, None).await?;
        let recovered = self.download(session, report, &retry).await?;
        report.check("recover-verified-publication",json!({"id":uncertain,"requests":requests_before}),json!({"id":recovered["job_id"],"requests":self.requests()?.iter().filter(|r|r["path"]=="/artifact/3").count()}))?;
        self.terminal(db, report, &uncertain, "COMPLETED", 2)
            .await?;
        self.mode("slow", "normal")?;
        let crash = self.plan(session, report, 4, None).await?;
        let (killed, durable) = self.interrupted(session, db, report, &crash, true).await?;
        let partial = session
            .work_dir()
            .join("staging")
            .join(&killed)
            .join("data.bin.partial");
        report.check(
            "crash-durable-bytes",
            json!(true),
            json!(std::fs::metadata(&partial).map_err(super::io)?.len() >= durable as u64),
        )?;
        self.mode("normal", "normal")?;
        session.restart().await?;
        self.terminal(db, report, &killed, "FAILED", 1).await?;
        report.check(
            "crash-preserves-partial",
            json!(true),
            json!(partial.is_file()),
        )?;
        let retry = self.plan(session, report, 4, None).await?;
        let final_receipt = self.download(session, report, &retry).await?;
        report.check(
            "crash-new-attempt-same-job",
            json!(killed),
            final_receipt["job_id"].clone(),
        )?;
        self.verify_publication(session, report, 4)?;
        self.terminal(db, report, &killed, "COMPLETED", 2).await?;
        let requests = self.requests()?;
        report.check(
            "no-authorization-header",
            json!(true),
            json!(requests.iter().all(|r| r["authorization_present"] == false)),
        )?;
        report
            .checkpoints
            .insert("http-requests".into(), json!(requests));
        report
            .checkpoints
            .insert("final-ledger".into(), snapshot(db).await?);
        let expected = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "source" && m.required)
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        report.check(
            "all-source-signatures",
            json!(expected),
            json!(report
                .methods
                .iter()
                .filter(|m| m.starts_with("source."))
                .collect::<BTreeSet<_>>()),
        )?;
        Ok(())
    }
    async fn run(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut SourceReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "reference.catalog"
            || session.manifest().family.as_str() != "source"
            || session
                .manifest()
                .capabilities
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != CAPS.into_iter().collect()
        {
            return Err(invalid("source_subject_mismatch"));
        }
        let actual: Value = serde_json::from_slice(
            &std::fs::read(session.package_dir().join("reference-capabilities.json"))
                .map_err(super::io)?,
        )
        .map_err(|_| invalid("source_profile_invalid"))?;
        report.check("exact-source-profile", self.profile.clone(), actual)?;
        for (index, path) in self.config.expected_files.iter().enumerate() {
            report.check(
                "pinned-artifact",
                self.entries()[index]["inspection"]["artifacts"][0]["sha256"].clone(),
                json!(file_digest(path)?),
            )?;
        }
        for path in [
            &self.config.control,
            &self.config.requests,
            &self.config.forbidden,
        ] {
            let path = canonical(path)?;
            if path.starts_with(canonical(session.work_dir())?)
                || path.starts_with(canonical(session.package_dir())?)
            {
                return Err(invalid("source_oracle_resource_exposed"));
            }
        }
        self.mode("normal", "normal")?;
        let wire = run_common(
            run,
            session,
            &WireProbe {
                method: "source.search".into(),
                params: self.search(),
                access: Access {
                    reads: vec![session.work_dir().join("catalog/catalog.json")],
                    ..Access::default()
                },
            },
        )
        .await;
        let result = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = result?;
        session.restart().await?;
        let path = session.work_dir().join("reference-source.sqlite");
        crate::package::check_managed_path(&path)?;
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(2));
        let connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(db_error)?;
        let ledger = Arc::new(LedgerIo(tokio::sync::Mutex::new(Some(connection))));
        session.register_io(ledger.clone());
        let mut guard = ledger.0.lock().await;
        let db = guard
            .as_mut()
            .ok_or_else(|| invalid("source_ledger_closed"))?;
        let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let result = if application_id != 0x4C415253 {
            Err(invalid("source_ledger_not_owned"))
        } else {
            self.functional(session, db, report).await
        };
        drop(guard);
        let mode = self.mode("normal", "normal");
        result?;
        mode?;
        let material = report.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:source",
            "capability:source.search",
            "capability:source.revisions",
            "capability:source.resume",
            "capability:source.sha256",
            "capability:source.sync",
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
fn execute(plan: &Value) -> Value {
    json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"approval_id":null})
}
#[async_trait]
impl NativeSuite for ReferenceSourceSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"source","capabilities":CAPS,"suite":"reference.source.catalog.v1",
        "source_id":self.config.source_id,"storage_id":self.config.storage_id,"base_url":self.config.base_url,
        "catalog":self.config.catalog,"expected_files":self.config.expected_files,"control":self.config.control,
        "requests":self.config.requests,"forbidden":self.config.forbidden,"evidence_root":self.config.evidence_root,"profile":self.profile})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut report = SourceReport {
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
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&report).map_err(|_| invalid("source_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}
