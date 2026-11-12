//! Oráculos comunes sobre el paquete original. Devuelven nueve checks reales;
//! cancel/progress/crash/permisos/estado/recuperación pertenecen a la suite de
//! familia. Un informe parcial no puede superar Proof::evaluate.
use super::{error, file_digest, invalid, NativeSession, ProbeExit};
use larm_adapter_sdk::contracts::Contracts;
use larm_adapter_sdk::permissions::Access;
use larm_adapter_sdk::protocol::{
    check_handshake, decode, Frame, HandshakeResult, Progress, Request, MAX_FRAME_BYTES,
};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// Caso positivo interno y de sólo lectura, con recursos concretos del perfil.
pub struct WireProbe {
    pub method: String,
    pub params: Value,
    pub access: Access,
}
impl WireProbe {
    pub fn descriptor(&self) -> Value {
        json!({"method":self.method,"params":self.params,
            "access":{"reads":self.access.reads,"writes":self.access.writes,"urls":self.access.urls,
                "processes":self.access.processes,"secrets":self.access.secrets}})
    }
}
#[derive(Clone, Serialize)]
pub struct WireCase {
    pub id: String,
    pub request_id: Option<String>,
    pub method: Option<String>,
    pub request_sha256: String,
    pub request_bytes: usize,
    pub response_sha256: String,
    pub expected: Value,
    pub observed: Value,
    pub progress_frames: usize,
}
#[derive(Clone, Serialize)]
pub struct WireReport {
    pub cases: Vec<WireCase>,
    pub before_work_sha256: Option<String>,
    pub rejected_work_sha256: Option<String>,
    pub exit: Option<ProbeExit>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
enum Expected {
    Result,
    Error(i64),
}
impl Expected {
    fn value(&self) -> Value {
        match self {
            Self::Result => json!({"result":"valid schema"}),
            Self::Error(code) => json!({"error_code":code}),
        }
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn failure(id: &str) -> LarmError {
    error(ErrorCode::ProtocolError, &format!("conformance_wire_{id}"))
}
pub(super) fn work_snapshot(root: &Path) -> Result<String, LarmError> {
    fn visit(
        root: &Path,
        path: &Path,
        found: &mut BTreeMap<String, String>,
    ) -> Result<(), LarmError> {
        crate::package::check_managed_path(path)?;
        for entry in std::fs::read_dir(path).map_err(super::io)? {
            let entry = entry.map_err(super::io)?;
            let path = entry.path();
            crate::package::check_managed_path(&path)?;
            let name = super::path_text(
                path.strip_prefix(root)
                    .map_err(|_| invalid("conformance_path_invalid"))?,
            )?
            .replace('\\', "/");
            if entry.file_type().map_err(super::io)?.is_dir() {
                found.insert(name, "directory".into());
                visit(root, &path, found)?;
            } else {
                found.insert(name, file_digest(&path)?);
            }
            if found.len() > crate::package::MAX_FILES {
                return Err(invalid("conformance_snapshot_too_large"));
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found)?;
    hash(&found)
}
struct Client<'a> {
    session: &'a mut NativeSession,
    cases: Vec<WireCase>,
    ids: BTreeSet<String>,
}
impl Client<'_> {
    async fn request(
        &mut self,
        id: &str,
        request: Value,
        expected: Expected,
        access: &Access,
    ) -> Result<Value, LarmError> {
        let method = request["method"].as_str().map(str::to_owned);
        let identity = request["id"]
            .as_str()
            .filter(|id| larm_domain::bridge::is_uuid_v7(id))
            .map(str::to_owned);
        let bytes = serde_json::to_vec(&request).map_err(|_| failure(id))?;
        self.exchange(id, &bytes, identity, method, expected, access)
            .await
    }
    async fn exchange(
        &mut self,
        id: &str,
        bytes: &[u8],
        identity: Option<String>,
        method: Option<String>,
        expected: Expected,
        access: &Access,
    ) -> Result<Value, LarmError> {
        self.session.probe_send(bytes, access).await?;
        let mut progress = 0usize;
        loop {
            let response = self.session.probe_next(Duration::from_secs(15)).await?;
            let decoded = decode(&response);
            if let Ok(Frame::Notification(notification)) = &decoded {
                let value: Progress =
                    serde_json::from_value(notification.params.clone()).map_err(|_| failure(id))?;
                if notification.method != "core.progress"
                    || identity.as_deref() != Some(value.request_id.as_str())
                {
                    return Err(failure(id));
                }
                progress += 1;
                if progress > 1024 {
                    return Err(failure(id));
                }
                continue;
            }
            let mut observed = json!({"invalid_frame":true});
            let mut value = Value::Null;
            let mut valid = false;
            if let Ok(Frame::Response(reply)) = decoded {
                observed = if let Some(error) = &reply.error {
                    json!({"error_code":error.code})
                } else {
                    json!({"result":"valid schema"})
                };
                valid = reply.id == identity
                    && match &expected {
                        Expected::Error(code) => {
                            reply.error.as_ref().is_some_and(|e| e.code == *code)
                                && reply.result.is_none()
                        }
                        Expected::Result => {
                            reply.result.as_ref().is_some_and(|result| {
                                method.as_deref().is_some_and(|method| {
                                    Contracts::shared()
                                        .and_then(|contracts| contracts.response(method, result))
                                        .is_ok()
                                })
                            }) && reply.error.is_none()
                        }
                    };
                if let Some(identity) = &reply.id {
                    valid &= self.ids.insert(identity.clone());
                }
                value = serde_json::to_value(reply).map_err(|_| failure(id))?;
            }
            self.cases.push(WireCase {
                id: id.into(),
                request_id: identity,
                method,
                request_sha256: digest(bytes),
                request_bytes: bytes.len(),
                response_sha256: digest(&response),
                expected: expected.value(),
                observed,
                progress_frames: progress,
            });
            if !valid {
                return Err(failure(id));
            }
            return Ok(value);
        }
    }
}
fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":Request::new(method,Value::Null).id,"method":method,"params":params})
}

/// Los datos son de una suite compilada del host, no un caso/PASS importado.
/// Conserva expected/observed y hashes incluso si falla un oráculo. El llamador
/// debe persistir el informe como evidencia y propagar observations, nunca llenar
/// los seis checks faltantes con el hecho de que este informe haya pasado.
pub async fn run_common(run: &Run, session: &mut NativeSession, probe: &WireProbe) -> WireReport {
    let mut report = WireReport {
        cases: vec![],
        before_work_sha256: None,
        rejected_work_sha256: None,
        exit: None,
        observations: Ok(vec![]),
    };
    let mut client = Client {
        session,
        cases: vec![],
        ids: BTreeSet::new(),
    };
    let result = execute_common(run, &mut client, probe, &mut report).await;
    // Cierra también tras fallo y nunca convierte cleanup incompleto en PASS.
    if report.exit.is_none() {
        match client.session.probe_close().await {
            Ok(exit) => report.exit = Some(exit),
            Err(close) if result.is_ok() => {
                report.observations = Err(close);
                report.cases = client.cases;
                return report;
            }
            Err(_) => {}
        }
    }
    report.cases = client.cases;
    report.observations=result.and_then(|()|{
        let mut observations=Vec::new();
        for check in ["handshake","schema","major","request_id","duplicate_reply","errors","frame_limit","malformed_json","unannounced_method"] {
            observations.push(Observation {check_id:check.into(),passed:true,
                evidence_sha256:hash(&json!({"run_sha256":run.digest()?,"check":check,"cases":report.cases,
                    "before_work":report.before_work_sha256,"after_rejection":report.rejected_work_sha256,"exit":report.exit}))?});
        }
        Ok(observations)
    });
    report
}
async fn execute_common(
    run: &Run,
    client: &mut Client<'_>,
    probe: &WireProbe,
    report: &mut WireReport,
) -> Result<(), LarmError> {
    if !client.session.protocol_pristine()
        || !probe.method.starts_with(&format!("{}.", run.family))
        || !client.session.manifest().methods.contains(&probe.method)
    {
        return Err(invalid("conformance_wire_probe_invalid"));
    }
    Contracts::shared()?.request(&probe.method, &probe.params)?;
    let before = work_snapshot(client.session.work_dir())?;
    report.before_work_sha256 = Some(before.clone());
    client.session.probe_start().await?;
    let early = client
        .request(
            "handshake-required",
            request(&probe.method, probe.params.clone()),
            Expected::Error(-32000),
            &probe.access,
        )
        .await?;
    if early["error"]["data"]["details"]["reason"] != "handshake_required" {
        return Err(failure("handshake-required"));
    }
    let adapter_id = client.session.manifest().id.clone();
    client
        .request(
            "incompatible-major",
            request(
                "core.handshake",
                json!({"protocol_version":"2.0","adapter_id":adapter_id,"nonce":"incompatible"}),
            ),
            Expected::Error(-32000),
            &Access::default(),
        )
        .await?;
    let after = work_snapshot(client.session.work_dir())?;
    report.rejected_work_sha256 = Some(after.clone());
    if before != after {
        return Err(failure("state-before-compatible-handshake"));
    }
    let nonce = uuid::Uuid::now_v7().to_string();
    let hello = client
        .request(
            "compatible-handshake",
            request(
                "core.handshake",
                json!({"protocol_version":"1.0","adapter_id":adapter_id,"nonce":nonce}),
            ),
            Expected::Result,
            &Access::default(),
        )
        .await?;
    let hello: HandshakeResult = serde_json::from_value(hello["result"].clone())
        .map_err(|_| failure("compatible-handshake"))?;
    check_handshake(&hello, &nonce, client.session.manifest())?;
    let mut extra = request(&probe.method, probe.params.clone());
    extra["unexpected"] = json!(true);
    client
        .request(
            "extra-frame-field",
            extra,
            Expected::Error(-32600),
            &probe.access,
        )
        .await?;
    client
        .request(
            "null-params",
            request(&probe.method, Value::Null),
            Expected::Error(-32602),
            &probe.access,
        )
        .await?;
    let mut params = probe.params.clone();
    params
        .as_object_mut()
        .ok_or_else(|| invalid("conformance_wire_probe_invalid"))?
        .insert("__p026_unexpected".into(), json!(true));
    client
        .request(
            "extra-param-field",
            request(&probe.method, params),
            Expected::Error(-32602),
            &probe.access,
        )
        .await?;
    for (id, raw) in [
        ("malformed-json", b"{".as_slice()),
        ("invalid-utf8", b"\xff".as_slice()),
        ("nonfinite-json", b"{\"value\":NaN}".as_slice()),
    ] {
        client
            .exchange(
                id,
                raw,
                None,
                None,
                Expected::Error(-32700),
                &Access::default(),
            )
            .await?;
    }
    let mut invalid_id = request(&probe.method, probe.params.clone());
    invalid_id["id"] = json!("unknown-id");
    client
        .request(
            "invalid-request-id",
            invalid_id,
            Expected::Error(-32600),
            &probe.access,
        )
        .await?;
    client
        .request(
            "unannounced-method",
            request("conformance.unannounced", json!({})),
            Expected::Error(-32601),
            &Access::default(),
        )
        .await?;
    let cancel = client
        .request(
            "unknown-cancel-id",
            request(
                "core.cancel",
                json!({"request_id":uuid::Uuid::now_v7().to_string()}),
            ),
            Expected::Result,
            &Access::default(),
        )
        .await?;
    if cancel["result"] != json!({"cancelled":false}) {
        return Err(failure("unknown-cancel-id"));
    }
    let boundary = request("core.heartbeat", json!({}));
    let mut bytes = serde_json::to_vec(&boundary).map_err(|_| failure("exact-frame-limit"))?;
    bytes.resize(MAX_FRAME_BYTES, b' ');
    client
        .exchange(
            "exact-frame-limit",
            &bytes,
            boundary["id"].as_str().map(str::to_owned),
            Some("core.heartbeat".into()),
            Expected::Result,
            &Access::default(),
        )
        .await?;
    bytes.push(b' ');
    client
        .exchange(
            "frame-limit-plus-one",
            &bytes,
            None,
            None,
            Expected::Error(-32700),
            &Access::default(),
        )
        .await?;
    client
        .exchange(
            "oversized-non-json",
            &vec![b'x'; MAX_FRAME_BYTES + 100],
            None,
            None,
            Expected::Error(-32700),
            &Access::default(),
        )
        .await?;
    client
        .request(
            "recover-valid-family-call",
            request(&probe.method, probe.params.clone()),
            Expected::Result,
            &probe.access,
        )
        .await?;
    let exit = client.session.probe_close().await?;
    let clean = exit.success && exit.extra_frames == 0;
    report.exit = Some(exit);
    if !clean {
        return Err(failure("clean-eof-single-replies"));
    }
    Ok(())
}
