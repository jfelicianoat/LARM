//! Receta nativa de transformación binaria/Base64. Ledger e intentos privados;
//! no acredita conversión de modelos ni admisión/reservas globales de Core.
use super::wire::{run_common, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};
use async_trait::async_trait;
use base64::Engine;
use larm_adapter_sdk::{
    contracts::Contracts, manifest::MethodTable, permissions::Access, protocol::Request,
};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CAP: &str = "vendor.reference.base64";
const SCOPE:&str="reference streaming binary/Base64 transformation and private ledger; no model conversion or Core admission";
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationConfig {
    pub profile: Value,
    pub expected_input: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct ReferenceTransformationSuite {
    config: TransformationConfig,
    bundle: Value,
}
#[derive(Serialize)]
pub struct TransformationReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl TransformationReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_transformation_{id}"),
            ))
        }
    }
    fn material(&self) -> Value {
        json!({"run_sha256":self.run_sha256,"scope":self.scope,"wire":self.wire,"cases":self.cases,"methods":self.methods,"checkpoints":self.checkpoints})
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn descriptor(bytes: &[u8]) -> Value {
    json!({"sha256":digest(bytes),"size_bytes":bytes.len().to_string()})
}
fn bundle() -> Result<Value, LarmError> {
    let schema = |maximum: usize| {
        json!({"type":"object","additionalProperties":false,"required":["sha256","size_bytes"],"properties":{
        "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"size_bytes":{"type":"string","pattern":"^(0|[1-9][0-9]*)$","maxLength":19,"x-maximum-decimal":maximum.to_string()}}})
    };
    let input = schema(64 << 20);
    let output = schema(4 * (64usize << 20).div_ceil(3));
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"LARM\n");
    let fixture = json!({"input":descriptor(b"LARM\n"),"output":descriptor(encoded.as_bytes())});
    Ok(
        json!({"schema_version":"1.0","entries":[{"id":CAP,"version":"1.0","input_schema":input,"output_schema":output,
        "input_sha256":hash(&input)?,"output_sha256":hash(&output)?,"mapping":"reference.binary.base64.v1","estimator_id":"reference.base64.buffers.v1","fixture":fixture,"fixture_sha256":hash(&fixture)?}]}),
    )
}
fn read_json(path: &Path) -> Result<Value, LarmError> {
    crate::package::check_managed_path(path)?;
    let raw = std::fs::read(path).map_err(super::io)?;
    if raw.len() > 1 << 20 {
        return Err(invalid("transformation_oracle_document_too_large"));
    }
    serde_json::from_slice(&raw).map_err(|_| invalid("transformation_oracle_document_invalid"))
}
fn db_error(_: sqlx::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "conformance_transformation_ledger_io",
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
// Restaura sólo bytes propios enumerados incluso si se cancela el oráculo.
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
    let mut value = serde_json::Map::new();
    for (name,query) in [
        ("ref_plan","SELECT json_group_array(json_array(id,hash,document,request,binding,job_id,outcome)) FROM (SELECT * FROM ref_plan ORDER BY rowid)"),
        ("ref_job","SELECT json_group_array(json_array(id,identity,attempt,phase,document,version)) FROM (SELECT * FROM ref_job ORDER BY rowid)"),
        ("ref_attempt","SELECT json_group_array(json_array(job_id,attempt,phase,snapshot)) FROM (SELECT * FROM ref_attempt ORDER BY rowid)"),
        ("ref_journal","SELECT json_group_array(json_array(job_id,attempt,plan_id,phase,payload,closed)) FROM (SELECT * FROM ref_journal ORDER BY rowid)"),
        ("ref_event","SELECT json_group_array(json_array(id,job_id,version,event_type,phase,occurred_at,payload)) FROM (SELECT * FROM ref_event ORDER BY rowid)"),
        ("ref_outbox","SELECT json_group_array(json_array(event_id,delivered_at)) FROM (SELECT * FROM ref_outbox ORDER BY rowid)"),
        ("ref_lock","SELECT json_group_array(json_array(job_id,generation)) FROM (SELECT * FROM ref_lock ORDER BY rowid)"),
        ("ref_reservation","SELECT json_group_array(json_array(job_id,total_bytes,remaining_bytes)) FROM (SELECT * FROM ref_reservation ORDER BY rowid)"),
        ("ref_checkpoint","SELECT json_group_array(json_array(job_id,attempt,offset)) FROM (SELECT * FROM ref_checkpoint ORDER BY rowid)"),
    ] {
        let raw:String=sqlx::query_scalar(query).fetch_one(&mut *db).await.map_err(db_error)?;
        value.insert(name.into(),serde_json::from_str(&raw).map_err(|_|invalid("transformation_snapshot_invalid"))?);
    }
    Ok(Value::Object(value))
}
async fn held(db: &mut SqliteConnection, id: &str) -> Result<i64, LarmError> {
    sqlx::query_scalar("SELECT (SELECT count(*) FROM ref_lock WHERE job_id=?)+(SELECT count(*) FROM ref_reservation WHERE job_id=?)").bind(id).bind(id).fetch_one(db).await.map_err(db_error)
}
async fn job(db: &mut SqliteConnection, id: &str) -> Result<Value, LarmError> {
    let row = sqlx::query("SELECT phase,attempt,document FROM ref_job WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(db_error)?;
    Ok(
        json!({"phase":row.get::<String,_>(0),"attempt":row.get::<i64,_>(1),"document":serde_json::from_str::<Value>(&row.get::<String,_>(2)).map_err(|_|invalid("transformation_job_invalid"))?}),
    )
}
impl ReferenceTransformationSuite {
    pub fn new(config: TransformationConfig) -> Result<Self, LarmError> {
        for key in ["input_variant_id", "storage_location_id"] {
            if !config.profile[key]
                .as_str()
                .is_some_and(larm_domain::bridge::is_uuid_v7)
            {
                return Err(invalid("transformation_config_invalid"));
            }
        }
        if config.profile["input"]["relative_path"] != "fixture.bin" {
            return Err(invalid("transformation_config_invalid"));
        }
        for path in [
            &config.expected_input,
            &config.forbidden,
            &config.evidence_root,
        ] {
            crate::package::check_managed_path(path)?;
        }
        let suite = Self {
            config,
            bundle: bundle()?,
        };
        Contracts::shared()?.request("transformation.plan", &suite.request())?;
        Ok(suite)
    }
    pub fn report_path(&self, id: &str) -> PathBuf {
        self.config
            .evidence_root
            .join(id)
            .join("transformation.json")
    }
    fn request(&self) -> Value {
        json!({"input_variant_id":self.config.profile["input_variant_id"],"adapter_id":"reference.transformation.base64","tool_version":"1.0.0","target_format":"BASE64","parameters":{},"storage_location_id":self.config.profile["storage_location_id"],"hard_budget_bytes":null})
    }
    fn input(&self, session: &NativeSession) -> PathBuf {
        session.work_dir().join("input/fixture.bin")
    }
    fn access(&self, session: &NativeSession) -> Access {
        Access {
            reads: ["input", "profile", "state", "output"]
                .into_iter()
                .map(|p| session.work_dir().join(p))
                .collect(),
            writes: ["state", "output"]
                .into_iter()
                .map(|p| session.work_dir().join(p))
                .collect(),
            ..Access::default()
        }
    }
    async fn call(
        &self,
        session: &NativeSession,
        report: &mut TransformationReport,
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
        report: &mut TransformationReport,
        method: &str,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<LarmError, LarmError> {
        match self.call(session, report, method, params, access).await {
            Err(error) => {
                report.check(reason, json!(reason), json!(error.details.reason))?;
                Ok(error)
            }
            Ok(_) => {
                report.check(reason, json!(reason), json!("unexpected success"))?;
                Err(invalid("transformation_unexpected_success"))
            }
        }
    }
    async fn plan(
        &self,
        session: &NativeSession,
        report: &mut TransformationReport,
        encoded: &[u8],
    ) -> Result<Value, LarmError> {
        let plan = self
            .call(
                session,
                report,
                "transformation.plan",
                self.request(),
                &self.access(session),
            )
            .await?;
        let mut unsigned = plan.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("transformation_plan_invalid"))?
            .remove("plan_hash");
        report.check(
            "plan-jcs",
            json!(hash(&unsigned)?),
            plan["plan_hash"].clone(),
        )?;
        report.check(
            "plan-predicted-by-rust-base64",
            descriptor(encoded),
            plan["steps"][0]["request"]["binding"]["predicted"].clone(),
        )?;
        let upper = encoded.len() as u64 + (3 << 20);
        let margin = (1 << 30).max(upper.div_ceil(20));
        report.check("private-storage-estimate",json!({"upper":upper.to_string(),"required":(upper+margin).to_string(),"confidence":"ESTIMATED","eligible":false}),
            json!({"upper":plan["storage_estimate"]["upper_bound_bytes"],"required":plan["storage_estimate"]["required_free_bytes"],"confidence":plan["storage_estimate"]["confidence"],"eligible":plan["storage_estimate"]["per_volume"][0]["eligible"]}))?;
        Ok(plan)
    }
    async fn execute(
        &self,
        session: &NativeSession,
        report: &mut TransformationReport,
        plan: &Value,
    ) -> Result<Value, LarmError> {
        self.call(
            session,
            report,
            "transformation.execute",
            execute(plan),
            &self.access(session),
        )
        .await
    }
    async fn state(
        &self,
        db: &mut SqliteConnection,
        report: &mut TransformationReport,
        id: &str,
        phase: &str,
        attempt: i64,
        resources: i64,
    ) -> Result<Value, LarmError> {
        let state = job(db, id).await?;
        report.check(
            "ledger-state-attempt-resources",
            json!({"phase":phase,"attempt":attempt,"held":resources}),
            json!({"phase":state["phase"],"attempt":state["attempt"],"held":held(db,id).await?}),
        )?;
        let journal: String =
            sqlx::query_scalar("SELECT phase FROM ref_journal WHERE job_id=? AND attempt=?")
                .bind(id)
                .bind(attempt)
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        report.check("journal-matches-phase", json!(phase), json!(journal))?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM ref_event),(SELECT count(*) FROM ref_outbox)",
        )
        .fetch_one(&mut *db)
        .await
        .map_err(db_error)?;
        report.check("event-outbox-pairing", json!(counts.0), json!(counts.1))?;
        report
            .checkpoints
            .insert(format!("{id}-{attempt}-{phase}"), state.clone());
        Ok(state)
    }
    async fn output(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut TransformationReport,
        receipt: &Value,
        original: &[u8],
        encoded: &[u8],
    ) -> Result<Value, LarmError> {
        let id = receipt["job_id"]
            .as_str()
            .filter(|s| larm_domain::bridge::is_uuid_v7(s))
            .ok_or_else(|| invalid("transformation_job_invalid"))?;
        let state = job(db, id).await?;
        let attempt = state["attempt"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("transformation_attempt_invalid"))?;
        self.state(db, report, id, "COMPLETED", attempt, 0).await?;
        let manifest = self
            .call(
                session,
                report,
                "transformation.verify",
                json!({"job_id":id}),
                &self.access(session),
            )
            .await?;
        let relative = format!("{id}-{attempt}");
        let root = session.work_dir().join("output").join(&relative);
        let artifacts = manifest["artifacts"]
            .as_array()
            .ok_or_else(|| invalid("transformation_manifest_invalid"))?;
        let names = artifacts
            .iter()
            .filter_map(|a| a["relative_path"].as_str())
            .collect::<BTreeSet<_>>();
        report.check(
            "manifest-only-owned-artifacts",
            json!([
                format!("{relative}/artifact.b64"),
                format!("{relative}/provenance.json")
            ]),
            json!(names),
        )?;
        let actual = std::fs::read(root.join("artifact.b64")).map_err(super::io)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&actual)
            .map_err(|_| invalid("transformation_output_not_base64"))?;
        report.check("real-base64-rust-oracle",json!({"descriptor":descriptor(encoded),"decodes_to_original":true,"changed":true}),json!({"descriptor":descriptor(&actual),"decodes_to_original":decoded==original,"changed":actual!=original}))?;
        let stored = read_json(&root.join("manifest.json"))?;
        report.check("manifest-file-and-rpc", stored, manifest.clone())?;
        let mut descriptors = Vec::new();
        for artifact in artifacts {
            let name = artifact["relative_path"]
                .as_str()
                .ok_or_else(|| invalid("transformation_artifact_invalid"))?;
            let path = session.work_dir().join("output").join(name);
            crate::package::check_managed_path(&path)?;
            let size = std::fs::metadata(&path).map_err(super::io)?.len();
            report.check(
                "artifact-byte-sha",
                artifact["sha256"].clone(),
                json!(file_digest(&path)?),
            )?;
            report.check(
                "artifact-byte-size",
                artifact["size_bytes"].clone(),
                json!(size.to_string()),
            )?;
            descriptors.push(json!({"relative_path":name,"sha256":artifact["sha256"],"size_bytes":size,"required":artifact["required"]}));
        }
        descriptors.sort_by(|a, b| {
            a["relative_path"]
                .as_str()
                .cmp(&b["relative_path"].as_str())
        });
        let manifest_hash = hash(&descriptors)?;
        report.check(
            "full-manifest-content-key",
            json!({"hash":manifest_hash,"revision":format!("content:{manifest_hash}")}),
            json!({"hash":manifest["manifest_hash"],"revision":manifest["revision_key"]}),
        )?;
        let provenance = read_json(&root.join("provenance.json"))?;
        report.check("provenance-backed-by-bytes",json!({"input":self.config.profile["input"],"variant":self.config.profile["input_variant_id"],"output":descriptor(encoded),"tool":{"id":session.manifest().id,"version":"1.0.0","adapter_package_sha256":session.manifest().package_sha256,"package_verification":"HOST_OWNED"},"parameters":{},"job_id":id,"attempt":attempt,"logs":["input_verified","base64_encoded","output_verified"]}),
            json!({"input":provenance["source"],"variant":provenance["input_variant_id"],"output":provenance["output"],"tool":provenance["tool"],"parameters":provenance["parameters"],"job_id":provenance["job_id"],"attempt":provenance["attempt"],"logs":provenance["logs"]}))?;
        let path = session
            .work_dir()
            .join("state/job-status")
            .join(format!("{id}.json"));
        let plain = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
        let uri = url::Url::from_file_path(&plain)
            .map_err(|_| invalid("transformation_status_uri_invalid"))?;
        report.check(
            "receipt-uri-is-owned-status",
            json!(uri.as_str()),
            receipt["status_url"].clone(),
        )?;
        report.check(
            "receipt-status-matches-ledger",
            state["document"].clone(),
            read_json(&path)?,
        )?;
        report.checkpoints.insert(format!("output-{id}-{attempt}"),json!({"manifest":manifest,"provenance":provenance,"decoded_sha256":digest(&decoded),"decoded_bytes":decoded.len().to_string()}));
        Ok(manifest)
    }
    async fn interrupted(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut TransformationReport,
        plan: &Value,
        how: &str,
    ) -> Result<(String, i64), LarmError> {
        let mut progress = session.progress()?;
        let request = Request::new("transformation.execute", execute(plan));
        let id = request.id.clone();
        let access = self.access(session);
        report.methods.insert("transformation.execute".into());
        let mut pending =
            Box::pin(session.call(request, &access, Some(Duration::from_secs(30)), None));
        let event = loop {
            tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("transformation_progress_missing"))?;if event.phase=="TRANSFORM_ENCODING"{break event;}},_=&mut pending=>return Err(invalid("transformation_completed_before_encoding_checkpoint"))}
        };
        report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
        let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
        report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
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
            "encoding-heartbeat",
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|v| v.contains(&json!(id)))),
        )?;
        if how == "eof" {
            drop(pending);
            session.restart().await?;
        } else {
            if how == "cancel" {
                report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
            } else if how == "crash" {
                session.crash()?;
            } else if how == "corrupt" {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.input(session))
                    .map_err(super::io)?;
                file.write_all(b"!").map_err(super::io)?;
                file.sync_all().map_err(super::io)?;
            } else {
                return Err(invalid("transformation_interrupt_mode_invalid"));
            }
            let failure = pending
                .await
                .err()
                .ok_or_else(|| invalid("transformation_interrupted_call_succeeded"))?;
            report.check(
                if how == "crash" {
                    "worker-crash"
                } else if how == "corrupt" {
                    "combined-fault-response"
                } else {
                    "cancel-result"
                },
                if how == "crash" {
                    json!("ADAPTER_CRASHED")
                } else if how == "corrupt" {
                    json!("reference_transform_persistence_failed")
                } else {
                    json!("operation_cancelled")
                },
                if how == "crash" {
                    json!(failure.code.as_str())
                } else {
                    json!(failure.details.reason)
                },
            )?;
            report.checkpoints.insert(
                format!("{how}-error"),
                serde_json::to_value(failure)
                    .map_err(|_| invalid("transformation_error_invalid"))?,
            );
        }
        report.checkpoints.insert(
            how.into(),
            json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
        );
        Ok((active.0, active.2))
    }
    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut TransformationReport,
        original: &[u8],
        encoded: &[u8],
    ) -> Result<(), LarmError> {
        let access = self.access(session);
        let description = self
            .call(
                session,
                report,
                "transformation.describe",
                json!({}),
                &Access::default(),
            )
            .await?;
        report.check("description-real-tool",json!({"formats_input":["BINARY"],"formats_output":["BASE64"],"parameter_schema":{"type":"object","properties":{},"additionalProperties":false},"tool_version":"1.0.0","pause_supported":false}),description)?;
        let before = snapshot(db).await?;
        let sentinel = file_digest(&self.config.forbidden)?;
        for (key, value, reason) in [
            (
                "input_variant_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "reference_transform_resource_not_bound",
            ),
            (
                "storage_location_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "reference_transform_resource_not_bound",
            ),
            (
                "parameters",
                json!({"script":"forbidden"}),
                "reference_transform_options_not_supported",
            ),
            (
                "tool_version",
                json!("2.0"),
                "reference_transform_options_not_supported",
            ),
            (
                "target_format",
                json!("GGUF"),
                "reference_transform_options_not_supported",
            ),
            (
                "hard_budget_bytes",
                json!("1"),
                "reference_transform_hard_budget_insufficient",
            ),
        ] {
            let mut request = self.request();
            request[key] = value;
            self.refused(
                session,
                report,
                "transformation.plan",
                request,
                &access,
                reason,
            )
            .await?;
        }
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
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
            "transformation.plan",
            self.request(),
            &Access {
                writes: vec![self.input(session)],
                ..Access::default()
            },
            "write_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
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
            "transformation.plan",
            self.request(),
            &Access {
                secrets: vec!["larm/ungranted".into()],
                ..Access::default()
            },
            "secret_denied",
        )
        .await?;
        report.check(
            "rejections-before-ledger-mutation",
            before,
            snapshot(db).await?,
        )?;
        let plan = self.plan(session, report, encoded).await?;
        report.check(
            "dry-plan-no-output",
            json!(0),
            json!(std::fs::read_dir(session.work_dir().join("output"))
                .map_err(super::io)?
                .count()),
        )?;
        let mut wrong = execute(&plan);
        wrong["plan_hash"] = json!("b".repeat(64));
        self.refused(
            session,
            report,
            "transformation.execute",
            wrong,
            &access,
            "reference_transform_plan_hash_mismatch",
        )
        .await?;
        let mut approved = execute(&plan);
        approved["approval_id"] = json!(uuid::Uuid::now_v7().to_string());
        self.refused(
            session,
            report,
            "transformation.execute",
            approved,
            &access,
            "reference_transform_approval_owned_by_core",
        )
        .await?;
        {
            let restore = Restore {
                path: self.input(session),
                bytes: original,
            };
            let mut changed = original.to_vec();
            changed[0] = b'!';
            std::fs::write(&restore.path, changed).map_err(super::io)?;
            self.refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "reference_transform_input_changed",
            )
            .await?;
            drop(restore);
        }
        let mut expired = self.plan(session, report, encoded).await?;
        expired["expires_at"] = json!("2000-01-01T00:00:00.000Z");
        let mut unsigned = expired.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("transformation_plan_invalid"))?
            .remove("plan_hash");
        expired["plan_hash"] = json!(hash(&unsigned)?);
        sqlx::query("UPDATE ref_plan SET document=?,hash=? WHERE id=?")
            .bind(
                serde_json::to_string(&expired)
                    .map_err(|_| invalid("transformation_plan_invalid"))?,
            )
            .bind(expired["plan_hash"].as_str())
            .bind(expired["plan_id"].as_str())
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        self.refused(
            session,
            report,
            "transformation.execute",
            execute(&expired),
            &access,
            "reference_transform_plan_expired",
        )
        .await?;
        sqlx::query("CREATE TRIGGER p026_transform_accept BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'owned_accept_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let before = snapshot(db).await?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "reference_transform_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_transform_accept")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        failure?;
        report.check("accept-transaction-rollback", before, snapshot(db).await?)?;
        let blocker = session.work_dir().join("state/staging");
        std::fs::write(&blocker, b"owned staging blocker").map_err(super::io)?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "reference_transform_io_failed",
            )
            .await;
        std::fs::remove_file(&blocker).map_err(super::io)?;
        let original_error = failure?;
        let failed: String = sqlx::query_scalar("SELECT id FROM ref_job WHERE phase='FAILED'")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        self.state(db, report, &failed, "FAILED", 1, 2).await?;
        self.refused(
            session,
            report,
            "transformation.verify",
            json!({"job_id":failed}),
            &access,
            "reference_transform_job_not_completed",
        )
        .await?;
        let retry = self.plan(session, report, encoded).await?;
        let completed = self.execute(session, report, &retry).await?;
        report.check("retry-same-job", json!(failed), completed["job_id"].clone())?;
        let manifest = self
            .output(session, db, report, &completed, original, encoded)
            .await?;
        let before = snapshot(db).await?;
        let replay = self.execute(session, report, &retry).await?;
        report.check("receipt-idempotent", completed.clone(), replay)?;
        let error = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "reference_transform_io_failed",
            )
            .await?;
        report.check(
            "old-error-immutable",
            serde_json::to_value(original_error)
                .map_err(|_| invalid("transformation_error_invalid"))?,
            serde_json::to_value(error).map_err(|_| invalid("transformation_error_invalid"))?,
        )?;
        report.check("replay-no-new-events", before, snapshot(db).await?)?;
        let payload = session
            .work_dir()
            .join("output")
            .join(format!("{failed}-2/artifact.b64"));
        {
            let restore = Restore {
                path: payload.clone(),
                bytes: encoded,
            };
            let mut changed = encoded.to_vec();
            changed[0] = b'!';
            std::fs::write(&payload, changed).map_err(super::io)?;
            self.refused(
                session,
                report,
                "transformation.verify",
                json!({"job_id":failed}),
                &access,
                "reference_transform_output_changed",
            )
            .await?;
            drop(restore);
        }
        let verified = self
            .call(
                session,
                report,
                "transformation.verify",
                json!({"job_id":failed}),
                &access,
            )
            .await?;
        report.check("verify-rereads-restored-bytes", manifest, verified)?;
        let cancel = self.plan(session, report, encoded).await?;
        let (cancelled, _) = self
            .interrupted(session, db, report, &cancel, "cancel")
            .await?;
        self.state(db, report, &cancelled, "CANCELLED", 1, 0)
            .await?;
        report.check("cancel-cleans-only-attempt",json!({"stage_files":0,"old_output_sha":digest(encoded)}),json!({"stage_files":std::fs::read_dir(session.work_dir().join("state/staging").join(format!("{cancelled}-1"))).map_err(super::io)?.count(),"old_output_sha":file_digest(&payload)?}))?;
        let crash = self.plan(session, report, encoded).await?;
        let (crashed, offset) = self
            .interrupted(session, db, report, &crash, "crash")
            .await?;
        let partial = session
            .work_dir()
            .join("state/staging")
            .join(format!("{crashed}-1/artifact.b64"));
        report.check("crash-durable-bytes-and-resources",json!({"durable":true,"held":2}),json!({"durable":std::fs::metadata(&partial).map_err(super::io)?.len()>=offset as u64,"held":held(db,&crashed).await?}))?;
        let partial_sha = file_digest(&partial)?;
        let incomplete = session
            .work_dir()
            .join("state")
            .join(format!("fault-{crashed}-1.tmp"));
        std::fs::write(&incomplete, b"{\"job_id\":").map_err(super::io)?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &crashed, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check(
            "crash-cause-preserved",
            json!("ADAPTER_CRASHED"),
            recovered["document"]["error"]["code"].clone(),
        )?;
        report.check(
            "incomplete-fault-not-consumed",
            json!(digest(b"{\"job_id\":")),
            json!(file_digest(&incomplete)?),
        )?;
        let restart = self.plan(session, report, encoded).await?;
        let recovered_receipt = self.execute(session, report, &restart).await?;
        report.check(
            "restart-new-attempt-same-job",
            json!(crashed),
            recovered_receipt["job_id"].clone(),
        )?;
        self.output(session, db, report, &recovered_receipt, original, encoded)
            .await?;
        report.check(
            "restart-does-not-resume-old-partial",
            json!(partial_sha),
            json!(file_digest(&partial)?),
        )?;
        let fault = self.plan(session, report, encoded).await?;
        sqlx::query("CREATE TRIGGER p026_transform_final BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned_final_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&fault),
                &access,
                "reference_transform_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_transform_final")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        failure?;
        let uncertain: String =
            sqlx::query_scalar("SELECT id FROM ref_job WHERE phase='VERIFYING'")
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        self.state(db, report, &uncertain, "VERIFYING", 1, 2)
            .await?;
        let published = session
            .work_dir()
            .join("output")
            .join(format!("{uncertain}-1/artifact.b64"));
        let published_sha = file_digest(&published)?;
        report.check(
            "uncertain-has-no-receipt",
            json!(false),
            json!(session
                .work_dir()
                .join("state/job-status")
                .join(format!("{uncertain}.json"))
                .exists()),
        )?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &uncertain, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check(
            "persistence-cause-survives-restart",
            json!("IntegrityError"),
            recovered["document"]["error"]["details"]["original_error"]["code"].clone(),
        )?;
        let retry = self.plan(session, report, encoded).await?;
        let receipt = self.execute(session, report, &retry).await?;
        report.check(
            "uncertain-retry-same-job",
            json!(uncertain),
            receipt["job_id"].clone(),
        )?;
        self.output(session, db, report, &receipt, original, encoded)
            .await?;
        report.check(
            "uncertain-publication-retained",
            json!(published_sha),
            json!(file_digest(&published)?),
        )?;
        let combined = self.plan(session, report, encoded).await?;
        sqlx::query("CREATE TRIGGER p026_transform_error BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='FAILED' BEGIN SELECT RAISE(ABORT,'owned_error_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let restore = Restore {
            path: self.input(session),
            bytes: original,
        };
        let fault = self
            .interrupted(session, db, report, &combined, "corrupt")
            .await;
        drop(restore);
        sqlx::query("DROP TRIGGER p026_transform_error")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        let (combined_id, _) = fault?;
        self.state(db, report, &combined_id, "VERIFYING", 1, 2)
            .await?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &combined_id, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check("original-integrity-precedes-outbox-error",json!({"code":"INTEGRITY_FAILED","reason":"reference_transform_final_verification_failed"}),json!({"code":recovered["document"]["error"]["code"],"reason":recovered["document"]["error"]["details"]["reason"]}))?;
        let retry = self.plan(session, report, encoded).await?;
        let receipt = self.execute(session, report, &retry).await?;
        report.check(
            "combined-fault-retry-same-job",
            json!(combined_id),
            receipt["job_id"].clone(),
        )?;
        self.output(session, db, report, &receipt, original, encoded)
            .await?;
        let eof = self.plan(session, report, encoded).await?;
        let (eof_id, _) = self.interrupted(session, db, report, &eof, "eof").await?;
        self.state(db, report, &eof_id, "CANCELLED", 1, 0).await?;
        report.check(
            "input-unchanged",
            json!(digest(original)),
            json!(file_digest(&self.input(session))?),
        )?;
        report.check(
            "forbidden-sentinel-unchanged",
            json!(sentinel),
            json!(file_digest(&self.config.forbidden)?),
        )?;
        report
            .checkpoints
            .insert("final-ledger".into(), snapshot(db).await?);
        let methods = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "transformation")
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        report.check(
            "all-transformation-signatures",
            json!(methods),
            json!(report
                .methods
                .iter()
                .filter(|m| m.starts_with("transformation."))
                .collect::<BTreeSet<_>>()),
        )?;
        Ok(())
    }
    async fn run(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut TransformationReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "reference.transformation.base64"
            || session.manifest().family.as_str() != "transformation"
            || session.manifest().capabilities != [CAP]
        {
            return Err(invalid("transformation_subject_mismatch"));
        }
        report.check(
            "exact-capability-profile",
            self.bundle.clone(),
            read_json(&session.package_dir().join("reference-capabilities.json"))?,
        )?;
        report.check(
            "exact-instance-profile",
            self.config.profile.clone(),
            read_json(&session.work_dir().join("profile/profile.json"))?,
        )?;
        for path in [&self.config.expected_input, &self.config.forbidden] {
            let path = canonical(path)?;
            if path.starts_with(canonical(session.work_dir())?)
                || path.starts_with(canonical(session.package_dir())?)
            {
                return Err(invalid("transformation_oracle_resource_exposed"));
            }
        }
        let original = std::fs::read(&self.config.expected_input).map_err(super::io)?;
        if original.is_empty() || original.len() > 64 << 20 {
            return Err(invalid("transformation_fixture_size_invalid"));
        }
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(&original)
            .into_bytes();
        report.check("input-pinned-independent-bytes",json!({"sha256":digest(&original),"size_bytes":original.len().to_string()}),json!({"sha256":self.config.profile["input"]["sha256"],"size_bytes":self.config.profile["input"]["size_bytes"]}))?;
        let wire = run_common(
            run,
            session,
            &WireProbe {
                method: "transformation.describe".into(),
                params: json!({}),
                access: Access::default(),
            },
        )
        .await;
        let result = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = result?;
        session.restart().await?;
        let path = session.work_dir().join("state/transform.sqlite");
        crate::package::check_managed_path(&path)?;
        let connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(2)),
        )
        .await
        .map_err(db_error)?;
        let ledger = Arc::new(LedgerIo(tokio::sync::Mutex::new(Some(connection))));
        session.register_io(ledger.clone());
        let mut guard = ledger.0.lock().await;
        let db = guard
            .as_mut()
            .ok_or_else(|| invalid("transformation_ledger_closed"))?;
        let app: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        if app != 0x4C415254 || version != 1 {
            return Err(invalid("transformation_ledger_not_owned"));
        }
        self.functional(session, db, report, &original, &encoded)
            .await?;
        drop(guard);
        let material = report.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:transformation",
            "capability:vendor.reference.base64",
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
impl NativeSuite for ReferenceTransformationSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"transformation","capabilities":[CAP],"suite":"reference.transformation.base64.v1","bundle":self.bundle,"profile":self.config.profile,
        "expected_input":self.config.expected_input,"forbidden":self.config.forbidden,"evidence_root":self.config.evidence_root})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut report = TransformationReport {
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
            serde_json::to_vec_pretty(&report)
                .map_err(|_| invalid("transformation_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}
