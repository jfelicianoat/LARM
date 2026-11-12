//! Lado del adaptador del protocolo v1 (P020): bucle `serve` sobre stdio.
//!
//! - El primer mensaje tiene que ser `core.handshake`; antes, cualquier otra
//!   petición recibe `-32000` `PROTOCOL_ERROR` `handshake_required`.
//! - `core.heartbeat` y `core.cancel` los responde el SDK; el resto va al
//!   [`AdapterHandler`] solo si el método está en el manifiesto.
//! - Método inexistente o no anunciado: `-32601` con `UNSUPPORTED_CAPABILITY`
//!   en `data`, nunca un éxito vacío.
//! - Cada petición corre en su propia tarea; una sola respuesta por id.
//! - stdout solo lleva frames; un frame mal formado recibe `-32700` y la
//!   sesión sigue.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use crate::manifest::AdapterManifest;
use crate::protocol::{
    accept_handshake, decode, encode, CancelRequest, CancelResult, Frame, HandshakeResult,
    Heartbeat, Notification, Progress, Response, RpcError, INVALID_PARAMS, METHOD_NOT_FOUND,
    PROTOCOL_VERSION,
};

/// Contexto de una llamada.
#[derive(Clone)]
pub struct CallContext {
    pub request_id: String,
    cancelled: Arc<Cancellation>,
    out: mpsc::UnboundedSender<Output>,
}

/// Mismos límites de callbacks y cola que el SDK Python; controles separados.
pub const MAX_CALLBACKS: usize = 4;
pub const MAX_QUEUED: usize = 64;

#[derive(Default)]
struct Cancellation {
    flag: AtomicBool,
    signal: Notify,
}
impl Cancellation {
    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.signal.notify_waiters();
    }
    async fn wait(&self) {
        loop {
            let notified = self.signal.notified();
            tokio::pin!(notified);
            // Registrar antes de leer el flag evita perder una cancelación
            // entre la comprobación y el primer poll del aviso.
            notified.as_mut().enable();
            if self.flag.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}
type Pending = Arc<Mutex<BTreeMap<String, Arc<Cancellation>>>>;

struct Admission {
    id: String,
    pending: Pending,
    _slot: OwnedSemaphorePermit,
}
impl Drop for Admission {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.id);
        }
    }
}
struct Output {
    frame: Frame,
    // Mantiene ID/plaza hasta escribir la respuesta, incluso con stdout lento.
    _admission: Option<Admission>,
}
impl From<Frame> for Output {
    fn from(frame: Frame) -> Self {
        Self {
            frame,
            _admission: None,
        }
    }
}

impl CallContext {
    /// ¿Ha pedido el host cancelar esta llamada?
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.flag.load(Ordering::SeqCst)
    }

    /// Espera la cancelación de esta llamada sin sondear ni consumir otro ID.
    pub async fn cancelled(&self) {
        self.cancelled.wait().await;
    }

    /// Emite `core.progress` (sin texto de usuario ni secretos).
    pub fn progress(
        &self,
        phase: &str,
        completed: Option<u64>,
        total: Option<u64>,
        code: Option<&str>,
    ) {
        let p = Progress {
            request_id: self.request_id.clone(),
            phase: phase.into(),
            completed_bytes: completed,
            total_bytes: total,
            message_code: code.map(String::from),
        };
        let _ = self.out.send(
            Frame::Notification(Notification {
                jsonrpc: "2.0".into(),
                method: "core.progress".into(),
                params: serde_json::to_value(p).unwrap_or_default(),
            })
            .into(),
        );
    }
}

/// Lo que implementa un adaptador.
#[async_trait]
pub trait AdapterHandler: Send + Sync + 'static {
    fn manifest(&self) -> &AdapterManifest;
    /// Cada capacidad de ejecución anunciada necesita contrato y estimador.
    /// `source.search` es un marcador de método source, validado por
    /// `SearchSupport`; no es una capacidad de memoria ejecutable.
    fn capability_registry(&self) -> Option<&crate::capabilities::CapabilityRegistry> {
        None
    }
    /// Versión de protocolo que anuncia en el handshake (normalmente 1.0).
    fn protocol_version(&self) -> &str {
        PROTOCOL_VERSION
    }
    /// Métodos de familia (los `core.*` los resuelve el SDK).
    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError>;
}

enum InputLine {
    Frame(Vec<u8>),
    TooLarge,
}

/// Drena una línea grande sin asignar más de 1 MiB; no pierde la siguiente.
async fn input_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<InputLine>> {
    let mut frame = Vec::new();
    let mut seen = false;
    let mut too_large = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(seen.then_some(if too_large {
                InputLine::TooLarge
            } else {
                InputLine::Frame(frame)
            }));
        }
        seen = true;
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let size = newline.unwrap_or(chunk.len());
        if !too_large {
            if frame.len() + size > crate::protocol::MAX_FRAME_BYTES {
                frame.clear();
                too_large = true;
            } else {
                frame.extend_from_slice(&chunk[..size]);
            }
        }
        reader.consume(size + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(if too_large {
                InputLine::TooLarge
            } else {
                InputLine::Frame(frame)
            }));
        }
    }
}

fn unsupported(method: &str) -> RpcError {
    RpcError {
        code: METHOD_NOT_FOUND,
        message: format!("método no soportado: {method}"),
        data: Some(Box::new(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El adaptador no anuncia ese método",
            "method_not_announced",
        ))),
    }
}

fn completed_failure(tasks: &mut JoinSet<()>) -> Option<std::io::Error> {
    while let Some(result) = tasks.try_join_next() {
        if result.is_err() {
            return Some(std::io::Error::other("callback del adaptador interrumpido"));
        }
    }
    None
}

/// Atiende al host hasta que cierra la entrada.
///
/// # Errors
/// E/S de la salida.
pub async fn serve<R, W, H>(mut reader: R, mut writer: W, handler: Arc<H>) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
    H: AdapterHandler,
{
    let contracts = crate::contracts::Contracts::shared()
        .map_err(|_| std::io::Error::other("contratos del adaptador no disponibles"))?;
    let (out, mut rx) = mpsc::unbounded_channel::<Output>();
    let writer_task = tokio::spawn(async move {
        while let Some(Output { frame, _admission }) = rx.recv().await {
            let bytes = match encode(&frame) {
                Ok(b) => b,
                Err(e) => match &frame {
                    Frame::Response(r) => encode(&Frame::Response(Response::err(
                        r.id.as_deref(),
                        RpcError::domain(e),
                    )))
                    .unwrap_or_default(),
                    _ => continue,
                },
            };
            writer.write_all(&bytes).await?;
            writer.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    });
    let running: Pending = Arc::default();
    let capacity = Arc::new(Semaphore::new(MAX_CALLBACKS + MAX_QUEUED));
    let callbacks = Arc::new(Semaphore::new(MAX_CALLBACKS));
    let mut tasks = JoinSet::new();
    let mut failure = None;
    let mut handshaken = false;
    loop {
        // Drenar handles terminados sin cancelar una lectura parcial de NDJSON.
        if let Some(error) = completed_failure(&mut tasks) {
            failure = Some(error);
        }
        if failure.is_some() || writer_task.is_finished() {
            break;
        }
        let line = match input_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        // Una tarea puede fallar mientras llega esta línea; no iniciar otra
        // mutación después del fallo, ni cancelar/descartar lecturas parciales.
        if let Some(error) = completed_failure(&mut tasks) {
            failure = Some(error);
        }
        if failure.is_some() || writer_task.is_finished() {
            break;
        }
        let line = match line {
            InputLine::Frame(line) => line,
            InputLine::TooLarge => {
                let _ = out.send(
                    Frame::Response(Response::err(
                        None,
                        RpcError::new(crate::protocol::PARSE_ERROR, "frame demasiado grande"),
                    ))
                    .into(),
                );
                continue;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request = match decode(&line) {
            Ok(Frame::Request(r)) => r,
            Ok(_) => continue, // El host no envía respuestas ni notificaciones.
            Err(e) => {
                let _ = out.send(Frame::Response(Response::err(e.id.as_deref(), e.error)).into());
                continue;
            }
        };
        let request = crate::protocol::translate_legacy(&request).unwrap_or(request);
        let reply = |result: Result<Value, RpcError>| match result {
            Ok(v) => Frame::Response(Response::ok(&request.id, v)).into(),
            Err(e) => Frame::Response(Response::err(Some(&request.id), e)).into(),
        };
        if running.lock().is_ok_and(|m| m.contains_key(&request.id)) {
            let _ = out.send(reply(Err(RpcError::domain(LarmError::new(
                ErrorCode::Conflict,
                "ID de llamada ya activo",
                "request_id_in_use",
            )))));
            continue;
        }
        if request.method == "core.handshake" {
            let manifest = handler.manifest();
            let result = accept_handshake(&request.params, &manifest.id).and_then(|h| {
                contracts
                    .request("core.handshake", &request.params)
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "Handshake fuera del contrato"))?;
                let value = serde_json::to_value(HandshakeResult {
                    protocol_version: handler.protocol_version().into(),
                    adapter_version: manifest.version.clone(),
                    methods: manifest.methods.clone(),
                    capabilities: manifest.capabilities.clone(),
                    nonce: h.nonce,
                })
                .unwrap_or_default();
                contracts
                    .response("core.handshake", &value)
                    .map_err(RpcError::domain)?;
                let described_capabilities: Vec<String> = manifest
                    .capabilities
                    .iter()
                    .filter(|capability| {
                        !(manifest.family == crate::manifest::Family::Source
                            && capability.as_str() == "source.search")
                    })
                    .cloned()
                    .collect();
                if !described_capabilities.is_empty() {
                    let registry = handler.capability_registry().ok_or_else(|| {
                        RpcError::domain(LarmError::new(
                            ErrorCode::UnsupportedCapability,
                            "Falta el contrato de las capacidades anunciadas",
                            "capability_descriptor_missing",
                        ))
                    })?;
                    registry
                        .check_advertised(&described_capabilities)
                        .map_err(RpcError::domain)?;
                }
                handshaken = true;
                Ok(value)
            });
            let _ = out.send(reply(result));
            continue;
        }
        if !handshaken {
            let _ = out.send(reply(Err(RpcError::domain(LarmError::new(
                ErrorCode::ProtocolError,
                "El primer mensaje debe ser core.handshake",
                "handshake_required",
            )))));
            continue;
        }
        if handler
            .manifest()
            .methods
            .iter()
            .any(|method| method == &request.method)
            && contracts.request(&request.method, &request.params).is_err()
        {
            let _ = out.send(reply(Err(RpcError::new(
                INVALID_PARAMS,
                "Parámetros fuera del contrato",
            ))));
            continue;
        }
        match request.method.as_str() {
            "core.heartbeat" => {
                if request.params != serde_json::json!({}) {
                    let _ = out.send(reply(Err(RpcError::new(INVALID_PARAMS, "Empty inválido"))));
                    continue;
                }
                let ids: Vec<String> = running
                    .lock()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                let _ = out.send(reply(Ok(serde_json::to_value(Heartbeat {
                    alive: true,
                    running_request_ids: ids,
                })
                .unwrap_or_default())));
            }
            "core.cancel" => {
                let result = serde_json::from_value::<CancelRequest>(request.params.clone())
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "CancelRequest inválido"))
                    .and_then(|c| {
                        if !larm_domain::bridge::is_uuid_v7(&c.request_id) {
                            return Err(RpcError::new(INVALID_PARAMS, "request_id no es UUIDv7"));
                        }
                        let flag = running
                            .lock()
                            .ok()
                            .and_then(|m| m.get(&c.request_id).cloned());
                        let cancelled = flag.is_some_and(|f| {
                            f.cancel();
                            true
                        });
                        Ok(serde_json::to_value(CancelResult { cancelled }).unwrap_or_default())
                    });
                let _ = out.send(reply(result));
            }
            method if !handler.manifest().methods.iter().any(|m| m == method) => {
                let _ = out.send(reply(Err(unsupported(method))));
            }
            _ => {
                let Ok(slot) = capacity.clone().try_acquire_owned() else {
                    let _ = out.send(reply(Err(RpcError::domain(
                        LarmError::new(
                            ErrorCode::ResourceUnavailable,
                            "Cola del adaptador llena",
                            "adapter_queue_full",
                        )
                        .retryable(true),
                    ))));
                    continue;
                };
                let flag = Arc::new(Cancellation::default());
                if let Ok(mut m) = running.lock() {
                    m.insert(request.id.clone(), flag.clone());
                }
                let admission = Admission {
                    id: request.id.clone(),
                    pending: running.clone(),
                    _slot: slot,
                };
                let ctx = CallContext {
                    request_id: request.id.clone(),
                    cancelled: flag,
                    out: out.clone(),
                };
                let (handler, callbacks, out, request) = (
                    handler.clone(),
                    callbacks.clone(),
                    out.clone(),
                    request.clone(),
                );
                tasks.spawn(async move {
                    let permit = tokio::select! {
                        permit = callbacks.acquire_owned() => permit.ok(),
                        () = ctx.cancelled.wait() => None,
                    };
                    let result = if permit.is_none() || ctx.is_cancelled() {
                        Err(RpcError::domain(LarmError::new(
                            ErrorCode::ResourceUnavailable,
                            "Operación cancelada",
                            "operation_cancelled",
                        )))
                    } else {
                        handler
                            .call(&request.method, request.params.clone(), ctx)
                            .await
                            .and_then(|value| {
                                contracts
                                    .response(&request.method, &value)
                                    .map_err(RpcError::domain)?;
                                Ok(value)
                            })
                    };
                    drop(permit);
                    let frame = match result {
                        Ok(v) => Frame::Response(Response::ok(&request.id, v)),
                        Err(e) => Frame::Response(Response::err(Some(&request.id), e)),
                    };
                    let _ = out.send(Output {
                        frame,
                        _admission: Some(admission),
                    });
                });
            }
        }
    }
    // EOF es fin de sesión: los callbacks cooperativos deben soltar recursos.
    if let Ok(pending) = running.lock() {
        for cancelled in pending.values() {
            cancelled.cancel();
        }
    }
    // Ninguna tarea queda separada de la sesión al fallar la lectura o llegar EOF.
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            failure = Some(std::io::Error::other("callback del adaptador interrumpido"));
        }
    }
    drop(out);
    let written = writer_task
        .await
        .map_err(|_| std::io::Error::other("escritor del adaptador interrumpido"))?;
    if let Some(error) = failure {
        Err(error)
    } else {
        written
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod cancellation_tests {
    use super::Cancellation;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelar_despierta_a_todos_los_observadores_de_la_llamada() {
        let cancel = Cancellation::default();
        let observers = async { tokio::join!(cancel.wait(), cancel.wait()) };
        tokio::pin!(observers);
        tokio::select! {
            _ = &mut observers => panic!("cancelación antes de solicitarla"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(1), observers)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn cancelar_antes_de_esperar_no_pierde_el_aviso() {
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(cancel.wait(), cancel.wait());
        })
        .await
        .is_ok());
    }
}
