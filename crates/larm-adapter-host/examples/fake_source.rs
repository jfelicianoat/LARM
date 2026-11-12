//! Fuente fixture para P043. No descarga ni simula éxito de instalación.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::protocol::RpcError;
use larm_adapter_sdk::server::{serve, AdapterHandler, CallContext};
use larm_adapter_sdk::source::*;
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

struct FakeSource {
    manifest: AdapterManifest,
    support: SearchSupport,
    fixture: Value,
    calls: AtomicU64,
    slow_active: AtomicBool,
    mode: String,
    registry: Option<larm_adapter_sdk::capabilities::CapabilityRegistry>,
}
#[async_trait]
impl ModelSourceAdapter for FakeSource {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn search_support(&self) -> &SearchSupport {
        &self.support
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.registry.as_ref()
    }
    async fn invoke(
        &self,
        request: SourceRequest,
        ctx: CallContext,
    ) -> Result<SourceReply, LarmError> {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let SourceRequest::Search(q) = &request {
            match q.q.as_str() {
                "crash" => std::process::exit(31),
                "rate" => {
                    let rate: RateLimit =
                        serde_json::from_value(self.fixture["rate_limit"].clone()).map_err(
                            |_| LarmError::new(ErrorCode::Internal, "Fixture inválida", "fixture"),
                        )?;
                    return Err(rate.to_error()?);
                }
                "bad-reply" => {
                    return SourceReply::parse(
                        &SourceRequest::GetModel(UpstreamRef {
                            source_id: q.source_id.clone(),
                            upstream_namespace: "fixture".into(),
                            upstream_model_id: "llama".into(),
                        }),
                        self.fixture["responses"]["source.get_model"].clone(),
                    )
                }
                "slow" => {
                    self.slow_active.store(true, Ordering::SeqCst);
                    ctx.progress("QUERY", None, None, Some("source_fixture_waiting"));
                    while self.mode == "deaf" || !ctx.is_cancelled() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    self.slow_active.store(false, Ordering::SeqCst);
                    return Err(LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "Consulta cancelada",
                        "operation_cancelled",
                    ));
                }
                _ => {}
            }
        }
        if matches!(request, SourceRequest::Download(_)) {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "La fixture no ejecuta descargas",
                "fixture_read_only",
            ));
        }
        if let SourceRequest::DownloadChunk(query) = &request {
            let bytes = if query.offset.get() == 0 { "YWJj" } else { "" };
            let chunk = SourceDownloadChunk {
                offset: query.offset,
                data_base64: bytes.into(),
                eof: true,
                total_bytes: Some(larm_domain::units::DecimalU64::new(3).ok_or_else(|| {
                    LarmError::new(ErrorCode::Internal, "Fixture inválida", "fixture")
                })?),
                validator: Some(SourceDownloadResumeValidator::StrongEtag {
                    value: "\"fixture-v1\"".into(),
                }),
            };
            return Ok(SourceReply::DownloadChunk(chunk));
        }
        let mut value = self.fixture["responses"][request.method()].clone();
        match &request {
            SourceRequest::Search(q) => {
                if q.q != "llama" {
                    value["items"] = json!([]);
                    value["next_cursor"] = Value::Null;
                } else if let Some(cursor) = &q.cursor {
                    if cursor != "page-2" {
                        return Err(LarmError::new(
                            ErrorCode::ValidationFailed,
                            "Cursor desconocido",
                            "bad_cursor",
                        ));
                    }
                    value["items"][0]["id"] = json!("0192aaaa-0000-7000-8000-000000000012");
                    value["next_cursor"] = Value::Null;
                }
                let metadata = SourceMetadata {
                    original: serde_json::from_value(json!({"unknown_vendor": {"tokenizer": "fixture-tokenizer", "added": [1,true,null]}, "architecture": null})).map_err(|_| LarmError::new(ErrorCode::Internal,"Fixture inválida","fixture"))?,
                    normalized: serde_json::from_value(json!({"architecture": null,"format":"GGUF"})).map_err(|_| LarmError::new(ErrorCode::Internal,"Fixture inválida","fixture"))?,
                    limitations: self.support.limitations.clone(), rate_limit: None,
                };
                if let Some(items) = value["items"].as_array_mut() {
                    for item in items {
                        if let Some(map) = item["metadata"].as_object_mut() {
                            metadata.attach(map)?;
                        }
                    }
                }
            }
            SourceRequest::Sync(query) => {
                if query.query.get("stats") == Some(&json!(true)) {
                    value["models"][0]["limits"]["invocations"] = json!(calls);
                    value["models"][0]["limits"]["slow_active"] =
                        json!(self.slow_active.load(Ordering::SeqCst));
                } else if query.etag.as_deref() == Some("fixture-etag") {
                    value["changed"] = json!(false);
                    value["models"] = json!([]);
                }
            }
            SourceRequest::AuthProbe(probe) if probe.auth_mode != AuthMode::None => {
                return Err(LarmError::new(
                    ErrorCode::AuthRequired,
                    "Credencial no resuelta por la fixture",
                    "credential_not_enrolled",
                ))
            }
            SourceRequest::DownloadPlan(q) => {
                let mut plan: larm_domain::artifacts::Plan = serde_json::from_value(value)
                    .map_err(|_| {
                        LarmError::new(ErrorCode::Internal, "Fixture inválida", "fixture")
                    })?;
                let mut spec = download_spec(&plan)?;
                spec.variant_id = q.variant_id.clone();
                spec.storage_location_id = q.storage_location_id.clone();
                spec.hard_budget_bytes = q.hard_budget_bytes;
                spec.priority = q.priority;
                plan.steps.clear();
                plan.plan_id = uuid::Uuid::now_v7().to_string();
                plan.expires_at = larm_domain::bridge::timestamp(
                    chrono::Utc::now() + chrono::Duration::seconds(900),
                );
                plan.resource_ids = vec![q.variant_id.clone(), q.storage_location_id.clone()];
                return Ok(SourceReply::DownloadPlan(spec.bind(
                    q,
                    plan,
                    &self.manifest.id,
                )?));
            }
            _ => {}
        }
        SourceReply::parse(&request, value)
    }
}

struct Raw {
    handler: SourceHandler<FakeSource>,
}
#[async_trait]
impl AdapterHandler for Raw {
    fn manifest(&self) -> &AdapterManifest {
        self.handler.manifest()
    }
    fn capability_registry(&self) -> Option<&larm_adapter_sdk::capabilities::CapabilityRegistry> {
        self.handler.capability_registry()
    }
    async fn call(&self, method: &str, params: Value, ctx: CallContext) -> Result<Value, RpcError> {
        if method == "source.search" {
            Ok(json!({"items":[],"next_cursor":null}))
        } else {
            self.handler.call(method, params, ctx).await
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name: &str| args.windows(2).find(|a| a[0] == name).map(|a| a[1].clone());
    let path = arg("--manifest").ok_or("falta --manifest")?;
    let mode = arg("--mode").unwrap_or_else(|| "normal".into());
    let manifest = AdapterManifest::parse(&std::fs::read_to_string(path)?)?;
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json"))?;
    let support = if mode == "import-only" {
        SearchSupport {
            search_applicable: false,
            supported_filters: Default::default(),
            limitations: vec!["import_only".into()],
        }
    } else {
        serde_json::from_value(fixture["support"].clone())?
    };
    use larm_adapter_sdk::capabilities::{CapabilityRegistry, Estimator};
    let mut bundle: Value = serde_json::from_str(include_str!(
        "../../../adapters/source/reference/capabilities.json"
    ))?;
    bundle["entries"] = Value::Array(
        bundle["entries"]
            .as_array()
            .ok_or("bundle inválido")?
            .iter()
            .filter(|entry| manifest.capabilities.iter().any(|id| entry["id"] == *id))
            .cloned()
            .collect(),
    );
    let mappings = bundle["entries"]
        .as_array()
        .ok_or("bundle inválido")?
        .iter()
        .filter_map(|e| e["mapping"].as_str().map(String::from))
        .collect();
    let estimator: Estimator = Arc::new(|_| {
        Ok(
            json!({"weights_bytes":null,"kv_bytes":null,"activation_bytes":null,"workspace_bytes":"2097152","overhead_bytes":null,"total_bytes":null,"multimodal_bytes":null,"temporary_bytes":null,"safety_margin_bytes":null,"per_pool":[],"confidence":"UNKNOWN","evidence_refs":["fixture:source-rpc-buffers-only"]}),
        )
    });
    let registry = if manifest.capabilities.is_empty() {
        None
    } else {
        Some(CapabilityRegistry::new(
            bundle,
            std::collections::BTreeMap::from([("reference.source.buffers.v1".into(), estimator)]),
            &mappings,
        )?)
    };
    let adapter = Arc::new(FakeSource {
        registry,
        manifest,
        support,
        fixture,
        calls: AtomicU64::new(0),
        slow_active: AtomicBool::new(false),
        mode: mode.clone(),
    });
    let handler = SourceHandler::new(adapter)?;
    let reader = tokio::io::BufReader::new(tokio::io::stdin());
    if mode == "raw-response" {
        serve(reader, tokio::io::stdout(), Arc::new(Raw { handler })).await?;
    } else {
        serve(reader, tokio::io::stdout(), Arc::new(handler)).await?;
    }
    Ok(())
}
