//! Qualified source metadata -> P041 atomic ingestion -> federated candidates.
//! The configured source executes as its approved backend actor (as watchers
//! do). This does not grant callers a new execution or credential permission.
use async_trait::async_trait;
use futures::{stream, StreamExt};
use larm_adapter_host::conformance::{
    profiles::{LocalProfile, Recipe},
    NativeConformanceRunner,
};
use larm_adapter_sdk::{
    manifest::AdapterManifest,
    permissions::{Access, Grants, RootMap},
    source::{RevisionRef, SearchSupport, SourceReply, SourceRequest, SourceSearch, UpstreamRef},
};
use larm_db::sources::{SourceExecutionBinding, SqlSources};
use larm_domain::{
    acquisition::AcquisitionPlanBinding,
    bridge::{timestamp, Page},
    conformance::Run,
    download::DownloadArtifactEstimate,
    error::{ErrorCode, LarmError},
    model::ArtifactSizeConfidence,
    ports::{ConformanceProfileRepository, MutationContext, SourceCatalogRepository},
    search::{CertificationStatus, CompatibilityFilter},
    sources::{AuthMode, SourceFilter},
    units::DecimalU64,
};
use larm_models::{
    acquisition::{
        RegisteredAcquisition, SourceCatalogIngestor, SourceCatalogObservation,
        SourceObservationQualification,
    },
    search::{FederatedSearchProvider, SearchCandidate, SearchSource},
};
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct QualifiedSourceSearch {
    sources: SqlSources,
    conformance: larm_db::conformance::SqlAdapterConformance,
    profiles: larm_db::conformance_profiles::SqlConformanceProfiles,
    runner: Arc<NativeConformanceRunner>,
    ingestor: SourceCatalogIngestor,
    sessions: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
}
struct Ready {
    binding: SourceExecutionBinding,
    run: Run,
    context: MutationContext,
    support: SearchSupport,
    access: Access,
}
struct ObservedPage {
    items: Vec<SourceCatalogObservation>,
    next_cursor: Option<String>,
    snapshot_at: String,
}

fn phase(source: &str, name: &str, started: std::time::Instant) {
    tracing::debug!(target:"larm_daemon::source_search",source_id=source,phase=name,
        elapsed_ms=started.elapsed().as_millis() as u64,"source metadata phase");
}

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::UpstreamUnavailable,
        "No se pudo consultar la fuente",
        reason,
    )
    .retryable(true)
}
fn unsupported(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::UnsupportedCapability,
        "La fuente no admite esta búsqueda",
        reason,
    )
}
fn protocol() -> LarmError {
    LarmError::new(
        ErrorCode::ProtocolError,
        "Respuesta de catálogo incoherente",
        "source_search_reply_mismatch",
    )
}
fn context(run: &Run) -> MutationContext {
    MutationContext {
        actor_id: run.actor_id.clone(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn support(binding: &larm_domain::sources::ModelSourceAdapter) -> Result<SearchSupport, LarmError> {
    let support: SearchSupport = serde_json::from_value(
        binding
            .metadata
            .get("source_protocol_v1")
            .cloned()
            .ok_or_else(|| unavailable("source_search_protocol_binding_missing"))?,
    )
    .map_err(|_| unavailable("source_search_protocol_binding_invalid"))?;
    support.validate()?;
    Ok(support)
}
fn normalized_url(value: &str) -> Result<url::Url, LarmError> {
    let url = url::Url::parse(value).map_err(|_| unavailable("source_search_url_invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(unavailable("source_search_url_invalid"));
    }
    Ok(url)
}
fn search_access(
    binding: &SourceExecutionBinding,
    profile: &LocalProfile,
) -> Result<Access, LarmError> {
    if binding.source.auth_mode != AuthMode::None || binding.source.account_id.is_some() {
        return Err(LarmError::new(
            ErrorCode::AuthRequired,
            "La búsqueda todavía no resuelve las credenciales de esta fuente",
            "source_search_auth_unavailable",
        ));
    }
    let (source_id, base) = match &profile.suite {
        Recipe::Source(config) => (config.source_id.as_str(), config.base_url.as_str()),
        Recipe::SourceHttp(config) => (config.source_id.as_str(), config.url.as_str()),
        _ => return Err(unsupported("source_search_profile_binding_unavailable")),
    };
    if source_id != binding.source.id
        || normalized_url(base)? != normalized_url(&binding.source.base_url)?
    {
        return Err(LarmError::new(
            ErrorCode::StaleRevision,
            "El perfil no corresponde a esta fuente",
            "source_search_profile_binding_mismatch",
        ));
    }
    let manifest = AdapterManifest::parse(&binding.installation.manifest.to_string())?;
    for method in ["source.search", "source.get_model", "source.get_manifest"] {
        if !manifest.methods.iter().any(|m| m == method) {
            return Err(unsupported("source_search_method_unavailable"));
        }
    }
    let grants = Grants::resolve(&manifest.permissions, &RootMap(profile.roots.clone()));
    let origin = normalized_url(base)?.origin().ascii_serialization();
    // Grants uses explicit default ports. Compare normalized URL origins.
    if !grants
        .network_origins
        .iter()
        .any(|o| normalized_url(o).is_ok_and(|u| u.origin().ascii_serialization() == origin))
    {
        return Err(LarmError::new(
            ErrorCode::Forbidden,
            "El origen no está concedido al adaptador",
            "source_search_origin_not_declared",
        ));
    }
    if grants.read_roots.len() != manifest.permissions.read_roots.len()
        || grants.write_roots.len() != manifest.permissions.write_roots.len()
    {
        return Err(unavailable("source_search_roots_unavailable"));
    }
    Ok(Access {
        reads: grants.read_roots,
        writes: grants.write_roots,
        urls: grants.network_origins.into_iter().collect(),
        secrets: vec![],
        processes: vec![],
    })
}

impl QualifiedSourceSearch {
    pub fn new(db: &larm_db::pool::Db, runner: Arc<NativeConformanceRunner>) -> Self {
        Self {
            sources: SqlSources {
                pool: db.pool.clone(),
                qualification: Some(runner.clone()),
            },
            conformance: larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            },
            profiles: larm_db::conformance_profiles::SqlConformanceProfiles {
                pool: db.pool.clone(),
            },
            ingestor: SourceCatalogIngestor {
                catalog: larm_db::catalog_acquisition::SqlCatalogAcquisitions {
                    pool: db.pool.clone(),
                    qualification: Some(runner.clone()),
                },
            },
            runner,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    async fn ready(&self, id: &str) -> Result<Ready, LarmError> {
        let started = std::time::Instant::now();
        let binding = self.sources.execution_binding(id).await?;
        let support = support(&binding.adapter)?;
        if !support.search_applicable {
            return Err(unsupported("source_search_import_only"));
        }
        let (installation, run) = self
            .conformance
            .current_for_consumer(&binding.installation.id)
            .await?
            .ok_or_else(|| unavailable("source_search_qualification_missing"))?;
        if installation != binding.installation {
            return Err(unavailable("source_search_qualification_stale"));
        }
        let profile = self
            .profiles
            .conformance_profile(&installation.id)
            .await?
            .ok_or_else(|| unavailable("source_search_profile_missing"))?;
        let local: LocalProfile = serde_json::from_value(profile.configuration)
            .map_err(|_| unavailable("source_search_profile_invalid"))?;
        let access = search_access(&binding, &local)?;
        let context = context(&run);
        self.runner
            .verify_qualification_snapshot(&installation, &run, &context)
            .await?;
        phase(id, "ready", started);
        Ok(Ready {
            binding,
            run,
            context,
            support,
            access,
        })
    }
    async fn descriptor(
        &self,
        source: larm_domain::sources::ModelSource,
    ) -> Result<SearchSource, LarmError> {
        let ready = self.ready(&source.id).await;
        let (priority, support, tag) = match ready {
            Ok(r) => (
                r.binding.source.priority,
                r.support,
                larm_domain::canonical::sha256_hex(&json!({
                "source_id":r.binding.source.id,"revision":r.binding.source.row_version,
                "configuration":r.binding.source.configuration(),"adapter":r.binding.adapter,
                "installation":r.binding.installation,"run":r.run.digest()?,"qualified":true}))
                .map_err(|_| unavailable("source_search_cache_binding"))?,
            ),
            Err(error) => {
                let declared =
                    if let Some(adapter) = self.sources.source_adapter(&source.adapter_id).await? {
                        support(&adapter).ok()
                    } else {
                        None
                    };
                let declared = declared.unwrap_or(SearchSupport {
                    search_applicable: false,
                    supported_filters: BTreeSet::new(),
                    limitations: vec!["source_search_protocol_binding_unavailable".into()],
                });
                let tag = larm_domain::canonical::sha256_hex(
                    &json!({"source":source.configuration(),"revision":source.row_version,
                    "unavailable":error.details.reason,"support":declared}),
                )
                .map_err(|_| unavailable("source_search_cache_binding"))?;
                (source.priority, declared, tag)
            }
        };
        Ok(SearchSource {
            id: source.id,
            priority,
            support,
            cache_tag: tag,
        })
    }
    async fn permit(&self, installation: &str) -> Result<OwnedSemaphorePermit, LarmError> {
        let semaphore = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|_, value| value.strong_count() > 0);
            if let Some(value) = sessions.get(installation).and_then(Weak::upgrade) {
                value
            } else {
                let value = Arc::new(Semaphore::new(1));
                sessions.insert(installation.into(), Arc::downgrade(&value));
                value
            }
        };
        semaphore
            .acquire_owned()
            .await
            .map_err(|_| unavailable("source_search_closed"))
    }
}

#[async_trait]
impl FederatedSearchProvider for QualifiedSourceSearch {
    async fn sources(&self, ids: &[String]) -> Result<Vec<SearchSource>, LarmError> {
        let mut sources = Vec::new();
        if ids.is_empty() {
            let mut after = None;
            loop {
                let page = self
                    .sources
                    .page_sources(after, 200, &SourceFilter::default())
                    .await?;
                for source in &page {
                    if source.enabled {
                        sources.push(source.clone());
                    }
                }
                if page.len() < 200 {
                    break;
                }
                after = page.last().map(|s| (s.created_at.clone(), s.id.clone()));
            }
        } else {
            for id in ids {
                if let Some(source) = self.sources.source(id).await? {
                    sources.push(source);
                }
            }
        }
        if sources.is_empty() {
            return Err(unavailable("source_search_sources_unavailable"));
        }
        let mut result = Vec::new();
        for source in sources {
            result.push(self.descriptor(source).await?);
        }
        // Explicit missing IDs must not disappear as a successful empty query.
        for id in ids {
            if !result.iter().any(|s| s.id == *id) {
                result.push(SearchSource {
                    id: id.clone(),
                    priority: 0,
                    cache_tag: "source-missing".into(),
                    support: SearchSupport {
                        search_applicable: false,
                        supported_filters: BTreeSet::new(),
                        limitations: vec!["source_missing".into()],
                    },
                });
            }
        }
        Ok(result)
    }
    async fn search_page(
        &self,
        id: &str,
        request: SourceSearch,
    ) -> Result<Page<SearchCandidate>, LarmError> {
        if request.source_id != id {
            return Err(protocol());
        }
        let ready = self.ready(id).await?;
        ready.support.require(&request)?;
        let original = ready.binding.clone();
        let permit = self.permit(&original.installation.id).await?;
        let runner = self.runner.clone();
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel::<()>();
        // The task owns startup/IO/cleanup even if the HTTP/federation timeout
        // drops its observer. Cancellation never detaches a live child owner.
        let task = tokio::spawn(async move {
            let source_id = ready.binding.source.id.clone();
            let started = std::time::Instant::now();
            let worker = runner
                .source_worker(
                    &ready.binding.installation,
                    &ready.run,
                    &ready.binding.source.id,
                    ready.support.clone(),
                    &ready.context,
                )
                .await?;
            phase(&source_id, "worker_start", started);
            let started = std::time::Instant::now();
            let stop = Arc::new(AtomicBool::new(false));
            let fetched = drain_when_cancelled(
                fetch(&worker, &ready, request, stop.clone()),
                &mut cancelled,
                stop,
            )
            .await;
            phase(&source_id, "fetch", started);
            let started = std::time::Instant::now();
            let closed = worker.close().await;
            drop(worker);
            phase(&source_id, "worker_close", started);
            closed?;
            Ok::<_, LarmError>((fetched?, permit))
        });
        let result = task
            .await
            .map_err(|_| unavailable("source_search_task_failed"));
        drop(cancel);
        let (page, _permit) = result??;
        let current = self.sources.execution_binding(id).await?;
        if current.source.row_version != original.source.row_version
            || current.source.configuration() != original.source.configuration()
            || current.adapter != original.adapter
            || current.installation != original.installation
        {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La fuente cambió durante la búsqueda",
                "source_search_binding_changed",
            ));
        }
        let ctx = context(
            &self
                .conformance
                .current_for_consumer(&current.installation.id)
                .await?
                .ok_or_else(|| unavailable("source_search_qualification_missing"))?
                .1,
        );
        let mut items = Vec::new();
        for observation in page.items {
            let started = std::time::Instant::now();
            let license = observation.inspection.license.clone();
            let registered = self.ingestor.ingest(&observation, &ctx).await?;
            phase(id, "ingest", started);
            items.push(candidate(
                registered,
                id,
                original.source.priority,
                license,
            )?);
        }
        Ok(Page {
            items,
            next_cursor: page.next_cursor,
            snapshot_at: page.snapshot_at,
        })
    }
}

const METADATA_CONCURRENCY: usize = 4;

fn active(stop: &AtomicBool) -> Result<(), LarmError> {
    if stop.load(Ordering::Acquire) {
        Err(unavailable("source_search_cancelled"))
    } else {
        Ok(())
    }
}

/// Dropping the observer requests cancellation; the owner finishes already
/// started qualification/RPC futures before closing the worker and its permit.
async fn drain_when_cancelled<R>(
    operation: impl std::future::Future<Output = Result<R, LarmError>>,
    cancelled: &mut tokio::sync::oneshot::Receiver<()>,
    stop: Arc<AtomicBool>,
) -> Result<R, LarmError> {
    tokio::pin!(operation);
    tokio::select! {biased;
        _=cancelled=> {
            stop.store(true,Ordering::Release);
            let _=operation.await;
            Err(unavailable("source_search_cancelled"))
        }
        result=&mut operation=>result,
    }
}

/// No detached metadata tasks. On error, drain started work and reject queued
/// IO cooperatively, then restore upstream order before identity resolution.
async fn collect_bounded<T, R, F, Fut>(
    items: Vec<T>,
    stop: Arc<AtomicBool>,
    mut observe: F,
) -> Result<Vec<R>, LarmError>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future<Output = Result<R, LarmError>>,
{
    let mut pending = stream::iter(items.into_iter().enumerate())
        .map(|(index, item)| {
            let operation = observe(item);
            async move { (index, operation.await) }
        })
        .buffer_unordered(METADATA_CONCURRENCY);
    let mut values = Vec::new();
    let mut failure = None;
    while let Some((index, result)) = pending.next().await {
        match result {
            Ok(value) if failure.is_none() => values.push((index, value)),
            Ok(_) => {}
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
                stop.store(true, Ordering::Release);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    values.sort_by_key(|(index, _)| *index);
    Ok(values.into_iter().map(|(_, value)| value).collect())
}

async fn fetch(
    worker: &larm_adapter_host::source::SourceWorker,
    ready: &Ready,
    request: SourceSearch,
    stop: Arc<AtomicBool>,
) -> Result<ObservedPage, LarmError> {
    active(&stop)?;
    let started = std::time::Instant::now();
    let SourceReply::Search(page) = worker
        .call(SourceRequest::Search(request), &ready.access, None)
        .await?
    else {
        return Err(protocol());
    };
    phase(&ready.binding.source.id, "search", started);
    active(&stop)?;
    let items = collect_bounded(page.items, stop.clone(), |hit| {
        observe_hit(worker, ready, hit, &stop)
    })
    .await?;
    Ok(ObservedPage {
        items,
        next_cursor: page.next_cursor,
        snapshot_at: page.snapshot_at,
    })
}

async fn observe_hit(
    worker: &larm_adapter_host::source::SourceWorker,
    ready: &Ready,
    hit: larm_domain::model::ModelVariant,
    stop: &AtomicBool,
) -> Result<SourceCatalogObservation, LarmError> {
    active(stop)?;
    let reference = larm_domain::acquisition::catalog_reference(&hit)?
        .ok_or_else(|| unsupported("source_search_reference_missing"))?;
    if reference.source_id != ready.binding.source.id {
        return Err(protocol());
    }
    let started = std::time::Instant::now();
    let SourceReply::GetModel(inspection) = worker
        .call(
            SourceRequest::GetModel(UpstreamRef {
                source_id: reference.source_id.clone(),
                upstream_namespace: reference.upstream_namespace.clone(),
                upstream_model_id: reference.upstream_model_id.clone(),
            }),
            &ready.access,
            None,
        )
        .await?
    else {
        return Err(protocol());
    };
    phase(&ready.binding.source.id, "inspection", started);
    active(stop)?;
    let started = std::time::Instant::now();
    let SourceReply::GetManifest(manifest) = worker
        .call(
            SourceRequest::GetManifest(RevisionRef {
                source_id: reference.source_id.clone(),
                upstream_namespace: reference.upstream_namespace.clone(),
                upstream_model_id: reference.upstream_model_id.clone(),
                revision_key: reference.revision_key.clone(),
            }),
            &ready.access,
            None,
        )
        .await?
    else {
        return Err(protocol());
    };
    phase(&ready.binding.source.id, "manifest", started);
    active(stop)?;
    Ok(SourceCatalogObservation {
        source_revision: ready.binding.source.row_version,
        id: uuid::Uuid::now_v7().to_string(),
        observed_at: timestamp(chrono::Utc::now()),
        reference,
        hit,
        inspection,
        manifest,
        qualification: SourceObservationQualification {
            adapter_installation_id: ready.binding.installation.id.clone(),
            adapter_installation_revision: ready.binding.installation.row_version.to_string(),
            adapter_installation_sha256: larm_domain::conformance::committed_installation_hash(
                &ready.binding.installation,
            )?,
            conformance_run_id: ready.run.id.clone(),
            conformance_run_sha256: ready.run.digest()?,
        },
    })
}

fn candidate(
    registered: RegisteredAcquisition,
    source: &str,
    priority: u64,
    license: Option<String>,
) -> Result<SearchCandidate, LarmError> {
    let mut point = Some(0u64);
    let mut confidence = ArtifactSizeConfidence::Exact;
    let mut artifacts = Vec::new();
    for a in &registered.manifest.artifacts {
        if a.required {
            confidence = match (confidence, a.size_estimate.confidence) {
                (ArtifactSizeConfidence::Unknown, _) | (_, ArtifactSizeConfidence::Unknown) => {
                    ArtifactSizeConfidence::Unknown
                }
                (ArtifactSizeConfidence::Estimated, _) | (_, ArtifactSizeConfidence::Estimated) => {
                    ArtifactSizeConfidence::Estimated
                }
                (ArtifactSizeConfidence::High, _) | (_, ArtifactSizeConfidence::High) => {
                    ArtifactSizeConfidence::High
                }
                _ => ArtifactSizeConfidence::Exact,
            };
            point = match (point, a.size_estimate.bytes) {
                (Some(sum), Some(value)) => Some(
                    sum.checked_add(value.get())
                        .filter(|v| *v <= larm_domain::units::DECIMAL_U64_MAX)
                        .ok_or_else(|| unavailable("source_search_size_overflow"))?,
                ),
                _ => None,
            };
        }
        artifacts.push(DownloadArtifactEstimate {
            artifact_id: a.artifact_id.clone(),
            relative_path: a.relative_path.clone(),
            required: a.required,
            selected: a.required,
            size_estimate: a.size_estimate.clone(),
            sha256: a.sha256.clone(),
        });
    }
    let acquisition = Some(AcquisitionPlanBinding {
        manifest_id: registered.manifest.id.clone(),
        snapshot_hash: registered.manifest.snapshot_hash.clone(),
        upstream_manifest_hash: registered.manifest.upstream_manifest_hash.clone(),
    });
    let variant = registered.variant;
    Ok(SearchCandidate {
        model: registered.model,
        source_id: source.into(),
        source_priority: priority,
        variant_id: variant.id,
        author: None,
        capabilities: variant.capabilities,
        format: variant.format,
        quantization: variant.quantization,
        architecture: variant.architecture,
        parameter_count: variant.model_limits.parameter_count,
        size_bytes: point.and_then(DecimalU64::new),
        size_confidence: confidence,
        artifact_estimates: artifacts,
        acquisition,
        licenses: license.into_iter().collect(),
        runtime_ids: vec![],
        compatibility: CompatibilityFilter::Unknown,
        technical_fit: None,
        trust_score: None,
        certification_status: CertificationStatus::Unknown,
        executable: None,
        recommended: None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use larm_domain::model::ArtifactSizeEstimate;

    struct NoTargets(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl larm_adapter_host::conformance::NativeTargets for NoTargets {
        async fn target(
            &self,
            _: &larm_domain::adapters::AdapterInstallation,
            _: &MutationContext,
        ) -> Result<larm_adapter_host::conformance::NativeTarget, LarmError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(unavailable("fixture_no_target"))
        }
    }
    #[tokio::test]
    async fn default_local_only_catalog_never_opens_remote_worker_or_publishes_models() {
        let (_root, db) = larm_db::testing::temp_migrated().await.unwrap();
        let targets = Arc::new(NoTargets(std::sync::atomic::AtomicUsize::new(0)));
        let runner = Arc::new(NativeConformanceRunner::new(
            Arc::new(larm_adapter_host::policy::Policy {
                trust: Default::default(),
                roots: RootMap(vec![]),
                elevation: Arc::new(larm_adapter_host::policy::WhoAmI),
            }),
            targets.clone(),
            Default::default(),
        ));
        let service = QualifiedSourceSearch::new(&db, runner.clone());
        // Fresh migrations deliberately register the automatic local source.
        let declared = service.sources(&[]).await.unwrap();
        assert!(!declared.is_empty());
        assert!(declared
            .iter()
            .all(|source| !source.support.search_applicable));
        assert_eq!(
            service
                .sources(&[uuid::Uuid::now_v7().to_string()])
                .await
                .unwrap_err()
                .details
                .reason,
            "source_search_sources_unavailable"
        );
        assert_eq!(targets.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        let models: i64 = sqlx::query_scalar("SELECT count(*) FROM canonical_model")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(models, 0);
        runner.shutdown().await.unwrap();
        db.pool.close().await;
    }

    fn binding() -> (SourceExecutionBinding, LocalProfile) {
        let protocol: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json"))
                .unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/fake_adapter/manifest.json"
        ))
        .unwrap();
        let table = larm_adapter_sdk::manifest::MethodTable::contract();
        manifest["family"] = json!("source");
        manifest["methods"] = json!(table
            .mandatory_common
            .iter()
            .cloned()
            .chain(
                table
                    .methods
                    .iter()
                    .filter(|m| m.family == "source" && m.required)
                    .map(|m| m.method.clone())
            )
            .collect::<Vec<_>>());
        manifest["permissions"] = json!({"read_roots":["catalog"],"write_roots":["work"],
            "network_origins":["https://example.invalid"],"secret_refs":[],"process_allowlist":[]});
        let id = "0192aaaa-0000-7000-8000-000000000004";
        let base = "https://example.invalid/catalog";
        let source =
            serde_json::from_value(json!({"id":id,"created_at":"2026-10-28T00:00:00.000Z",
            "updated_at":"2026-10-28T00:00:00.000Z","row_version":1,"metadata":{},"name":"Fixture",
            "adapter_id":id,"base_url":base,"priority":0,"enabled":true,"auth_mode":"NONE",
            "account_id":null,"policy":{}}))
            .unwrap();
        let adapter = serde_json::from_value(json!({"id":id,"created_at":"2026-10-28T00:00:00.000Z",
            "updated_at":"2026-10-28T00:00:00.000Z","row_version":1,"metadata":{"source_protocol_v1":protocol["support"]},
            "adapter_installation_id":id,"source_type":"HTTP","capabilities":[]})).unwrap();
        let installation = serde_json::from_value(json!({"id":id,"created_at":"2026-10-28T00:00:00.000Z",
            "updated_at":"2026-10-28T00:00:00.000Z","row_version":1,"metadata":{},"adapter_id":manifest["id"],
            "version":manifest["version"],"family":"source","package_hash":manifest["package_sha256"],
            "publisher":manifest["publisher"],"trust":"VERIFIED","state":"VALIDATED","manifest":manifest})).unwrap();
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let profile = LocalProfile {
            schema_version: "1.0".into(),
            package_dir: root.clone(),
            work_dir: root.clone(),
            roots: vec![
                ("catalog".into(), root.join("catalog")),
                ("work".into(), root.join("work")),
                ("unrelated".into(), root.join("unrelated")),
            ],
            custom: false,
            capability_profile: None,
            immutable_inputs: vec![],
            timeout_seconds: 30,
            suite: Recipe::Source(larm_adapter_host::conformance::source::SourceConfig {
                source_id: id.into(),
                storage_id: id.into(),
                base_url: base.into(),
                catalog: json!({}),
                expected_files: vec![],
                control: root.join("control"),
                requests: root.join("requests"),
                forbidden: root.join("forbidden"),
                evidence_root: root,
            }),
        };
        (
            SourceExecutionBinding {
                source,
                adapter,
                installation,
            },
            profile,
        )
    }
    #[test]
    fn metadata_access_uses_only_declared_roots_and_origin() {
        let (b, p) = binding();
        let access = search_access(&b, &p).unwrap();
        assert_eq!(access.reads, vec![p.roots[0].1.clone()]);
        assert_eq!(access.writes, vec![p.roots[1].1.clone()]);
        assert_eq!(access.urls.len(), 1);
        assert!(access.secrets.is_empty());
        assert!(access.processes.is_empty());
    }
    #[test]
    fn metadata_access_rejects_auth_and_account_without_resolving_secrets() {
        let (mut b, p) = binding();
        for mode in [AuthMode::Token, AuthMode::Basic, AuthMode::OauthDevice] {
            b.source.auth_mode = mode;
            assert_eq!(
                search_access(&b, &p).unwrap_err().details.reason,
                "source_search_auth_unavailable"
            );
        }
        b.source.auth_mode = AuthMode::None;
        b.source.account_id = Some(b.source.id.clone());
        assert_eq!(
            search_access(&b, &p).unwrap_err().details.reason,
            "source_search_auth_unavailable"
        );
    }
    #[test]
    fn metadata_access_rejects_different_source_url_origin_or_missing_root() {
        let (b, p) = binding();
        let mut wrong = b.clone();
        wrong.source.id = uuid::Uuid::now_v7().to_string();
        assert_eq!(
            search_access(&wrong, &p).unwrap_err().details.reason,
            "source_search_profile_binding_mismatch"
        );
        let mut wrong = b.clone();
        wrong.source.base_url.push_str("/different");
        assert_eq!(
            search_access(&wrong, &p).unwrap_err().details.reason,
            "source_search_profile_binding_mismatch"
        );
        let mut wrong = b.clone();
        wrong.installation.manifest["permissions"]["network_origins"] =
            json!(["https://other.invalid"]);
        assert_eq!(
            search_access(&wrong, &p).unwrap_err().details.reason,
            "source_search_origin_not_declared"
        );
        let mut wrong = p.clone();
        wrong.roots.retain(|(name, _)| name != "work");
        assert_eq!(
            search_access(&b, &wrong).unwrap_err().details.reason,
            "source_search_roots_unavailable"
        );
    }
    fn registered() -> RegisteredAcquisition {
        let manifest: larm_domain::acquisition::ModelAcquisitionManifest = serde_json::from_str(
            include_str!("../../../tests/fixtures/models/acquisition-3.12.0.json"),
        )
        .unwrap();
        let protocol: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json"))
                .unwrap();
        let mut variant: larm_domain::model::ModelVariant =
            serde_json::from_value(protocol["responses"]["source.search"]["items"][0].clone())
                .unwrap();
        variant.id = manifest.variant_id.clone();
        variant.revision_id = manifest.revision_id.clone();
        RegisteredAcquisition {
            model: larm_domain::model::CanonicalModel {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: manifest.observed_at.clone(),
                updated_at: manifest.observed_at.clone(),
                row_version: 1,
                metadata: Default::default(),
                name: "Remote model".into(),
                upstream_identity: None,
                capabilities: variant.capabilities.clone(),
                possible_duplicates: vec![],
            },
            variant,
            manifest,
            report: Default::default(),
        }
    }
    #[test]
    fn typed_projection_preserves_estimated_precision_without_invented_eligibility() {
        let r = registered();
        let c = candidate(r.clone(), &r.manifest.source_id, 4, Some("MIT".into())).unwrap();
        assert_eq!(c.size_bytes.unwrap().get(), 9_007_199_254_740_993);
        assert_eq!(c.size_confidence, ArtifactSizeConfidence::High);
        assert_eq!(c.variant_id, r.variant.id);
        assert_eq!(
            c.acquisition.unwrap().snapshot_hash,
            r.manifest.snapshot_hash
        );
        assert_eq!(c.compatibility, CompatibilityFilter::Unknown);
        assert_eq!(c.certification_status, CertificationStatus::Unknown);
        assert!(
            c.trust_score.is_none()
                && c.technical_fit.is_none()
                && c.executable.is_none()
                && c.recommended.is_none()
        );
        assert!(c.runtime_ids.is_empty());
    }
    #[test]
    fn range_and_unknown_stay_without_point_and_optional_is_not_selected() {
        let mut r = registered();
        let mut optional = r.manifest.artifacts[0].clone();
        optional.artifact_id = uuid::Uuid::now_v7().to_string();
        optional.relative_path = "optional.bin".into();
        optional.required = false;
        r.manifest.artifacts.push(optional);
        r.manifest.artifacts[0].size_estimate=serde_json::from_value(json!({"confidence":"ESTIMATED","bytes":null,
            "lower_bound_bytes":"10","upper_bound_bytes":"20","source":"SOURCE_METADATA","evidence_refs":["fixture:estimate"],"reason":null})).unwrap();
        let c = candidate(r.clone(), &r.manifest.source_id, 0, None).unwrap();
        assert_eq!(c.size_bytes, None);
        assert_eq!(c.size_confidence, ArtifactSizeConfidence::Estimated);
        assert!(c.artifact_estimates[0].selected);
        assert!(!c.artifact_estimates[1].selected);
        r.manifest.artifacts[0].size_estimate =
            ArtifactSizeEstimate::unknown("not_reported", "fixture:source");
        let c = candidate(r.clone(), &r.manifest.source_id, 0, None).unwrap();
        assert_eq!(c.size_bytes, None);
        assert_eq!(c.size_confidence, ArtifactSizeConfidence::Unknown);
    }

    struct LiveWork {
        live: Arc<std::sync::atomic::AtomicUsize>,
        abandoned: Arc<std::sync::atomic::AtomicUsize>,
        finished: bool,
    }
    impl LiveWork {
        fn start(
            live: Arc<std::sync::atomic::AtomicUsize>,
            maximum: &std::sync::atomic::AtomicUsize,
            abandoned: Arc<std::sync::atomic::AtomicUsize>,
        ) -> Self {
            let count = live.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(count, Ordering::SeqCst);
            Self {
                live,
                abandoned,
                finished: false,
            }
        }
    }
    impl Drop for LiveWork {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
            if !self.finished {
                self.abandoned.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    #[tokio::test]
    async fn metadata_reads_overlap_with_bound_four_and_restore_input_order() {
        use std::sync::atomic::AtomicUsize;
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let abandoned = Arc::new(AtomicUsize::new(0));
        let result = collect_bounded((0..10usize).collect(), stop.clone(), |index| {
            let (stop, live, max, abandoned) =
                (stop.clone(), live.clone(), max.clone(), abandoned.clone());
            async move {
                active(&stop)?;
                let mut work = LiveWork::start(live, &max, abandoned);
                tokio::time::sleep(std::time::Duration::from_millis((10 - index) as u64)).await;
                work.finished = true;
                Ok(index)
            }
        })
        .await
        .unwrap();
        assert_eq!(result, (0..10).collect::<Vec<_>>());
        assert_eq!(max.load(Ordering::SeqCst), 4);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn metadata_error_drains_started_reads_and_prevents_queued_io() {
        use std::sync::atomic::AtomicUsize;
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let abandoned = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let result = collect_bounded((0..20usize).collect(), stop.clone(), |index| {
            let (stop, live, max, abandoned, started) = (
                stop.clone(),
                live.clone(),
                max.clone(),
                abandoned.clone(),
                started.clone(),
            );
            async move {
                active(&stop)?;
                started.fetch_add(1, Ordering::SeqCst);
                let mut work = LiveWork::start(live, &max, abandoned);
                tokio::time::sleep(std::time::Duration::from_millis(if index == 0 {
                    1
                } else {
                    20
                }))
                .await;
                work.finished = true;
                if index == 0 {
                    Err(unavailable("fixture_metadata_failure"))
                } else {
                    active(&stop)?;
                    Ok(index)
                }
            }
        })
        .await;
        assert_eq!(
            result.unwrap_err().details.reason,
            "fixture_metadata_failure"
        );
        assert_eq!(started.load(Ordering::SeqCst), 4);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn dropped_observer_is_drained_before_owner_can_close() {
        use std::sync::atomic::AtomicUsize;
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let abandoned = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(5));
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let (owner_stop, owner_live, owner_max, owner_abandoned, owner_barrier) = (
            stop.clone(),
            live.clone(),
            max.clone(),
            abandoned.clone(),
            barrier.clone(),
        );
        let owner = tokio::spawn(async move {
            let operation = collect_bounded((0..12usize).collect(), owner_stop.clone(), |index| {
                let (stop, live, max, abandoned, barrier) = (
                    owner_stop.clone(),
                    owner_live.clone(),
                    owner_max.clone(),
                    owner_abandoned.clone(),
                    owner_barrier.clone(),
                );
                async move {
                    active(&stop)?;
                    let mut work = LiveWork::start(live, &max, abandoned);
                    barrier.wait().await;
                    while !stop.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                    work.finished = true;
                    active(&stop)?;
                    Ok(index)
                }
            });
            let result = drain_when_cancelled(operation, &mut receiver, owner_stop.clone()).await;
            assert_eq!(
                owner_live.load(Ordering::SeqCst),
                0,
                "closing requires no live read"
            );
            result
        });
        barrier.wait().await;
        drop(sender);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.unwrap_err().details.reason,
            "source_search_cancelled"
        );
        assert_eq!(max.load(Ordering::SeqCst), 4);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn cancelled_before_fetch_never_starts_metadata_io() {
        use std::sync::atomic::AtomicUsize;
        let stop = Arc::new(AtomicBool::new(false));
        let started = AtomicUsize::new(0);
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        drop(sender);
        let operation = async {
            active(&stop)?;
            started.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        assert_eq!(
            drain_when_cancelled(operation, &mut receiver, stop.clone())
                .await
                .unwrap_err()
                .details
                .reason,
            "source_search_cancelled"
        );
        assert_eq!(started.load(Ordering::SeqCst), 0);
    }
}
