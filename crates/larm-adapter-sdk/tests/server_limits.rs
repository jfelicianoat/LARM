//! Oráculos de saturación/cancel/EOF sobre el servidor de producción.
//! Fixture de transporte; no benchmark ni cualificación funcional.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::collections::BTreeSet;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::{
    decode, encode, handshake_request, Frame, Request, Response, RpcError,
};
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::sync::Semaphore;

#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod fixture;

#[derive(Clone, Copy)]
enum Mode {
    Controlled,
    BigReply,
    Panic,
}
struct Handler {
    manifest: AdapterManifest,
    registry: Option<larm_adapter_sdk::capabilities::CapabilityRegistry>,
    mode: Mode,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    seen: Mutex<BTreeSet<String>>,
    gate: Arc<Semaphore>,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl AdapterHandler for Handler {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.registry.as_ref()
    }
    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        self.seen.lock().unwrap().insert(ctx.request_id.clone());
        if matches!(self.mode, Mode::Panic) {
            panic!("fixture callback panic");
        }
        match method {
            "benchmark.describe" => {
                let mut result = fixture::describe(vec![]);
                if matches!(self.mode, Mode::BigReply) {
                    result["settings_schema"]["fixture_fill"] = json!("x".repeat(64 << 10));
                }
                Ok(result)
            }
            "benchmark.run" => {
                if matches!(self.mode, Mode::Controlled) {
                    loop {
                        if ctx.is_cancelled() {
                            return Err(RpcError::domain(LarmError::new(
                                ErrorCode::ResourceUnavailable,
                                "Cancelada",
                                "fixture_cancelled",
                            )));
                        }
                        if let Ok(permit) = self.gate.clone().try_acquire_owned() {
                            permit.forget();
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                }
                Ok(fixture::output(&params, json!({"fixture":true})))
            }
            _ => Err(RpcError::domain(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "Fixture sólo de transporte",
                "fixture_transport_only",
            ))),
        }
    }
}

struct ErrorAtEof(BufReader<tokio::io::DuplexStream>);
impl AsyncRead for ErrorAtEof {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}
impl AsyncBufRead for ErrorAtEof {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<&[u8]>> {
        match Pin::new(&mut self.get_mut().0).poll_fill_buf(cx) {
            Poll::Ready(Ok([])) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture reader error",
            ))),
            result => result,
        }
    }
    fn consume(self: Pin<&mut Self>, amount: usize) {
        Pin::new(&mut self.get_mut().0).consume(amount);
    }
}
struct ObserveWrite {
    inner: tokio::io::DuplexStream,
    writes: Arc<AtomicUsize>,
}
impl AsyncWrite for ObserveWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        this.writes.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut this.inner).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
struct Session {
    handler: Arc<Handler>,
    input: tokio::io::DuplexStream,
    output: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    writes: Arc<AtomicUsize>,
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
impl Session {
    async fn new(mode: Mode, error_at_eof: bool) -> Self {
        let manifest = AdapterManifest::parse(include_str!(
            "../../../tests/fixtures/adapters/demo-benchmark.manifest.json"
        ))
        .unwrap();
        let handler = Arc::new(Handler {
            registry: fixture::registry(&manifest.capabilities).unwrap(),
            manifest,
            mode,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            seen: Mutex::default(),
            gate: Arc::new(Semaphore::new(0)),
        });
        let (input, reader) = tokio::io::duplex(1 << 18);
        let reader: Box<dyn AsyncBufRead + Unpin + Send> = if error_at_eof {
            Box::new(ErrorAtEof(BufReader::new(reader)))
        } else {
            Box::new(BufReader::new(reader))
        };
        let (writer, output) = tokio::io::duplex(256);
        let writes = Arc::new(AtomicUsize::new(0));
        let writer = ObserveWrite {
            inner: writer,
            writes: writes.clone(),
        };
        let worker = handler.clone();
        let task = tokio::spawn(async move { serve(reader, writer, worker).await });
        let mut session = Self {
            handler,
            input,
            output: BufReader::new(output).lines(),
            task,
            writes,
        };
        let request = handshake_request(&session.handler.manifest.id).0;
        session.send(&request).await;
        assert!(session.next().await.result.is_some());
        session
    }
    async fn send(&mut self, request: &Request) {
        self.input
            .write_all(&encode(&Frame::Request(request.clone())).unwrap())
            .await
            .unwrap();
    }
    async fn next(&mut self) -> Response {
        let line = tokio::time::timeout(Duration::from_secs(5), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Frame::Response(response) = decode(line.as_bytes()).unwrap() else {
            panic!("respuesta esperada");
        };
        response
    }
    async fn heartbeat(&mut self) -> Value {
        let request = Request::new("core.heartbeat", json!({}));
        self.send(&request).await;
        let response = self.next().await;
        assert_eq!(response.id.as_deref(), Some(request.id.as_str()));
        response.result.unwrap()
    }
    async fn finish(self) -> std::io::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cuatro_activos_sesenta_y_cuatro_en_cola_y_controles_sin_perder_ids() {
    let mut session = Session::new(Mode::Controlled, false).await;
    let mut requests = vec![];
    for _ in 0..68 {
        let request = Request::new("benchmark.run", fixture::request(1));
        session.send(&request).await;
        requests.push(request);
    }
    until(|| session.handler.calls.load(Ordering::SeqCst) == 4).await;
    assert_eq!(
        session.heartbeat().await["running_request_ids"]
            .as_array()
            .unwrap()
            .len(),
        68
    );
    assert_eq!(session.handler.peak.load(Ordering::SeqCst), 4);
    let excess = Request::new("benchmark.run", fixture::request(1));
    session.send(&excess).await;
    let error = session.next().await.error.unwrap().data.unwrap();
    assert_eq!(error.details.reason, "adapter_queue_full");
    assert!(error.retryable);
    session.send(&requests[0]).await;
    assert_eq!(
        session
            .next()
            .await
            .error
            .unwrap()
            .data
            .unwrap()
            .details
            .reason,
        "request_id_in_use"
    );
    let queued = requests
        .iter()
        .find(|request| !session.handler.seen.lock().unwrap().contains(&request.id))
        .unwrap()
        .id
        .clone();
    let cancel = Request::new("core.cancel", json!({"request_id":queued}));
    session.send(&cancel).await;
    let mut cancelled = std::collections::BTreeMap::new();
    for _ in 0..2 {
        let response = session.next().await;
        assert!(cancelled
            .insert(response.id.clone().unwrap(), response)
            .is_none());
    }
    assert_eq!(
        cancelled[&cancel.id].result.as_ref().unwrap()["cancelled"],
        true
    );
    assert_eq!(
        cancelled[&queued]
            .error
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .details
            .reason,
        "operation_cancelled"
    );
    assert_eq!(session.handler.calls.load(Ordering::SeqCst), 4);
    let replacement = Request::new("benchmark.run", fixture::request(1));
    session.send(&replacement).await;
    assert_eq!(
        session.heartbeat().await["running_request_ids"]
            .as_array()
            .unwrap()
            .len(),
        68
    );
    session.input.shutdown().await.unwrap();
    let mut identities = BTreeSet::new();
    for _ in 0..68 {
        let response = session.next().await;
        assert!(response.error.is_some());
        assert!(identities.insert(response.id.unwrap()));
    }
    let expected: BTreeSet<_> = requests
        .iter()
        .filter(|r| r.id != queued)
        .map(|r| r.id.clone())
        .chain([replacement.id])
        .collect();
    assert_eq!(identities, expected);
    assert_eq!(session.handler.calls.load(Ordering::SeqCst), 4);
    assert_eq!(session.handler.active.load(Ordering::SeqCst), 0);
    session.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completar_libera_plazas_y_la_concurrencia_observada_no_supera_cuatro() {
    let mut session = Session::new(Mode::Controlled, false).await;
    for _ in 0..12 {
        session
            .send(&Request::new("benchmark.run", fixture::request(1)))
            .await;
    }
    until(|| session.handler.calls.load(Ordering::SeqCst) == 4).await;
    session.handler.gate.add_permits(2);
    for _ in 0..2 {
        assert!(session.next().await.result.is_some());
    }
    until(|| session.handler.calls.load(Ordering::SeqCst) == 6).await;
    assert_eq!(
        session.heartbeat().await["running_request_ids"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    session.handler.gate.add_permits(10);
    for _ in 0..10 {
        assert!(session.next().await.result.is_some());
    }
    assert_eq!(session.handler.peak.load(Ordering::SeqCst), 4);
    assert_eq!(session.heartbeat().await["running_request_ids"], json!([]));
    session
        .send(&Request::new("benchmark.describe", json!({})))
        .await;
    assert!(session.next().await.result.is_some());
    session.input.shutdown().await.unwrap();
    session.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn id_y_plaza_se_conservan_hasta_escribir_un_reply_con_stdout_lento() {
    let mut session = Session::new(Mode::BigReply, false).await;
    let baseline = session.writes.load(Ordering::SeqCst);
    let request = Request::new("benchmark.describe", json!({}));
    session.send(&request).await;
    until(|| session.writes.load(Ordering::SeqCst) > baseline).await;
    session.send(&request).await;
    // Este callback demuestra que el lector procesó antes el duplicado mientras
    // el escritor está bloqueado: no depende de una espera temporal arbitraria.
    let barrier = Request::new("benchmark.run", fixture::request(1));
    session.send(&barrier).await;
    until(|| session.handler.seen.lock().unwrap().contains(&barrier.id)).await;
    let first = session.next().await;
    assert_eq!(first.id.as_deref(), Some(request.id.as_str()));
    assert!(first.result.is_some());
    let second = session.next().await;
    assert_eq!(second.id.as_deref(), Some(request.id.as_str()));
    assert_eq!(
        second.error.unwrap().data.unwrap().details.reason,
        "request_id_in_use"
    );
    let third = session.next().await;
    assert_eq!(third.id.as_deref(), Some(barrier.id.as_str()));
    assert!(third.result.is_some());
    assert_eq!(session.handler.calls.load(Ordering::SeqCst), 2);
    session.input.shutdown().await.unwrap();
    session.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_de_lectura_cancela_activos_y_no_ejecuta_la_cola() {
    let mut session = Session::new(Mode::Controlled, true).await;
    for _ in 0..6 {
        session
            .send(&Request::new("benchmark.run", fixture::request(1)))
            .await;
    }
    until(|| session.handler.calls.load(Ordering::SeqCst) == 4).await;
    assert_eq!(
        session.heartbeat().await["running_request_ids"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    session.input.shutdown().await.unwrap();
    for _ in 0..6 {
        assert!(session.next().await.error.is_some());
    }
    assert_eq!(session.handler.calls.load(Ordering::SeqCst), 4);
    assert_eq!(session.handler.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        session.finish().await.unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[tokio::test]
async fn callback_interrumpido_no_deja_un_id_que_cuelgue_el_cierre() {
    let mut session = Session::new(Mode::Panic, false).await;
    session
        .send(&Request::new("benchmark.describe", json!({})))
        .await;
    until(|| session.handler.calls.load(Ordering::SeqCst) == 1).await;
    session.input.shutdown().await.unwrap();
    let handler = session.handler.clone();
    assert!(session.finish().await.is_err());
    assert_eq!(handler.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminar_un_callback_no_pierde_un_frame_de_entrada_leido_a_medias() {
    let mut session = Session::new(Mode::BigReply, false).await;
    let first = Request::new("benchmark.describe", json!({}));
    session.send(&first).await;
    let heartbeat = Request::new("core.heartbeat", json!({}));
    let bytes = encode(&Frame::Request(heartbeat.clone())).unwrap();
    let split = bytes.len() / 2;
    session.input.write_all(&bytes[..split]).await.unwrap();
    assert!(session.next().await.result.is_some());
    session.input.write_all(&bytes[split..]).await.unwrap();
    let response = session.next().await;
    assert_eq!(response.id.as_deref(), Some(heartbeat.id.as_str()));
    assert!(response.result.is_some());
    session.input.shutdown().await.unwrap();
    session.finish().await.unwrap();
}

#[tokio::test]
async fn no_inicia_otro_callback_tras_observar_un_panic_entre_frames() {
    let mut session = Session::new(Mode::Panic, false).await;
    session
        .send(&Request::new("benchmark.describe", json!({})))
        .await;
    until(|| {
        session.handler.calls.load(Ordering::SeqCst) == 1
            && session.handler.active.load(Ordering::SeqCst) == 0
    })
    .await;
    session
        .send(&Request::new("benchmark.describe", json!({})))
        .await;
    let handler = session.handler.clone();
    assert!(session.finish().await.is_err());
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
}
