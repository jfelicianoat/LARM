//! Sesión del protocolo v1 (P020) con el bucle `serve` del SDK sobre un
//! canal dúplex: handshake, versión, JSON mal formado, métodos no
//! anunciados, parámetros inválidos, heartbeat, cancelación y progreso.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::{
    check_handshake, decode, encode, handshake_request, Frame, HandshakeResult, Request, Response,
    RpcError, DOMAIN_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR,
};
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod benchmark_fixture;

const MANIFEST: &str =
    include_str!("../../../tests/fixtures/adapters/demo-benchmark.manifest.json");

struct Bench {
    manifest: AdapterManifest,
    version: String,
    registry: Option<larm_adapter_sdk::capabilities::CapabilityRegistry>,
}

#[async_trait]
impl AdapterHandler for Bench {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn protocol_version(&self) -> &str {
        &self.version
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.registry.as_ref()
    }
    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        match method {
            "benchmark.run" => {
                let n = benchmark_fixture::amount(&params);
                for i in 0..n {
                    if ctx.is_cancelled() {
                        return Err(RpcError::domain(larm_domain::error::LarmError::new(
                            larm_domain::error::ErrorCode::Conflict,
                            "cancelada",
                            "request_cancelled",
                        )));
                    }
                    ctx.progress("RUNNING", Some(i), Some(n), Some("iteration"));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Ok(benchmark_fixture::output(&params, json!({"iterations": n})))
            }
            "benchmark.describe" => Ok(benchmark_fixture::describe(vec![])),
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "no implementado")),
        }
    }
}

struct Host {
    to: tokio::io::DuplexStream,
    from: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    manifest: AdapterManifest,
}

async fn session(version: &str) -> Host {
    let manifest = AdapterManifest::parse(MANIFEST).unwrap();
    let (host_to, adapter_in) = tokio::io::duplex(1 << 16);
    let (adapter_out, host_from) = tokio::io::duplex(1 << 16);
    let handler = Arc::new(Bench {
        registry: benchmark_fixture::registry(&manifest.capabilities).unwrap(),
        manifest: manifest.clone(),
        version: version.into(),
    });
    tokio::spawn(async move { serve(BufReader::new(adapter_in), adapter_out, handler).await });
    Host {
        to: host_to,
        from: BufReader::new(host_from).lines(),
        manifest,
    }
}

impl Host {
    async fn send(&mut self, req: &Request) {
        self.to
            .write_all(&encode(&Frame::Request(req.clone())).unwrap())
            .await
            .unwrap();
    }
    async fn raw(&mut self, line: &[u8]) {
        self.to.write_all(line).await.unwrap();
        self.to.write_all(b"\n").await.unwrap();
    }
    async fn next(&mut self) -> Frame {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.from.next_line())
            .await
            .expect("respuesta a tiempo")
            .unwrap()
            .expect("sesión abierta");
        decode(line.as_bytes()).unwrap()
    }
    /// Siguiente respuesta (salta notificaciones de progreso).
    async fn response(&mut self) -> Response {
        loop {
            if let Frame::Response(r) = self.next().await {
                return r;
            }
        }
    }
    async fn handshake(&mut self) -> (HandshakeResult, String) {
        let (req, nonce) = handshake_request(&self.manifest.id);
        self.send(&req).await;
        let r = self.response().await;
        assert_eq!(r.id.as_deref(), Some(req.id.as_str()));
        (serde_json::from_value(r.result.unwrap()).unwrap(), nonce)
    }
}

#[tokio::test]
async fn frame_grande_se_drena_y_la_siguiente_peticion_funciona() {
    let mut h = session("1.0").await;
    h.handshake().await;
    h.raw(&vec![b'x'; (1 << 20) + 19]).await;
    assert_eq!(h.response().await.error.unwrap().code, PARSE_ERROR);
    h.send(&Request::new("core.heartbeat", json!({}))).await;
    assert_eq!(h.response().await.result.unwrap()["alive"], true);
}

#[tokio::test]
async fn eof_cancela_el_callback_antes_de_cerrar() {
    let mut h = session("1.0").await;
    h.handshake().await;
    let request = Request::new("benchmark.run", benchmark_fixture::request(10000));
    h.send(&request).await;
    while !matches!(h.next().await, Frame::Notification(_)) {}
    h.to.shutdown().await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), h.response())
        .await
        .unwrap();
    assert_eq!(response.id.as_deref(), Some(request.id.as_str()));
    assert_eq!(
        response.error.unwrap().data.unwrap().details.reason,
        "request_cancelled"
    );
}

#[tokio::test]
async fn duplicado_activo_no_reemplaza_ni_invoca_otro_callback() {
    let mut h = session("1.0").await;
    h.handshake().await;
    let request = Request::new("benchmark.run", benchmark_fixture::request(10000));
    h.send(&request).await;
    while !matches!(h.next().await, Frame::Notification(_)) {}
    let duplicate = Request {
        params: benchmark_fixture::request(0),
        ..request.clone()
    };
    h.send(&duplicate).await;
    assert_eq!(
        h.response()
            .await
            .error
            .unwrap()
            .data
            .unwrap()
            .details
            .reason,
        "request_id_in_use"
    );
    let cancel = Request::new("core.cancel", json!({"request_id":request.id}));
    h.send(&cancel).await;
    let mut replies = std::collections::BTreeMap::new();
    while replies.len() < 2 {
        let r = h.response().await;
        assert!(replies.insert(r.id.clone().unwrap(), r).is_none());
    }
    assert_eq!(
        replies[&cancel.id].result.as_ref().unwrap()["cancelled"],
        true
    );
    assert_eq!(
        replies[&request.id]
            .error
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .details
            .reason,
        "request_cancelled"
    );
}

#[tokio::test]
async fn handshake_obligatorio_y_negociacion() {
    let mut h = session("1.0").await;
    // Antes del handshake nada se ejecuta.
    let early = Request::new("benchmark.describe", json!({}));
    h.send(&early).await;
    let r = h.response().await;
    let e = r.error.unwrap();
    assert_eq!(
        (e.code, e.data.unwrap().details.reason.as_str()),
        (DOMAIN_ERROR, "handshake_required")
    );
    let (result, nonce) = h.handshake().await;
    assert_eq!(result.nonce, nonce, "nonce exacto");
    check_handshake(&result, &nonce, &h.manifest).unwrap();
    assert!(
        result
            .capabilities
            .contains(&"vendor.demo.experimental".to_string()),
        "capacidad desconocida conservada"
    );
    // Handshake para otro adaptador.
    let mut wrong = handshake_request("otro.adaptador").0;
    wrong.params["adapter_id"] = "otro.adaptador".into();
    h.send(&wrong).await;
    assert_eq!(
        h.response()
            .await
            .error
            .unwrap()
            .data
            .unwrap()
            .details
            .reason,
        "adapter_id_mismatch"
    );
}

#[tokio::test]
async fn version_incompatible_se_rechaza_antes_de_io() {
    let mut h = session("2.0").await;
    let (req, _) = handshake_request(&h.manifest.id);
    h.send(&req).await;
    let error = h.response().await.error.unwrap();
    assert_eq!(error.data.unwrap().details.reason, "invalid_adapter_reply");
    // Y el adaptador rechaza un host de otra versión mayor.
    let mut h = session("1.0").await;
    let (mut req, _) = handshake_request(&h.manifest.id);
    req.params["protocol_version"] = "2.0".into();
    h.send(&req).await;
    assert_eq!(
        h.response()
            .await
            .error
            .unwrap()
            .data
            .unwrap()
            .details
            .reason,
        "protocol_version_mismatch"
    );
}

#[tokio::test]
async fn json_mal_formado_y_metodos() {
    let mut h = session("1.0").await;
    h.handshake().await;
    h.raw(b"{esto no es json").await;
    let r = h.response().await;
    assert_eq!((r.id, r.error.unwrap().code), (None, PARSE_ERROR));
    // La sesión sigue viva.
    let unknown = Request::new("runtime.teleport", json!({}));
    h.send(&unknown).await;
    let e = h.response().await.error.unwrap();
    assert_eq!(e.code, METHOD_NOT_FOUND);
    assert_eq!(
        e.data.unwrap().code,
        larm_domain::error::ErrorCode::UnsupportedCapability,
        "sin éxito vacío"
    );
    // Método publicado pero de otra familia / no anunciado.
    let other = Request::new("runtime.health", json!({}));
    h.send(&other).await;
    assert_eq!(h.response().await.error.unwrap().code, METHOD_NOT_FOUND);
    let bad = Request::new("benchmark.run", json!({"iteraciones": 3}));
    h.send(&bad).await;
    assert_eq!(h.response().await.error.unwrap().code, INVALID_PARAMS);
    let cancel_bad = Request::new("core.cancel", json!({"id": "x"}));
    h.send(&cancel_bad).await;
    assert_eq!(h.response().await.error.unwrap().code, INVALID_PARAMS);
    h.send(&Request::new(
        "core.cancel",
        json!({"request_id":"invalid"}),
    ))
    .await;
    assert_eq!(h.response().await.error.unwrap().code, INVALID_PARAMS);
    h.send(&Request::new("core.heartbeat", json!({"unexpected":true})))
        .await;
    assert_eq!(h.response().await.error.unwrap().code, INVALID_PARAMS);
}

#[tokio::test]
async fn progreso_heartbeat_y_cancelacion() {
    let mut h = session("1.0").await;
    h.handshake().await;
    let run = Request::new("benchmark.run", benchmark_fixture::request(1000));
    h.send(&run).await;
    // Progreso con el id de la petición.
    let first = loop {
        if let Frame::Notification(n) = h.next().await {
            break n;
        }
    };
    assert_eq!(
        (first.method.as_str(), first.params["request_id"].as_str()),
        ("core.progress", Some(run.id.as_str()))
    );
    let hb = Request::new("core.heartbeat", json!({}));
    h.send(&hb).await;
    let beat = h.response().await;
    assert_eq!(beat.id.as_deref(), Some(hb.id.as_str()));
    assert_eq!(beat.result.unwrap()["running_request_ids"], json!([run.id]));
    let cancel = Request::new("core.cancel", json!({"request_id": run.id}));
    h.send(&cancel).await;
    // Dos respuestas: la de cancel (cancelled=true) y la de run (cancelada), una por id.
    let mut got = std::collections::BTreeMap::new();
    while got.len() < 2 {
        let r = h.response().await;
        assert!(
            got.insert(r.id.clone().unwrap(), r).is_none(),
            "una respuesta por id"
        );
    }
    assert_eq!(got[&cancel.id].result.as_ref().unwrap()["cancelled"], true);
    assert_eq!(
        got[&run.id]
            .error
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .details
            .reason,
        "request_cancelled"
    );
    // Cancelar algo que ya no corre: cancelled=false.
    let again = Request::new("core.cancel", json!({"request_id": run.id}));
    h.send(&again).await;
    assert_eq!(h.response().await.result.unwrap()["cancelled"], false);
    let hb = Request::new("core.heartbeat", json!({}));
    h.send(&hb).await;
    assert_eq!(
        h.response().await.result.unwrap()["running_request_ids"],
        json!([])
    );
}
