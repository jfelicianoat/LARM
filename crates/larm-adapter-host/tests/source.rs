//! Host P021 + permisos/firma P022 + SourceHandler en un proceso real.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ed25519_dalek::SigningKey;
use larm_adapter_host::policy::{GuardedWorker, Policy, WhoAmI};
use larm_adapter_host::source::SourceWorker;
use larm_adapter_host::worker::{HostConfig, TreeKill, WorkerSpec, WorkerState};
use larm_adapter_sdk::manifest::{AdapterManifest, Family, MethodTable};
use larm_adapter_sdk::permissions::{package_hash, sign, Access, PinnedKey, RootMap, TrustStore};
use larm_adapter_sdk::source::*;
use larm_domain::error::ErrorCode;
use larm_download::transfer::{
    StagingStore, TransferControl, TransferLimits, TransferOutcome, TransferProgressSink,
    TransferRequest,
};
use serde_json::{json, Value};
use sha2::Digest;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct ContinueProgress;

#[async_trait::async_trait]
impl TransferProgressSink for ContinueProgress {
    async fn checkpoint(
        &self,
        _job_id: &str,
        _artifact_id: &str,
        _durable_offset: u64,
        _resume_validator: Option<&SourceDownloadResumeValidator>,
    ) -> io::Result<TransferControl> {
        Ok(TransferControl::Continue)
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json")).unwrap()
}
fn request(method: &str) -> SourceRequest {
    SourceRequest::parse(method, fixture()["requests"][method].clone()).unwrap()
}
fn search(query: &str) -> SourceRequest {
    let SourceRequest::Search(mut q) = request("source.search") else {
        panic!("search");
    };
    q.q = query.into();
    SourceRequest::Search(q)
}
struct Package {
    _dir: tempfile::TempDir,
    path: PathBuf,
    manifest: AdapterManifest,
    support: SearchSupport,
    policy: Policy,
}
impl Package {
    fn new(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg");
        std::fs::create_dir_all(path.join("bin")).unwrap();
        let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/examples/fake_source.exe");
        assert!(
            executable.exists(),
            "falta cargo build -p larm-adapter-host --example fake_source"
        );
        std::fs::copy(executable, path.join("bin/fake_source.exe")).unwrap();
        let mut support: SearchSupport =
            serde_json::from_value(fixture()["support"].clone()).unwrap();
        let capabilities = if mode == "import-only" {
            support.search_applicable = false;
            support.supported_filters.clear();
            vec!["source.sync"]
        } else {
            vec!["source.search", "source.sync"]
        };
        let mut methods = vec!["core.handshake", "core.cancel", "core.heartbeat"];
        let table = MethodTable::contract();
        methods.extend(table.required_for(Family::Source));
        if mode == "chunks" {
            methods.push("source.download_chunk");
        }
        let mut manifest = AdapterManifest::parse(&json!({
            "id":"fixture.source","version":"1.0.0","family":"source","protocol_version":"1.0","publisher":"LARM source fixture",
            "executable":{"relative_path":"bin/fake_source.exe","args":["--manifest",path.join("manifest.json").display().to_string(),"--mode",mode]},
            "platforms":["windows"],"architectures":["x86_64"],"methods":methods,
            "permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
            "capabilities":capabilities,"package_sha256":package_hash(&path,&["manifest.json"]).unwrap(),"signature":null,"installation_strategies":[]
        }).to_string()).unwrap();
        let key = SigningKey::from_bytes(&[43u8; 32]);
        manifest.signature = Some(sign(&manifest, "source-fixture-key", &key).unwrap());
        std::fs::write(
            path.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let policy = Policy {
            trust: TrustStore {
                keys: vec![PinnedKey {
                    key_id: "source-fixture-key".into(),
                    publisher: manifest.publisher.clone(),
                    public_key: hex::encode(key.verifying_key().to_bytes()),
                    official: true,
                    revoked: false,
                }],
                revoked_packages: vec![],
            },
            roots: RootMap(vec![]),
            elevation: Arc::new(WhoAmI),
        };
        Self {
            _dir: dir,
            path,
            manifest,
            support,
            policy,
        }
    }
    async fn start(&self, generation: u64) -> Arc<SourceWorker> {
        let authorization = self
            .policy
            .authorize(&self.path, &self.manifest, false, None, chrono::Utc::now())
            .unwrap();
        let guard = GuardedWorker::start(
            WorkerSpec {
                package_dir: self.path.clone(),
                manifest: self.manifest.clone(),
                environment: vec![],
                work_dir: self.path.parent().unwrap().join("work"),
            },
            &authorization,
            HostConfig {
                handshake_timeout: Duration::from_secs(10),
                rpc_timeout: Duration::from_secs(3),
                cancel_grace: Duration::from_millis(500),
                ..HostConfig::default()
            },
            generation,
            Arc::new(TreeKill),
        )
        .await
        .unwrap();
        Arc::new(
            SourceWorker::new(
                Arc::new(guard),
                fixture()["source_id"].as_str().unwrap().into(),
                self.support.clone(),
            )
            .unwrap(),
        )
    }
}
async fn stats(worker: &SourceWorker) -> Value {
    let SourceRequest::Sync(mut query) = request("source.sync") else {
        panic!("sync");
    };
    query.query.insert("stats".into(), json!(true));
    let SourceReply::Sync(response) = worker
        .call(SourceRequest::Sync(query), &Access::default(), None)
        .await
        .unwrap()
    else {
        panic!("sync response");
    };
    Value::Object(response.models[0].limits.clone())
}

#[tokio::test]
async fn paginas_y_metadata_parcial_por_worker_real() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    let SourceRequest::Search(query) = request("source.search") else {
        panic!("query");
    };
    let mut pagination = SourcePagination::new(query, "a".repeat(64)).unwrap();
    let mut ids = vec![];
    while let Some(query) = pagination.next_request(&"a".repeat(64)).unwrap() {
        let SourceReply::Search(page) = worker
            .call(
                SourceRequest::Search(query.clone()),
                &Access::default(),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("page");
        };
        let metadata = SourceMetadata::read(&page.items[0].metadata)
            .unwrap()
            .unwrap();
        assert_eq!(
            metadata.original["unknown_vendor"]["added"],
            json!([1, true, null])
        );
        assert_eq!(metadata.normalized["architecture"], Value::Null);
        assert!(
            page.items[0].architecture.is_none()
                && page.items[0].model_limits.parameter_count.is_none()
        );
        ids.push(page.items[0].id.clone());
        pagination.accept(&query, &page).unwrap();
    }
    assert_eq!(
        ids,
        [
            "0192aaaa-0000-7000-8000-000000000011",
            "0192aaaa-0000-7000-8000-000000000012"
        ]
    );
    for method in [
        "source.get_model",
        "source.list_revisions",
        "source.get_manifest",
        "source.estimate_size",
        "source.auth_probe",
    ] {
        assert_eq!(
            worker
                .call(request(method), &Access::default(), None)
                .await
                .unwrap()
                .value()
                .unwrap(),
            fixture()["responses"][method]
        );
    }
    worker.stop().await;
    assert_eq!(worker.state(), WorkerState::Stopped);
}

#[tokio::test]
async fn bloques_de_descarga_solo_en_adaptadores_que_lo_anuncian_y_con_source_fijado() {
    let package = Package::new("chunks");
    let worker = package.start(20).await;
    assert!(worker.supports_download_chunks());
    let SourceRequest::DownloadChunk(chunk_req) = request("source.download_chunk") else {
        panic!("chunk request");
    };
    let SourceReply::DownloadChunk(chunk) = worker
        .call(
            SourceRequest::DownloadChunk(chunk_req.clone()),
            &Access::default(),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("chunk response");
    };
    assert_eq!(chunk.offset, chunk_req.offset);
    assert_eq!(chunk.decoded_bytes().unwrap(), b"abc");

    let directory = tempfile::tempdir().unwrap();
    let store = StagingStore::new(directory.path().to_path_buf());
    let expected_hash = hex::encode(sha2::Sha256::digest(b"abc"));
    let staged = store
        .write_source_chunks(
            TransferRequest {
                job_id: &chunk_req.job_id,
                artifact_id: "0192aaaa-0000-7000-8000-000000000061",
                resume_offset: 0,
                limits: TransferLimits {
                    expected_size: Some(3),
                    max_bytes: 3,
                },
                expected_sha256: Some(&expected_hash),
            },
            chunk_req.clone(),
            |request| async {
                match worker
                    .call(
                        SourceRequest::DownloadChunk(request),
                        &Access::default(),
                        None,
                    )
                    .await?
                {
                    SourceReply::DownloadChunk(chunk) => Ok(chunk),
                    _ => Err(larm_domain::error::LarmError::new(
                        ErrorCode::ProtocolError,
                        "La respuesta no es un bloque",
                        "source_response_mismatch",
                    )),
                }
            },
            &ContinueProgress,
        )
        .await
        .unwrap();
    let TransferOutcome::Completed(artifact) = staged else {
        panic!("expected completed staged artifact");
    };
    assert_eq!(std::fs::read(artifact.path).unwrap(), b"abc");
    assert_eq!(artifact.trust, larm_domain::model::Trust::Verified);

    let mut other_source = chunk_req;
    other_source.source_id = "0192aaaa-0000-7000-8000-000000000002".into();
    other_source.revision.source_id = other_source.source_id.clone();
    assert_eq!(
        worker
            .call(
                SourceRequest::DownloadChunk(other_source),
                &Access::default(),
                None,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    worker.close().await.unwrap();
    assert!(worker.io_drained());

    let package = Package::new("normal");
    let worker = package.start(21).await;
    assert!(!worker.supports_download_chunks());
    assert_eq!(
        worker
            .call(request("source.download_chunk"), &Access::default(), None,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    worker.stop().await;
}

#[tokio::test]
async fn permisos_filtros_identidad_e_import_only_no_llegan_al_callback() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    let before = stats(&worker).await["invocations"].as_u64().unwrap();
    let SourceRequest::Search(mut q) = search("llama") else {
        panic!("q");
    };
    q.capabilities.push("text.generate".into());
    assert_eq!(
        worker
            .call(SourceRequest::Search(q.clone()), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    q.capabilities.clear();
    q.source_id = "0192aaaa-0000-7000-8000-000000000099".into();
    assert_eq!(
        worker
            .call(SourceRequest::Search(q), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        worker
            .call(
                search("llama"),
                &Access {
                    urls: vec!["https://not-allowed.invalid".into()],
                    ..Access::default()
                },
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        stats(&worker).await["invocations"].as_u64().unwrap(),
        before + 1,
        "sólo la nueva consulta de stats llegó al proceso"
    );
    worker.stop().await;
    let package = Package::new("import-only");
    let worker = package.start(2).await;
    assert_eq!(
        worker
            .call(search(""), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    assert!(worker
        .call(request("source.get_model"), &Access::default(), None)
        .await
        .is_ok());
    worker.stop().await;
}

#[tokio::test]
async fn rate_auth_plan_y_respuesta_invalida_por_proceso_real() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    let error = worker
        .call(search("rate"), &Access::default(), None)
        .await
        .unwrap_err();
    let rate = RateLimit::from_error(&error).unwrap();
    assert_eq!(rate.retry_after_seconds.unwrap().get(), 900);
    assert_eq!(rate.retry_delay().unwrap().as_secs(), 60);
    let SourceReply::DownloadPlan(plan) = worker
        .call(request("source.download_plan"), &Access::default(), None)
        .await
        .unwrap()
    else {
        panic!("plan");
    };
    let spec = download_spec(&plan).unwrap();
    assert_eq!(spec.download_bytes().unwrap().unwrap().get(), 18);
    assert!(!spec.resumable);
    assert_eq!(plan.plan_hash, plan_hash(&plan).unwrap());
    let SourceRequest::AuthProbe(mut auth) = request("source.auth_probe") else {
        panic!("auth");
    };
    let SourceReply::AuthProbe(public_probe) = worker
        .call(
            SourceRequest::AuthProbe(auth.clone()),
            &Access::default(),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("auth probe reply");
    };
    assert!(!public_probe.authenticated);
    assert_eq!(public_probe.reason, "public_fixture");
    auth.auth_mode = AuthMode::Bearer;
    auth.account_id = Some("0192aaaa-0000-7000-8000-000000000098".into());
    auth.credential_reference_id = Some("0192aaaa-0000-7000-8000-000000000099".into());
    assert_eq!(
        worker
            .call(SourceRequest::AuthProbe(auth), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::AuthRequired
    );
    assert_eq!(
        worker
            .call(search("bad-reply"), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProtocolError
    );
    assert_eq!(
        worker
            .call(request("source.download"), &Access::default(), None)
            .await
            .unwrap_err()
            .details
            .reason,
        "fixture_read_only"
    );
    assert_eq!(worker.state(), WorkerState::Ready);
    worker.stop().await;
    let package = Package::new("raw-response");
    let worker = package.start(2).await;
    assert_eq!(
        worker
            .call(search("llama"), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProtocolError
    );
    worker.stop().await;
}

#[tokio::test]
async fn timeout_y_cancelacion_cooperativa_conservan_sesion() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    assert_eq!(
        worker
            .call(
                search("slow"),
                &Access::default(),
                Some(Duration::from_millis(100))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    assert_eq!(worker.state(), WorkerState::Ready);
    let id = uuid::Uuid::now_v7().to_string();
    let cloned = worker.clone();
    let request_id = id.clone();
    let call = tokio::spawn(async move {
        cloned
            .call_with_id(&request_id, search("slow"), &Access::default(), None)
            .await
    });
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    while stats(&worker).await["slow_active"] != json!(true) {
        assert!(tokio::time::Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(worker.cancel(&id).await.unwrap());
    assert_eq!(
        call.await.unwrap().unwrap_err().details.reason,
        "operation_cancelled"
    );
    assert!(!worker.cancel(&id).await.unwrap());
    assert_eq!(worker.state(), WorkerState::Ready);
    worker.stop().await;
}

#[tokio::test]
async fn crash_y_cancel_no_atendido_exigen_proceso_nuevo() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    assert_eq!(
        worker
            .call(search("crash"), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::AdapterCrashed
    );
    assert_eq!(worker.state(), WorkerState::Failed);
    worker.stop().await;
    let restarted = package.start(2).await;
    assert!(restarted
        .call(search("llama"), &Access::default(), None)
        .await
        .is_ok());
    restarted.stop().await;
    let package = Package::new("deaf");
    let worker = package.start(3).await;
    assert_eq!(
        worker
            .call(
                search("slow"),
                &Access::default(),
                Some(Duration::from_millis(100))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    assert_eq!(worker.state(), WorkerState::Failed);
    worker.stop().await;
}

#[tokio::test]
async fn id_rpc_duplicado_no_roba_respuesta_ni_cancelacion() {
    let package = Package::new("normal");
    let worker = package.start(1).await;
    let id = uuid::Uuid::now_v7().to_string();
    let cloned = worker.clone();
    let original_id = id.clone();
    let original = tokio::spawn(async move {
        cloned
            .call_with_id(&original_id, search("slow"), &Access::default(), None)
            .await
    });
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    while stats(&worker).await["slow_active"] != json!(true) {
        assert!(tokio::time::Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let error = worker
        .call_with_id(&id, search("llama"), &Access::default(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.details.reason, "request_id_in_use");
    assert!(worker.cancel(&id).await.unwrap());
    assert_eq!(
        original.await.unwrap().unwrap_err().details.reason,
        "operation_cancelled"
    );
    assert_eq!(worker.state(), WorkerState::Ready);
    assert!(worker
        .call(search("llama"), &Access::default(), None)
        .await
        .is_ok());
    worker.stop().await;
    assert_eq!(
        worker
            .call(search("llama"), &Access::default(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceUnavailable
    );
}
