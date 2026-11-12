//! Validación antes/después del callback en las cinco familias. Son datos de
//! schema sobre el servidor real en canales dúplex, no ejemplos funcionales.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use larm_adapter_sdk::manifest::{AdapterManifest, MethodTable};
use larm_adapter_sdk::protocol::{
    decode, encode, handshake_request, Frame, Request, Response, RpcError, INVALID_PARAMS,
    METHOD_NOT_FOUND,
};
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Handler {
    manifest: AdapterManifest,
    cases: BTreeMap<(String, String), Value>,
    calls: AtomicUsize,
    bad_reply: bool,
}
#[async_trait]
impl AdapterHandler for Handler {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    async fn call(
        &self,
        method: &str,
        _params: Value,
        _ctx: CallContext,
    ) -> Result<Value, RpcError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.bad_reply {
            return Ok(json!({"private_marker":"do-not-echo-p026"}));
        }
        Ok(self.cases[&(method.into(), "response".into())].clone())
    }
}
struct Session {
    handler: Arc<Handler>,
    input: tokio::io::DuplexStream,
    output: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Session {
    async fn new(family: &str, bad_reply: bool) -> Self {
        let table = MethodTable::contract();
        let methods: Vec<_> = table
            .methods
            .iter()
            .filter(|m| m.family == "all" || m.family == family)
            .map(|m| m.method.clone())
            .collect();
        let manifest = AdapterManifest::parse(&json!({"id":format!("fixture.schema.{family}"),"version":"1.0.0","family":family,"protocol_version":"1.0","publisher":"Schema tests only","executable":{"relative_path":"worker.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":methods,"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":[],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]}).to_string()).unwrap();
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/adapters/method-schema-corpus.json"
        ))
        .unwrap();
        let mut cases = BTreeMap::new();
        for case in corpus["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["valid"] == true)
        {
            cases
                .entry((
                    case["method"].as_str().unwrap().into(),
                    case["direction"].as_str().unwrap().into(),
                ))
                .or_insert_with(|| case["value"].clone());
        }
        let handler = Arc::new(Handler {
            manifest,
            cases,
            calls: AtomicUsize::new(0),
            bad_reply,
        });
        let (input, reader) = tokio::io::duplex(1 << 18);
        let (writer, output) = tokio::io::duplex(1 << 18);
        let worker = handler.clone();
        let task = tokio::spawn(async move { serve(BufReader::new(reader), writer, worker).await });
        Self {
            handler,
            input,
            output: BufReader::new(output).lines(),
            task,
        }
    }
    async fn call(&mut self, request: Request) -> Response {
        self.input
            .write_all(&encode(&Frame::Request(request.clone())).unwrap())
            .await
            .unwrap();
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Frame::Response(response) = decode(line.as_bytes()).unwrap() else {
            panic!("respuesta esperada");
        };
        assert_eq!(response.id.as_deref(), Some(request.id.as_str()));
        response
    }
    async fn close(mut self) {
        self.input.shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn treinta_y_siete_metodos_de_familia_validan_antes_del_callback() {
    let mut tested = 0;
    for family in [
        "runtime",
        "source",
        "protocol",
        "transformation",
        "benchmark",
    ] {
        let mut session = Session::new(family, false).await;
        let methods: Vec<_> = session
            .handler
            .manifest
            .methods
            .iter()
            .filter(|m| !m.starts_with("core."))
            .cloned()
            .collect();
        let first = &methods[0];
        let value = session.handler.cases[&(first.clone(), "request".into())].clone();
        let early = session.call(Request::new(first, value)).await;
        assert_eq!(
            early.error.unwrap().data.unwrap().details.reason,
            "handshake_required"
        );
        let handshake = handshake_request(&session.handler.manifest.id).0;
        assert!(session.call(handshake).await.result.is_some());
        for method in methods {
            let before = session.handler.calls.load(Ordering::SeqCst);
            let request = session.handler.cases[&(method.clone(), "request".into())].clone();
            let invalid = session.call(Request::new(&method, Value::Null)).await;
            assert_eq!(invalid.error.unwrap().code, INVALID_PARAMS);
            let mut extra = request.clone();
            extra["unpublished_field"] = json!(true);
            assert_eq!(
                session
                    .call(Request::new(&method, extra))
                    .await
                    .error
                    .unwrap()
                    .code,
                INVALID_PARAMS
            );
            assert_eq!(session.handler.calls.load(Ordering::SeqCst), before);
            let reply = session.call(Request::new(&method, request)).await;
            assert_eq!(
                reply.result.unwrap(),
                session.handler.cases[&(method.clone(), "response".into())]
            );
            assert_eq!(session.handler.calls.load(Ordering::SeqCst), before + 1);
            tested += 1;
        }
        assert_eq!(
            session
                .call(Request::new("unpublished.method", json!({})))
                .await
                .error
                .unwrap()
                .code,
            METHOD_NOT_FOUND
        );
        assert_eq!(
            session
                .call(Request::new("core.heartbeat", json!({})))
                .await
                .result
                .unwrap()["alive"],
            true
        );
        session.close().await;
    }
    assert_eq!(tested, 37);
}

#[tokio::test]
async fn reply_invalido_se_normaliza_en_cada_familia_sin_filtrar_el_valor() {
    for family in [
        "runtime",
        "source",
        "protocol",
        "transformation",
        "benchmark",
    ] {
        let mut session = Session::new(family, true).await;
        let handshake = handshake_request(&session.handler.manifest.id).0;
        session.call(handshake).await.result.unwrap();
        let method = session
            .handler
            .manifest
            .methods
            .iter()
            .find(|m| !m.starts_with("core."))
            .unwrap()
            .clone();
        let request = session.handler.cases[&(method.clone(), "request".into())].clone();
        let response = session.call(Request::new(&method, request)).await;
        assert!(!serde_json::to_string(&response)
            .unwrap()
            .contains("do-not-echo-p026"));
        assert_eq!(
            response.error.unwrap().data.unwrap().details.reason,
            "invalid_adapter_reply"
        );
        assert_eq!(session.handler.calls.load(Ordering::SeqCst), 1);
        assert!(session
            .call(Request::new("core.heartbeat", json!({})))
            .await
            .result
            .is_some());
        session.close().await;
    }
}
