//! La negociación prueba el estimador antes de anunciar, nunca antes de versión.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use larm_adapter_sdk::capabilities::{CapabilityRegistry, Estimator};
use larm_adapter_sdk::manifest::{AdapterManifest, Family};
use larm_adapter_sdk::protocol::{decode, encode, Frame, Request, RpcError};
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod fixture;

struct Handler {
    manifest: AdapterManifest,
    registry: Option<CapabilityRegistry>,
    calls: Arc<AtomicUsize>,
    version: &'static str,
}
#[async_trait]
impl AdapterHandler for Handler {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn capability_registry(&self) -> Option<&CapabilityRegistry> {
        self.registry.as_ref()
    }
    fn protocol_version(&self) -> &str {
        self.version
    }
    async fn call(
        &self,
        _method: &str,
        _params: Value,
        _ctx: CallContext,
    ) -> Result<Value, RpcError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(fixture::describe(vec![]))
    }
}
async fn scenario(
    registry: Option<CapabilityRegistry>,
    capabilities: Vec<String>,
    version: &'static str,
    requested: &str,
) -> (Value, Value, usize) {
    let mut manifest = AdapterManifest::parse(include_str!(
        "../../../tests/fixtures/adapters/demo-benchmark.manifest.json"
    ))
    .unwrap();
    if capabilities
        .iter()
        .any(|capability| capability == "source.search")
    {
        manifest.family = Family::Source;
    }
    manifest.capabilities = capabilities;
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(Handler {
        manifest: manifest.clone(),
        registry,
        calls: calls.clone(),
        version,
    });
    let (mut input, reader) = tokio::io::duplex(1 << 16);
    let (writer, output) = tokio::io::duplex(1 << 16);
    let task = tokio::spawn(async move { serve(BufReader::new(reader), writer, handler).await });
    let mut lines = BufReader::new(output).lines();
    let handshake = Request::new(
        "core.handshake",
        json!({"protocol_version":requested,"adapter_id":manifest.id,"nonce":"0123456789abcdef0123456789abcdef"}),
    );
    input
        .write_all(&encode(&Frame::Request(handshake)).unwrap())
        .await
        .unwrap();
    let first = lines.next_line().await.unwrap().unwrap();
    let first: Value = serde_json::from_str(&first).unwrap();
    input
        .write_all(
            &encode(&Frame::Request(Request::new(
                "benchmark.describe",
                json!({}),
            )))
            .unwrap(),
        )
        .await
        .unwrap();
    let second = lines.next_line().await.unwrap().unwrap();
    assert!(matches!(
        decode(second.as_bytes()).unwrap(),
        Frame::Response(_)
    ));
    let second: Value = serde_json::from_str(&second).unwrap();
    input.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    (first, second, calls.load(Ordering::SeqCst))
}
fn registry(estimates: Arc<AtomicUsize>, valid: bool) -> CapabilityRegistry {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/adapters/capability-registry.json"
    ))
    .unwrap();
    let estimate: Estimator = Arc::new(move |_| {
        estimates.fetch_add(1, Ordering::SeqCst);
        Ok(if valid {
            json!({"weights_bytes":null,"kv_bytes":null,"activation_bytes":null,"workspace_bytes":"32768","overhead_bytes":null,"total_bytes":null,"multimodal_bytes":null,"temporary_bytes":null,"safety_margin_bytes":null,"per_pool":[],"confidence":"UNKNOWN","evidence_refs":["fixture:formal-negotiation-only"]})
        } else {
            json!({})
        })
    });
    CapabilityRegistry::new(
        json!({"schema_version":"1.0","entries":[corpus["echo"]]}),
        BTreeMap::from([("reference.echo.buffers.v1".into(), estimate)]),
        &BTreeSet::from(["reference.http.echo.v1".into()]),
    )
    .unwrap()
}
#[tokio::test]
async fn sin_descriptor_no_anuncia_ni_ejecuta_callback() {
    let (hello, call, count) =
        scenario(None, vec!["vendor.reference.echo".into()], "1.0", "1.0").await;
    assert_eq!(
        hello["error"]["data"]["details"]["reason"],
        "capability_descriptor_missing"
    );
    assert_eq!(
        call["error"]["data"]["details"]["reason"],
        "handshake_required"
    );
    assert_eq!(count, 0);
}

#[tokio::test]
async fn source_search_no_necesita_descriptor_de_estimacion_de_memoria() {
    let (hello, call, count) = scenario(None, vec!["source.search".into()], "1.0", "1.0").await;
    assert_eq!(hello["result"]["capabilities"], json!(["source.search"]));
    assert!(call.get("result").is_some());
    assert_eq!(count, 1);
}
#[tokio::test]
async fn estimador_invalido_no_habilita_sesion_y_valido_si() {
    for valid in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let (hello, call, count) = scenario(
            Some(registry(calls.clone(), valid)),
            vec!["vendor.reference.echo".into()],
            "1.0",
            "1.0",
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if valid {
            assert_eq!(
                hello["result"]["capabilities"],
                json!(["vendor.reference.echo"])
            );
            assert!(call.get("result").is_some());
            assert_eq!(count, 1);
        } else {
            assert_eq!(
                hello["error"]["data"]["details"]["reason"],
                "capability_estimator_failed"
            );
            assert!(call.get("error").is_some());
            assert_eq!(count, 0);
        }
    }
}
#[tokio::test]
async fn ambas_mayores_incompatibles_rechazan_antes_del_estimador() {
    for (version, requested) in [("1.0", "2.0"), ("2.0", "1.0")] {
        let estimates = Arc::new(AtomicUsize::new(0));
        let (hello, _, calls) = scenario(
            Some(registry(estimates.clone(), true)),
            vec!["vendor.reference.echo".into()],
            version,
            requested,
        )
        .await;
        assert!(hello.get("error").is_some());
        assert_eq!(estimates.load(Ordering::SeqCst), 0);
        assert_eq!(calls, 0);
    }
}
#[tokio::test]
async fn capacidad_ausente_y_alias_no_se_anuncian_y_transporte_vacio_es_valido() {
    for missing in ["vendor.absent", "embedding"] {
        let estimates = Arc::new(AtomicUsize::new(0));
        let (hello, _, calls) = scenario(
            Some(registry(estimates.clone(), true)),
            vec![missing.into()],
            "1.0",
            "1.0",
        )
        .await;
        assert_eq!(
            hello["error"]["data"]["details"]["reason"],
            "capability_descriptor_missing"
        );
        assert_eq!(estimates.load(Ordering::SeqCst), 0);
        assert_eq!(calls, 0);
    }
    let (hello, call, count) = scenario(None, vec![], "1.0", "1.0").await;
    assert_eq!(hello["result"]["capabilities"], json!([]));
    assert!(call.get("result").is_some());
    assert_eq!(count, 1);
}
