//! Worker genérico de adaptadores declarativos (P023; spec §13; contrato 03,
//! contrato 11 H03–H04). Lo lanza el host como cualquier adaptador; lee el
//! documento de su propio paquete.
//!
//! Métodos que hace de verdad:
//!
//! - `runtime.list_versions`: ejecuta el sondeo de versión (solo lectura,
//!   10 s como mucho) y devuelve una `runtime_version` en canal `custom`
//!   (contrato 11 H01: un canal que no se puede interpretar es `custom`,
//!   nunca `stable`), con `artifact_hash` = sha256 del ejecutable.
//! - `runtime.launch_plan`: valida los parámetros antes de IO, simula la
//!   expansión y guarda un plan de un solo uso (900 s).
//! - `runtime.start`: ejecuta un plan por `plan_id` + `plan_hash`.
//! - `runtime.health`: GET al health loopback; el proceso muerto es
//!   `FAILED`, sin respuesta `LOADING`, otra respuesta `DEGRADED`.
//! - `runtime.stop`: `HTTP` (POST y 5 s de gracia) o `SIGNAL`; al final
//!   siempre se mata el árbol.
//!
//! El resto de métodos obligatorios de la familia devuelve
//! `UNSUPPORTED_CAPABILITY` `declarative_method_not_applicable`: un
//! declarativo no instala, ni inspecciona modelos, ni estima memoria.
//!
//! Al cerrarse la sesión (EOF en stdin) el worker termina el árbol de todas
//! sus instancias ([`GenericRuntime::shutdown`]). Si el worker muere de golpe,
//! sus runtimes sobreviven y además conservan heredado su pipe de stdout
//! (Rust estable no limita la herencia de handles sin `unsafe`): matarlos con
//! él es el Job Object kill-on-close de P130. El host detecta igualmente la
//! caída por la salida del proceso (P021).
//!
//! Los procesos arrancan con el entorno vaciado, el mínimo de Windows y las
//! variables efectivas. Una `credential_reference` bloquea el arranque hasta
//! que exista su resolución en el worker (P045): nunca se lanza sin ella.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use larm_domain::bridge::{timestamp, Constraints};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::launch::{EnvironmentBinding, EnvironmentConfig, LaunchParameters};
use larm_domain::runtime::RuntimeChannel as Channel;
use larm_domain::runtime_install::VersionQuery;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::declarative::{parse_version, DeclarativeAdapter, Prepared, StopMode};
use crate::manifest::AdapterManifest;
use crate::protocol::{RpcError, INVALID_PARAMS};
use crate::server::{AdapterHandler, CallContext};

/// Validez de un plan de lanzamiento.
pub const PLAN_TTL_SECONDS: i64 = 900;
/// Tiempo máximo del sondeo de versión.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Tiempo máximo de una petición HTTP de health o parada.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
/// Gracia tras pedir la parada por HTTP.
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// Salida máxima que se lee del sondeo o de una respuesta HTTP.
const OUTPUT_LIMIT: usize = 1 << 20;

fn err(code: ErrorCode, message: impl Into<String>, reason: &str) -> LarmError {
    LarmError::new(code, message, reason)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchRequest {
    installation_id: String,
    variant_id: String,
    launch_profile: LaunchParameters,
    constraints: Constraints,
    #[allow(dead_code)]
    resource_requirements: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutePlan {
    plan_id: String,
    plan_hash: String,
    #[allow(dead_code)]
    approval_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceRef {
    instance_id: String,
    generation: u64,
}

struct StoredPlan {
    hash: String,
    prepared: Prepared,
    expires_at: DateTime<Utc>,
    used: bool,
}

struct Instance {
    generation: u64,
    child: Child,
    prepared: Prepared,
}

/// Handler del worker genérico.
pub struct GenericRuntime {
    doc: DeclarativeAdapter,
    manifest: AdapterManifest,
    capability_registry: Option<crate::capabilities::CapabilityRegistry>,
    plans: Mutex<BTreeMap<String, StoredPlan>>,
    instances: Mutex<BTreeMap<String, Instance>>,
}

/// Entorno mínimo explícito de Windows más el temporal del worker.
fn minimal_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "TEMP",
        "TMP",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.to_string(), v));
        }
    }
    env
}

/// Valores del entorno efectivo. Una referencia de credencial bloquea: el
/// worker aún no resuelve secretos (P045) y nunca arranca sin ellos.
fn resolve_env(env: &EnvironmentConfig) -> Result<Vec<(String, String)>, LarmError> {
    env.variables
        .iter()
        .map(|b| match b {
            EnvironmentBinding::Value { name, value } => Ok((name.clone(), value.clone())),
            EnvironmentBinding::Credential { name, .. } => Err(err(
                ErrorCode::ResourceUnavailable,
                format!("{name}: la credencial no está disponible en este worker"),
                "credential_not_enrolled",
            )
            .retryable(false)),
        })
        .collect()
}

fn command(prepared: &Prepared) -> Result<Command, LarmError> {
    let mut cmd = Command::new(&prepared.executable);
    cmd.args(&prepared.args)
        .env_clear()
        .envs(minimal_env())
        .envs(resolve_env(&prepared.environment)?)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = &prepared.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let system = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let _ = std::process::Command::new(system.join("System32").join("taskkill.exe"))
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

async fn bounded_probe_read<R: AsyncRead + Unpin>(reader: R) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > OUTPUT_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe output limit",
        ));
    }
    Ok(bytes)
}

async fn probe_output(mut child: Child) -> std::io::Result<std::process::Output> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("probe stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("probe stderr missing"))?;
    match tokio::try_join!(
        child.wait(),
        bounded_probe_read(stdout),
        bounded_probe_read(stderr)
    ) {
        Ok((status, stdout, stderr)) => Ok(std::process::Output {
            status,
            stdout,
            stderr,
        }),
        Err(error) => {
            if let Some(pid) = child.id() {
                kill_tree(pid);
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, LarmError> {
    let bytes = std::fs::read(path).map_err(|e| {
        err(
            ErrorCode::ResourceUnavailable,
            "No se pudo leer el ejecutable",
            "io_failed",
        )
        .with_technical(e.to_string())
    })?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Lee la hora de creación del sistema operativo y liga el PID al ejecutable
/// que se lanzó. La hora tomada antes de `spawn` no identifica el proceso.
async fn process_started_at(pid: u32, expected_exe: &Path) -> Result<String, LarmError> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let ids = [Pid::from_u32(pid)];
    let expected_exe = std::fs::canonicalize(expected_exe).map_err(|error| {
        err(
            ErrorCode::IntegrityFailed,
            "No se pudo verificar el ejecutable del proceso",
            "runtime_process_executable_unavailable",
        )
        .with_technical(error.to_string())
    })?;
    let mut system = System::new();
    for _ in 0..100 {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&ids),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        if let Some(process) = system.process(ids[0]) {
            if let Some(actual_exe) = process.exe() {
                let actual_exe = std::fs::canonicalize(actual_exe).map_err(|error| {
                    err(
                        ErrorCode::IntegrityFailed,
                        "No se pudo verificar el ejecutable observado",
                        "runtime_process_executable_unavailable",
                    )
                    .with_technical(error.to_string())
                })?;
                if actual_exe != expected_exe {
                    return Err(err(
                        ErrorCode::IntegrityFailed,
                        "El PID observado no pertenece al ejecutable lanzado",
                        "runtime_process_executable_mismatch",
                    ));
                }
                let start_seconds = i64::try_from(process.start_time()).map_err(|error| {
                    err(
                        ErrorCode::IntegrityFailed,
                        "La hora de creación del proceso no es válida",
                        "runtime_process_start_time_invalid",
                    )
                    .with_technical(error.to_string())
                })?;
                let started = chrono::DateTime::<Utc>::from_timestamp(start_seconds, 0)
                    .ok_or_else(|| {
                        err(
                            ErrorCode::IntegrityFailed,
                            "La hora de creación del proceso no es válida",
                            "runtime_process_start_time_invalid",
                        )
                    })?;
                return Ok(timestamp(started));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(err(
        ErrorCode::Timeout,
        "No se pudo observar el proceso recién iniciado",
        "runtime_process_identity_observation_timeout",
    ))
}

/// Puerto de una URL loopback ya expandida.
fn url_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

/// Petición HTTP/1.1 mínima a loopback (las URLs ya se validaron).
///
/// # Errors
/// Texto técnico (conexión, timeout o respuesta mal formada).
pub async fn http(method: &str, url: &str, timeout: Duration) -> Result<(u16, Vec<u8>), String> {
    let rest = url.strip_prefix("http://").ok_or("no es http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let address = authority.replace("localhost", "127.0.0.1");
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(&address)
            .await
            .map_err(|e| format!("conexión: {e}"))?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: 0\r\nAccept: */*\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("envío: {e}"))?;
        let mut raw = Vec::new();
        stream
            .take(OUTPUT_LIMIT as u64)
            .read_to_end(&mut raw)
            .await
            .map_err(|e| format!("lectura: {e}"))?;
        Ok::<Vec<u8>, String>(raw)
    };
    let raw = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| "timeout".to_string())??;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("respuesta sin cabeceras")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("línea de estado inválida")?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        body = dechunk(&body).ok_or("chunked mal formado")?;
    }
    Ok((status, body))
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(data.get(..size)?);
        data = data.get(size + 2..)?;
    }
}

fn params<T: serde::de::DeserializeOwned>(value: Value, schema: &str) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| {
        let mut r = RpcError::new(INVALID_PARAMS, &format!("{schema} inválido"));
        r.data = Some(Box::new(
            err(
                ErrorCode::ValidationFailed,
                format!("Los parámetros no cumplen {schema}"),
                "schema_violation",
            )
            .with_technical(e.to_string()),
        ));
        r
    })
}

fn domain(e: LarmError) -> RpcError {
    RpcError::domain(e)
}

impl GenericRuntime {
    /// Carga el paquete materializado.
    ///
    /// # Errors
    /// Paquete incompleto, inválido o con documento y manifiesto distintos.
    pub fn load(package_dir: &Path) -> Result<Self, LarmError> {
        let (doc, manifest) = DeclarativeAdapter::load_package(package_dir)?;
        Ok(Self::new(doc, manifest))
    }

    #[must_use]
    pub fn new(doc: DeclarativeAdapter, manifest: AdapterManifest) -> Self {
        Self {
            doc,
            manifest,
            capability_registry: None,
            plans: Mutex::default(),
            instances: Mutex::default(),
        }
    }

    /// Un autor registra explícitamente mappings/estimadores implementados.
    /// No se infieren por nombre de runtime ni por un profile candidato.
    #[must_use]
    pub fn with_capability_registry(
        mut self,
        registry: crate::capabilities::CapabilityRegistry,
    ) -> Self {
        self.capability_registry = Some(registry);
        self
    }

    /// Termina el árbol de todas las instancias (fin de sesión).
    pub async fn shutdown(&self) {
        let mut instances = self.instances.lock().await;
        for (_, mut instance) in std::mem::take(&mut *instances) {
            if let Some(pid) = instance.child.id() {
                kill_tree(pid);
            }
            let _ = instance.child.kill().await;
        }
    }

    async fn list_versions(&self, q: VersionQuery, ctx: &CallContext) -> Result<Value, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(&q.runtime_id) {
            return Err(err(
                ErrorCode::ValidationFailed,
                "runtime_id no es UUIDv7",
                "runtime_id_invalid",
            ));
        }
        if q.cursor.is_some() {
            return Err(err(
                ErrorCode::ValidationFailed,
                "Este adaptador nunca emite cursores",
                "cursor_invalid",
            ));
        }
        let prepared = self.doc.prepare_probe()?;
        let mut cmd = command(&prepared)?;
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| {
            err(
                ErrorCode::ResourceUnavailable,
                "No se pudo ejecutar el sondeo de versión",
                "spawn_failed",
            )
            .with_technical(e.to_string())
        })?;
        let pid = child.id();
        ctx.progress("VERSION_PROBE", None, None, Some("version_probe_started"));
        let output = tokio::select! {
            output = tokio::time::timeout(PROBE_TIMEOUT, probe_output(child)) => output,
            () = async {
                while !ctx.is_cancelled() { tokio::time::sleep(Duration::from_millis(10)).await; }
            } => {
                if let Some(pid) = pid { kill_tree(pid); }
                return Err(err(ErrorCode::ResourceUnavailable, "Sondeo cancelado", "request_cancelled"));
            }
        }
            .map_err(|_| {
                if let Some(pid) = pid { kill_tree(pid); }
                err(
                    ErrorCode::Timeout,
                    "El sondeo de versión no terminó a tiempo",
                    "version_probe_timeout",
                )
            })?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    return err(ErrorCode::ProtocolError, "El sondeo excedió el límite de salida", "version_probe_output_limit");
                }
                err(
                    ErrorCode::ResourceUnavailable,
                    "El sondeo de versión falló",
                    "spawn_failed",
                )
                .with_technical(e.to_string())
            })?;
        let text = |b: &[u8]| String::from_utf8_lossy(&b[..b.len().min(OUTPUT_LIMIT)]).to_string();
        let version = parse_version(
            self.doc.version_probe.parse_mode,
            self.doc.version_probe.json_field.as_deref(),
            &text(&output.stdout),
            &text(&output.stderr),
        )
        .map_err(|e| e.with_technical(format!("exit={:?}", output.status.code())))?;
        let at = timestamp(now());
        let items = if q.channel == Channel::Custom {
            vec![json!({
                "id": uuid::Uuid::now_v7().to_string(),
                "created_at": at,
                "updated_at": at,
                "row_version": 1,
                "metadata": {
                    "source": "declarative.version_probe",
                    "adapter_id": self.manifest.id,
                    "channel_reason": "UNKNOWN",
                },
                "runtime_id": q.runtime_id,
                "version_string": version,
                "commit": null,
                "build": null,
                "channel": "custom",
                "released_at": null,
                "artifact_hash": sha256_file(Path::new(&prepared.executable))?,
                "promotion": "NONE",
            })]
        } else {
            Vec::new()
        };
        Ok(json!({"items": items, "next_cursor": null, "snapshot_at": at}))
    }

    async fn launch_plan(&self, r: LaunchRequest) -> Result<Value, LarmError> {
        for (field, id) in [
            ("installation_id", &r.installation_id),
            ("variant_id", &r.variant_id),
        ] {
            if !larm_domain::bridge::is_uuid_v7(id) {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    format!("{field} no es UUIDv7"),
                    "id_invalid",
                ));
            }
        }
        let prepared = self
            .doc
            .prepare_start(&r.launch_profile, r.constraints.max_memory_bytes.as_deref())?;
        let port = prepared
            .port
            .or_else(|| prepared.health_url.as_deref().and_then(url_port))
            .unwrap_or(0);
        let plan_id = uuid::Uuid::now_v7().to_string();
        let expires = now() + chrono::Duration::seconds(PLAN_TTL_SECONDS);
        let mut plan = json!({
            "plan_id": plan_id,
            "executable": prepared.executable,
            "args": prepared.args,
            "cwd": prepared.cwd,
            "env_allowlist": prepared.env_allowlist,
            "port": port,
            "memory_estimate": {
                "weights_bytes": null, "kv_bytes": null, "activation_bytes": null,
                "workspace_bytes": null, "overhead_bytes": null, "total_bytes": null,
                "per_pool": [], "confidence": "UNKNOWN", "evidence_refs": [],
                "multimodal_bytes": null, "temporary_bytes": null, "safety_margin_bytes": null
            },
            "artifact_ids": [],
            "expires_at": timestamp(expires),
            "effective_parameters": r.launch_profile,
            "environment": prepared.environment,
            "effective_memory_limit_bytes": null,
        });
        let hash = larm_domain::canonical::sha256_hex(&plan).map_err(|e| {
            err(
                ErrorCode::Internal,
                "Plan no canonicalizable",
                "serialization",
            )
            .with_technical(e)
        })?;
        plan["plan_hash"] = json!(hash);
        let mut plans = self.plans.lock().await;
        plans.retain(|_, p| p.expires_at > now() && !p.used);
        plans.insert(
            plan_id,
            StoredPlan {
                hash,
                prepared,
                expires_at: expires,
                used: false,
            },
        );
        Ok(plan)
    }

    async fn start(&self, e: ExecutePlan) -> Result<Value, LarmError> {
        let prepared = {
            let mut plans = self.plans.lock().await;
            let plan = plans.get_mut(&e.plan_id).ok_or_else(|| {
                err(ErrorCode::NotFound, "Plan desconocido", "plan_not_found")
                    .with_resource(e.plan_id.clone())
            })?;
            if plan.hash != e.plan_hash {
                return Err(err(
                    ErrorCode::Conflict,
                    "plan_hash no coincide con el plan",
                    "plan_hash_mismatch",
                ));
            }
            if plan.used {
                return Err(err(
                    ErrorCode::Conflict,
                    "El plan ya se usó",
                    "plan_already_used",
                ));
            }
            if plan.expires_at <= now() {
                return Err(err(
                    ErrorCode::InvalidStateTransition,
                    "El plan ha caducado",
                    "plan_expired",
                ));
            }
            plan.used = true;
            plan.prepared.clone()
        };
        let mut cmd = command(&prepared)?;
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| {
            err(
                ErrorCode::RuntimeCrash,
                "No se pudo lanzar el runtime",
                "spawn_failed",
            )
            .with_technical(e.to_string())
        })?;
        let Some(pid) = child.id() else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(err(
                ErrorCode::IntegrityFailed,
                "El sistema no devolvió el PID del proceso lanzado",
                "runtime_process_pid_missing",
            ));
        };
        let process_started_at =
            match process_started_at(pid, Path::new(&prepared.executable)).await {
                Ok(value) => value,
                Err(error) => {
                    // The Child handle is authoritative. A numeric PID alone is
                    // not safe to pass to taskkill when identity capture failed.
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(error);
                }
            };
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(_)) => {
                let _ = child.wait().await;
                return Err(err(
                    ErrorCode::RuntimeCrash,
                    "El proceso terminó antes de registrar su identidad",
                    "runtime_process_exited_during_identity_capture",
                ));
            }
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(err(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo confirmar que el proceso siga activo",
                    "runtime_process_identity_confirmation_failed",
                )
                .with_technical(error.to_string()));
            }
        }
        let instance_id = uuid::Uuid::now_v7().to_string();
        self.instances.lock().await.insert(
            instance_id.clone(),
            Instance {
                generation: 0,
                child,
                prepared,
            },
        );
        Ok(json!({
            "instance_id": instance_id,
            "generation": 0,
            "pid": pid,
            "process_started_at": process_started_at,
        }))
    }

    fn unknown_instance(r: &InstanceRef) -> LarmError {
        err(
            ErrorCode::NotFound,
            "Instancia desconocida",
            "instance_not_found",
        )
        .with_resource(r.instance_id.clone())
    }

    async fn health(&self, r: InstanceRef) -> Result<Value, LarmError> {
        let url = {
            let mut instances = self.instances.lock().await;
            let i = instances
                .get_mut(&r.instance_id)
                .ok_or_else(|| Self::unknown_instance(&r))?;
            if i.generation != r.generation {
                return Err(err(
                    ErrorCode::Conflict,
                    "Generación distinta",
                    "generation_mismatch",
                ));
            }
            if matches!(i.child.try_wait(), Ok(Some(_))) {
                return Ok(health_result("FAILED", "process_exited"));
            }
            i.prepared.health_url.clone().unwrap_or_default()
        };
        let h = &self.doc.health;
        let (state, reason) = match http("GET", &url, HTTP_TIMEOUT).await {
            Err(_) => ("LOADING", "health_unreachable"),
            Ok((status, _)) if u32::from(status) != h.expected_status => {
                ("DEGRADED", "health_status_mismatch")
            }
            Ok((_, body)) => match (&h.json_pointer, &h.expected_value) {
                (Some(pointer), Some(expected)) => {
                    let found = serde_json::from_slice::<Value>(&body)
                        .ok()
                        .and_then(|v| v.pointer(pointer).cloned());
                    let matches = match found {
                        Some(Value::String(s)) => &s == expected,
                        Some(other) => &other.to_string() == expected,
                        None => false,
                    };
                    if matches {
                        ("READY", "health_ok")
                    } else {
                        ("DEGRADED", "health_value_mismatch")
                    }
                }
                _ => ("READY", "health_ok"),
            },
        };
        Ok(health_result(state, reason))
    }

    async fn stop(&self, r: InstanceRef) -> Result<Value, LarmError> {
        let mut instance = {
            let mut instances = self.instances.lock().await;
            let i = instances
                .get(&r.instance_id)
                .ok_or_else(|| Self::unknown_instance(&r))?;
            if i.generation != r.generation {
                return Err(err(
                    ErrorCode::Conflict,
                    "Generación distinta",
                    "generation_mismatch",
                ));
            }
            instances
                .remove(&r.instance_id)
                .ok_or_else(|| Self::unknown_instance(&r))?
        };
        if self.doc.stop.mode == StopMode::Http {
            if let Some(endpoint) = &instance.prepared.stop_endpoint {
                let _ = http("POST", endpoint, HTTP_TIMEOUT).await;
                if tokio::time::timeout(STOP_GRACE, instance.child.wait())
                    .await
                    .is_ok()
                {
                    return Ok(json!({"stopped": true}));
                }
            }
        }
        if let Some(pid) = instance.child.id() {
            kill_tree(pid);
        }
        let _ = instance.child.kill().await;
        Ok(json!({"stopped": true}))
    }
}

fn health_result(state: &str, reason: &str) -> Value {
    json!({"state": state, "backend_effective": null, "reason": reason, "evidence_refs": []})
}

#[async_trait]
impl AdapterHandler for GenericRuntime {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn capability_registry(&self) -> Option<&crate::capabilities::CapabilityRegistry> {
        self.capability_registry.as_ref()
    }

    async fn call(
        &self,
        method: &str,
        params_value: Value,
        ctx: CallContext,
    ) -> Result<Value, RpcError> {
        match method {
            "runtime.list_versions" => self
                .list_versions(params(params_value, "VersionQuery")?, &ctx)
                .await
                .map_err(domain),
            "runtime.launch_plan" => self
                .launch_plan(params(params_value, "LaunchRequest")?)
                .await
                .map_err(domain),
            "runtime.start" => self
                .start(params(params_value, "ExecutePlan")?)
                .await
                .map_err(domain),
            "runtime.health" => self
                .health(params(params_value, "InstanceRef")?)
                .await
                .map_err(domain),
            "runtime.stop" => self
                .stop(params(params_value, "InstanceRef")?)
                .await
                .map_err(domain),
            other => Err(domain(
                err(
                    ErrorCode::UnsupportedCapability,
                    format!("{other} no aplica a un adaptador declarativo"),
                    "declarative_method_not_applicable",
                )
                .retryable(false),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn dechunk_y_puerto() {
        assert_eq!(
            dechunk(b"4\r\nWiki\r\n5;x=1\r\npedia\r\n0\r\n\r\n").unwrap(),
            b"Wikipedia"
        );
        assert!(dechunk(b"zz\r\n").is_none());
        assert_eq!(url_port("http://127.0.0.1:8123/health"), Some(8123));
        assert_eq!(url_port("http://[::1]:9/x"), Some(9));
    }

    #[tokio::test]
    async fn process_started_at_usa_la_identidad_observada_por_el_sistema() {
        let executable = std::env::current_exe().unwrap();
        let started_at = process_started_at(std::process::id(), &executable)
            .await
            .unwrap();
        let started = chrono::DateTime::parse_from_rfc3339(&started_at).unwrap();
        let age = chrono::Utc::now() - started.with_timezone(&chrono::Utc);
        assert!(age >= chrono::Duration::zero());
        assert!(age < chrono::Duration::days(1));
        assert!(started_at.ends_with(".000Z"));

        let wrong_executable = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = process_started_at(std::process::id(), &wrong_executable)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "runtime_process_executable_mismatch");
    }

    #[test]
    fn credencial_bloquea_el_entorno() {
        let env: EnvironmentConfig = serde_json::from_value(json!({
            "allowlist": ["A", "B"],
            "variables": [{"name": "A", "value": "x"}, {"name": "B", "credential_reference": "larm/k"}]
        }))
        .unwrap();
        assert_eq!(
            resolve_env(&env).unwrap_err().details.reason,
            "credential_not_enrolled"
        );
    }
}
