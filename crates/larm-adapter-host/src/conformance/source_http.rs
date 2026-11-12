//! Oráculo HTTP para fuentes import-only que entregan bloques al Core.
//! Los bytes y el servidor pertenecen al operador; no se consulta estado privado
//! del plugin ni se atribuye al worker el journal/CAS del Download Manager.
use super::source::SourceReport;
use super::wire::{run_common, WireProbe};
use super::{canonical, file_digest, invalid, NativeSession, NativeSuite};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use larm_adapter_sdk::permissions::Access;
use larm_adapter_sdk::protocol::Request;
use larm_adapter_sdk::source::{download_spec, SourceDownloadChunk};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::LarmError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const BLOCK: u64 = 512 << 10;
const SCOPE: &str = "import-only HTTP product worker, controlled external HTTP faults and bytes; no Core journal/CAS or upstream-wide qualification";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSourceConfig {
    pub source_id: String,
    pub storage_id: String,
    pub url: String,
    pub expected_file: PathBuf,
    pub control: PathBuf,
    pub requests: PathBuf,
    pub evidence_root: PathBuf,
}

pub struct HttpSourceSuite {
    config: HttpSourceConfig,
}

impl HttpSourceSuite {
    pub fn new(config: HttpSourceConfig) -> Result<Self, LarmError> {
        let url = url::Url::parse(&config.url).map_err(|_| invalid("source_http_url_invalid"))?;
        if !larm_domain::bridge::is_uuid_v7(&config.source_id)
            || !larm_domain::bridge::is_uuid_v7(&config.storage_id)
            || url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid("source_http_config_invalid"));
        }
        for path in [
            &config.expected_file,
            &config.control,
            &config.requests,
            &config.evidence_root,
        ] {
            if !path.is_absolute() {
                return Err(invalid("source_http_path_invalid"));
            }
            crate::package::check_managed_path(path)?;
        }
        Ok(Self { config })
    }

    pub fn report_path(&self, run_id: &str) -> PathBuf {
        self.config
            .evidence_root
            .join(run_id)
            .join("source-http.json")
    }

    fn mode(&self, mode: &str) -> Result<(), LarmError> {
        let temp = self.config.control.with_extension("tmp");
        crate::package::check_managed_path(&temp)?;
        crate::package::check_managed_path(&self.config.control)?;
        std::fs::write(&temp, json!({"mode":mode}).to_string()).map_err(|error| {
            let message = format!(
                "kind={:?}; os_error={:?}",
                error.kind(),
                error.raw_os_error()
            );
            super::io(error).with_original("http-fixture", "CONTROL_WRITE", &message)
        })?;
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match std::fs::rename(&temp, &self.config.control) {
                Ok(()) => return Ok(()),
                Err(error)
                    if cfg!(windows)
                        && matches!(error.raw_os_error(), Some(5 | 32 | 33))
                        && Instant::now() < deadline =>
                {
                    // El servidor lee este control mientras espera la caída.
                    // Windows puede negar temporalmente su reemplazo si ese
                    // handle no comparte DELETE. Conservar el JSON anterior y
                    // reintentar sólo el reemplazo atómico, con cota breve.
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let message = format!(
                        "kind={:?}; os_error={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                    return Err(super::io(error).with_original(
                        "http-fixture",
                        "CONTROL_REPLACE",
                        &message,
                    ));
                }
            }
        }
    }

    fn requests(&self) -> Result<Vec<Value>, LarmError> {
        let bytes = std::fs::read(&self.config.requests).map_err(super::io)?;
        if bytes.len() > 1 << 20 {
            return Err(invalid("source_http_log_too_large"));
        }
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|line| {
                serde_json::from_slice(line).map_err(|_| invalid("source_http_log_invalid"))
            })
            .collect()
    }

    fn access(&self, session: &NativeSession) -> Access {
        Access {
            reads: vec![session.work_dir().join("profile.json")],
            writes: vec![session.work_dir().to_owned()],
            urls: vec![self.config.url.clone()],
            ..Access::default()
        }
    }

    async fn call(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        method: &str,
        params: Value,
    ) -> Result<Value, LarmError> {
        report.methods.insert(method.into());
        session
            .call(
                Request::new(method, params),
                &self.access(session),
                Some(Duration::from_secs(20)),
                None,
            )
            .await
    }

    async fn chunk(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        request: Value,
    ) -> Result<SourceDownloadChunk, LarmError> {
        serde_json::from_value(
            self.call(session, report, "source.download_chunk", request)
                .await?,
        )
        .map_err(|_| invalid("source_http_chunk_invalid"))
    }

    async fn interrupted(
        &self,
        session: &NativeSession,
        report: &mut SourceReport,
        params: Value,
        crash: bool,
    ) -> Result<(), LarmError> {
        self.mode("held")?;
        let before = self.requests()?.len();
        let request = Request::new("source.download_chunk", params);
        let id = request.id.clone();
        let mut progress = session.progress()?;
        let access = self.access(session);
        let pending = session.call(request, &access, Some(Duration::from_secs(20)), None);
        tokio::pin!(pending);
        let event = tokio::select! {
            event = progress.recv() => event.map_err(|_| invalid("source_http_progress_missing"))?,
            _ = &mut pending => return Err(invalid("source_http_finished_before_progress")),
        };
        report.check(
            "chunk-progress",
            json!({"id":id,"phase":"SOURCE_READ","offset":0}),
            json!({"id":event.request_id,"phase":event.phase,"offset":event.completed_bytes}),
        )?;
        // No basta una notificación: observar GET en vuelo en el servidor propio.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if self
                .requests()?
                .iter()
                .skip(before)
                .any(|r| r["method"] == "GET" && r["mode"] == "held")
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(invalid("source_http_inflight_missing"));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                _ = &mut pending => return Err(invalid("source_http_finished_before_fault")),
            }
        }
        let heartbeat = self
            .call(session, report, "core.heartbeat", json!({}))
            .await?;
        report.check(
            "inflight-heartbeat",
            json!(true),
            json!(heartbeat["running_request_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&json!(id)))),
        )?;
        let pid = session.pid()?;
        if crash {
            session.crash()?;
        } else {
            report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
        }
        let failure = pending
            .await
            .err()
            .ok_or_else(|| invalid("source_http_fault_succeeded"))?;
        let observed = if crash {
            failure.code.as_str().to_owned()
        } else {
            failure.details.reason
        };
        report.check(
            if crash { "crash-error" } else { "cancel-error" },
            json!(if crash {
                "ADAPTER_CRASHED"
            } else {
                "operation_cancelled"
            }),
            json!(observed),
        )?;
        if !crash {
            report.check("cancel-cooperative-pid", json!(pid), json!(session.pid()?))?;
            self.call(session, report, "core.heartbeat", json!({}))
                .await?;
            report.check(
                "cancel-finished-id",
                json!(false),
                json!(session.cancel(&id).await?),
            )?;
        }
        report.checkpoints.insert(
            if crash { "crash" } else { "cancel" }.into(),
            json!({"pid":pid,"request_id":id,"inflight_get":true}),
        );
        self.mode("normal")
    }

    async fn run(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut SourceReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().family.as_str() != "source"
            || !session.manifest().capabilities.is_empty()
        {
            return Err(invalid("source_http_manifest_mismatch"));
        }
        for path in [
            &self.config.expected_file,
            &self.config.control,
            &self.config.requests,
            &self.config.evidence_root,
        ] {
            let path = canonical(path)?;
            if path.starts_with(canonical(session.work_dir())?)
                || path.starts_with(canonical(session.package_dir())?)
            {
                return Err(invalid("source_http_oracle_exposed"));
            }
        }
        let expected_size = std::fs::metadata(&self.config.expected_file)
            .map_err(super::io)?
            .len();
        if expected_size <= BLOCK || expected_size > 8 << 20 {
            return Err(invalid("source_http_fixture_size_invalid"));
        }
        let expected = std::fs::read(&self.config.expected_file).map_err(super::io)?;
        let expected_hash = file_digest(&self.config.expected_file)?;
        let profile: Value = serde_json::from_slice(
            &std::fs::read(session.work_dir().join("profile.json")).map_err(super::io)?,
        )
        .map_err(|_| invalid("source_http_profile_invalid"))?;
        report.check("profile-pinned-http", json!({"source_id":self.config.source_id,"source_type":"HTTP","url":self.config.url,"sha256":expected_hash}), profile)?;
        self.mode("normal")?;
        let upstream = json!({"source_id":self.config.source_id,"upstream_namespace":"direct-http","upstream_model_id":"explicit-url"});
        let wire = run_common(
            run,
            session,
            &WireProbe {
                method: "source.get_model".into(),
                params: upstream.clone(),
                access: self.access(session),
            },
        )
        .await;
        let common = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = common?;
        session.restart().await?;
        let inspection = self
            .call(session, report, "source.get_model", upstream.clone())
            .await?;
        report.check(
            "inspection-identity",
            json!("explicit-url"),
            inspection["upstream_identity"].clone(),
        )?;
        report.check(
            "inspection-checksum",
            json!(expected_hash),
            inspection["artifacts"][0]["sha256"].clone(),
        )?;
        let revisions = self
            .call(session, report, "source.list_revisions", upstream.clone())
            .await?;
        let key = revisions["items"][0]["revision_key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("source_http_revision_missing"))?;
        let mut revision = upstream;
        revision["revision_key"] = json!(key);
        let manifest = self
            .call(session, report, "source.get_manifest", revision.clone())
            .await?;
        report.check("manifest-pinned", json!({"revision":key,"size":expected.len().to_string(),"sha":expected_hash,"count":1}), json!({"revision":manifest["revision_key"],"size":manifest["artifacts"][0]["size_bytes"],"sha":manifest["artifacts"][0]["sha256"],"count":manifest["artifacts"].as_array().map(Vec::len)}))?;
        let estimate = self
            .call(session, report, "source.estimate_size", revision.clone())
            .await?;
        report.check(
            "estimate-exact",
            json!({"confidence":"EXACT","bytes":expected.len().to_string()}),
            json!({"confidence":estimate["confidence"],"bytes":estimate["download_bytes"]}),
        )?;
        // Metadatos obtenidos por RPC, para preparar el catálogo central sin
        // leer UUIDs/estado privado del worker ni reconstruir su manifiesto.
        report.checkpoints.insert(
            "source_metadata".into(),
            json!({"inspection":inspection,"revisions":revisions,"manifest":manifest,"estimate":estimate}),
        );
        let sync = self
            .call(
                session,
                report,
                "source.sync",
                json!({"source_id":self.config.source_id,"cursor":null,"etag":null,"query":{}}),
            )
            .await?;
        report.check(
            "sync-inspection",
            json!([inspection]),
            sync["models"].clone(),
        )?;
        let auth = self.call(session, report, "source.auth_probe", json!({"source_id":self.config.source_id,"auth_mode":"NONE","account_id":null,"credential_reference_id":null})).await?;
        report.check(
            "anonymous-auth",
            json!(false),
            auth["auth_required"].clone(),
        )?;
        let search = self.call(session, report, "source.search", json!({"source_id":self.config.source_id,"q":"","capabilities":[],"cursor":null,"limit":1})).await.err().ok_or_else(|| invalid("source_http_search_succeeded"))?;
        report.check(
            "import-only-search",
            json!("UNSUPPORTED_CAPABILITY"),
            json!(search.code.as_str()),
        )?;
        let variant = uuid::Uuid::now_v7().to_string();
        let plan_value = self.call(session, report, "source.download_plan", json!({"source_id":self.config.source_id,"variant_id":variant,"storage_location_id":self.config.storage_id,"hard_budget_bytes":null,"priority":"HIGH"})).await?;
        let plan: larm_domain::artifacts::Plan = serde_json::from_value(plan_value.clone())
            .map_err(|_| invalid("source_http_plan_invalid"))?;
        let spec = download_spec(&plan)?;
        report.check(
            "plan-artifacts-pinned",
            manifest["artifacts"].clone(),
            serde_json::to_value(&spec.artifacts)
                .map_err(|_| invalid("source_http_plan_invalid"))?,
        )?;
        report.check(
            "plan-core-variant",
            json!({"variant":variant,"revision":revision}),
            json!({"variant":spec.variant_id,"revision":spec.revision}),
        )?;
        let mut unsigned = plan_value;
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("source_http_plan_invalid"))?
            .remove("plan_hash");
        report.check("plan-jcs", json!(hash(&unsigned)?), json!(plan.plan_hash))?;
        let owned = self
            .call(
                session,
                report,
                "source.download",
                json!({"plan_id":plan.plan_id,"plan_hash":plan.plan_hash,"approval_id":null}),
            )
            .await
            .err()
            .ok_or_else(|| invalid("source_http_created_private_download"))?;
        report.check(
            "core-owns-download-job",
            json!("download_execution_owned_by_download_manager"),
            json!(owned.details.reason),
        )?;
        let path = manifest["artifacts"][0]["relative_path"].clone();
        let mut request = json!({"source_id":self.config.source_id,"job_id":uuid::Uuid::now_v7().to_string(),"plan_id":plan.plan_id,"plan_hash":plan.plan_hash,"approval_id":null,"revision":revision,"relative_path":path,"offset":"0","max_bytes":BLOCK.to_string(),"validator":null});
        let mut received = Vec::new();
        loop {
            let chunk = self.chunk(session, report, request.clone()).await?;
            let data = STANDARD
                .decode(&chunk.data_base64)
                .map_err(|_| invalid("source_http_base64_invalid"))?;
            report.check(
                "bounded-contiguous-chunk",
                json!(true),
                json!(
                    chunk.offset.get() == received.len() as u64
                        && !data.is_empty()
                        && data.len() <= BLOCK as usize
                        && chunk.total_bytes.map(|n| n.get()) == Some(expected.len() as u64)
                ),
            )?;
            received.extend(data);
            if received.len() > expected.len() {
                return Err(invalid("source_http_bytes_overflow"));
            }
            if chunk.eof {
                break;
            }
            request["offset"] = json!(received.len().to_string());
            request["validator"] = serde_json::to_value(chunk.validator)
                .map_err(|_| invalid("source_http_validator_invalid"))?;
            if received.len() == BLOCK as usize {
                session.restart().await?;
            }
        }
        report.check(
            "full-bytes-after-restart",
            json!(true),
            json!(received == expected),
        )?;
        report.check(
            "actual-range-if-range",
            json!(true),
            json!(self.requests()?.iter().any(|r| r["range"]
                == format!("bytes={}-{}", BLOCK, 2 * BLOCK - 1)
                && r["if_range"].as_str().is_some_and(|v| !v.is_empty()))),
        )?;
        request["offset"] = json!("0");
        request["validator"] = Value::Null;
        let mut traversal = request.clone();
        traversal["relative_path"] = json!("../outside.bin");
        let traversal_rejected = self.chunk(session, report, traversal).await.is_err();
        report.check("traversal-rejected", json!(true), json!(traversal_rejected))?;
        let denied_access = Access {
            urls: vec!["http://127.0.0.1:1/denied".into()],
            ..Access::default()
        };
        let before_denied = self.requests()?.len();
        let denied = session.call(Request::new("source.get_model", json!({"source_id":self.config.source_id,"upstream_namespace":"direct-http","upstream_model_id":"explicit-url"})), &denied_access, None, None).await.err().ok_or_else(|| invalid("source_http_permission_succeeded"))?;
        report.check(
            "ungranted-origin",
            json!({"code":"FORBIDDEN","reason":"network_denied"}),
            json!({"code":denied.code.as_str(),"reason":denied.details.reason}),
        )?;
        report.check(
            "denied-without-network",
            json!(before_denied),
            json!(self.requests()?.len()),
        )?;
        for (mode, code) in [
            ("auth", "AUTH_REQUIRED"),
            ("rate", "RESOURCE_UNAVAILABLE"),
            ("cut", "RESOURCE_UNAVAILABLE"),
        ] {
            self.mode(mode)?;
            let failure = self
                .chunk(session, report, request.clone())
                .await
                .err()
                .ok_or_else(|| invalid("source_http_fault_succeeded"))?;
            report.check(mode, json!(code), json!(failure.code.as_str()))?;
            report.check(
                "http-fault-observed",
                json!(if mode == "auth" {
                    401
                } else if mode == "rate" {
                    429
                } else {
                    206
                }),
                self.requests()?
                    .last()
                    .ok_or_else(|| invalid("source_http_log_missing"))?["status"]
                    .clone(),
            )?;
        }
        self.mode("normal")?;
        self.interrupted(session, report, request.clone(), false)
            .await?;
        self.interrupted(session, report, request.clone(), true)
            .await?;
        session.restart().await?;
        let recovered = self.chunk(session, report, request).await?;
        report.check(
            "read-after-crash",
            json!(true),
            json!(
                STANDARD
                    .decode(recovered.data_base64)
                    .map_err(|_| invalid("source_http_base64_invalid"))?
                    == expected[..BLOCK as usize]
            ),
        )?;
        report.check(
            "origin-immutable",
            json!(expected_hash),
            json!(file_digest(&self.config.expected_file)?),
        )?;
        let allowed = BTreeSet::from(["profile.json", "p048-identities.json", "tmp"]);
        let files = std::fs::read_dir(session.work_dir())
            .map_err(super::io)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(super::io)?;
        report.check(
            "read-only-worker-no-private-downloads",
            json!(allowed),
            json!(files),
        )?;
        // El host crea este TMP antes del worker y fija su entorno allí.
        // Admitir ese directorio concreto no admite descargas ocultas dentro.
        let temporary = session.work_dir().join("tmp");
        crate::package::check_managed_path(&temporary)?;
        report.check(
            "host-tmp-empty",
            json!(true),
            json!(
                std::fs::symlink_metadata(&temporary)
                    .map_err(super::io)?
                    .is_dir()
                    && std::fs::read_dir(&temporary)
                        .map_err(super::io)?
                        .next()
                        .is_none()
            ),
        )?;
        let material = report.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:source",
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
impl NativeSuite for HttpSourceSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"source","capabilities":[],"suite":"source.http.chunks.v1","configuration":self.config,"scope":SCOPE})
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
        let reset = self.mode("normal");
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&report)
                .map_err(|_| invalid("source_http_report_invalid"))?,
        )
        .map_err(super::io)?;
        reset?;
        report.observations
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn config(root: &std::path::Path) -> HttpSourceConfig {
        HttpSourceConfig {
            source_id: uuid::Uuid::now_v7().to_string(),
            storage_id: uuid::Uuid::now_v7().to_string(),
            url: "http://127.0.0.1:12345/weights.bin".into(),
            expected_file: root.join("expected.bin"),
            control: root.join("control.json"),
            requests: root.join("requests.ndjson"),
            evidence_root: root.join("evidence"),
        }
    }

    #[test]
    fn receta_controlada_rechaza_url_externa_secreto_y_ruta_relativa() {
        let root = tempfile::tempdir().unwrap();
        let valid = config(root.path());
        assert!(HttpSourceSuite::new(valid.clone()).is_ok());
        for url in [
            "https://example.com/weights.bin",
            "http://localhost:12345/weights.bin",
            "http://127.0.0.1:12345/weights.bin?token=x",
            "http://user:password@127.0.0.1:12345/weights.bin",
        ] {
            let mut invalid = valid.clone();
            invalid.url = url.into();
            assert!(HttpSourceSuite::new(invalid).is_err());
        }
        let mut invalid = valid;
        invalid.expected_file = "relative.bin".into();
        assert!(HttpSourceSuite::new(invalid).is_err());
    }

    #[test]
    fn receta_no_acepta_observaciones_ni_codigo_de_cliente() {
        let root = tempfile::tempdir().unwrap();
        let mut value = serde_json::to_value(config(root.path())).unwrap();
        for name in ["observations", "proof", "script"] {
            value[name] = json!("PASS");
            assert!(serde_json::from_value::<HttpSourceConfig>(value.clone()).is_err());
            value.as_object_mut().unwrap().remove(name);
        }
    }

    #[cfg(windows)]
    #[test]
    fn control_atomico_espera_a_un_lector_windows_transitorio() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let cfg = config(root.path());
        std::fs::write(&cfg.control, br#"{"mode":"held"}"#).unwrap();
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&cfg.control)
            .unwrap();
        let released = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(reader);
        });
        let suite = HttpSourceSuite::new(cfg.clone()).unwrap();
        let result = suite.mode("normal");
        released.join().unwrap();
        assert!(
            result.is_ok(),
            "un lector temporal del oráculo no debe invalidar la cualificación: {result:?}"
        );
        let value: Value = serde_json::from_slice(&std::fs::read(&cfg.control).unwrap()).unwrap();
        assert_eq!(value, json!({"mode":"normal"}));
        assert!(!cfg.control.with_extension("tmp").exists());
    }
}
