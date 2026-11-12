#![allow(clippy::panic)] // en tests un fallo inesperado debe abortar el caso
//! Tests del puente de adopción externa contra un runtime simulado por HTTP.
//!
//! El runtime simulado imita la API de Ollama que usa la adopción
//! (`/api/tags`, `/api/show`, `/api/version`, `/api/ps`, `/api/generate` y
//! `/v1/chat/completions`) y cuenta las cargas. Prueba CODE_PASS; la prueba
//! real usa el Ollama del PC IA (tools/p107_real.py en el Broker).

use super::*;
use axum::extract::State as AxumState;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Profiles(Mutex<Option<larm_domain::hardware::HardwareProfile>>);

#[async_trait]
impl HardwareRepository for Profiles {
    async fn latest(&self) -> Result<Option<larm_domain::hardware::HardwareProfile>, ApiError> {
        Ok(self.0.lock().clone())
    }
    async fn record(
        &self,
        _: &larm_domain::hardware::HardwareProbe,
    ) -> Result<larm_domain::hardware::HardwareProfile, ApiError> {
        Err(ApiError::new("FORBIDDEN", "Sólo lectura", "read_only"))
    }
    async fn list(
        &self,
        _: Option<(String, String)>,
        _: u32,
    ) -> Result<Vec<larm_domain::hardware::HardwareProfile>, ApiError> {
        Ok(self.0.lock().iter().cloned().collect())
    }
    async fn get(
        &self,
        id: &str,
    ) -> Result<Option<larm_domain::hardware::HardwareProfile>, ApiError> {
        Ok(self.0.lock().as_ref().filter(|p| p.id == id).cloned())
    }
}

struct FixedPinnedRuntimeVersion {
    variant_id: String,
    launch_profile_id: String,
    result: ResolvedRuntimeVersion,
}

#[async_trait]
impl RuntimeVersionResolver for FixedPinnedRuntimeVersion {
    async fn resolve(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
        _configured_runtime_version_id: &str,
        _host_id: &str,
    ) -> Result<ResolvedRuntimeVersion, ApiError> {
        if variant_id != self.variant_id || launch_profile_id != self.launch_profile_id {
            return Err(ApiError::new(
                "INTEGRITY_FAILED",
                "El test recibió una pareja de pin distinta",
                "test_pin_pair_mismatch",
            ));
        }
        Ok(self.result.clone())
    }
}

#[tokio::test]
async fn runtime_pin_controls_catalog_and_new_lease_constraints() {
    let mut h = harness(BIG_BUDGET).await;
    let pinned_id = uuid::Uuid::now_v7().to_string();
    h.bridge = h
        .bridge
        .with_runtime_version_resolver(Arc::new(FixedPinnedRuntimeVersion {
            variant_id: h.variant_id.clone(),
            launch_profile_id: h.profile_id.clone(),
            result: ResolvedRuntimeVersion {
                runtime_version_id: pinned_id.clone(),
                version_string: "0.99.0".into(),
                pinned: true,
            },
        }));

    let catalog = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|error| panic!("catalog: {error}"));
    assert_eq!(catalog.items.len(), 1);
    assert_eq!(
        catalog.items[0].runtime_version_ids.as_slice(),
        std::slice::from_ref(&pinned_id)
    );
    assert_eq!(
        catalog.items[0].constraints.runtime_version_id.as_deref(),
        Some(pinned_id.as_str())
    );

    let mut request = lease_request(&h);
    request.constraints.runtime_version_id = Some(pinned_id.clone());
    let outcome = h
        .bridge
        .create_lease(&broker(), request, "pinned-runtime-version")
        .await
        .unwrap_or_else(|error| panic!("lease with pin: {error}"));
    assert_eq!(outcome.status, 202);
    let job_id = outcome.body["job_id"]
        .as_str()
        .unwrap_or_else(|| panic!("job receipt has no job_id"))
        .to_owned();
    let mut job = h
        .bridge
        .get_job(&broker(), &job_id)
        .await
        .unwrap_or_else(|error| panic!("get lease job: {error}"));
    for _ in 0..200 {
        if job.state != JobPhase::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        job = h
            .bridge
            .get_job(&broker(), &job_id)
            .await
            .unwrap_or_else(|error| panic!("poll lease job: {error}"));
    }
    assert_eq!(job.state, JobPhase::Completed, "{:?}", job.error);
    let response: LeaseResponse = serde_json::from_value(
        job.result
            .unwrap_or_else(|| panic!("completed lease job has no result")),
    )
    .unwrap_or_else(|error| panic!("lease response: {error}"));
    let stored = h
        .bridge
        .inner
        .state
        .lock()
        .leases
        .get(&response.id)
        .map(|entry| entry.record.constraints.runtime_version_id.clone())
        .unwrap_or_else(|| panic!("lease record missing"));
    assert_eq!(stored.as_deref(), Some(pinned_id.as_str()));
}

#[tokio::test]
async fn runtime_pin_must_match_the_observed_runtime_version() {
    let mut h = harness(BIG_BUDGET).await;
    h.bridge = h
        .bridge
        .with_runtime_version_resolver(Arc::new(FixedPinnedRuntimeVersion {
            variant_id: h.variant_id.clone(),
            launch_profile_id: h.profile_id.clone(),
            result: ResolvedRuntimeVersion {
                runtime_version_id: uuid::Uuid::now_v7().to_string(),
                version_string: "0.99.1".into(),
                pinned: true,
            },
        }));
    let error = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|error| panic!("catalog response should be closed: {error}"));
    assert!(error.items.is_empty());
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "pinned-version-mismatch")
        .await
        .err()
        .unwrap_or_else(|| panic!("a mismatched runtime version must fail closed"));
    assert_eq!(error.details.reason, "runtime_pin_version_mismatch");
}

#[tokio::test]
async fn runtime_default_must_match_the_observed_runtime_version() {
    let mut h = harness(BIG_BUDGET).await;
    h.bridge = h
        .bridge
        .with_runtime_version_resolver(Arc::new(FixedPinnedRuntimeVersion {
            variant_id: h.variant_id.clone(),
            launch_profile_id: h.profile_id.clone(),
            result: ResolvedRuntimeVersion {
                runtime_version_id: uuid::Uuid::now_v7().to_string(),
                version_string: "0.99.1".into(),
                pinned: false,
            },
        }));
    let catalog = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|error| panic!("catalog response should be closed: {error}"));
    assert!(catalog.items.is_empty());
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "default-version-mismatch")
        .await
        .err()
        .unwrap_or_else(|| panic!("an unobserved runtime default must fail closed"));
    assert_eq!(error.details.reason, "runtime_version_observation_mismatch");
}

#[tokio::test]
async fn catalogo_y_lease_usan_hardware_actual_y_drivers_sin_inventar() {
    let mut h = harness(16 * 1024 * MIB).await;
    let profile: larm_domain::hardware::HardwareProfile = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/hardware/profile-pcia.json"
    ))
    .unwrap_or_else(|e| panic!("fixture: {e}"));
    let profiles = Arc::new(Profiles(Mutex::new(Some(profile.clone()))));
    h.bridge = h.bridge.with_hardware(profiles.clone());
    let catalog = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("catalog: {e}"));
    let components = &catalog.items[0].fingerprint.components;
    assert_eq!(
        components["hardware_fingerprint"].value.as_deref(),
        Some(profile.hardware_fingerprint.as_str())
    );
    assert_eq!(
        components["hardware_fingerprint"].status,
        EvidenceStatus::Verified
    );
    assert_eq!(
        components["driver_versions"].status,
        EvidenceStatus::Verified
    );
    let drivers: Value = serde_json::from_str(
        components["driver_versions"]
            .value
            .as_deref()
            .unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("drivers: {e}"));
    assert_eq!(
        drivers.as_array().map(Vec::len),
        Some(profile.snapshot.devices.len())
    );
    let original_hash = catalog.items[0].fingerprint.hash.clone();

    // La misma instancia del puente ve el siguiente perfil, sin reiniciar.
    let mut changed = profile.clone();
    changed.snapshot.devices[0].driver_version = Some("test-next-driver".into());
    changed.hardware_fingerprint = changed
        .snapshot
        .fingerprint()
        .unwrap_or_else(|e| panic!("fingerprint: {e}"));
    changed.row_version += 1;
    *profiles.0.lock() = Some(changed.clone());
    let updated = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("updated: {e}"));
    assert_ne!(updated.items[0].fingerprint.hash, original_hash);
    assert_eq!(
        updated.items[0].fingerprint.components["hardware_fingerprint"]
            .value
            .as_deref(),
        Some(changed.hardware_fingerprint.as_str())
    );
    assert!(updated.items[0].fingerprint.components["driver_versions"]
        .value
        .as_deref()
        .unwrap_or_default()
        .contains("test-next-driver"));
    let lease = active_lease(&h).await;
    assert_eq!(
        lease.execution_fingerprint.components["hardware_fingerprint"],
        updated.items[0].fingerprint.components["hardware_fingerprint"]
    );
    assert_eq!(
        lease.execution_fingerprint.components["driver_versions"],
        updated.items[0].fingerprint.components["driver_versions"]
    );

    changed.snapshot.devices[0].driver_version = None;
    *profiles.0.lock() = Some(changed);
    let partial = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("partial: {e}"));
    assert_eq!(
        partial.items[0].fingerprint.components["driver_versions"].status,
        EvidenceStatus::Unavailable
    );
    assert!(partial.items[0].fingerprint.components["driver_versions"]
        .value
        .is_none());
    *profiles.0.lock() = None;
    let missing = h
        .bridge
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("missing: {e}"));
    assert_eq!(
        missing.items[0].fingerprint.components["hardware_fingerprint"].status,
        EvidenceStatus::Unavailable
    );
}

#[derive(Default)]
struct FakeOllama {
    pause_show: AtomicBool,
    show_started: tokio::sync::Notify,
    resume_show: tokio::sync::Notify,
    tags_redirect: Mutex<Option<String>>,
    chat_redirect: Mutex<Option<String>>,
    tag_requests: AtomicUsize,
    tags_override: Mutex<Option<Value>>,
    show_override: Mutex<Option<Value>>,
    shows: AtomicUsize,
    generates: AtomicUsize,
    loaded: Mutex<Vec<String>>,
    loads: AtomicUsize,
    pause_load: AtomicBool,
    load_started: tokio::sync::Notify,
    resume_load: tokio::sync::Notify,
    chats: Mutex<Vec<Value>>,
    foreign_size: Mutex<u64>,
    offload_to_cpu: Mutex<bool>,
}

const MODEL: &str = "tiny:1b";
const MODEL_SIZE: u64 = 800 * MIB;

async fn tags(AxumState(fake): AxumState<Arc<FakeOllama>>) -> Response {
    fake.tag_requests.fetch_add(1, Ordering::SeqCst);
    if let Some(target) = fake.tags_redirect.lock().clone() {
        return Redirect::temporary(&target).into_response();
    }
    if let Some(value) = fake.tags_override.lock().clone() {
        return Json(value).into_response();
    }
    Json(
        json!({"models": [{"name": MODEL, "model": MODEL, "size": MODEL_SIZE, "digest": "d".repeat(64)}]}),
    ).into_response()
}

async fn show(AxumState(fake): AxumState<Arc<FakeOllama>>, Json(body): Json<Value>) -> Json<Value> {
    fake.shows.fetch_add(1, Ordering::SeqCst);
    assert_eq!(body["model"], MODEL);
    if fake.pause_show.load(Ordering::SeqCst) {
        fake.show_started.notify_one();
        fake.resume_show.notified().await;
    }
    if let Some(value) = fake.show_override.lock().clone() {
        return Json(value);
    }
    Json(json!({
        "template": "{{ .Prompt }}",
        "model_info": {
            "general.architecture": "llama",
            "llama.block_count": 16,
            "llama.attention.head_count_kv": 8,
            "llama.attention.key_length": 64,
            "llama.attention.value_length": 64
        }
    }))
}

async fn version() -> Json<Value> {
    Json(json!({"version": "0.99.0"}))
}

async fn ps(AxumState(fake): AxumState<Arc<FakeOllama>>) -> Json<Value> {
    let cpu = *fake.offload_to_cpu.lock();
    let mut models: Vec<Value> = fake
        .loaded
        .lock()
        .iter()
        .map(|name| json!({"name": name, "size": MODEL_SIZE, "size_vram": if cpu { 0 } else { MODEL_SIZE }}))
        .collect();
    let foreign = *fake.foreign_size.lock();
    if foreign > 0 {
        models.push(json!({"name": "ajeno:70b", "size": foreign, "size_vram": foreign}));
    }
    Json(json!({"models": models}))
}

async fn generate(
    AxumState(fake): AxumState<Arc<FakeOllama>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    fake.generates.fetch_add(1, Ordering::SeqCst);
    if body["keep_alive"] == json!(-1) {
        fake.loads.fetch_add(1, Ordering::SeqCst);
        if fake.pause_load.load(Ordering::SeqCst) {
            fake.load_started.notify_one();
            fake.resume_load.notified().await;
        }
        let mut loaded = fake.loaded.lock();
        if !loaded.iter().any(|m| m == MODEL) {
            loaded.push(MODEL.to_string());
        }
    }
    Json(json!({"model": MODEL, "response": "", "done": true}))
}

async fn chat(AxumState(fake): AxumState<Arc<FakeOllama>>, Json(body): Json<Value>) -> Response {
    fake.chats.lock().push(body);
    if let Some(target) = fake.chat_redirect.lock().clone() {
        return Redirect::temporary(&target).into_response();
    }
    Json(
        json!({"model": MODEL, "choices": [{"index": 0, "message": {"role": "assistant", "content": "hola"}, "finish_reason": "stop"}]}),
    ).into_response()
}

async fn start_fake() -> (Arc<FakeOllama>, String) {
    let fake = Arc::new(FakeOllama::default());
    let app = Router::new()
        .route("/api/tags", get(tags))
        .route("/api/show", post(show))
        .route("/api/version", get(version))
        .route("/api/ps", get(ps))
        .route("/api/generate", post(generate))
        .route("/v1/chat/completions", post(chat))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| panic!("bind: {e}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("addr: {e}"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (fake, format!("http://{address}"))
}

#[derive(Default)]
struct MemorySecrets {
    entries: Mutex<HashMap<(String, String), String>>,
    fail: Mutex<bool>,
}

impl SecretStore for MemorySecrets {
    fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String> {
        if *self.fail.lock() {
            return Err("credential manager no disponible".into());
        }
        self.entries
            .lock()
            .insert((service.into(), username.into()), secret.into());
        Ok(())
    }

    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
        Ok(self
            .entries
            .lock()
            .get(&(service.to_string(), username.to_string()))
            .cloned())
    }

    fn delete(&self, service: &str, username: &str) -> Result<(), String> {
        self.entries
            .lock()
            .remove(&(service.to_string(), username.to_string()));
        Ok(())
    }
}

struct Harness {
    bridge: ExternalAdoptionBridge,
    fake: Arc<FakeOllama>,
    secrets: Arc<MemorySecrets>,
    variant_id: String,
    profile_id: String,
}

fn config(base_url: &str, budget: u64, accepted: bool) -> (AdoptionConfig, String, String) {
    let variant_id = uuid::Uuid::now_v7().to_string();
    let profile_id = uuid::Uuid::now_v7().to_string();
    let config = AdoptionConfig {
        host_id: uuid::Uuid::now_v7().to_string(),
        runtime: "Ollama".into(),
        base_url: base_url.into(),
        accept_external_local_trusted: accepted,
        runtime_version_id: uuid::Uuid::now_v7().to_string(),
        memory_budget_bytes: budget.to_string(),
        gateway_base_url: "http://127.0.0.1:8790/api/v1/gateway".into(),
        launch_profile: json!({"id": profile_id, "name": "BALANCED", "parameters": {"context": 4096}}),
        variants: vec![AdoptedVariant {
            variant_id: variant_id.clone(),
            model: MODEL.into(),
            capabilities: vec!["text.generate".into()],
        }],
        request_timeout_seconds: 5,
        load_timeout_seconds: 5,
        inference_timeout_seconds: 5,
    };
    (config, variant_id, profile_id)
}

async fn harness(budget: u64) -> Harness {
    let (fake, base) = start_fake().await;
    let (config, variant_id, profile_id) = config(&base, budget, true);
    let secrets = Arc::new(MemorySecrets::default());
    let bridge = ExternalAdoptionBridge::new(config, secrets.clone())
        .unwrap_or_else(|e| panic!("bridge: {e}"));
    Harness {
        bridge,
        fake,
        secrets,
        variant_id,
        profile_id,
    }
}

fn broker() -> Principal {
    larm_domain::auth::Principal::new(
        "0192aaaa-0000-7000-8000-000000000003",
        "ai-broker",
        larm_domain::auth::PrincipalKind::Broker,
        [
            larm_domain::auth::ApiScope::Read,
            larm_domain::auth::ApiScope::LeasesManage,
        ],
        [],
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn lease_request(h: &Harness) -> LeaseRequest {
    serde_json::from_value(json!({
        "variant_id": h.variant_id, "capability": "text.generate", "launch_profile_id": h.profile_id,
        "constraints": {"runtime_version_id": null, "backend_id": null, "execution_fingerprint": null,
                        "allow_backend_fallback": false, "max_memory_bytes": null},
        "ttl_seconds": 120, "owner_id": "ai-broker", "request_id": "task:inv",
        "resource_requirements": {"capability": "text.generate", "context_tokens": 100, "max_output_tokens": 50,
                                  "batch": 1, "width": null, "height": null, "frames": null, "sample_rate": null,
                                  "audio_duration_ms": null, "hard_memory_budget_bytes": null}
    }))
    .unwrap_or_else(|e| panic!("request: {e}"))
}

const BIG_BUDGET: u64 = 64 * 1024 * MIB;

fn listed_model() -> Value {
    json!({"name": MODEL, "model": MODEL, "size": MODEL_SIZE, "digest": "d".repeat(64)})
}

#[tokio::test]
async fn local_adoption_rejects_remote_and_malformed_tags_before_show_or_lease() {
    for (field, value, reason) in [
        (
            "remote_host",
            json!("https://ollama.com"),
            "external_model_is_remote",
        ),
        (
            "remote_model",
            json!("upstream-model"),
            "external_model_is_remote",
        ),
        (
            "remote_host",
            json!(false),
            "external_model_metadata_invalid",
        ),
        ("remote_model", json!({}), "external_model_metadata_invalid"),
        (
            "remote_host",
            Value::Null,
            "external_model_metadata_invalid",
        ),
    ] {
        let h = harness(BIG_BUDGET).await;
        let mut model = listed_model();
        model[field] = value;
        *h.fake.tags_override.lock() = Some(json!({"models": [model]}));
        let catalog = h.bridge.catalog().await.unwrap_or_else(|e| panic!("{e}"));
        assert!(
            catalog.items.is_empty(),
            "{field}: remote candidate was published"
        );
        let error = h
            .bridge
            .create_lease(&broker(), lease_request(&h), "remote-tag")
            .await
            .err()
            .unwrap_or_else(|| panic!("remote candidate admitted: {field}"));
        assert_eq!(error.details.reason, reason);
        assert_eq!(h.fake.shows.load(Ordering::SeqCst), 0);
        assert_eq!(h.fake.generates.load(Ordering::SeqCst), 0);
        let state = h.bridge.inner.state.lock();
        assert!(state.leases.is_empty());
        assert!(state.jobs.is_empty());
        assert!(h.secrets.entries.lock().is_empty());
    }
}

#[tokio::test]
async fn local_adoption_rejects_remote_show_even_when_tags_look_local() {
    let h = harness(BIG_BUDGET).await;
    *h.fake.show_override.lock() =
        Some(json!({"remote_model":"upstream-model", "remote_host":"https://ollama.com"}));
    let catalog = h.bridge.catalog().await.unwrap_or_else(|e| panic!("{e}"));
    assert!(catalog.items.is_empty());
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "remote-show")
        .await
        .err()
        .unwrap_or_else(|| panic!("remote show admitted"));
    assert_eq!(error.details.reason, "external_model_is_remote");
    assert_eq!(h.fake.generates.load(Ordering::SeqCst), 0);
    assert!(h.bridge.inner.state.lock().leases.is_empty());
}

#[tokio::test]
async fn local_adoption_gateway_rechecks_remote_metadata_before_sending_input() {
    let h = harness(BIG_BUDGET).await;
    let lease = active_lease(&h).await;
    let token = h.bridge.inner.state.lock().leases[&lease.id].token.clone();
    let mut model = listed_model();
    model["remote_host"] = json!("https://ollama.com");
    *h.fake.tags_override.lock() = Some(json!({"models": [model]}));
    let error = h
        .bridge
        .gateway(
            &lease.id,
            Some(&token),
            "chat/completions",
            json!({"messages":[{"role":"user","content":"private fixture"}]}),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("gateway sent input to a remote alias"));
    assert_eq!(error.details.reason, "external_model_is_remote");
    assert!(h.fake.chats.lock().is_empty());
    assert_eq!(h.fake.generates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_adoption_missing_model_has_no_show_load_or_secret() {
    let h = harness(BIG_BUDGET).await;
    *h.fake.tags_override.lock() = Some(json!({"models": []}));
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "missing-model")
        .await
        .err()
        .unwrap_or_else(|| panic!("missing model admitted"));
    assert_eq!(error.code, "MODEL_OFFLINE");
    assert_eq!(h.fake.shows.load(Ordering::SeqCst), 0);
    assert_eq!(h.fake.generates.load(Ordering::SeqCst), 0);
    assert!(h.secrets.entries.lock().is_empty());
}

#[tokio::test]
async fn local_adoption_empty_remote_fields_preserve_local_execution() {
    let h = harness(BIG_BUDGET).await;
    let mut model = listed_model();
    model["remote_host"] = json!("");
    model["remote_model"] = json!("");
    *h.fake.tags_override.lock() = Some(json!({"models":[model]}));
    *h.fake.show_override.lock() = Some(json!({"remote_host":"", "remote_model":""}));
    let lease = active_lease(&h).await;
    let token = h.bridge.inner.state.lock().leases[&lease.id].token.clone();
    let response = h
        .bridge
        .gateway(
            &lease.id,
            Some(&token),
            "chat/completions",
            json!({"messages":[{"role":"user","content":"fixture"}]}),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(response.status, 200);
    assert_eq!(h.fake.chats.lock().len(), 1);
}

#[tokio::test]
async fn local_adoption_gateway_rechecks_show_before_sending_input() {
    let h = harness(BIG_BUDGET).await;
    let lease = active_lease(&h).await;
    let token = h.bridge.inner.state.lock().leases[&lease.id].token.clone();
    *h.fake.show_override.lock() = Some(json!({"remote_model":"retagged-upstream"}));
    let error = h
        .bridge
        .gateway(&lease.id, Some(&token), "chat/completions", json!({}))
        .await
        .err()
        .unwrap_or_else(|| panic!("remote show forwarded"));
    assert_eq!(error.details.reason, "external_model_is_remote");
    assert!(h.fake.chats.lock().is_empty());
}

#[tokio::test]
async fn local_adoption_gateway_rechecks_lease_after_metadata_io() {
    let h = harness(BIG_BUDGET).await;
    let lease = active_lease(&h).await;
    let token = h.bridge.inner.state.lock().leases[&lease.id].token.clone();
    h.fake.pause_show.store(true, Ordering::SeqCst);
    let bridge = h.bridge.clone();
    let lease_id = lease.id.clone();
    let call = tokio::spawn(async move {
        bridge
            .gateway(&lease_id, Some(&token), "chat/completions", json!({}))
            .await
    });
    h.fake.show_started.notified().await;
    let record = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    h.bridge
        .release_lease(&broker(), &lease.id, record.row_version)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    h.fake.pause_show.store(false, Ordering::SeqCst);
    h.fake.resume_show.notify_waiters();
    let error = call
        .await
        .unwrap_or_else(|e| panic!("{e}"))
        .err()
        .unwrap_or_else(|| panic!("released lease forwarded after metadata IO"));
    assert_eq!(error.details.reason, "lease_not_active");
    assert!(h.fake.chats.lock().is_empty());
}

#[tokio::test]
async fn local_adoption_idle_policy_does_not_send_generate_to_remote_alias() {
    let h = harness(BIG_BUDGET).await;
    let mut model = listed_model();
    model["remote_model"] = json!("retagged-upstream");
    *h.fake.tags_override.lock() = Some(json!({"models":[model]}));
    let error = h
        .bridge
        .restore_idle_policy(&h.bridge.inner.config.variants[0])
        .await
        .err()
        .unwrap_or_else(|| panic!("idle policy dispatched to remote alias"));
    assert_eq!(error.details.reason, "external_model_is_remote");
    assert_eq!(h.fake.generates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_adoption_inventory_does_not_follow_redirects() {
    let h = harness(BIG_BUDGET).await;
    let (target, target_base) = start_fake().await;
    *h.fake.tags_redirect.lock() = Some(format!("{target_base}/api/tags"));
    assert!(h
        .bridge
        .probe(&h.bridge.inner.config.variants[0])
        .await
        .is_err());
    assert_eq!(target.tag_requests.load(Ordering::SeqCst), 0);
    assert_eq!(h.fake.shows.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_adoption_gateway_does_not_follow_redirects_with_input() {
    let h = harness(BIG_BUDGET).await;
    let lease = active_lease(&h).await;
    let token = h.bridge.inner.state.lock().leases[&lease.id].token.clone();
    let (target, target_base) = start_fake().await;
    *h.fake.chat_redirect.lock() = Some(format!("{target_base}/v1/chat/completions"));
    let response = h
        .bridge
        .gateway(
            &lease.id,
            Some(&token),
            "chat/completions",
            json!({"messages":[{"role":"user","content":"private fixture"}]}),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(response.status, 307);
    assert!(target.chats.lock().is_empty());
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "P070 opt-in metadata-only observation of attributed installed Ollama; not qualification"]
async fn installed_ollama_readonly_metadata_and_remote_rejection() {
    assert_eq!(std::env::var("LARM_P070_READONLY").as_deref(), Ok("1"));
    let (mut config, _, _) = config("http://127.0.0.1:11434", BIG_BUDGET, false);
    config.variants[0].model = "granite-embedding:30m".into();
    let secrets = Arc::new(MemorySecrets::default());
    let bridge =
        ExternalAdoptionBridge::new(config, secrets.clone()).unwrap_or_else(|e| panic!("{e}"));
    let local = bridge
        .probe(&bridge.inner.config.variants[0])
        .await
        .unwrap_or_else(|e| panic!("read-only local metadata: {e}"));
    assert_eq!(local.runtime_version.as_deref(), Some("0.40.11"));
    assert_eq!(local.size_bytes, 62_534_804);
    assert_eq!(
        local.digest.as_deref(),
        Some("eb4c533ba6f7983afdc681a327251a34667340567bb160adfde1a3e07c468ec7")
    );
    for model in [
        "beam:cloud",
        "gemma4:31b-cloud",
        "larm-p070-does-not-exist:fixture",
    ] {
        let mut variant = bridge.inner.config.variants[0].clone();
        variant.model = model.into();
        let error = bridge
            .probe(&variant)
            .await
            .err()
            .unwrap_or_else(|| panic!("remote or missing model accepted: {model}"));
        assert_eq!(
            error.details.reason,
            if model.contains("does-not-exist") {
                "model_absent_in_runtime"
            } else {
                "external_model_is_remote"
            }
        );
    }
    assert!(!bridge.inner.config.accept_external_local_trusted);
    let state = bridge.inner.state.lock();
    assert!(state.leases.is_empty());
    assert!(state.jobs.is_empty());
    assert!(secrets.entries.lock().is_empty());
    println!(
        "P070 metadata-only: local=1 remote-rejected=2 missing-rejected=1; no leases/jobs/secrets"
    );
}

async fn active_lease(h: &Harness) -> LeaseResponse {
    let outcome = h
        .bridge
        .create_lease(&broker(), lease_request(h), "task:inv")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let body = if outcome.status == 202 {
        let job_id = outcome.body["job_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut job = h
            .bridge
            .get_job(&broker(), &job_id)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        for _ in 0..200 {
            if job.state != JobPhase::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            job = h
                .bridge
                .get_job(&broker(), &job_id)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(job.state, JobPhase::Completed, "{:?}", job.error);
        job.result.unwrap_or_default()
    } else {
        outcome.body
    };
    serde_json::from_value(body).unwrap_or_else(|e| panic!("lease: {e}"))
}

async fn seed_qualified_endpoint(
    repository: &larm_db::runtime_leases::SqlRuntimeLeases,
    generation: u64,
) -> (String, String) {
    let instance_id = "0192aaaa-0000-7000-8000-000000000950".to_string();
    let endpoint_id = "0192aaaa-0000-7000-8000-000000000951".to_string();
    seed_qualified_references(
        repository,
        QualifiedEndpointSeed {
            instance_id: &instance_id,
            endpoint_id: &endpoint_id,
            generation,
            protocol: "openai-compatible",
            protocol_profile_hash: &"a".repeat(64),
            base_url: "http://127.0.0.1:1234",
            capabilities: "[]",
            execution_fingerprint: "fingerprint",
        },
    )
    .await;
    (instance_id, endpoint_id)
}

struct QualifiedEndpointSeed<'a> {
    instance_id: &'a str,
    endpoint_id: &'a str,
    generation: u64,
    protocol: &'a str,
    protocol_profile_hash: &'a str,
    base_url: &'a str,
    capabilities: &'a str,
    execution_fingerprint: &'a str,
}

async fn seed_qualified_references(
    repository: &larm_db::runtime_leases::SqlRuntimeLeases,
    seed: QualifiedEndpointSeed<'_>,
) {
    let pool = &repository.pool;
    for statement in [
        "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-ai','t','t',1,'{}','source-adapter','1.0','source','hash','test','UNTRUSTED','{}','INSTALLED')",
        "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter','t','t',1,'{}','source-ai','HTTP','{}')",
        "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('canonical','t','t',1,'{}','test','{}','[]')",
        "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source','t','t',1,'{}','test','source-adapter','https://example.test',0,0,'NONE','{}')",
        "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','canonical','source','test','model')",
        "INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('revision','t','t',1,'{}','distribution','main','hash','CURRENT')",
        "INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES('variant','t','t',1,'{}','revision','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)",
        "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('runtime-ai','t','t',1,'{}','runtime-adapter','1.0','runtime','hash','test','UNTRUSTED','{}','INSTALLED')",
        "INSERT INTO runtime_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,runtime_family,capabilities) VALUES('runtime-adapter','t','t',1,'{}','runtime-ai','test','{}')",
        "INSERT INTO runtime(id,created_at,updated_at,row_version,metadata,channel_policy,name,adapter_id,capabilities,default_version_id) VALUES('runtime','t','t',1,'{}','{}','test','runtime-adapter','{}',NULL)",
        "INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('runtime-version','t','t',1,'{}','runtime','1.0','stable','NONE')",
        "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,state,ownership) VALUES('MANAGED','VALIDATED','installation','t','t',1,'{}','runtime-version','CUSTOM','VALIDATED','LARM_MANAGED')",
        "INSERT INTO runtime_backend(limits,id,created_at,updated_at,row_version,metadata,runtime_installation_id,backend_type,status,evidence_refs) VALUES('{}','backend','t','t',1,'{}','installation','test','AVAILABLE','[]')",
        "INSERT INTO launch_profile(id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id) VALUES('profile','t','t',1,'{}','BALANCED','{}','hash','backend')",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("seed qualified endpoint: {error}"));
    }
    sqlx::query(
        "INSERT INTO runtime_instance(id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) \
         VALUES(?, 't','t',1,'{}','installation','variant','backend','profile','READY',NULL,NULL,NULL,?,?,NULL,'EXTERNAL')",
    )
    .bind(seed.instance_id)
    .bind(
        i64::try_from(seed.generation)
            .unwrap_or_else(|error| panic!("generation: {error}")),
    )
    .bind(seed.execution_fingerprint)
    .execute(pool)
    .await
    .unwrap_or_else(|error| panic!("seed runtime instance: {error}"));
    sqlx::query(
        "INSERT INTO inference_endpoint(id,created_at,updated_at,row_version,metadata,instance_id,protocol,protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs) \
         VALUES(?, 't','t',1,'{}',?,?,?,?,NULL,?,'QUALIFIED','[\"test:evidence\"]')",
    )
    .bind(seed.endpoint_id)
    .bind(seed.instance_id)
    .bind(seed.protocol)
    .bind(seed.protocol_profile_hash)
    .bind(seed.base_url)
    .bind(seed.capabilities)
    .execute(pool)
    .await
    .unwrap_or_else(|error| panic!("seed inference endpoint: {error}"));
}

async fn persist_active_lease(
    repository: &larm_db::runtime_leases::SqlRuntimeLeases,
    record: &RuntimeLease,
) -> RuntimeLease {
    let now = timestamp(chrono::Utc::now());
    let mut requested = record.clone();
    requested.created_at = now.clone();
    requested.updated_at = now;
    requested.row_version = 1;
    requested.instance_id = None;
    requested.endpoint_id = None;
    requested.state = LeaseState::Requested;
    requested.expires_at = None;
    let context = MutationContext {
        actor_id: record.owner_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    repository
        .create_requested(
            &requested,
            &"a".repeat(64),
            &format!("setup-{}", record.id),
            &CreateLeaseOutcome {
                status: 201,
                body: json!({"id": record.id}),
            },
            &context,
        )
        .await
        .unwrap_or_else(|error| panic!("persist requested lease: {error}"));
    let admitting = repository
        .transition(
            &record.id,
            &record.owner_id,
            1,
            record.generation,
            &LeaseStateUpdate {
                from: LeaseState::Requested,
                to: LeaseState::Admitting,
                instance_id: None,
                endpoint_id: None,
                expires_at: None,
            },
            &context,
        )
        .await
        .unwrap_or_else(|error| panic!("persist admitting lease: {error}"));
    assert_eq!(admitting.state, LeaseState::Admitting);
    let (instance_id, endpoint_id) = seed_qualified_endpoint(repository, record.generation).await;
    repository
        .transition(
            &record.id,
            &record.owner_id,
            admitting.row_version,
            record.generation,
            &LeaseStateUpdate {
                from: LeaseState::Admitting,
                to: LeaseState::Active,
                instance_id: Some(instance_id),
                endpoint_id: Some(endpoint_id),
                expires_at: Some(timestamp(chrono::Utc::now() + chrono::Duration::minutes(2))),
            },
            &context,
        )
        .await
        .unwrap_or_else(|error| panic!("persist active lease: {error}"));
    larm_domain::ports::RuntimeLeaseRepository::get_for_owner(
        repository,
        &record.id,
        &record.owner_id,
    )
    .await
    .unwrap_or_else(|error| panic!("read seeded active lease: {error}"))
    .unwrap_or_else(|| panic!("seeded lease missing"))
}

struct TestReferenceStore {
    repository: Arc<larm_db::runtime_leases::SqlRuntimeLeases>,
}

#[async_trait]
impl QualifiedAdoptionReferenceStore for TestReferenceStore {
    async fn qualify_and_register(
        &self,
        request: &ExternalAdoptionQualification,
    ) -> Result<QualifiedAdoptionReferences, ApiError> {
        let endpoint_id = uuid::Uuid::now_v7().to_string();
        seed_qualified_references(
            &self.repository,
            QualifiedEndpointSeed {
                instance_id: &request.instance_id,
                endpoint_id: &endpoint_id,
                generation: request.generation,
                protocol: &request.endpoint.protocol,
                protocol_profile_hash: &request.endpoint.protocol_profile_hash,
                base_url: &request.endpoint.base_url,
                capabilities: &serde_json::to_string(&request.endpoint.capabilities)
                    .unwrap_or_default(),
                execution_fingerprint: &request.execution_fingerprint,
            },
        )
        .await;
        Ok(QualifiedAdoptionReferences {
            instance_id: request.instance_id.clone(),
            endpoint_id,
        })
    }
}

#[tokio::test]
async fn admision_durable_no_termina_sin_instancia_y_endpoint_cualificados() {
    let mut h = harness(BIG_BUDGET).await;
    let (_directory, db) = larm_db::testing::temp_migrated()
        .await
        .unwrap_or_else(|error| panic!("db: {error}"));
    let leases = Arc::new(larm_db::runtime_leases::SqlRuntimeLeases::new(
        db.pool.clone(),
    ));
    let jobs = Arc::new(larm_db::jobs::SqlJournalJobs::broker_lease_admission(
        db.pool.clone(),
    ));
    h.bridge = h
        .bridge
        .with_lease_repository(leases.clone())
        .with_job_repository(jobs.clone());

    let outcome = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "durable-create")
        .await
        .unwrap_or_else(|error| panic!("create durable lease: {error}"));
    assert_eq!(outcome.status, 202);
    let receipt: JobReceipt =
        serde_json::from_value(outcome.body).unwrap_or_else(|error| panic!("receipt: {error}"));
    assert_eq!(receipt.state, JobPhase::Queued);

    let mut job = h
        .bridge
        .get_job(&broker(), &receipt.job_id)
        .await
        .unwrap_or_else(|error| panic!("get durable job: {error}"));
    for _ in 0..200 {
        if job.state != JobPhase::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        job = h
            .bridge
            .get_job(&broker(), &receipt.job_id)
            .await
            .unwrap_or_else(|error| panic!("poll durable job: {error}"));
    }
    assert_eq!(job.state, JobPhase::Failed, "{:?}", job.error);
    assert_eq!(
        job.error
            .as_ref()
            .map(|error| error.details.reason.as_str()),
        Some("lease_activation_reference_unavailable")
    );
    assert_eq!(h.fake.loads.load(Ordering::SeqCst), 0);

    let lease_id = job
        .resource_id
        .as_deref()
        .unwrap_or_else(|| panic!("job without lease id"));
    let durable_lease = larm_domain::ports::RuntimeLeaseRepository::get_for_owner(
        leases.as_ref(),
        lease_id,
        "ai-broker",
    )
    .await
    .unwrap_or_else(|error| panic!("get durable lease: {error}"))
    .unwrap_or_else(|| panic!("missing durable lease"));
    // Sin referencias cualificadas la admisión falla explícitamente; no se
    // entrega una LeaseResponse ACTIVE que SQLite no pueda respaldar.
    assert_eq!(durable_lease.state, LeaseState::Failed);
    assert!(durable_lease.instance_id.is_none());
    assert!(durable_lease.endpoint_id.is_none());
}

#[tokio::test]
async fn admision_durable_persiste_referencias_y_transiciona_lease_a_active() {
    let mut h = harness(BIG_BUDGET).await;
    let (_directory, db) = larm_db::testing::temp_migrated()
        .await
        .unwrap_or_else(|error| panic!("db: {error}"));
    let leases = Arc::new(larm_db::runtime_leases::SqlRuntimeLeases::new(
        db.pool.clone(),
    ));
    let jobs = Arc::new(larm_db::jobs::SqlJournalJobs::broker_lease_admission(
        db.pool.clone(),
    ));
    h.bridge = h
        .bridge
        .with_lease_repository(leases.clone())
        .with_job_repository(jobs)
        .with_reference_store(Arc::new(TestReferenceStore {
            repository: leases.clone(),
        }));

    let outcome = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "durable-create-qualified")
        .await
        .unwrap_or_else(|error| panic!("create durable lease: {error}"));
    assert_eq!(outcome.status, 202);
    let receipt: JobReceipt =
        serde_json::from_value(outcome.body).unwrap_or_else(|error| panic!("receipt: {error}"));
    let mut job = h
        .bridge
        .get_job(&broker(), &receipt.job_id)
        .await
        .unwrap_or_else(|error| panic!("get durable job: {error}"));
    for _ in 0..300 {
        if !matches!(job.state, JobPhase::Queued | JobPhase::Running) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        job = h
            .bridge
            .get_job(&broker(), &receipt.job_id)
            .await
            .unwrap_or_else(|error| panic!("poll durable job: {error}"));
    }
    assert_eq!(job.state, JobPhase::Completed, "{:?}", job.error);
    let response: LeaseResponse = serde_json::from_value(
        job.result
            .clone()
            .unwrap_or_else(|| panic!("completed job without result")),
    )
    .unwrap_or_else(|error| panic!("lease response: {error}"));
    let lease = larm_domain::ports::RuntimeLeaseRepository::get_for_owner(
        leases.as_ref(),
        &response.id,
        "ai-broker",
    )
    .await
    .unwrap_or_else(|error| panic!("read active lease: {error}"))
    .unwrap_or_else(|| panic!("active lease missing"));
    assert_eq!(lease.state, LeaseState::Active);
    assert_eq!(
        lease.instance_id.as_deref(),
        Some(response.instance_id.as_str())
    );
    assert_eq!(
        lease.endpoint_id.as_deref(),
        Some(response.endpoint_id.as_str())
    );
    let (instance_state, generation): (String, i64) =
        sqlx::query_as("SELECT state,generation FROM runtime_instance WHERE id=?")
            .bind(&response.instance_id)
            .fetch_one(&leases.pool)
            .await
            .unwrap_or_else(|error| panic!("read qualified instance: {error}"));
    assert_eq!(instance_state, "READY");
    assert_eq!(u64::try_from(generation).ok(), Some(response.generation));
    let endpoint_qualification: String = sqlx::query_scalar(
        "SELECT qualification FROM inference_endpoint WHERE id=? AND instance_id=?",
    )
    .bind(&response.endpoint_id)
    .bind(&response.instance_id)
    .fetch_one(&leases.pool)
    .await
    .unwrap_or_else(|error| panic!("read qualified endpoint: {error}"));
    assert_eq!(endpoint_qualification, "QUALIFIED");
    assert_eq!(h.fake.loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn catalog_publishes_only_accepted_variants_with_honest_evidence() {
    let h = harness(BIG_BUDGET).await;
    let catalog = h.bridge.catalog().await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(catalog.contract_version, "1.0");
    let item = &catalog.items[0];
    assert_eq!(item.variant_id, h.variant_id);
    let c = &item.fingerprint.components;
    assert_eq!(c["artifact_bundle_hash"].status, EvidenceStatus::Verified);
    assert_eq!(c["runtime_version"].value.as_deref(), Some("0.99.0"));
    // Antes de cargar no se sabe el backend: no se inventa.
    assert_eq!(c["backend"].status, EvidenceStatus::Unavailable);
    assert_eq!(
        c["hardware_fingerprint"].status,
        EvidenceStatus::Unavailable
    );
    assert_eq!(item.fingerprint.hash, fingerprint_hash(c));

    let (fake_base_config, _, _) = config("http://127.0.0.1:9", BIG_BUDGET, false);
    let refused = ExternalAdoptionBridge::new(fake_base_config, Arc::new(MemorySecrets::default()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(refused
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("{e}"))
        .items
        .is_empty());
    drop(h.fake);
}

#[tokio::test]
async fn cold_variant_is_admitted_by_job_and_activated() {
    let h = harness(BIG_BUDGET).await;
    let outcome = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "k1")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(outcome.status, 202);
    let lease = active_lease(&h).await; // misma clave → otra lease (clave distinta "task:inv")
    assert_eq!(
        lease.execution_fingerprint.components["backend"]
            .value
            .as_deref(),
        Some("GPU")
    );
    assert_eq!(lease.renew_after_seconds, 40);
    assert!(lease
        .endpoint
        .base_url
        .ends_with(&format!("/{}/v1", lease.id)));
    assert!(lease.endpoint.credential_reference.is_some());
    // El token de la lease vive en el almacén de secretos.
    assert!(h
        .secrets
        .entries
        .lock()
        .contains_key(&(format!("larm/lease/{}", lease.id), "ai-broker".to_string())));
}

#[tokio::test]
async fn idempotent_replay_returns_the_same_outcome_without_a_second_admission() {
    let h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let first = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "same")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let again = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "same")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first, again);
    assert_eq!(first.status, 201);
    assert_eq!(
        h.bridge
            .list_leases(&broker())
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .items
            .len(),
        1
    );
    let mut changed = lease_request(&h);
    changed.ttl_seconds = 300;
    let conflict = h
        .bridge
        .create_lease(&broker(), changed, "same")
        .await
        .err();
    assert_eq!(
        conflict.map(|e| e.code),
        Some("IDEMPOTENCY_CONFLICT".into())
    );
}

#[tokio::test]
async fn admission_rejects_invalid_or_unauthorized_requests_before_io() {
    let h = harness(BIG_BUDGET).await;
    let reader = larm_domain::auth::Principal::new(
        "0192aaaa-0000-7000-8000-000000000004",
        "ai-broker",
        larm_domain::auth::PrincipalKind::Broker,
        [larm_domain::auth::ApiScope::Read],
        [],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let cases: Vec<(Principal, LeaseRequest, &str)> = vec![
        (reader, lease_request(&h), "FORBIDDEN"),
        (
            broker(),
            LeaseRequest {
                owner_id: "otro".into(),
                ..lease_request(&h)
            },
            "FORBIDDEN",
        ),
        (
            broker(),
            LeaseRequest {
                ttl_seconds: 10,
                ..lease_request(&h)
            },
            "VALIDATION_FAILED",
        ),
        (
            broker(),
            LeaseRequest {
                variant_id: Some(uuid::Uuid::now_v7().to_string()),
                ..lease_request(&h)
            },
            "MODEL_NOT_FOUND",
        ),
        (
            broker(),
            LeaseRequest {
                capability: Some("image.generate".into()),
                ..lease_request(&h)
            },
            "UNSUPPORTED_CAPABILITY",
        ),
        (
            broker(),
            LeaseRequest {
                launch_profile_id: uuid::Uuid::now_v7().to_string(),
                ..lease_request(&h)
            },
            "NOT_FOUND",
        ),
    ];
    for (index, (principal, request, code)) in cases.into_iter().enumerate() {
        let error = h
            .bridge
            .create_lease(&principal, request, &format!("neg-{index}"))
            .await
            .err();
        assert_eq!(
            error.map(|e| e.code),
            Some(larm_domain::error::ErrorCode::from(code)),
            "caso {index}"
        );
    }
    let mut constrained = lease_request(&h);
    constrained.constraints.backend_id = Some(uuid::Uuid::now_v7().to_string());
    let error = h
        .bridge
        .create_lease(&broker(), constrained, "neg-backend")
        .await
        .err();
    assert_eq!(error.map(|e| e.code), Some("INCOMPATIBLE".into()));
    assert_eq!(h.fake.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn memory_admission_distinguishes_absolute_and_current_shortage() {
    let small = harness(MODEL_SIZE).await;
    let error = small
        .bridge
        .create_lease(&broker(), lease_request(&small), "k")
        .await
        .err()
        .unwrap_or_else(|| panic!("debía fallar"));
    assert_eq!(
        (error.code.as_str(), error.retryable),
        ("RESOURCE_UNAVAILABLE", false)
    );
    assert!(!error.details.evidence_refs.is_empty());

    let h = harness(4 * 1024 * MIB).await;
    *h.fake.foreign_size.lock() = 3 * 1024 * MIB;
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "k")
        .await
        .err()
        .unwrap_or_else(|| panic!("debía esperar"));
    assert_eq!(
        (error.code.as_str(), error.retryable),
        ("RESOURCE_UNAVAILABLE", true)
    );
    assert_eq!(error.http_status(), 409);
    // Lo ajeno no se toca: ni se descarga ni se mata.
    assert_eq!(h.fake.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn renew_needs_current_revision_and_generation_and_is_idempotent() {
    let h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    let record = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let renew = Renew {
        generation: lease.generation,
        ttl_seconds: 60,
    };
    let stale = h
        .bridge
        .renew_lease(
            &broker(),
            &lease.id,
            record.row_version + 5,
            "r1",
            renew.clone(),
        )
        .await
        .err();
    assert_eq!(stale.map(|e| e.code), Some("STALE_REVISION".into()));
    let wrong_generation = Renew {
        generation: lease.generation + 1,
        ..renew.clone()
    };
    let invalid = h
        .bridge
        .renew_lease(
            &broker(),
            &lease.id,
            record.row_version,
            "r2",
            wrong_generation,
        )
        .await
        .err();
    assert_eq!(
        invalid.map(|e| e.code),
        Some("INVALID_STATE_TRANSITION".into())
    );
    let renewed = h
        .bridge
        .renew_lease(
            &broker(),
            &lease.id,
            record.row_version,
            "r3",
            renew.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(renewed.row_version, record.row_version + 1);
    let replay = h
        .bridge
        .renew_lease(&broker(), &lease.id, record.row_version, "r3", renew)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(replay, renewed);
}

#[tokio::test]
async fn renovacion_y_liberacion_durables_actualizan_sqlite_y_revocan_secreto() {
    let mut h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    let (_directory, db) = larm_db::testing::temp_migrated()
        .await
        .unwrap_or_else(|error| panic!("db: {error}"));
    let repository = Arc::new(larm_db::runtime_leases::SqlRuntimeLeases::new(
        db.pool.clone(),
    ));
    let memory_record = h
        .bridge
        .inner
        .state
        .lock()
        .leases
        .get(&lease.id)
        .map(|entry| entry.record.clone())
        .unwrap_or_else(|| panic!("active in-memory lease missing"));
    let durable = persist_active_lease(repository.as_ref(), &memory_record).await;
    h.bridge = h.bridge.clone().with_lease_repository(repository.clone());
    if let Some(entry) = h.bridge.inner.state.lock().leases.get_mut(&lease.id) {
        entry.record = durable.clone();
    } else {
        panic!("active in-memory lease disappeared");
    }

    let request = Renew {
        generation: durable.generation,
        ttl_seconds: 60,
    };
    let renewed = h
        .bridge
        .renew_lease(
            &broker(),
            &lease.id,
            durable.row_version,
            "durable-renew",
            request.clone(),
        )
        .await
        .unwrap_or_else(|error| panic!("renew durable lease: {error}"));
    assert_eq!(renewed.state, LeaseState::Active);
    assert_eq!(renewed.row_version, durable.row_version + 1);
    let replay = h
        .bridge
        .renew_lease(
            &broker(),
            &lease.id,
            durable.row_version,
            "durable-renew",
            request,
        )
        .await
        .unwrap_or_else(|error| panic!("replay durable renewal: {error}"));
    assert_eq!(replay, renewed);

    h.bridge
        .release_lease(&broker(), &lease.id, renewed.row_version)
        .await
        .unwrap_or_else(|error| panic!("release durable lease: {error}"));
    let released = larm_domain::ports::RuntimeLeaseRepository::get_for_owner(
        repository.as_ref(),
        &lease.id,
        "ai-broker",
    )
    .await
    .unwrap_or_else(|error| panic!("read released lease: {error}"))
    .unwrap_or_else(|| panic!("released durable lease missing"));
    assert_eq!(released.state, LeaseState::Released);
    assert!(h.secrets.entries.lock().is_empty());
}

#[tokio::test]
async fn release_is_idempotent_and_owner_bound() {
    let h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    let record = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let intruder = larm_domain::auth::Principal::new(
        "0192aaaa-0000-7000-8000-000000000005",
        "otro",
        larm_domain::auth::PrincipalKind::Operator,
        [
            larm_domain::auth::ApiScope::Read,
            larm_domain::auth::ApiScope::LeasesManage,
        ],
        [],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let forbidden = h
        .bridge
        .release_lease(&intruder, &lease.id, record.row_version)
        .await
        .err();
    assert_eq!(forbidden.map(|e| e.code), Some("FORBIDDEN".into()));
    h.bridge
        .release_lease(&broker(), &lease.id, record.row_version)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    // Ya liberada: 204 aunque la revisión sea vieja.
    h.bridge
        .release_lease(&broker(), &lease.id, record.row_version)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let after = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(after.state, LeaseState::Released);
    assert!(
        h.secrets.entries.lock().is_empty(),
        "el token se revoca al liberar"
    );
}

#[tokio::test]
async fn liberacion_durante_admission_se_rechaza_hasta_que_la_lease_este_activa() {
    let h = harness(BIG_BUDGET).await;
    h.fake.pause_load.store(true, Ordering::SeqCst);
    let outcome = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "release-while-loading")
        .await
        .unwrap_or_else(|error| panic!("create lease: {error}"));
    assert_eq!(outcome.status, 202);
    let receipt: JobReceipt =
        serde_json::from_value(outcome.body).unwrap_or_else(|error| panic!("receipt: {error}"));

    tokio::time::timeout(Duration::from_secs(3), h.fake.load_started.notified())
        .await
        .unwrap_or_else(|_| panic!("worker did not enter model load"));
    let job = h
        .bridge
        .get_job(&broker(), &receipt.job_id)
        .await
        .unwrap_or_else(|error| panic!("get job: {error}"));
    let lease_id = job
        .resource_id
        .as_deref()
        .unwrap_or_else(|| panic!("job without lease id"));
    let admitting = h
        .bridge
        .get_lease(&broker(), lease_id)
        .await
        .unwrap_or_else(|error| panic!("get admitting lease: {error}"));
    assert_eq!(admitting.state, LeaseState::Admitting);

    let rejected = h
        .bridge
        .release_lease(&broker(), lease_id, admitting.row_version)
        .await
        .err();
    assert_eq!(
        rejected.map(|error| error.code),
        Some("INVALID_STATE_TRANSITION".into())
    );
    assert!(
        !h.secrets.entries.lock().is_empty(),
        "un intento rechazado no debe revocar el token"
    );
    h.fake.resume_load.notify_one();

    let mut job = h
        .bridge
        .get_job(&broker(), &receipt.job_id)
        .await
        .unwrap_or_else(|error| panic!("get job after release: {error}"));
    for _ in 0..100 {
        if job.state != JobPhase::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        job = h
            .bridge
            .get_job(&broker(), &receipt.job_id)
            .await
            .unwrap_or_else(|error| panic!("poll job after release: {error}"));
    }
    assert_eq!(job.state, JobPhase::Completed);
    let active = h
        .bridge
        .get_lease(&broker(), lease_id)
        .await
        .unwrap_or_else(|error| panic!("get active lease: {error}"));
    assert_eq!(active.state, LeaseState::Active);
    h.bridge
        .release_lease(&broker(), lease_id, active.row_version)
        .await
        .unwrap_or_else(|error| panic!("release active lease: {error}"));
    let released = h
        .bridge
        .get_lease(&broker(), lease_id)
        .await
        .unwrap_or_else(|error| panic!("get released lease: {error}"));
    assert_eq!(released.state, LeaseState::Released);
    assert!(h.secrets.entries.lock().is_empty());
}

#[tokio::test]
async fn unrenewed_lease_expires_after_grace_and_loses_its_token() {
    let h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    let start = Instant::now();
    h.bridge.sweep_at(start + Duration::from_secs(121));
    assert_eq!(
        h.bridge
            .get_lease(&broker(), &lease.id)
            .await
            .map(|l| l.state)
            .ok(),
        Some(LeaseState::Expiring)
    );
    let expiring = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|error| panic!("get expiring lease: {error}"));
    let rejected = h
        .bridge
        .release_lease(&broker(), &lease.id, expiring.row_version)
        .await
        .err();
    assert_eq!(
        rejected.map(|error| error.code),
        Some("INVALID_STATE_TRANSITION".into())
    );
    assert!(
        !h.secrets.entries.lock().is_empty(),
        "la lease conserva su token durante la gracia de expiración"
    );
    let gateway = h
        .bridge
        .gateway(&lease.id, None, "chat/completions", json!({}))
        .await
        .err();
    assert_eq!(gateway.map(|e| e.code), Some("AUTH_REQUIRED".into()));
    h.bridge.sweep_at(start + Duration::from_secs(152));
    assert_eq!(
        h.bridge
            .get_lease(&broker(), &lease.id)
            .await
            .map(|l| l.state)
            .ok(),
        Some(LeaseState::Expired)
    );
    assert!(h.secrets.entries.lock().is_empty());
}

#[tokio::test]
async fn gateway_serves_only_the_leased_variant_with_its_token() {
    let h = harness(BIG_BUDGET).await;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    let reference = lease
        .endpoint
        .credential_reference
        .clone()
        .unwrap_or_default();
    let credential = h
        .bridge
        .endpoint_credential(&broker(), &lease.id, lease.generation, &reference)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let wrong = h
        .bridge
        .gateway(&lease.id, Some("x"), "chat/completions", json!({}))
        .await
        .err();
    assert_eq!(wrong.map(|e| e.code), Some("AUTH_REQUIRED".into()));
    let streaming = h
        .bridge
        .gateway(
            &lease.id,
            Some(&credential.token),
            "chat/completions",
            json!({"stream": true}),
        )
        .await
        .err();
    assert_eq!(
        streaming.map(|e| e.code),
        Some("UNSUPPORTED_CAPABILITY".into())
    );
    let served = h
        .bridge
        .gateway(
            &lease.id,
            Some(&credential.token),
            "chat/completions",
            json!({"model": "otro:70b", "messages": []}),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(served.status, 200);
    assert_eq!(
        h.fake.chats.lock()[0]["model"],
        MODEL,
        "el modelo lo fija la lease"
    );
    let foreign_ref = h
        .bridge
        .endpoint_credential(&broker(), &lease.id, lease.generation, "otra-ref")
        .await
        .err();
    assert_eq!(foreign_ref.map(|e| e.code), Some("FORBIDDEN".into()));
    let record = h
        .bridge
        .get_lease(&broker(), &lease.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    h.bridge
        .release_lease(&broker(), &lease.id, record.row_version)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let after = h
        .bridge
        .gateway(
            &lease.id,
            Some(&credential.token),
            "chat/completions",
            json!({}),
        )
        .await
        .err();
    assert_eq!(
        after.map(|e| e.code),
        Some("INVALID_STATE_TRANSITION".into())
    );
}

#[tokio::test]
async fn cpu_fallback_of_the_runtime_is_reported_not_hidden() {
    let h = harness(BIG_BUDGET).await;
    *h.fake.offload_to_cpu.lock() = true;
    h.fake.loaded.lock().push(MODEL.into());
    let lease = active_lease(&h).await;
    assert_eq!(
        lease.execution_fingerprint.components["backend"]
            .value
            .as_deref(),
        Some("CPU")
    );
}

#[tokio::test]
async fn secret_store_failure_fails_the_lease() {
    let h = harness(BIG_BUDGET).await;
    *h.secrets.fail.lock() = true;
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "k")
        .await
        .err();
    assert_eq!(
        error.map(|e| e.code),
        Some("AUTH_BACKEND_UNAVAILABLE".into())
    );
    let leases = h
        .bridge
        .list_leases(&broker())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(leases.items[0].state, LeaseState::Failed);
}

#[tokio::test]
async fn unreachable_runtime_is_a_retryable_upstream_error() {
    let (config, variant_id, profile_id) = config("http://127.0.0.1:9", BIG_BUDGET, true);
    let bridge = ExternalAdoptionBridge::new(config, Arc::new(MemorySecrets::default()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(bridge
        .catalog()
        .await
        .unwrap_or_else(|e| panic!("{e}"))
        .items
        .is_empty());
    let h = Harness {
        bridge,
        fake: Arc::new(FakeOllama::default()),
        secrets: Arc::new(MemorySecrets::default()),
        variant_id,
        profile_id,
    };
    let error = h
        .bridge
        .create_lease(&broker(), lease_request(&h), "k")
        .await
        .err()
        .unwrap_or_else(|| panic!("debía fallar"));
    assert!(matches!(
        error.code.as_str(),
        "UPSTREAM_UNAVAILABLE" | "TIMEOUT"
    ));
    assert!(error.retryable);
}

#[test]
fn kv_geometry_is_used_only_when_published() {
    let info = json!({"general.architecture": "llama", "llama.block_count": 2, "llama.attention.head_count_kv": 4,
                      "llama.attention.key_length": 8, "llama.attention.value_length": 8});
    assert_eq!(kv_bytes_per_token(Some(&info)), Some(2 * 4 * 16 * 2));
    let partial = json!({"general.architecture": "llama", "llama.block_count": 2});
    assert_eq!(kv_bytes_per_token(Some(&partial)), None);
    assert_eq!(kv_bytes_per_token(None), None);
}

#[test]
fn backend_is_read_from_the_runtime_report() {
    let ps = |vram: u64| vec![json!({"name": MODEL, "size": 100, "size_vram": vram})];
    assert_eq!(
        backend_from_ps(&ps(100), MODEL).value.as_deref(),
        Some("GPU")
    );
    assert_eq!(backend_from_ps(&ps(0), MODEL).value.as_deref(), Some("CPU"));
    assert_eq!(
        backend_from_ps(&ps(40), MODEL).value.as_deref(),
        Some("HYBRID")
    );
    assert_eq!(
        backend_from_ps(&[], MODEL).status,
        EvidenceStatus::Unavailable
    );
}

#[test]
fn daemon_generation_is_shared_with_the_adoption_bridge() {
    let (cfg, _, _) = config("http://127.0.0.1:11434", BIG_BUDGET, true);
    let generation = 987_654_321;
    let bridge = ExternalAdoptionBridge::new_in_generation(
        cfg,
        Arc::new(MemorySecrets::default()),
        generation,
    )
    .unwrap_or_else(|e| panic!("puente: {e}"));
    assert_eq!(bridge.inner.generation, generation);

    let (cfg, _, _) = config("http://127.0.0.1:11434", BIG_BUDGET, true);
    assert!(
        ExternalAdoptionBridge::new_in_generation(cfg, Arc::new(MemorySecrets::default()), 0,)
            .is_err()
    );
}

#[test]
fn config_validation_rejects_lan_and_bad_ids() {
    let (mut cfg, _, _) = config("http://127.0.0.1:11434", BIG_BUDGET, true);
    assert!(cfg.validate().is_ok());
    cfg.base_url = "http://192.168.1.10:11434".into();
    assert!(cfg.validate().is_err());
    let (mut cfg, _, _) = config("http://127.0.0.1:11434", BIG_BUDGET, true);
    cfg.variants[0].variant_id = uuid::Uuid::new_v4().to_string();
    assert!(cfg.validate().is_err());
    let (mut cfg, _, _) = config("http://localhost:11434", BIG_BUDGET, true);
    cfg.memory_budget_bytes = "01".into();
    assert!(cfg.validate().is_err());
    assert!(is_loopback_http("http://[::1]:8790/x"));
    assert!(!is_loopback_http("https://127.0.0.1"));
}

#[tokio::test]
async fn leases_reconciliadas_siguen_disponibles_en_get_y_list() {
    let (_dir, db) = larm_db::testing::temp_migrated()
        .await
        .unwrap_or_else(|error| panic!("db: {error}"));
    let repository = larm_db::runtime_leases::SqlRuntimeLeases::new(db.pool);
    let mut first_persisted = None;
    for index in 0..201 {
        let now = timestamp(chrono::Utc::now());
        let persisted = RuntimeLease {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::from_iter([(
                "job_id".into(),
                json!(uuid::Uuid::now_v7().to_string()),
            )]),
            owner_id: "ai-broker".into(),
            request_id: uuid::Uuid::now_v7().to_string(),
            instance_id: None,
            generation: 1,
            state: LeaseState::Requested,
            expires_at: None,
            endpoint_id: None,
            constraints: Constraints {
                runtime_version_id: None,
                backend_id: None,
                execution_fingerprint: None,
                allow_backend_fallback: false,
                max_memory_bytes: None,
            },
        };
        let context = larm_domain::ports::MutationContext {
            actor_id: persisted.owner_id.clone(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let job_id = persisted.metadata["job_id"].as_str().unwrap_or_default();
        let initial_response = CreateLeaseOutcome {
            status: 202,
            body: json!({
                "job_id": job_id,
                "status_url": format!("/api/v1/jobs/{job_id}"),
                "state": "QUEUED"
            }),
        };
        repository
            .create_requested(
                &persisted,
                &"a".repeat(64),
                &format!("durable-key-{index}"),
                &initial_response,
                &context,
            )
            .await
            .unwrap_or_else(|error| panic!("create persisted lease: {error}"));
        if index == 0 {
            first_persisted = Some(persisted);
        }
    }
    let persisted = first_persisted.unwrap_or_else(|| panic!("expected durable lease"));
    let recovered = repository
        .expire_unreconciled(
            2,
            &larm_domain::ports::MutationContext {
                actor_id: "larm:lease-recovery".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("recover persisted lease: {error}"));
    assert_eq!(recovered.len(), 201);
    assert!(recovered
        .iter()
        .all(|lease| lease.state == LeaseState::Expired));

    // Una instancia nueva del puente no tiene datos en memoria; GET y LIST
    // deben exponer el resultado durable de la reconciliación del daemon.
    let mut h = harness(BIG_BUDGET).await;
    h.bridge = h.bridge.with_lease_repository(Arc::new(repository));
    let loaded = h
        .bridge
        .get_lease(&broker(), &persisted.id)
        .await
        .unwrap_or_else(|error| panic!("get persisted lease: {error}"));
    assert_eq!(loaded.state, LeaseState::Expired);
    assert_eq!(loaded.row_version, 2);

    let page = h
        .bridge
        .list_leases(&broker())
        .await
        .unwrap_or_else(|error| panic!("list persisted leases: {error}"));
    assert_eq!(page.items.len(), 201);
    assert_eq!(page.items[0].id, persisted.id);
    assert_eq!(page.items[0].state, LeaseState::Expired);

    let mut in_memory = loaded.clone();
    in_memory.state = LeaseState::Failed;
    in_memory.row_version += 1;
    in_memory.updated_at = timestamp(chrono::Utc::now());
    h.bridge.inner.state.lock().leases.insert(
        in_memory.id.clone(),
        LeaseEntry {
            record: in_memory,
            variant_id: h.variant_id.clone(),
            model: MODEL.into(),
            credential_reference: uuid::Uuid::now_v7().to_string(),
            token: "memory-only-test-token".into(),
            reserved_bytes: 0,
            expires: None,
            grace_until: None,
            response: None,
        },
    );
    let overlaid = h
        .bridge
        .get_lease(&broker(), &persisted.id)
        .await
        .unwrap_or_else(|error| panic!("get in-memory override: {error}"));
    assert_eq!(overlaid.state, LeaseState::Failed);

    let merged = h
        .bridge
        .list_leases(&broker())
        .await
        .unwrap_or_else(|error| panic!("list merged leases: {error}"));
    assert_eq!(merged.items.len(), 201);
    assert_eq!(
        merged
            .items
            .iter()
            .find(|lease| lease.id == persisted.id)
            .map(|lease| lease.state),
        Some(LeaseState::Failed)
    );

    let other = Principal::new(
        "0192aaaa-0000-7000-8000-000000000004",
        "otro-broker",
        larm_domain::auth::PrincipalKind::Broker,
        [ApiScope::Read, ApiScope::LeasesManage],
        [],
    )
    .unwrap_or_else(|error| panic!("other principal: {error}"));
    let hidden = h.bridge.get_lease(&other, &persisted.id).await;
    assert_eq!(
        hidden.err().map(|error| error.code),
        Some("FORBIDDEN".into())
    );
    let other_page = h
        .bridge
        .list_leases(&other)
        .await
        .unwrap_or_else(|error| panic!("other list: {error}"));
    assert!(other_page.items.is_empty());
}
