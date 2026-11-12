//! Federación, postfiltrado y orden global de resultados (P044/H05).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use larm_adapter_sdk::source::{SearchSupport, SourceSearch};
use larm_domain::bridge::{timestamp, ApiError, Page};
use larm_domain::error::ErrorCode;
use larm_domain::model::{self, CanonicalModel};
use larm_domain::search::{CertificationStatus, CompatibilityFilter, Search};
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Mutex, Semaphore};

/// Fuente habilitada para búsqueda. La identidad canónica de cada hit debe
/// resolverla el proveedor aplicando P041 antes de devolverlo al motor.
#[derive(Debug, Clone)]
pub struct SearchSource {
    pub id: String,
    /// Current configuration/qualification fingerprint supplied by the backend.
    /// An unavailable source must have a different tag from its qualified state.
    pub cache_tag: String,
    pub priority: u64,
    pub support: SearchSupport,
}

/// Hit normalizado que el proveedor obtiene para una fuente concreta.
/// Los `Option` conservan explícitamente los datos desconocidos.
#[derive(Debug, Clone)]
pub struct SearchCandidate {
    pub model: CanonicalModel,
    pub source_id: String,
    pub source_priority: u64,
    pub variant_id: String,
    pub author: Option<String>,
    pub capabilities: Vec<String>,
    pub format: Option<String>,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub parameter_count: Option<larm_domain::units::DecimalU64>,
    pub size_bytes: Option<larm_domain::units::DecimalU64>,
    pub size_confidence: larm_domain::model::ArtifactSizeConfidence,
    pub artifact_estimates: Vec<larm_domain::download::DownloadArtifactEstimate>,
    pub acquisition: Option<larm_domain::acquisition::AcquisitionPlanBinding>,
    pub licenses: Vec<String>,
    pub runtime_ids: Vec<String>,
    pub compatibility: CompatibilityFilter,
    pub technical_fit: Option<u8>,
    pub trust_score: Option<u8>,
    pub certification_status: CertificationStatus,
    /// `Some(true)` exige hard gates PASS y disponibilidad real vigente.
    pub executable: Option<bool>,
    /// `Some(true)` exige recomendación vigente del contrato 05.
    pub recommended: Option<bool>,
}

/// Advertencia de una fuente que no pudo completar su consulta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchWarning {
    pub source_id: String,
    pub reason: String,
    pub limitations: Vec<String>,
}

/// Resultado interno del motor federado, adaptado a la frontera HTTP en el daemon.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub page: Page<CanonicalModel>,
    pub warnings: Vec<SearchWarning>,
}

/// Frontera de ejecución por fuente. El adaptador productivo debe conservar
/// los gates de P022/P043 y resolver cada hit contra la identidad P041.
#[async_trait::async_trait]
pub trait FederatedSearchProvider: Send + Sync {
    async fn sources(&self, source_ids: &[String]) -> Result<Vec<SearchSource>, ApiError>;

    async fn search_page(
        &self,
        source_id: &str,
        request: SourceSearch,
    ) -> Result<Page<SearchCandidate>, ApiError>;
}

/// Firma y verifica cursores con la clave efímera del proceso.
pub trait SearchCursorSigner: Send + Sync {
    fn sign(&self, sort_key: &str, id: &str, filter_digest: &str) -> String;

    fn verify(&self, cursor: &str, filter_digest: &str) -> Result<(String, String), ApiError>;
}

/// Límites iniciales del servicio; configurables por tests y por la
/// composición del daemon.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub source_timeout: Duration,
    pub source_concurrency: usize,
    pub cache_ttl: Duration,
    pub max_snapshots: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            source_timeout: Duration::from_secs(30),
            source_concurrency: 4,
            cache_ttl: Duration::from_secs(30),
            max_snapshots: 128,
        }
    }
}

#[derive(Clone)]
pub struct FederatedSearch {
    provider: Arc<dyn FederatedSearchProvider>,
    cursors: Arc<dyn SearchCursorSigner>,
    config: SearchConfig,
    cache: Arc<Mutex<SearchCache>>,
}

#[derive(Default)]
struct SearchCache {
    sources: HashMap<String, CachedSource>,
    snapshots: HashMap<String, SearchSnapshot>,
}

#[derive(Clone)]
struct CachedSource {
    expires_at: Instant,
    candidates: Vec<SearchCandidate>,
    limitations: Vec<String>,
    upstream_cursors: Vec<Option<String>>,
}

#[derive(Clone)]
struct SearchSnapshot {
    expires_at: Instant,
    source_ids: BTreeSet<String>,
    source_tags: BTreeMap<String, String>,
    upstream_cursors: HashMap<String, Vec<Option<String>>>,
    items: Vec<CanonicalModel>,
    warnings: Vec<SearchWarning>,
    snapshot_at: String,
}

struct SourceResult {
    source: SearchSource,
    result: Result<CachedSource, ApiError>,
}

#[derive(Clone)]
struct ModelGroup {
    model: CanonicalModel,
    sources: BTreeSet<String>,
    variants: BTreeSet<String>,
    capabilities: BTreeSet<String>,
    best: SearchCandidate,
}

impl FederatedSearch {
    #[must_use]
    pub fn new(
        provider: Arc<dyn FederatedSearchProvider>,
        cursors: Arc<dyn SearchCursorSigner>,
        config: SearchConfig,
    ) -> Self {
        Self {
            provider,
            cursors,
            config,
            cache: Arc::new(Mutex::new(SearchCache::default())),
        }
    }

    /// Vacía el snapshot y las consultas de una fuente tras editarla,
    /// deshabilitarla o invalidar su catálogo.
    pub async fn invalidate_source(&self, source_id: &str) {
        let mut cache = self.cache.lock().await;
        cache
            .sources
            .retain(|key, _| !key.starts_with(&format!("{source_id}:")));
        cache
            .snapshots
            .retain(|_, snapshot| !snapshot.source_ids.contains(source_id));
    }

    /// Invalida todas las consultas tras un cambio global del catálogo.
    pub async fn invalidate_all(&self) {
        *self.cache.lock().await = SearchCache::default();
    }

    async fn page_from_snapshot(
        &self,
        snapshot_id: &str,
        offset: usize,
        cursor_state_tag: Option<&str>,
        request: &Search,
        digest: &str,
    ) -> Result<SearchResult, ApiError> {
        let snapshot = {
            let mut cache = self.cache.lock().await;
            cache
                .snapshots
                .retain(|_, value| value.expires_at > Instant::now());
            cache.snapshots.get(snapshot_id).cloned()
        }
        .ok_or_else(bad_cursor)?;
        if !snapshot.source_tags.is_empty() {
            let ids = snapshot.source_ids.iter().cloned().collect::<Vec<_>>();
            let current = self.provider.sources(&ids).await?;
            let tags = current
                .iter()
                .map(|s| (s.id.clone(), s.cache_tag.clone()))
                .collect::<BTreeMap<_, _>>();
            if tags != snapshot.source_tags || tags.len() != current.len() {
                for (id, tag) in &snapshot.source_tags {
                    if tags.get(id) != Some(tag) {
                        self.invalidate_source(id).await;
                    }
                }
                return Err(ApiError::new(
                    ErrorCode::StaleRevision,
                    "Las fuentes cambiaron; repite la búsqueda",
                    "search_source_snapshot_changed",
                )
                .retryable(true));
            }
        }
        let expected_state_tag = upstream_cursor_state(&snapshot)?;
        if cursor_state_tag.is_some_and(|value| value != expected_state_tag) {
            return Err(bad_cursor());
        }
        if offset >= snapshot.items.len() && !(offset == 0 && snapshot.items.is_empty()) {
            return Err(bad_cursor());
        }
        let end = offset
            .saturating_add(usize::from(request.limit))
            .min(snapshot.items.len());
        let items = snapshot.items[offset.min(end)..end].to_vec();
        let next_cursor = if end < snapshot.items.len() {
            Some(
                self.cursors
                    .sign(&format!("{end}:{expected_state_tag}"), snapshot_id, digest),
            )
        } else {
            None
        };
        Ok(SearchResult {
            page: Page {
                items,
                next_cursor,
                snapshot_at: snapshot.snapshot_at,
            },
            warnings: snapshot.warnings,
        })
    }

    async fn fetch_source(
        &self,
        source: SearchSource,
        request: Search,
    ) -> Result<CachedSource, ApiError> {
        let cache_key = source_cache_key(&source, &request)?;
        if let Some(found) = self.cache.lock().await.sources.get(&cache_key).cloned() {
            if found.expires_at > Instant::now() {
                return Ok(found);
            }
        }

        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_variants = HashSet::new();
        let mut upstream_cursors = Vec::new();
        let mut snapshot_at: Option<String> = None;
        let mut candidates = Vec::new();
        loop {
            let projection = source
                .support
                .project(&request, &source.id, cursor.clone())?;
            // Federation consumes the full source snapshot before global
            // ranking. A small visible page must not reopen a qualified
            // worker for every one or two remote hits.
            let mut upstream_request = projection.upstream;
            upstream_request.limit = upstream_request.limit.max(50);
            upstream_cursors.push(upstream_request.cursor.clone());
            let page = self
                .provider
                .search_page(&source.id, upstream_request.clone())
                .await?;
            if page.items.len() > usize::from(upstream_request.limit)
                || chrono::DateTime::parse_from_rfc3339(&page.snapshot_at).is_err()
            {
                return Err(source_protocol_error());
            }
            if snapshot_at
                .as_ref()
                .is_some_and(|previous| previous != &page.snapshot_at)
            {
                return Err(ApiError::new(
                    ErrorCode::ResourceUnavailable,
                    "La fuente cambió durante la búsqueda",
                    "source_snapshot_changed",
                )
                .retryable(true));
            }
            snapshot_at = Some(page.snapshot_at);
            if page.next_cursor.as_ref().is_some_and(|next| {
                !seen_cursors.insert(next.clone()) || cursor.as_ref() == Some(next)
            }) {
                return Err(source_protocol_error());
            }
            for candidate in page.items {
                let candidate = normalize_candidate(candidate, &source)?;
                if !seen_variants.insert(candidate.variant_id.clone()) {
                    return Err(source_protocol_error());
                }
                candidates.push(candidate);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        let mut limitations = source.support.limitations.clone();
        limitations.sort();
        limitations.dedup();
        let cached = CachedSource {
            expires_at: Instant::now() + self.config.cache_ttl,
            candidates,
            limitations,
            upstream_cursors,
        };
        let mut cache = self.cache.lock().await;
        cache
            .sources
            .retain(|_, value| value.expires_at > Instant::now());
        if cache.sources.len() >= 256 {
            if let Some(oldest_key) = cache.sources.keys().next().cloned() {
                cache.sources.remove(&oldest_key);
            }
        }
        cache.sources.insert(cache_key, cached.clone());
        Ok(cached)
    }

    async fn build_snapshot(&self, request: &Search) -> Result<String, ApiError> {
        let mut sources = self.provider.sources(&request.source_ids).await?;
        sources.retain(|source| {
            request.source_ids.is_empty() || request.source_ids.contains(&source.id)
        });
        sources.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        let mut source_ids_seen = HashSet::new();
        if sources.iter().any(|source| {
            !larm_domain::bridge::is_uuid_v7(&source.id)
                || source.cache_tag.is_empty()
                || !source_ids_seen.insert(source.id.clone())
        }) {
            return Err(source_protocol_error());
        }
        let selected_ids: BTreeSet<String> = sources.iter().map(|s| s.id.clone()).collect();
        let source_tags = sources
            .iter()
            .map(|s| (s.id.clone(), s.cache_tag.clone()))
            .collect();

        let permits = Arc::new(Semaphore::new(self.config.source_concurrency.max(1)));
        let mut tasks = tokio::task::JoinSet::new();
        for source in sources {
            let provider = self.provider.clone();
            let cache = self.cache.clone();
            let request = request.clone();
            let permits = permits.clone();
            let config = self.config.clone();
            let cursors = self.cursors.clone();
            let source_timeout = config.source_timeout;
            tasks.spawn(async move {
                let source_for_fetch = source.clone();
                let fetch = async move {
                    let _permit = permits.acquire_owned().await.map_err(|_| {
                        ApiError::new(
                            ErrorCode::ResourceUnavailable,
                            "La búsqueda se ha cerrado",
                            "search_cancelled",
                        )
                    })?;
                    let fetcher = FederatedSearch {
                        provider,
                        cursors,
                        config,
                        cache,
                    };
                    fetcher.fetch_source(source_for_fetch, request).await
                };
                let result = match tokio::time::timeout(source_timeout, fetch).await {
                    Ok(result) => result,
                    Err(_) => Err(ApiError::new(
                        ErrorCode::Timeout,
                        "La fuente agotó el tiempo de búsqueda",
                        "source_search_timeout",
                    )
                    .retryable(true)),
                };
                SourceResult { source, result }
            });
        }

        let mut candidates = Vec::new();
        let mut warnings = Vec::new();
        let mut failed_sources = 0;
        let mut upstream_cursors = HashMap::new();
        while let Some(joined) = tasks.join_next().await {
            let output = match joined {
                Ok(output) => output,
                Err(_) => {
                    failed_sources += 1;
                    warnings.push(SearchWarning {
                        source_id: String::new(),
                        reason: "source_search_task_failed".into(),
                        limitations: Vec::new(),
                    });
                    continue;
                }
            };
            match output.result {
                Ok(found) => {
                    upstream_cursors
                        .insert(output.source.id.clone(), found.upstream_cursors.clone());
                    candidates.extend(found.candidates);
                    if !found.limitations.is_empty() {
                        warnings.push(SearchWarning {
                            source_id: output.source.id.clone(),
                            reason: "source_search_limited".into(),
                            limitations: found
                                .limitations
                                .iter()
                                .map(|limitation| larm_domain::redact::redact(limitation))
                                .collect(),
                        });
                    }
                }
                Err(error) => {
                    failed_sources += 1;
                    warnings.push(SearchWarning {
                        source_id: output.source.id,
                        reason: safe_reason(&error),
                        limitations: Vec::new(),
                    });
                }
            }
        }
        warnings.sort_by(|a, b| a.source_id.cmp(&b.source_id).then(a.reason.cmp(&b.reason)));
        warnings.dedup();

        if !selected_ids.is_empty() && candidates.is_empty() && failed_sources > 0 {
            return Err(ApiError::new(
                ErrorCode::UpstreamUnavailable,
                "No se pudieron consultar fuentes para esta búsqueda",
                "all_sources_unavailable",
            )
            .retryable(true));
        }

        candidates.retain(|candidate| matches_search(candidate, request));
        candidates.sort_by(|a, b| candidate_order(a, b, &request.q));
        let items = group_candidates(candidates, &request.q);
        if items.is_empty() && failed_sources > 0 {
            return Err(ApiError::new(
                ErrorCode::UpstreamUnavailable,
                "La búsqueda quedó incompleta porque una fuente no respondió",
                "partial_search_without_results",
            )
            .retryable(true));
        }
        let snapshot_id = uuid::Uuid::now_v7().to_string();
        let snapshot = SearchSnapshot {
            expires_at: Instant::now() + self.config.cache_ttl,
            source_ids: selected_ids,
            source_tags,
            upstream_cursors,
            items,
            warnings,
            snapshot_at: timestamp(chrono::Utc::now()),
        };
        let mut cache = self.cache.lock().await;
        cache
            .snapshots
            .retain(|_, value| value.expires_at > Instant::now());
        if cache.snapshots.len() >= self.config.max_snapshots.max(1) {
            if let Some(oldest_key) = cache.snapshots.keys().next().cloned() {
                cache.snapshots.remove(&oldest_key);
            }
        }
        cache.snapshots.insert(snapshot_id.clone(), snapshot);
        Ok(snapshot_id)
    }
}

impl FederatedSearch {
    /// Ejecuta una búsqueda completa y conserva el snapshot para su cursor global.
    pub async fn search(&self, request: &Search) -> Result<SearchResult, ApiError> {
        request.validate()?;
        let mut normalized = request.clone();
        normalized.capabilities = normalize_capabilities(&request.capabilities)?;
        let request = &normalized;
        let digest = request.filter_digest()?;
        if let Some(cursor) = request.cursor.as_deref() {
            let (sort_key, snapshot_id) = self.cursors.verify(cursor, &digest)?;
            let (offset, state_tag) = sort_key.split_once(':').ok_or_else(bad_cursor)?;
            let offset = offset.parse::<usize>().map_err(|_| bad_cursor())?;
            return self
                .page_from_snapshot(&snapshot_id, offset, Some(state_tag), request, &digest)
                .await;
        }
        if request.is_contradictory() {
            let snapshot_id = uuid::Uuid::now_v7().to_string();
            let snapshot = SearchSnapshot {
                expires_at: Instant::now() + self.config.cache_ttl,
                source_ids: BTreeSet::new(),
                source_tags: BTreeMap::new(),
                upstream_cursors: HashMap::new(),
                items: Vec::new(),
                warnings: Vec::new(),
                snapshot_at: timestamp(chrono::Utc::now()),
            };
            self.cache
                .lock()
                .await
                .snapshots
                .insert(snapshot_id.clone(), snapshot);
            return self
                .page_from_snapshot(&snapshot_id, 0, None, request, &digest)
                .await;
        }
        let snapshot_id = self.build_snapshot(request).await?;
        self.page_from_snapshot(&snapshot_id, 0, None, request, &digest)
            .await
    }
}

fn upstream_cursor_state(snapshot: &SearchSnapshot) -> Result<String, ApiError> {
    let value = serde_json::to_value(&snapshot.upstream_cursors).map_err(|error| {
        ApiError::new(
            ErrorCode::Internal,
            "No se pudo conservar la paginación de fuentes",
            "search_cursor_state",
        )
        .with_technical(error.to_string())
    })?;
    larm_domain::canonical::sha256_hex(&value).map_err(|error| {
        ApiError::new(
            ErrorCode::Internal,
            "No se pudo firmar la paginación de fuentes",
            "search_cursor_state",
        )
        .with_technical(error)
    })
}

fn source_cache_key(source: &SearchSource, request: &Search) -> Result<String, ApiError> {
    let mut capabilities = request.capabilities.clone();
    capabilities.sort();
    capabilities.dedup();
    let value = json!({
        "source_id": source.id,
        "source_cache_tag": source.cache_tag,
        "source_priority": source.priority,
        "support": source.support,
        "q": request.q.trim().to_ascii_lowercase(),
        "capabilities": capabilities,
    });
    larm_domain::canonical::sha256_hex(&value)
        .map(|hash| format!("{}:{hash}", source.id))
        .map_err(|detail| {
            ApiError::new(
                ErrorCode::Internal,
                "No se pudo calcular la clave de búsqueda",
                "search_cache_key",
            )
            .with_technical(detail)
        })
}

fn normalize_candidate(
    mut candidate: SearchCandidate,
    source: &SearchSource,
) -> Result<SearchCandidate, ApiError> {
    if candidate.source_id != source.id
        || (candidate.size_confidence == model::ArtifactSizeConfidence::Unknown
            && candidate.size_bytes.is_some())
        || (matches!(
            candidate.size_confidence,
            model::ArtifactSizeConfidence::Exact | model::ArtifactSizeConfidence::High
        ) && candidate.size_bytes.is_none())
        || !larm_domain::bridge::is_uuid_v7(&candidate.variant_id)
        || !larm_domain::bridge::is_uuid_v7(&candidate.model.id)
        || candidate
            .runtime_ids
            .iter()
            .any(|id| !larm_domain::bridge::is_uuid_v7(id))
        || candidate.technical_fit.is_some_and(|value| value > 100)
        || candidate.trust_score.is_some_and(|value| value > 100)
    {
        return Err(source_protocol_error());
    }
    candidate.model.name = normalize_name(&larm_domain::redact::redact(&candidate.model.name));
    if candidate.model.name.is_empty() {
        return Err(source_protocol_error());
    }
    candidate.model.capabilities = normalize_capabilities(&candidate.model.capabilities)?;
    candidate.capabilities = normalize_capabilities(&candidate.capabilities)?;
    candidate.capabilities.sort();
    candidate.capabilities.dedup();
    candidate.author = candidate
        .author
        .map(|value| normalize_name(&larm_domain::redact::redact(&value)))
        .filter(|v| !v.is_empty());
    for field in [
        &mut candidate.format,
        &mut candidate.quantization,
        &mut candidate.architecture,
    ] {
        *field = field
            .take()
            .map(|value| value.trim().to_string())
            .filter(|v| !v.is_empty());
    }
    candidate.licenses = candidate
        .licenses
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    candidate.runtime_ids.sort();
    candidate.runtime_ids.dedup();
    candidate.source_priority = source.priority;
    if candidate.compatibility == CompatibilityFilter::All {
        return Err(source_protocol_error());
    }
    candidate.model.validate()?;
    Ok(candidate)
}

fn normalize_capabilities(values: &[String]) -> Result<Vec<String>, ApiError> {
    let normalized: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect();
    model::capabilities(&normalized)
}

fn normalize_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn matches_search(candidate: &SearchCandidate, request: &Search) -> bool {
    let contains = |value: &str| {
        value
            .to_lowercase()
            .contains(&request.q.trim().to_lowercase())
    };
    if !request.q.trim().is_empty()
        && !contains(&candidate.model.name)
        && !candidate.author.as_deref().is_some_and(contains)
        && !candidate
            .model
            .upstream_identity
            .as_deref()
            .is_some_and(contains)
    {
        return false;
    }
    if !request.source_ids.is_empty() && !request.source_ids.contains(&candidate.source_id) {
        return false;
    }
    if !request.capabilities.is_empty() {
        let candidate_capabilities: HashSet<&str> = candidate
            .capabilities
            .iter()
            .chain(candidate.model.capabilities.iter())
            .map(String::as_str)
            .collect();
        if !request
            .capabilities
            .iter()
            .any(|capability| candidate_capabilities.contains(capability.as_str()))
        {
            return false;
        }
    }
    if request.compatibility != CompatibilityFilter::All
        && candidate.compatibility != request.compatibility
    {
        return false;
    }
    if !request.show_incompatible && candidate.compatibility == CompatibilityFilter::Incompatible {
        return false;
    }
    if !request.architectures.is_empty()
        && !contains_any(&request.architectures, candidate.architecture.as_deref())
    {
        return false;
    }
    if !request.formats.is_empty() && !contains_any(&request.formats, candidate.format.as_deref()) {
        return false;
    }
    if !request.quantizations.is_empty()
        && !contains_any(&request.quantizations, candidate.quantization.as_deref())
    {
        return false;
    }
    if !request.licenses.is_empty()
        && !request.licenses.iter().any(|license| {
            candidate
                .licenses
                .iter()
                .any(|v| v.eq_ignore_ascii_case(license))
        })
    {
        return false;
    }
    if !request.runtime_ids.is_empty()
        && !request
            .runtime_ids
            .iter()
            .any(|runtime| candidate.runtime_ids.contains(runtime))
    {
        return false;
    }
    if !range_matches(
        candidate.parameter_count,
        request.parameter_count.minimum,
        request.parameter_count.maximum,
    ) || !range_matches(
        candidate.size_bytes,
        request.size_bytes.minimum,
        request.size_bytes.maximum,
    ) || !score_matches(
        candidate.trust_score,
        request.trust_score.minimum,
        request.trust_score.maximum,
    ) {
        return false;
    }
    if !request.certification_statuses.is_empty()
        && !request
            .certification_statuses
            .contains(&candidate.certification_status)
    {
        return false;
    }
    if request.only_executable && candidate.executable != Some(true) {
        return false;
    }
    if request.only_recommended && candidate.recommended != Some(true) {
        return false;
    }
    true
}

fn contains_any(choices: &[String], value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        choices
            .iter()
            .any(|choice| choice.eq_ignore_ascii_case(value))
    })
}

fn range_matches(
    value: Option<larm_domain::units::DecimalU64>,
    min: Option<larm_domain::units::DecimalU64>,
    max: Option<larm_domain::units::DecimalU64>,
) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    value.is_some_and(|value| {
        min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
    })
}

fn score_matches(value: Option<u8>, min: Option<u8>, max: Option<u8>) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    value.is_some_and(|value| {
        min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
    })
}

fn candidate_order(a: &SearchCandidate, b: &SearchCandidate, query: &str) -> Ordering {
    let rank = |candidate: &SearchCandidate| {
        let compatibility = match candidate.compatibility {
            CompatibilityFilter::Compatible => 2u8,
            CompatibilityFilter::Unknown | CompatibilityFilter::All => 1,
            CompatibilityFilter::Incompatible => 0,
        };
        let fit = candidate.technical_fit.map_or(-1, i16::from);
        let trust = candidate.trust_score.map_or(-1, i16::from);
        let relevance = query_relevance(query, &candidate.model.name, candidate.author.as_deref());
        (
            compatibility,
            fit,
            trust,
            candidate.source_priority,
            relevance,
        )
    };
    rank(b)
        .cmp(&rank(a))
        .then(
            a.model
                .name
                .to_lowercase()
                .cmp(&b.model.name.to_lowercase()),
        )
        .then(a.model.id.cmp(&b.model.id))
        .then(a.variant_id.cmp(&b.variant_id))
}

fn query_relevance(query: &str, name: &str, author: Option<&str>) -> u8 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 0;
    }
    let name = name.to_lowercase();
    if name == query {
        3
    } else if name.starts_with(&query) {
        2
    } else if name.contains(&query)
        || author.is_some_and(|value| value.to_lowercase().contains(&query))
    {
        1
    } else {
        0
    }
}

fn group_candidates(candidates: Vec<SearchCandidate>, query: &str) -> Vec<CanonicalModel> {
    let mut groups: Vec<ModelGroup> = Vec::new();
    let mut positions = HashMap::new();
    for candidate in candidates {
        let id = candidate.model.id.clone();
        if let Some(position) = positions.get(&id).copied() {
            let group: &mut ModelGroup = &mut groups[position];
            group.sources.insert(candidate.source_id.clone());
            group.variants.insert(candidate.variant_id.clone());
            group
                .capabilities
                .extend(candidate.capabilities.iter().cloned());
            group
                .capabilities
                .extend(candidate.model.capabilities.iter().cloned());
            if candidate_order(&candidate, &group.best, query) == Ordering::Less {
                group.best = candidate;
            }
        } else {
            let position = groups.len();
            positions.insert(id, position);
            let mut sources = BTreeSet::new();
            sources.insert(candidate.source_id.clone());
            let mut variants = BTreeSet::new();
            variants.insert(candidate.variant_id.clone());
            let mut capabilities: BTreeSet<String> = candidate
                .capabilities
                .iter()
                .chain(candidate.model.capabilities.iter())
                .cloned()
                .collect();
            capabilities.extend(candidate.model.capabilities.iter().cloned());
            groups.push(ModelGroup {
                model: candidate.model.clone(),
                sources,
                variants,
                capabilities: std::mem::take(&mut capabilities),
                best: candidate,
            });
        }
    }
    groups
        .into_iter()
        .map(|mut group| {
            group.model.capabilities = group.capabilities.into_iter().collect();
            group.model.metadata.insert(
                "search".into(),
                json!({
                    "source_ids": group.sources,
                    "variant_count": group.variants.len(),
                    "best_variant": {
                        "variant_id": group.best.variant_id,
                        "author": group.best.author,
                        "format": group.best.format,
                        "quantization": group.best.quantization,
                        "architecture": group.best.architecture,
                        "parameter_count": group.best.parameter_count,
                        "size_bytes": group.best.size_bytes,
                        "size_confidence": group.best.size_confidence,
                        "artifact_estimates": group.best.artifact_estimates,
                        "acquisition": group.best.acquisition,
                        "licenses": group.best.licenses,
                        "compatibility": group.best.compatibility,
                        "technical_fit": group.best.technical_fit,
                        "trust_score": group.best.trust_score,
                        "certification_status": group.best.certification_status,
                        "executable": group.best.executable,
                        "recommended": group.best.recommended,
                    }
                }),
            );
            group.model
        })
        .collect()
}

fn safe_reason(error: &ApiError) -> String {
    match error.details.reason.as_str() {
        "source_search_timeout" => "source_search_timeout".into(),
        "source_snapshot_changed" => "source_snapshot_changed".into(),
        "source_filter_not_supported" => "source_filter_not_supported".into(),
        "source_search_invalid" | "source_response_invalid" => "source_protocol_error".into(),
        "source_rate_limited" => "source_rate_limited".into(),
        _ => "source_unavailable".into(),
    }
}

fn source_protocol_error() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolError,
        "La fuente devolvió una página fuera de contrato",
        "source_response_invalid",
    )
}

fn bad_cursor() -> ApiError {
    ApiError::new(
        ErrorCode::ValidationFailed,
        "Cursor de búsqueda no válido o expirado",
        "bad_cursor",
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use larm_adapter_sdk::source::{CAPABILITY_FILTER, TEXT_FILTER};
    use larm_domain::search::CertificationStatus;
    use larm_domain::units::DecimalU64;
    use serde_json::Map;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    const SOURCE_A: &str = "0192aaaa-0000-7000-8000-000000000001";
    const SOURCE_B: &str = "0192aaaa-0000-7000-8000-000000000002";
    const MODEL_A: &str = "0192aaaa-0000-7000-8000-000000000101";
    const MODEL_B: &str = "0192aaaa-0000-7000-8000-000000000102";
    const MODEL_C: &str = "0192aaaa-0000-7000-8000-000000000103";
    const RUNTIME: &str = "0192aaaa-0000-7000-8000-000000000201";

    #[derive(Default)]
    struct MemoryCursorSigner;

    impl SearchCursorSigner for MemoryCursorSigner {
        fn sign(&self, sort_key: &str, id: &str, filter_digest: &str) -> String {
            serde_json::json!([sort_key, id, filter_digest]).to_string()
        }

        fn verify(&self, cursor: &str, filter_digest: &str) -> Result<(String, String), ApiError> {
            let parts: (String, String, String) =
                serde_json::from_str(cursor).map_err(|_| bad_cursor())?;
            if parts.2 != filter_digest {
                return Err(bad_cursor());
            }
            Ok((parts.0, parts.1))
        }
    }

    struct MemoryProvider {
        sources: Vec<SearchSource>,
        rows: HashMap<String, Vec<SearchCandidate>>,
        calls: AtomicUsize,
        slow_source: Option<String>,
        delay: Duration,
        page_cap: Option<u16>,
        requested_limits: std::sync::Mutex<Vec<u16>>,
    }

    impl MemoryProvider {
        fn new(rows: Vec<(String, Vec<SearchCandidate>)>) -> Self {
            Self {
                sources: vec![source(SOURCE_A, 2), source(SOURCE_B, 1)],
                rows: rows.into_iter().collect(),
                calls: AtomicUsize::new(0),
                slow_source: None,
                delay: Duration::ZERO,
                page_cap: None,
                requested_limits: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn slow(mut self, source_id: &str, delay: Duration) -> Self {
            self.slow_source = Some(source_id.into());
            self.delay = delay;
            self
        }
        fn page_cap(mut self, cap: u16) -> Self {
            self.page_cap = Some(cap);
            self
        }
    }

    #[async_trait]
    impl FederatedSearchProvider for MemoryProvider {
        async fn sources(&self, source_ids: &[String]) -> Result<Vec<SearchSource>, ApiError> {
            Ok(self
                .sources
                .iter()
                .filter(|source| source_ids.is_empty() || source_ids.contains(&source.id))
                .cloned()
                .collect())
        }

        async fn search_page(
            &self,
            source_id: &str,
            request: larm_adapter_sdk::source::SourceSearch,
        ) -> Result<Page<SearchCandidate>, ApiError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.requested_limits.lock().unwrap().push(request.limit);
            if self.slow_source.as_deref() == Some(source_id) {
                tokio::time::sleep(self.delay).await;
            }
            let rows = self.rows.get(source_id).cloned().unwrap_or_default();
            let start = request
                .cursor
                .as_deref()
                .and_then(|cursor| cursor.parse::<usize>().ok())
                .unwrap_or(0);
            let end = start
                .saturating_add(usize::from(
                    request.limit.min(self.page_cap.unwrap_or(request.limit)),
                ))
                .min(rows.len());
            let items = rows[start.min(end)..end].to_vec();
            let next_cursor = (end < rows.len()).then(|| end.to_string());
            Ok(Page {
                items,
                next_cursor,
                snapshot_at: "2026-10-22T10:00:00.000Z".into(),
            })
        }
    }

    fn source(id: &str, priority: u64) -> SearchSource {
        SearchSource {
            id: id.into(),
            cache_tag: "fixture-current-configuration".into(),
            priority,
            support: larm_adapter_sdk::source::SearchSupport {
                search_applicable: true,
                supported_filters: [TEXT_FILTER.into(), CAPABILITY_FILTER.into()]
                    .into_iter()
                    .collect(),
                limitations: Vec::new(),
            },
        }
    }

    fn model(id: &str, name: &str) -> CanonicalModel {
        CanonicalModel {
            id: id.into(),
            created_at: "2026-10-22T10:00:00.000Z".into(),
            updated_at: "2026-10-22T10:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            name: name.into(),
            upstream_identity: Some(format!("org/{name}")),
            capabilities: vec!["text.generate".into()],
            possible_duplicates: Vec::new(),
        }
    }

    fn candidate(source_id: &str, variant_id: &str, model_id: &str, name: &str) -> SearchCandidate {
        SearchCandidate {
            model: model(model_id, name),
            source_id: source_id.into(),
            source_priority: 0,
            variant_id: variant_id.into(),
            author: Some("Qwen Team".into()),
            capabilities: vec!["text.generate".into()],
            format: Some("GGUF".into()),
            quantization: Some("Q4_K_M".into()),
            architecture: Some("qwen3".into()),
            parameter_count: DecimalU64::new(7_000_000_000),
            size_bytes: DecimalU64::new(10_000_000_000),
            size_confidence: larm_domain::model::ArtifactSizeConfidence::Exact,
            artifact_estimates: Vec::new(),
            acquisition: None,
            licenses: vec!["Apache-2.0".into()],
            runtime_ids: vec![RUNTIME.into()],
            compatibility: CompatibilityFilter::Compatible,
            technical_fit: Some(90),
            trust_score: Some(85),
            certification_status: CertificationStatus::Uncertified,
            executable: Some(true),
            recommended: Some(false),
        }
    }

    fn request(limit: u16) -> Search {
        serde_json::from_value(json!({
            "q":"", "capabilities":[], "source_ids":[], "compatibility":"ALL",
            "architectures":[], "formats":[], "quantizations":[], "licenses":[],
            "runtime_ids":[], "parameter_count":{"minimum":null,"maximum":null},
            "size_bytes":{"minimum":null,"maximum":null},
            "trust_score":{"minimum":null,"maximum":null}, "certification_statuses":[],
            "only_executable":false,"only_recommended":false,"show_incompatible":true,
            "cursor":null,"limit":limit
        }))
        .expect("Search válido")
    }

    fn service(provider: Arc<MemoryProvider>, config: SearchConfig) -> FederatedSearch {
        FederatedSearch::new(provider, Arc::new(MemoryCursorSigner), config)
    }

    fn one(source_id: &str, variant_id: &str, model_id: &str, name: &str) -> SearchCandidate {
        candidate(source_id, variant_id, model_id, name)
    }

    #[tokio::test]
    async fn mezcla_dos_fuentes_deduplica_modelo_canonico_y_pagina_global() {
        let provider = Arc::new(MemoryProvider::new(vec![
            (
                SOURCE_A.into(),
                vec![
                    one(
                        SOURCE_A,
                        "0192aaaa-0000-7000-8000-000000000301",
                        MODEL_A,
                        "Qwen A",
                    ),
                    one(
                        SOURCE_A,
                        "0192aaaa-0000-7000-8000-000000000302",
                        MODEL_B,
                        "Qwen B",
                    ),
                ],
            ),
            (
                SOURCE_B.into(),
                vec![
                    one(
                        SOURCE_B,
                        "0192aaaa-0000-7000-8000-000000000303",
                        MODEL_A,
                        "Qwen A",
                    ),
                    one(
                        SOURCE_B,
                        "0192aaaa-0000-7000-8000-000000000304",
                        MODEL_C,
                        "Qwen C",
                    ),
                ],
            ),
        ]));
        let service = service(provider.clone(), SearchConfig::default());
        let first = service.search(&request(2)).await.expect("primera página");
        assert_eq!(first.page.items.len(), 2);
        assert!(first.page.next_cursor.is_some());
        assert_eq!(first.page.items[0].id, MODEL_A);
        let grouped = &first.page.items[0].metadata["search"];
        assert_eq!(grouped["source_ids"].as_array().unwrap().len(), 2);
        assert_eq!(grouped["variant_count"], 2);

        let mut next = request(2);
        next.cursor = first.page.next_cursor;
        let second = service.search(&next).await.expect("segunda página");
        assert_eq!(second.page.items.len(), 1);
        assert_eq!(second.page.items[0].id, MODEL_C);
        assert!(second.page.next_cursor.is_none());
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn una_fuente_lenta_no_descarta_la_respuesta_de_la_otra() {
        let provider = Arc::new(
            MemoryProvider::new(vec![
                (
                    SOURCE_A.into(),
                    vec![one(
                        SOURCE_A,
                        "0192aaaa-0000-7000-8000-000000000311",
                        MODEL_A,
                        "Fast model",
                    )],
                ),
                (SOURCE_B.into(), Vec::new()),
            ])
            .slow(SOURCE_B, Duration::from_millis(80)),
        );
        let service = service(
            provider,
            SearchConfig {
                source_timeout: Duration::from_millis(10),
                ..SearchConfig::default()
            },
        );
        let result = service.search(&request(50)).await.expect("éxito parcial");
        assert_eq!(result.page.items.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].source_id, SOURCE_B);
        assert_eq!(result.warnings[0].reason, "source_search_timeout");
    }

    #[tokio::test]
    async fn limitaciones_de_fuente_se_conservan_y_redactan_en_advertencias() {
        let mut provider = MemoryProvider::new(vec![(
            SOURCE_A.into(),
            vec![one(
                SOURCE_A,
                "0192aaaa-0000-7000-8000-000000000315",
                MODEL_A,
                "Qwen A",
            )],
        )]);
        provider.sources[0].support.limitations = vec![
            "filtro de licencia diferido; Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456"
                .into(),
        ];
        let service = service(Arc::new(provider), SearchConfig::default());
        let mut query = request(20);
        query.source_ids = vec![SOURCE_A.into()];
        let result = service
            .search(&query)
            .await
            .expect("resultado parcial señalado");
        assert_eq!(result.page.items.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].reason, "source_search_limited");
        assert!(result.warnings[0].limitations[0].contains("[REDACTED]"));
        assert!(!result.warnings[0].limitations[0].contains("abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[tokio::test]
    async fn cache_por_fuente_y_query_se_invalida_explicita() {
        let provider = Arc::new(MemoryProvider::new(vec![(
            SOURCE_A.into(),
            vec![one(
                SOURCE_A,
                "0192aaaa-0000-7000-8000-000000000321",
                MODEL_A,
                "Cache model",
            )],
        )]));
        let service = service(
            provider.clone(),
            SearchConfig {
                cache_ttl: Duration::from_millis(25),
                ..SearchConfig::default()
            },
        );
        let mut query = request(20);
        query.source_ids = vec![SOURCE_A.into()];
        service.search(&query).await.expect("consulta inicial");
        service.search(&query).await.expect("consulta en caché");
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);
        service.invalidate_source(SOURCE_A).await;
        service.search(&query).await.expect("consulta invalidada");
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 2);
        tokio::time::sleep(Duration::from_millis(35)).await;
        service
            .search(&query)
            .await
            .expect("caché expirada por TTL");
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 3);
    }

    #[tokio::test]
    async fn sigue_paginas_upstream_antes_de_filtrar_y_paginar_globalmente() {
        let mut first = candidate(
            SOURCE_A,
            "0192aaaa-0000-7000-8000-000000000351",
            MODEL_A,
            "A safe 1",
        );
        first.format = Some("SafeTensors".into());
        let mut second = candidate(
            SOURCE_A,
            "0192aaaa-0000-7000-8000-000000000352",
            MODEL_B,
            "B safe 2",
        );
        second.format = Some("SafeTensors".into());
        let third = candidate(
            SOURCE_A,
            "0192aaaa-0000-7000-8000-000000000353",
            MODEL_C,
            "C gguf 3",
        );
        let mirror = candidate(
            SOURCE_B,
            "0192aaaa-0000-7000-8000-000000000354",
            "0192aaaa-0000-7000-8000-000000000104",
            "D gguf 4",
        );
        let provider = Arc::new(
            MemoryProvider::new(vec![
                (SOURCE_A.into(), vec![first, second, third]),
                (SOURCE_B.into(), vec![mirror]),
            ])
            .page_cap(1),
        );
        let service = service(provider.clone(), SearchConfig::default());
        let mut query = request(1);
        query.formats = vec!["gguf".into()];
        let first_page = service.search(&query).await.expect("snapshot federado");
        assert_eq!(first_page.page.items.len(), 1);
        assert_eq!(first_page.page.items[0].id, MODEL_C);
        assert!(first_page.page.next_cursor.is_some());
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 4);

        query.cursor = first_page.page.next_cursor;
        let second_page = service
            .search(&query)
            .await
            .expect("página global siguiente");
        assert_eq!(second_page.page.items.len(), 1);
        assert_eq!(second_page.page.items[0].name, "D gguf 4");
        assert!(second_page.page.next_cursor.is_none());
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 4);
    }

    #[tokio::test]
    async fn cancelar_peticion_aborta_las_consultas_de_fuente_pendientes() {
        let provider = Arc::new(
            MemoryProvider::new(vec![
                (SOURCE_A.into(), Vec::new()),
                (SOURCE_B.into(), Vec::new()),
            ])
            .slow(SOURCE_A, Duration::from_millis(200)),
        );
        let service = service(provider.clone(), SearchConfig::default());
        let result =
            tokio::time::timeout(Duration::from_millis(10), service.search(&request(10))).await;
        assert!(result.is_err(), "la llamada del cliente se cancela");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(provider.calls.load(AtomicOrdering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn todos_los_filtros_h05_aplican_and_or_y_unknown_no_coincide() {
        let mut known = candidate(
            SOURCE_A,
            "0192aaaa-0000-7000-8000-000000000331",
            MODEL_A,
            "Qwen Fine",
        );
        known.capabilities = vec!["text.generate".into(), "vision.understand".into()];
        let mut unknown = candidate(
            SOURCE_B,
            "0192aaaa-0000-7000-8000-000000000332",
            MODEL_B,
            "Other Model",
        );
        unknown.parameter_count = None;
        unknown.size_bytes = None;
        unknown.size_confidence = model::ArtifactSizeConfidence::Unknown;
        unknown.trust_score = None;
        unknown.runtime_ids.clear();
        unknown.executable = None;
        unknown.recommended = None;
        unknown.compatibility = CompatibilityFilter::Unknown;
        let provider = Arc::new(MemoryProvider::new(vec![
            (SOURCE_A.into(), vec![known]),
            (SOURCE_B.into(), vec![unknown]),
        ]));
        let service = service(provider, SearchConfig::default());

        let checks: Vec<(Search, usize)> = vec![
            (
                {
                    let mut q = request(50);
                    q.q = "fine".into();
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.capabilities = vec!["vision.understand".into(), "audio.transcribe".into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.source_ids = vec![SOURCE_B.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.compatibility = CompatibilityFilter::Compatible;
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.compatibility = CompatibilityFilter::Unknown;
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.architectures = vec!["QWEN3".into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.formats = vec!["gguf".into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.quantizations = vec!["q4_k_m".into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.licenses = vec!["apache-2.0".into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.runtime_ids = vec![RUNTIME.into()];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.parameter_count.minimum = DecimalU64::new(7_000_000_000);
                    q.parameter_count.maximum = DecimalU64::new(7_000_000_000);
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.size_bytes.minimum = DecimalU64::new(9_000_000_000);
                    q.size_bytes.maximum = DecimalU64::new(10_000_000_000);
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.trust_score.minimum = Some(85);
                    q.trust_score.maximum = Some(85);
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.certification_statuses = vec![CertificationStatus::Uncertified];
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.only_executable = true;
                    q.source_ids = vec![SOURCE_B.into()];
                    q
                },
                0,
            ),
            (
                {
                    let mut q = request(50);
                    q.only_recommended = true;
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                0,
            ),
            (
                {
                    let mut q = request(50);
                    q.show_incompatible = false;
                    q.source_ids = vec![SOURCE_A.into()];
                    q
                },
                1,
            ),
            (
                {
                    let mut q = request(50);
                    q.capabilities = vec!["vision.understand".into()];
                    q.formats = vec!["safetensors".into()];
                    q
                },
                0,
            ),
        ];
        for (query, expected) in checks {
            let result = service.search(&query).await.expect("filtro válido");
            assert_eq!(result.page.items.len(), expected, "{query:?}");
        }
    }

    #[tokio::test]
    async fn cambiar_filtro_invalida_cursor_y_contradiccion_no_consulta_fuentes() {
        let provider = Arc::new(MemoryProvider::new(vec![
            (
                SOURCE_A.into(),
                vec![
                    one(
                        SOURCE_A,
                        "0192aaaa-0000-7000-8000-000000000341",
                        MODEL_A,
                        "A",
                    ),
                    one(
                        SOURCE_A,
                        "0192aaaa-0000-7000-8000-000000000342",
                        MODEL_B,
                        "B",
                    ),
                ],
            ),
            (SOURCE_B.into(), Vec::new()),
        ]));
        let service = service(provider.clone(), SearchConfig::default());
        let first = service.search(&request(1)).await.expect("primera página");
        let mut changed = request(1);
        changed.only_recommended = true;
        changed.cursor = first.page.next_cursor;
        let error = service.search(&changed).await.expect_err("cursor obsoleto");
        assert_eq!(error.details.reason, "bad_cursor");

        let before = provider.calls.load(AtomicOrdering::SeqCst);
        let mut contradiction = request(10);
        contradiction.compatibility = CompatibilityFilter::Incompatible;
        contradiction.show_incompatible = false;
        let result = service
            .search(&contradiction)
            .await
            .expect("contradicción visible vacía");
        assert!(result.page.items.is_empty());
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), before);
    }

    struct ChangingProvider {
        inner: MemoryProvider,
        state: AtomicUsize,
        flip_during_fetch: bool,
    }
    impl ChangingProvider {
        fn new() -> Self {
            Self {
                inner: MemoryProvider::new(vec![
                    (
                        SOURCE_A.into(),
                        vec![
                            one(
                                SOURCE_A,
                                "0192aaaa-0000-7000-8000-000000000311",
                                MODEL_A,
                                "A",
                            ),
                            one(
                                SOURCE_A,
                                "0192aaaa-0000-7000-8000-000000000312",
                                MODEL_B,
                                "B",
                            ),
                        ],
                    ),
                    (
                        SOURCE_B.into(),
                        vec![one(
                            SOURCE_B,
                            "0192aaaa-0000-7000-8000-000000000313",
                            MODEL_C,
                            "C",
                        )],
                    ),
                ]),
                state: AtomicUsize::new(0),
                flip_during_fetch: false,
            }
        }
    }
    #[async_trait]
    impl FederatedSearchProvider for ChangingProvider {
        async fn sources(&self, ids: &[String]) -> Result<Vec<SearchSource>, ApiError> {
            let state = self.state.load(AtomicOrdering::SeqCst);
            Ok(self
                .inner
                .sources(ids)
                .await?
                .into_iter()
                .filter(|s| state != 2 || s.id != SOURCE_A)
                .map(|mut s| {
                    if s.id == SOURCE_A {
                        s.cache_tag = format!("source-configuration-{state}");
                    }
                    s
                })
                .collect())
        }
        async fn search_page(
            &self,
            id: &str,
            q: SourceSearch,
        ) -> Result<Page<SearchCandidate>, ApiError> {
            if id == SOURCE_A && self.state.load(AtomicOrdering::SeqCst) == 3 {
                return Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "Fixture qualification revoked",
                    "fixture_source_revoked",
                ));
            }
            if id == SOURCE_A && self.flip_during_fetch {
                self.state.store(1, AtomicOrdering::SeqCst);
            }
            self.inner.search_page(id, q).await
        }
    }
    #[tokio::test]
    async fn source_fingerprint_change_invalidates_its_cached_query() {
        let provider = Arc::new(ChangingProvider::new());
        let engine = FederatedSearch::new(
            provider.clone(),
            Arc::new(MemoryCursorSigner),
            SearchConfig::default(),
        );
        engine
            .search(&request(1))
            .await
            .expect("initial source snapshot");
        assert_eq!(provider.inner.calls.load(AtomicOrdering::SeqCst), 2);
        provider.state.store(1, AtomicOrdering::SeqCst);
        engine
            .search(&request(1))
            .await
            .expect("new configuration fetches source again");
        assert_eq!(provider.inner.calls.load(AtomicOrdering::SeqCst), 3);
    }
    #[tokio::test]
    async fn source_disabled_after_first_page_rejects_cached_cursor() {
        let provider = Arc::new(ChangingProvider::new());
        let engine = FederatedSearch::new(
            provider.clone(),
            Arc::new(MemoryCursorSigner),
            SearchConfig::default(),
        );
        let first = engine.search(&request(1)).await.expect("initial page");
        provider.state.store(2, AtomicOrdering::SeqCst);
        let mut next = request(1);
        next.cursor = first.page.next_cursor;
        let error = engine
            .search(&next)
            .await
            .expect_err("disabled source cannot serve cached page");
        assert_eq!(error.details.reason, "search_source_snapshot_changed");
    }
    #[tokio::test]
    async fn lost_qualification_does_not_reuse_positive_source_cache_or_hide_other_sources() {
        let provider = Arc::new(ChangingProvider::new());
        let engine = FederatedSearch::new(
            provider.clone(),
            Arc::new(MemoryCursorSigner),
            SearchConfig::default(),
        );
        engine.search(&request(10)).await.expect("initial catalog");
        provider.state.store(3, AtomicOrdering::SeqCst);
        let result = engine
            .search(&request(10))
            .await
            .expect("remaining qualified source");
        assert_eq!(
            result
                .page
                .items
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec![MODEL_C]
        );
        assert!(result
            .warnings
            .iter()
            .any(|w| w.source_id == SOURCE_A && w.reason == "source_unavailable"));
    }
    #[tokio::test]
    async fn configuration_change_during_fetch_prevents_even_the_first_cached_page() {
        let mut p = ChangingProvider::new();
        p.flip_during_fetch = true;
        let engine = FederatedSearch::new(
            Arc::new(p),
            Arc::new(MemoryCursorSigner),
            SearchConfig::default(),
        );
        assert_eq!(
            engine
                .search(&request(1))
                .await
                .expect_err("changed while fetching")
                .details
                .reason,
            "search_source_snapshot_changed"
        );
    }
    #[tokio::test]
    async fn successful_empty_source_with_declared_limitations_is_not_an_upstream_failure() {
        let mut p = MemoryProvider::new(vec![]);
        p.sources[0]
            .support
            .limitations
            .push("core_postfilter_required".into());
        let engine = service(Arc::new(p), SearchConfig::default());
        let result = engine
            .search(&request(5))
            .await
            .expect("actual successful empty query");
        assert!(result.page.items.is_empty());
        assert_eq!(result.warnings[0].reason, "source_search_limited");
    }

    #[tokio::test]
    async fn small_global_pages_share_one_upstream_batch_without_losing_hits() {
        let rows = (0..6)
            .map(|i| {
                candidate(
                    SOURCE_A,
                    &format!("0192aaaa-0000-7000-8000-{:012x}", 400 + i),
                    &format!("0192aaaa-0000-7000-8000-{:012x}", 500 + i),
                    &format!("Model {i}"),
                )
            })
            .collect();
        let provider = Arc::new(MemoryProvider::new(vec![(SOURCE_A.into(), rows)]));
        let engine = service(provider.clone(), SearchConfig::default());
        let mut query = request(1);
        query.source_ids = vec![SOURCE_A.into()];
        let mut ids = std::collections::BTreeSet::new();
        loop {
            let page = engine.search(&query).await.unwrap().page;
            assert_eq!(page.items.len(), 1);
            assert!(ids.insert(page.items[0].id.clone()));
            query.cursor = page.next_cursor;
            if query.cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids.len(), 6);
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(*provider.requested_limits.lock().unwrap(), vec![50]);
    }
}
