//! Implementación de fuentes P048. Las llamadas de red solo leen metadatos o
//! entregan bloques al Download Manager; el adaptador nunca escribe destinos.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::server::CallContext;
use larm_adapter_sdk::source::{
    ArtifactManifest, AuthProbe, AuthResult, DownloadPlanRequest, ModelInspection,
    ModelSourceAdapter, SearchSupport, SourceArtifact, SourceDownloadChunk,
    SourceDownloadChunkRequest, SourceDownloadResumeValidator, SourceReply, SourceRequest,
    SourceSearch, SyncRequest, SyncResult, UpstreamRef,
};
use larm_domain::artifacts::Plan;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{Confidence, StorageEstimate};
use larm_domain::model::{ArtifactSizeConfidence, ArtifactSizeEstimate, ArtifactSizeSource};
use larm_domain::units::DecimalU64;
use reqwest::header::{
    HeaderMap, ACCEPT, ACCEPT_ENCODING, CONTENT_LENGTH, ETAG, IF_RANGE, LAST_MODIFIED, LOCATION,
    RANGE, RETRY_AFTER,
};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::time::timeout;
use url::{Host, Url};
use uuid::Uuid;

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const FS_TIMEOUT: Duration = Duration::from_secs(20);
const JSON_LIMIT: usize = 8 << 20;
const CHUNK_LIMIT: u64 = 512 << 10;
const MAX_RELEASE_PAGES: u32 = 10;
const RELEASE_PAGE_SIZE: usize = 100;
const GITHUB_API: &str = "https://api.github.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceType {
    Github,
    Http,
    Local,
    UncPrivate,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub source_id: String,
    pub source_type: SourceType,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

#[derive(Debug, Clone)]
struct Artifact {
    path: String,
    size: Option<u64>,
    sha256: Option<String>,
    url: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
}

#[derive(Debug, Clone)]
struct Record {
    key: String,
    name: String,
    revision_key: String,
    format: Option<String>,
    architecture: Option<String>,
    artifacts: Vec<Artifact>,
    evidence: String,
}

#[derive(Default)]
struct IdentityStore {
    path: Option<PathBuf>,
    ids: BTreeMap<String, String>,
}

impl IdentityStore {
    fn open(path: PathBuf) -> Self {
        let ids = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            ids,
        }
    }

    fn get(&mut self, key: &str) -> Result<String, LarmError> {
        if let Some(id) = self.ids.get(key) {
            return Ok(id.clone());
        }
        let id = Uuid::now_v7().to_string();
        self.ids.insert(key.to_string(), id.clone());
        if let Some(path) = &self.path {
            let temp = path.with_extension("json.tmp");
            let bytes =
                serde_json::to_vec(&self.ids).map_err(|_| internal("identity_serialize"))?;
            std::fs::write(&temp, bytes).map_err(|_| unavailable("identity_store_write", false))?;
            std::fs::rename(temp, path)
                .map_err(|_| unavailable("identity_store_replace", false))?;
        }
        Ok(id)
    }
}

pub struct P048Adapter {
    manifest: AdapterManifest,
    profile: Profile,
    support: SearchSupport,
    client: reqwest::Client,
    credentials: std::sync::Arc<dyn CredentialReader>,
    identities: Mutex<IdentityStore>,
}

trait CredentialReader: Send + Sync {
    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String>;
}

struct WindowsCredentialReader;

impl CredentialReader for WindowsCredentialReader {
    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
        match keyring::Entry::new(service, username).and_then(|entry| entry.get_password()) {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("credential_manager_unavailable".into()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BasicCredentials {
    username: String,
    password: String,
}

impl P048Adapter {
    pub fn new(
        manifest: AdapterManifest,
        profile: Profile,
        work_dir: &Path,
    ) -> Result<Self, LarmError> {
        Self::new_with_credential_reader(
            manifest,
            profile,
            work_dir,
            std::sync::Arc::new(WindowsCredentialReader),
        )
    }

    fn new_with_credential_reader(
        manifest: AdapterManifest,
        profile: Profile,
        work_dir: &Path,
        credentials: std::sync::Arc<dyn CredentialReader>,
    ) -> Result<Self, LarmError> {
        validate_profile(&profile, &manifest.permissions.network_origins)?;
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("LARM-P048-Source/1.0")
            .build()
            .map_err(|_| unavailable("http_client_build", false))?;
        let support = SearchSupport {
            search_applicable: profile.source_type == SourceType::Github,
            supported_filters: if profile.source_type == SourceType::Github {
                [larm_adapter_sdk::source::TEXT_FILTER.to_string()]
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            },
            limitations: match profile.source_type {
                SourceType::Github => vec![
                    "configured_repository_only".into(),
                    "public_releases_only_pending_p045".into(),
                ],
                SourceType::Http => vec!["explicit_url_import_only".into()],
                SourceType::Local => {
                    vec!["authorized_root_import_only".into(), "p017_scanner".into()]
                }
                SourceType::UncPrivate => {
                    vec!["windows_identity_only".into(), "p017_scanner".into()]
                }
            },
        };
        Ok(Self {
            manifest,
            profile,
            support,
            client,
            credentials,
            identities: Mutex::new(IdentityStore::open(work_dir.join("p048-identities.json"))),
        })
    }

    fn source_id(&self, got: &str) -> Result<(), LarmError> {
        if got == self.profile.source_id {
            Ok(())
        } else {
            Err(invalid("source_identity_mismatch"))
        }
    }

    fn id(&self, key: &str) -> Result<String, LarmError> {
        self.identities
            .lock()
            .map_err(|_| internal("identity_lock"))?
            .get(key)
    }

    async fn records(&self, ctx: &CallContext) -> Result<Vec<Record>, LarmError> {
        if ctx.is_cancelled() {
            return Err(cancelled());
        }
        match self.profile.source_type {
            SourceType::Github => self.github_records(ctx).await,
            SourceType::Http => self.http_records().await,
            SourceType::Local | SourceType::UncPrivate => self.local_records().await,
        }
    }

    async fn local_records(&self) -> Result<Vec<Record>, LarmError> {
        let root_text = self
            .profile
            .root
            .clone()
            .ok_or_else(|| invalid("root_missing"))?;
        let root = timeout(
            FS_TIMEOUT,
            tokio::task::spawn_blocking(move || scan_local_root(&root_text)),
        )
        .await
        .map_err(|_| {
            LarmError::new(
                ErrorCode::Timeout,
                "La carpeta de origen no respondió a tiempo",
                "source_root_timeout",
            )
            .retryable(true)
        })?
        .map_err(|_| unavailable("source_scan_process_failed", false))??;
        Ok(root)
    }

    async fn http_records(&self) -> Result<Vec<Record>, LarmError> {
        let url = self
            .profile
            .url
            .as_ref()
            .ok_or_else(|| invalid("url_missing"))?;
        let response = timeout(
            HTTP_TIMEOUT,
            self.client
                .request(Method::HEAD, url)
                .header(ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| {
            LarmError::new(
                ErrorCode::Timeout,
                "La URL no respondió a tiempo",
                "http_head_timeout",
            )
            .retryable(true)
        })?
        .map_err(|error| unavailable("http_head_failed", true).with_technical(error.to_string()))?;
        if response.status().is_redirection() {
            return Err(forbidden("http_redirect_not_allowed"));
        }
        if !response.status().is_success() && response.status() != StatusCode::METHOD_NOT_ALLOWED {
            return Err(http_status_error(response.status(), response.headers()));
        }
        let headers = response.headers().clone();
        let size = parse_u64_header(&headers, CONTENT_LENGTH.as_str());
        let etag = strong_etag(&headers);
        let last_modified = header_text(&headers, LAST_MODIFIED.as_str());
        let filename = Url::parse(url)
            .ok()
            .and_then(|value| {
                value
                    .path_segments()
                    .and_then(Iterator::last)
                    .map(str::to_string)
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "download.bin".into());
        validate_relative(&filename)?;
        let checksum = self.profile.sha256.clone();
        let revision_hash = sha256(
            format!(
                "{}\0{}\0{}\0{}",
                url,
                etag.as_deref().unwrap_or(""),
                checksum.as_deref().unwrap_or(""),
                size.map_or_else(|| "unknown".to_string(), |n| n.to_string())
            )
            .as_bytes(),
        );
        let artifact = Artifact {
            path: filename,
            size,
            sha256: checksum,
            url: Some(url.clone()),
            etag,
            last_modified,
        };
        Ok(vec![Record {
            key: "explicit-url".into(),
            name: filename_from_url(url),
            revision_key: format!("url:{revision_hash}"),
            format: None,
            architecture: None,
            artifacts: vec![artifact],
            evidence: "http:head".into(),
        }])
    }

    async fn github_records(&self, ctx: &CallContext) -> Result<Vec<Record>, LarmError> {
        let repo = self
            .profile
            .repository
            .as_ref()
            .ok_or_else(|| invalid("repository_missing"))?;
        let mut records = Vec::new();
        for page in 1..=MAX_RELEASE_PAGES {
            if ctx.is_cancelled() {
                return Err(cancelled());
            }
            let url = format!("{GITHUB_API}/repos/{repo}/releases?per_page=100&page={page}");
            let response = timeout(
                HTTP_TIMEOUT,
                self.client
                    .get(url)
                    .header(ACCEPT, "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .send(),
            )
            .await
            .map_err(|_| {
                LarmError::new(
                    ErrorCode::Timeout,
                    "GitHub no respondió a tiempo",
                    "github_releases_timeout",
                )
                .retryable(true)
            })?
            .map_err(|_| unavailable("github_releases_failed", true))?;
            let status = response.status();
            let headers = response.headers().clone();
            if !status.is_success() {
                return Err(github_status_error(status, &headers));
            }
            let bytes = read_bounded(response, JSON_LIMIT).await?;
            let releases: Vec<Value> = serde_json::from_slice(&bytes).map_err(|_| {
                LarmError::new(
                    ErrorCode::ProtocolError,
                    "GitHub devolvió una respuesta no válida",
                    "github_releases_schema",
                )
            })?;
            let count = releases.len();
            records.extend(parse_releases(repo, releases)?);
            if release_page_complete(page, count)? {
                break;
            }
        }
        Ok(records)
    }

    fn record_for(&self, upstream: &UpstreamRef, records: &[Record]) -> Result<Record, LarmError> {
        self.source_id(&upstream.source_id)?;
        if upstream.upstream_namespace != self.namespace() {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "No se encontró el modelo en esta fuente",
                "source_model_not_found",
            ));
        }
        records
            .iter()
            .find(|record| record.key == upstream.upstream_model_id)
            .cloned()
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "No se encontró el modelo en esta fuente",
                    "source_model_not_found",
                )
            })
    }

    async fn plan(
        &self,
        request: DownloadPlanRequest,
        ctx: &CallContext,
    ) -> Result<Plan, LarmError> {
        if request.acquisition.is_none()
            && request
                .optional_artifact_ids
                .as_deref()
                .is_some_and(|ids| !ids.is_empty())
        {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "P048 no puede resolver IDs de artefacto del catálogo central",
                "optional_artifact_binding_owned_by_core",
            ));
        }
        if let Some(source) = request.source_id.as_deref() {
            self.source_id(source)?;
        }
        let records = self.records(ctx).await?;
        self.plan_records(request, records)
    }

    fn plan_records(
        &self,
        request: DownloadPlanRequest,
        mut records: Vec<Record>,
    ) -> Result<Plan, LarmError> {
        let selected = if let Some(binding) = &request.acquisition {
            binding.validate_for(&request)?;
            Some(self.record_for(
                &UpstreamRef {
                    source_id: binding.revision.source_id.clone(),
                    upstream_namespace: binding.revision.upstream_namespace.clone(),
                    upstream_model_id: binding.revision.upstream_model_id.clone(),
                },
                &records,
            )?)
        } else if self.profile.source_type == SourceType::Http {
            // Una URL explícita representa un único modelo. Su variant_id es
            // propiedad del catálogo central; el perfil fija el origen y el
            // plan devuelve revisión/manifest/hash para que el Core los coteje.
            // No exigir aquí un UUID privado que import-only nunca publica.
            if records.len() != 1 {
                return Err(invalid("http_source_record_ambiguous"));
            }
            records.pop()
        } else {
            let mut selected = None;
            for record in records {
                let key = self.variant_key(&record);
                if self.id(&format!("variant:{key}"))? == request.variant_id {
                    selected = Some(record);
                    break;
                }
            }
            selected
        };
        let record = selected.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "La variante ya no está disponible en la fuente",
                "source_variant_not_found",
            )
        })?;
        let artifacts = if let Some(binding) = &request.acquisition {
            binding.select(&request, &artifact_manifest(&record)?)?
        } else {
            source_artifacts(&record)?
        };
        let selected_paths = artifacts
            .iter()
            .map(|a| a.relative_path.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut selected_record = record.clone();
        selected_record
            .artifacts
            .retain(|a| selected_paths.contains(a.path.as_str()));
        if let Some(hard) = request.hard_budget_bytes {
            if let Some(total) = selected_record
                .artifacts
                .iter()
                .map(|a| a.size)
                .try_fold(0u64, |sum, size| sum.checked_add(size?))
            {
                if total > hard.get() {
                    return Err(LarmError::new(
                        ErrorCode::StorageInsufficient,
                        "El tamaño supera el presupuesto indicado",
                        "hard_budget_exceeded",
                    ));
                }
            }
        }
        let spec = larm_adapter_sdk::source::SourceDownloadSpec {
            variant_id: request.variant_id.clone(),
            storage_location_id: request.storage_location_id.clone(),
            revision: larm_adapter_sdk::source::RevisionRef {
                source_id: self.profile.source_id.clone(),
                upstream_namespace: self.namespace(),
                upstream_model_id: record.key.clone(),
                revision_key: record.revision_key.clone(),
            },
            artifacts,
            auth: AuthResult {
                authenticated: false,
                auth_required: false,
                license_required: false,
                reason: "p048_read_only_source".into(),
            },
            resumable: true,
            priority: request.priority,
            hard_budget_bytes: request.hard_budget_bytes,
        };
        let estimate = storage_estimate(&selected_record)?;
        spec.check_execution_requirements(estimate.confidence)?;
        let plan = Plan {
            plan_id: Uuid::now_v7().to_string(),
            plan_hash: "0".repeat(64),
            operation: "source.download".into(),
            resource_ids: vec![
                request.variant_id.clone(),
                request.storage_location_id.clone(),
            ],
            storage_estimate: estimate,
            steps: vec![],
            expires_at: larm_domain::bridge::timestamp(
                chrono::Utc::now() + chrono::Duration::minutes(15),
            ),
            approval_required: false,
        };
        spec.bind(&request, plan, &self.manifest.id)
    }

    fn namespace(&self) -> String {
        match self.profile.source_type {
            SourceType::Github => self.profile.repository.clone().unwrap_or_default(),
            SourceType::Http => "direct-http".into(),
            SourceType::Local => "local".into(),
            SourceType::UncPrivate => "unc".into(),
        }
    }

    fn variant_key(&self, record: &Record) -> String {
        format!(
            "{}\0{}\0{}",
            self.namespace(),
            record.key,
            record.revision_key
        )
    }

    async fn chunk(
        &self,
        request: SourceDownloadChunkRequest,
        ctx: &CallContext,
    ) -> Result<SourceDownloadChunk, LarmError> {
        self.source_id(&request.source_id)?;
        if request.max_bytes.get() > CHUNK_LIMIT {
            return Err(invalid("chunk_max_bytes"));
        }
        if ctx.is_cancelled() {
            return Err(cancelled());
        }
        let upstream = UpstreamRef {
            source_id: request.source_id.clone(),
            upstream_namespace: request.revision.upstream_namespace.clone(),
            upstream_model_id: request.revision.upstream_model_id.clone(),
        };
        let records = self.records(ctx).await?;
        let record = self.record_for(&upstream, &records)?;
        if record.revision_key != request.revision.revision_key {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La fuente cambió desde que se preparó el plan",
                "source_revision_changed",
            ));
        }
        let artifact = record
            .artifacts
            .iter()
            .find(|a| a.path == request.relative_path)
            .ok_or_else(|| invalid("artifact_path_not_in_manifest"))?;
        if self.profile.source_type == SourceType::Local
            || self.profile.source_type == SourceType::UncPrivate
        {
            return self.local_chunk(artifact.clone(), request).await;
        }
        self.remote_chunk(artifact.clone(), request).await
    }

    async fn local_chunk(
        &self,
        artifact: Artifact,
        request: SourceDownloadChunkRequest,
    ) -> Result<SourceDownloadChunk, LarmError> {
        let root = self
            .profile
            .root
            .clone()
            .ok_or_else(|| invalid("root_missing"))?;
        let rel = artifact.path.clone();
        let offset = request.offset.get();
        let max = request.max_bytes.get();
        let task = tokio::task::spawn_blocking(move || read_local_chunk(&root, &rel, offset, max));
        let (bytes, total) = timeout(FS_TIMEOUT, task)
            .await
            .map_err(|_| {
                LarmError::new(
                    ErrorCode::Timeout,
                    "La carpeta de origen no respondió a tiempo",
                    "source_chunk_timeout",
                )
                .retryable(true)
            })?
            .map_err(|_| unavailable("source_chunk_process_failed", false))??;
        let end = offset.saturating_add(bytes.len() as u64);
        let validator = artifact
            .sha256
            .map(|sha| SourceDownloadResumeValidator::StrongEtag {
                value: format!("\"sha256-{sha}\""),
            });
        // El Core compara el validador observado antes de guardar estos bytes
        // y reinicia desde cero si cambió; el worker no mezcla parciales.
        Ok(SourceDownloadChunk {
            offset: request.offset,
            data_base64: STANDARD.encode(bytes),
            eof: end == total,
            total_bytes: DecimalU64::new(total),
            validator,
        })
    }

    async fn remote_chunk(
        &self,
        artifact: Artifact,
        request: SourceDownloadChunkRequest,
    ) -> Result<SourceDownloadChunk, LarmError> {
        let mut url = artifact
            .url
            .clone()
            .ok_or_else(|| invalid("artifact_url_missing"))?;
        let client = self.client.clone();
        let offset = request.offset.get();
        let max = request.max_bytes.get();
        let mut response = None;
        for _ in 0..=3 {
            let end = offset
                .checked_add(max.saturating_sub(1))
                .ok_or_else(|| invalid("chunk_range_overflow"))?;
            let mut chunk_request = client
                .get(&url)
                .header(ACCEPT_ENCODING, "identity")
                .header(RANGE, format!("bytes={offset}-{end}"));
            if offset > 0 {
                if let Some(validator) = &request.validator {
                    let value = match validator {
                        SourceDownloadResumeValidator::StrongEtag { value } => value,
                        SourceDownloadResumeValidator::LastModifiedAndSha256 {
                            last_modified,
                            ..
                        } => last_modified,
                    };
                    chunk_request = chunk_request.header(IF_RANGE, value);
                }
            }
            let sent = timeout(HTTP_TIMEOUT, chunk_request.send())
                .await
                .map_err(|_| {
                    LarmError::new(
                        ErrorCode::Timeout,
                        "La fuente no respondió al bloque",
                        "source_chunk_timeout",
                    )
                    .retryable(true)
                })?
                .map_err(|_| unavailable("source_chunk_failed", true))?;
            if sent.status().is_redirection() {
                let location = sent
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| forbidden("asset_redirect_location_invalid"))?;
                let next = Url::parse(&url)
                    .and_then(|base| base.join(location))
                    .map_err(|_| forbidden("asset_redirect_url_invalid"))?;
                if self.profile.source_type != SourceType::Github || !github_asset_origin(&next) {
                    return Err(forbidden("asset_redirect_origin_forbidden"));
                }
                url = next.to_string();
                continue;
            }
            response = Some(sent);
            break;
        }
        let response = response.ok_or_else(|| forbidden("asset_redirect_limit"))?;
        let status = response.status();
        let headers = response.headers().clone();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return unsatisfied_range_reply(&headers, &artifact, &request);
        }
        if offset > 0 && status == StatusCode::OK {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La fuente no validó la reanudación del bloque",
                "source_resume_validator_changed",
            ));
        }
        if !(status == StatusCode::PARTIAL_CONTENT || (offset == 0 && status == StatusCode::OK)) {
            return Err(http_status_error(status, &headers));
        }
        let (bytes, overflow) = read_chunk_bounded(response, max as usize).await?;
        if overflow {
            return Err(LarmError::new(
                ErrorCode::ProtocolError,
                "La fuente superó el tamaño máximo del bloque",
                "source_chunk_over_limit",
            ));
        }
        let mut total = if status == StatusCode::OK {
            parse_u64_header(&headers, CONTENT_LENGTH.as_str())
        } else {
            let (range_start, range_end, range_total) =
                parse_content_range(&headers).ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ProtocolError,
                        "La fuente devolvió un Content-Range inválido",
                        "source_content_range_invalid",
                    )
                })?;
            let declared_length = range_end
                .checked_sub(range_start)
                .and_then(|n| n.checked_add(1));
            if range_start != offset
                || declared_length != Some(bytes.len() as u64)
                || range_total.is_some_and(|size| range_end >= size)
            {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "El bloque no coincide con el rango solicitado",
                    "source_content_range_mismatch",
                ));
            }
            range_total
        };
        let etag = strong_etag(&headers).or(artifact.etag.clone());
        let validator = etag
            .map(|value| SourceDownloadResumeValidator::StrongEtag { value })
            .or_else(|| {
                artifact
                    .sha256
                    .clone()
                    .zip(
                        header_text(&headers, LAST_MODIFIED.as_str())
                            .or(artifact.last_modified.clone()),
                    )
                    .map(|(sha256, last_modified)| {
                        SourceDownloadResumeValidator::LastModifiedAndSha256 {
                            last_modified,
                            sha256,
                        }
                    })
            });
        // Una respuesta 206 conserva el offset exacto; el Core descarta el
        // bloque y su parcial si este validador difiere del checkpoint.
        let end = offset.saturating_add(bytes.len() as u64);
        let eof = total.is_some_and(|size| end == size)
            || ((status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT)
                && total.is_none()
                && bytes.len() < max as usize);
        if eof && total.is_none() {
            total = Some(end);
        }
        if !eof && bytes.is_empty() {
            return Err(unavailable("source_empty_chunk", true));
        }
        Ok(SourceDownloadChunk {
            offset: request.offset,
            data_base64: STANDARD.encode(bytes),
            eof,
            total_bytes: total.and_then(DecimalU64::new),
            validator,
        })
    }
}

#[async_trait]
impl ModelSourceAdapter for P048Adapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }
    fn search_support(&self) -> &SearchSupport {
        &self.support
    }

    async fn invoke(
        &self,
        request: SourceRequest,
        ctx: CallContext,
    ) -> Result<SourceReply, LarmError> {
        if matches!(
            self.profile.source_type,
            SourceType::Http | SourceType::Github
        ) {
            // Estas operaciones sólo tienen IO de red asíncrono: soltar el
            // futuro cierra la lectura sin dejar tareas de filesystem vivas.
            tokio::select! {
                biased;
                () = ctx.cancelled() => Err(cancelled()),
                result = self.invoke_request(request, ctx.clone()) => result,
            }
        } else {
            self.invoke_request(request, ctx).await
        }
    }
}

impl P048Adapter {
    async fn invoke_request(
        &self,
        request: SourceRequest,
        ctx: CallContext,
    ) -> Result<SourceReply, LarmError> {
        match request {
            SourceRequest::Search(query) => self.search(query, &ctx).await,
            SourceRequest::GetModel(query) => {
                let records = self.records(&ctx).await?;
                let record = self.record_for(&query, &records)?;
                SourceReply::parse(&SourceRequest::GetModel(query), model_inspection(&record)?)
            }
            SourceRequest::ListRevisions(query) => {
                let records = self.records(&ctx).await?;
                let record = self.record_for(&query, &records)?;
                SourceReply::parse(
                    &SourceRequest::ListRevisions(query),
                    self.revisions(&record)?,
                )
            }
            SourceRequest::GetManifest(query) => {
                self.source_id(&query.source_id)?;
                let records = self.records(&ctx).await?;
                let upstream = UpstreamRef {
                    source_id: query.source_id.clone(),
                    upstream_namespace: query.upstream_namespace.clone(),
                    upstream_model_id: query.upstream_model_id.clone(),
                };
                let record = self.record_for(&upstream, &records)?;
                if record.revision_key != query.revision_key {
                    return Err(LarmError::new(
                        ErrorCode::StaleRevision,
                        "La revisión ya no está disponible",
                        "source_revision_changed",
                    ));
                }
                let value = serde_json::to_value(artifact_manifest(&record)?)
                    .map_err(|_| internal("manifest_serialize"))?;
                SourceReply::parse(&SourceRequest::GetManifest(query), value)
            }
            SourceRequest::EstimateSize(query) => {
                self.source_id(&query.source_id)?;
                let records = self.records(&ctx).await?;
                let upstream = UpstreamRef {
                    source_id: query.source_id.clone(),
                    upstream_namespace: query.upstream_namespace.clone(),
                    upstream_model_id: query.upstream_model_id.clone(),
                };
                let record = self.record_for(&upstream, &records)?;
                if record.revision_key != query.revision_key {
                    return Err(LarmError::new(
                        ErrorCode::StaleRevision,
                        "La revisión ya no está disponible",
                        "source_revision_changed",
                    ));
                }
                let value = serde_json::to_value(estimate(&record)?)
                    .map_err(|_| internal("estimate_serialize"))?;
                SourceReply::parse(&SourceRequest::EstimateSize(query), value)
            }
            SourceRequest::DownloadPlan(query) => {
                Ok(SourceReply::DownloadPlan(self.plan(query, &ctx).await?))
            }
            SourceRequest::Download(query) => {
                let _ = query;
                Err(LarmError::new(
                    ErrorCode::UnsupportedCapability,
                    "La ejecución pertenece al Download Manager",
                    "download_execution_owned_by_download_manager",
                ))
            }
            SourceRequest::DownloadChunk(query) => {
                let offset = query.offset.get();
                ctx.progress(
                    "SOURCE_READ",
                    Some(offset),
                    None,
                    Some("source_chunk_start"),
                );
                let chunk = self.chunk(query, &ctx).await?;
                let size = STANDARD
                    .decode(&chunk.data_base64)
                    .map_err(|_| internal("chunk_encoding_invalid"))?
                    .len() as u64;
                ctx.progress(
                    "SOURCE_READ",
                    offset.checked_add(size),
                    chunk.total_bytes.map(|value| value.get()),
                    Some("source_chunk_complete"),
                );
                Ok(SourceReply::DownloadChunk(chunk))
            }
            SourceRequest::Sync(query) => self.sync(query, &ctx).await,
            SourceRequest::AuthProbe(query) => self.auth_probe(query).await,
        }
    }
}

impl P048Adapter {
    async fn search(
        &self,
        query: SourceSearch,
        ctx: &CallContext,
    ) -> Result<SourceReply, LarmError> {
        self.source_id(&query.source_id)?;
        if self.profile.source_type != SourceType::Github {
            return Err(unsupported("import_only_search"));
        }
        if !query.capabilities.is_empty() {
            return Err(unsupported("github_capability_filter_unsupported"));
        }
        let records = self.records(ctx).await?;
        let needle = query.q.to_lowercase();
        let start = query
            .cursor
            .as_deref()
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| invalid("cursor_invalid"))?
            .unwrap_or(0);
        let mut variants = Vec::new();
        for record in records
            .into_iter()
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
        {
            let key = self.variant_key(&record);
            variants.push(model_variant(
                &record,
                &self.id(&format!("revision:{key}"))?,
                &self.id(&format!("variant:{key}"))?,
                &self.namespace(),
                &self.profile.source_id,
            )?);
        }
        let filtered_total = variants.len();
        let page_items: Vec<Value> = variants
            .into_iter()
            .skip(start)
            .take(usize::from(query.limit))
            .collect();
        let next = if start + page_items.len() < filtered_total {
            Some((start + page_items.len()).to_string())
        } else {
            None
        };
        let value = json!({"items":page_items,"next_cursor":next,"snapshot_at":now()});
        SourceReply::parse(&SourceRequest::Search(query), value)
    }

    fn revisions(&self, record: &Record) -> Result<Value, LarmError> {
        let key = self.variant_key(record);
        let revision_id = self.id(&format!("revision:{key}"))?;
        let distribution_id = self.id(&format!("distribution:{}", self.namespace()))?;
        let manifest = artifact_manifest(record)?;
        let item = json!({"id":revision_id,"created_at":now(),"updated_at":now(),"row_version":1,"metadata":{},"distribution_id":distribution_id,"revision_key":record.revision_key,"upstream_commit":null,"manifest_hash":manifest.manifest_hash,"state":"CANDIDATE"});
        Ok(json!({"items":[item],"next_cursor":null,"snapshot_at":now()}))
    }

    async fn sync(&self, query: SyncRequest, ctx: &CallContext) -> Result<SourceReply, LarmError> {
        self.source_id(&query.source_id)?;
        if query.query.keys().any(|key| key != "q") {
            return Err(unsupported("source_sync_query_unsupported"));
        }
        let needle = query
            .query
            .get("q")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let records: Vec<Record> = self
            .records(ctx)
            .await?
            .into_iter()
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
            .collect();
        let digest_value = serde_json::to_value(
            records
                .iter()
                .map(|r| (&r.key, &r.revision_key))
                .collect::<Vec<_>>(),
        )
        .map_err(|_| internal("sync_serialize"))?;
        let digest =
            larm_domain::canonical::sha256_hex(&digest_value).map_err(|_| internal("sync_hash"))?;
        let changed = query.etag.as_deref() != Some(&digest);
        let cursor = query
            .cursor
            .as_deref()
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| invalid("cursor_invalid"))?
            .unwrap_or(0);
        let models: Vec<ModelInspection> = if changed || cursor > 0 {
            records
                .iter()
                .skip(cursor)
                .take(200)
                .map(|record| {
                    serde_json::from_value(model_inspection(record)?)
                        .map_err(|_| internal("sync_model_decode"))
                })
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let next = if (changed || cursor > 0) && cursor + models.len() < records.len() {
            Some((cursor + models.len()).to_string())
        } else {
            None
        };
        let result = SyncResult {
            cursor: next,
            etag: Some(digest),
            changed,
            models,
            rate_limit: None,
        };
        let value = serde_json::to_value(result).map_err(|_| internal("sync_serialize"))?;
        SourceReply::parse(&SourceRequest::Sync(query), value)
    }

    async fn auth_probe(&self, query: AuthProbe) -> Result<SourceReply, LarmError> {
        self.source_id(&query.source_id)?;
        let result = match (&self.profile.source_type, &query.auth_mode) {
            (SourceType::UncPrivate, larm_adapter_sdk::source::AuthMode::WindowsIntegrated) => {
                let root = self.profile.root.as_deref().unwrap_or_default();
                let root = root.to_string();
                let reachable = timeout(
                    FS_TIMEOUT,
                    tokio::task::spawn_blocking(move || Path::new(&root).is_dir()),
                )
                .await
                .map_err(|_| {
                    LarmError::new(
                        ErrorCode::Timeout,
                        "La carpeta de red no respondió a tiempo",
                        "source_auth_probe_timeout",
                    )
                    .retryable(true)
                })?
                .map_err(|_| unavailable("source_auth_probe_process_failed", false))?;
                AuthResult {
                    authenticated: reachable,
                    auth_required: !reachable,
                    license_required: false,
                    reason: if reachable {
                        "windows_worker_identity_has_read_access"
                    } else {
                        "windows_worker_identity_access_unavailable"
                    }
                    .into(),
                }
            }
            (_, larm_adapter_sdk::source::AuthMode::None) => AuthResult {
                authenticated: false,
                auth_required: false,
                license_required: false,
                reason: "anonymous_or_local_source".into(),
            },
            (SourceType::Http, larm_adapter_sdk::source::AuthMode::Basic) => {
                let account_id = query
                    .account_id
                    .as_deref()
                    .ok_or_else(|| invalid("source_auth_account_missing"))?;
                if query.credential_reference_id.is_none() {
                    return Err(invalid("source_auth_reference_missing"));
                }
                let url = Url::parse(self.profile.url.as_deref().unwrap_or_default())
                    .map_err(|_| invalid("source_auth_url_invalid"))?;
                if url.scheme() != "https" && !is_loopback_url(&url) {
                    return Err(LarmError::new(
                        ErrorCode::UnsupportedCapability,
                        "La autenticación Basic requiere HTTPS",
                        "basic_requires_https",
                    ));
                }
                let service = format!("larm/source/{}", self.profile.source_id);
                let secret = match self.credentials.read(&service, account_id).map_err(|_| {
                    LarmError::new(
                        ErrorCode::AuthBackendUnavailable,
                        "Credential Manager no está disponible",
                        "source_auth_backend_unavailable",
                    )
                })? {
                    Some(secret) => secret,
                    None => {
                        let result = AuthResult {
                            authenticated: false,
                            auth_required: true,
                            license_required: false,
                            reason: "credential_unavailable".into(),
                        };
                        return SourceReply::parse(
                            &SourceRequest::AuthProbe(query),
                            serde_json::to_value(result)
                                .map_err(|_| internal("auth_probe_serialize"))?,
                        );
                    }
                };
                if !secret.is_empty() {
                    let credentials: BasicCredentials =
                        serde_json::from_str(&secret).map_err(|_| {
                            LarmError::new(
                                ErrorCode::AuthBackendUnavailable,
                                "La credencial guardada no tiene el formato esperado",
                                "source_credential_malformed",
                            )
                        })?;
                    if credentials.username.is_empty() || credentials.password.is_empty() {
                        return Err(LarmError::new(
                            ErrorCode::AuthBackendUnavailable,
                            "La credencial guardada no tiene el formato esperado",
                            "source_credential_malformed",
                        ));
                    }
                    let response = self
                        .client
                        .head(url)
                        .basic_auth(&credentials.username, Some(&credentials.password))
                        .send()
                        .await
                        .map_err(|error| {
                            if error.is_timeout() {
                                LarmError::new(
                                    ErrorCode::Timeout,
                                    "La fuente no respondió a tiempo",
                                    "source_auth_probe_timeout",
                                )
                                .retryable(true)
                            } else {
                                LarmError::new(
                                    ErrorCode::UpstreamUnavailable,
                                    "No se pudo comprobar la autenticación de la fuente",
                                    "source_auth_probe_upstream_unavailable",
                                )
                                .retryable(true)
                            }
                        })?;
                    let status = response.status();
                    if status == StatusCode::UNAUTHORIZED {
                        AuthResult {
                            authenticated: false,
                            auth_required: true,
                            license_required: false,
                            reason: "credentials_rejected".into(),
                        }
                    } else if status == StatusCode::FORBIDDEN {
                        AuthResult {
                            authenticated: false,
                            auth_required: false,
                            license_required: true,
                            reason: "gated_access_required".into(),
                        }
                    } else if status.is_success() {
                        AuthResult {
                            authenticated: true,
                            auth_required: false,
                            license_required: false,
                            reason: "authenticated".into(),
                        }
                    } else {
                        AuthResult {
                            authenticated: false,
                            auth_required: false,
                            license_required: false,
                            reason: format!("source_http_status_{}", status.as_u16()),
                        }
                    }
                } else {
                    return Err(LarmError::new(
                        ErrorCode::AuthBackendUnavailable,
                        "La credencial guardada no tiene el formato esperado",
                        "source_credential_malformed",
                    ));
                }
            }
            _ => {
                return Err(LarmError::new(
                    ErrorCode::UnsupportedCapability,
                    "Este adaptador no admite el mecanismo de autenticación solicitado",
                    "unsupported_auth",
                ));
            }
        };
        SourceReply::parse(
            &SourceRequest::AuthProbe(query),
            serde_json::to_value(result).map_err(|_| internal("auth_probe_serialize"))?,
        )
    }
}

fn is_loopback_url(url: &Url) -> bool {
    url.host().is_some_and(|host| match host {
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
    })
}

fn validate_profile(profile: &Profile, allowed_origins: &[String]) -> Result<(), LarmError> {
    larm_domain::bridge::is_uuid_v7(&profile.source_id)
        .then_some(())
        .ok_or_else(|| invalid("source_id_invalid"))?;
    match profile.source_type {
        SourceType::Github => {
            let repo = profile
                .repository
                .as_deref()
                .ok_or_else(|| invalid("repository_missing"))?;
            let mut parts = repo.split('/');
            let valid = parts.next().is_some_and(valid_repo_part)
                && parts.next().is_some_and(valid_repo_part)
                && parts.next().is_none();
            if !valid {
                return Err(invalid("repository_invalid"));
            }
            for origin in [
                "https://api.github.com:443",
                "https://github.com:443",
                "https://objects.githubusercontent.com:443",
                "https://release-assets.githubusercontent.com:443",
            ] {
                if !allowed_origins.iter().any(|allowed| allowed == origin) {
                    return Err(forbidden("github_network_origin_not_permitted"));
                }
            }
        }
        SourceType::Http => {
            let url = Url::parse(
                profile
                    .url
                    .as_deref()
                    .ok_or_else(|| invalid("url_missing"))?,
            )
            .map_err(|_| invalid("url_invalid"))?;
            if !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || !safe_http_origin(&url)
            {
                return Err(forbidden("http_url_origin_forbidden"));
            }
            if url.query_pairs().any(|(key, _)| sensitive_query_key(&key)) {
                return Err(forbidden("http_url_secret_in_query"));
            }
            if !allowed_origins
                .iter()
                .any(|allowed| allowed == &origin_string(&url))
            {
                return Err(forbidden("http_network_origin_not_permitted"));
            }
            if let Some(checksum) = &profile.sha256 {
                if !is_sha256(checksum) {
                    return Err(invalid("sha256_invalid"));
                }
            }
        }
        SourceType::Local | SourceType::UncPrivate => {
            let root = profile
                .root
                .as_deref()
                .ok_or_else(|| invalid("root_missing"))?;
            validate_root_syntax(root, profile.source_type)?;
        }
    }
    Ok(())
}

fn valid_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

fn validate_root_syntax(root: &str, source_type: SourceType) -> Result<(), LarmError> {
    if root.trim() != root
        || root.is_empty()
        || root.contains(':') && !is_drive_colon(root)
        || root.replace('/', "\\").split('\\').any(|part| part == "..")
        || root.starts_with(r"\\.\")
        || root.starts_with(r"\?")
    {
        return Err(invalid("root_path_invalid"));
    }
    let unc = root.starts_with(r"\\") || root.starts_with("//");
    if (source_type == SourceType::UncPrivate && !unc)
        || (source_type == SourceType::Local && (!Path::new(root).is_absolute() || unc))
    {
        return Err(invalid("root_path_kind_mismatch"));
    }
    Ok(())
}

fn is_drive_colon(root: &str) -> bool {
    let bytes = root.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && !root[2..].contains(':')
}

fn sensitive_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "token",
        "accesstoken",
        "refreshtoken",
        "signature",
        "secret",
        "credential",
        "password",
        "apikey",
        "cookie",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn origin_string(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    format!(
        "{}://{}:{}",
        url.scheme(),
        host,
        url.port_or_known_default().unwrap_or_default()
    )
}

fn safe_http_origin(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
            || host == "[::1]"
            || host == "::1"
    })
}

pub fn validate_relative(path: &str) -> Result<(), LarmError> {
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains(':')
        || path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with('.')
                || part.ends_with(' ')
                || part.chars().any(char::is_control)
        })
    {
        return Err(invalid("artifact_path_invalid"));
    }
    larm_domain::model::bundle_path(path).map_err(|_| invalid("artifact_path_invalid"))?;
    Ok(())
}

fn scan_local_root(root: &str) -> Result<Vec<Record>, LarmError> {
    let path = PathBuf::from(root);
    let meta =
        std::fs::metadata(&path).map_err(|_| unavailable("source_root_unavailable", true))?;
    if !meta.is_dir() {
        return Err(invalid("source_root_not_directory"));
    }
    if is_reparse(
        &std::fs::symlink_metadata(&path)
            .map_err(|_| unavailable("source_root_unavailable", true))?,
    ) {
        return Err(forbidden("source_root_reparse_point"));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(part, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&current)
            .map_err(|_| unavailable("source_root_unavailable", true))?;
        if is_reparse(&meta) {
            return Err(forbidden("source_root_reparse_point"));
        }
    }
    let canonical =
        std::fs::canonicalize(&path).map_err(|_| unavailable("source_root_unavailable", true))?;
    let preview = larm_models::import::preview_dir(&canonical)
        .map_err(|_| unavailable("source_scan_failed", true))?;
    let mut records = Vec::new();
    for variant in preview.variants {
        let mut artifacts = Vec::new();
        for file in variant.files {
            validate_relative(&file.relative_path)?;
            let safe = safe_local_file(&canonical, &file.relative_path)?;
            let file_meta = std::fs::metadata(&safe)
                .map_err(|_| unavailable("source_file_unavailable", true))?;
            let modified = file_meta
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis().to_string());
            artifacts.push(Artifact {
                path: file.relative_path,
                size: Some(file.size_bytes.get()),
                sha256: None,
                url: None,
                etag: None,
                last_modified: modified,
            });
        }
        if artifacts.is_empty() {
            continue;
        }
        artifacts.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest_hash = artifact_hash(&artifacts)?;
        let path_key = format!(
            "{}|{}|{}",
            variant.dir,
            variant.format,
            artifacts
                .iter()
                .map(|a| a.path.as_str())
                .collect::<Vec<_>>()
                .join("|")
        );
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path_key.as_bytes());
        records.push(Record {
            key: format!(
                "{}:{encoded}",
                if root.starts_with(r"\\") {
                    "unc"
                } else {
                    "local"
                }
            ),
            name: variant.name,
            revision_key: format!("local:{manifest_hash}"),
            format: Some(variant.format),
            architecture: variant.architecture,
            artifacts,
            evidence: "p017:local_observation".into(),
        });
    }
    Ok(records)
}

fn safe_local_file(root: &Path, relative: &str) -> Result<PathBuf, LarmError> {
    validate_relative(relative)?;
    let mut candidate = root.to_path_buf();
    for part in relative.split('/') {
        candidate.push(part);
    }
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current)
            .map_err(|_| unavailable("source_file_unavailable", true))?;
        if is_reparse(&meta) {
            return Err(forbidden("source_file_reparse_point"));
        }
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|_| unavailable("source_file_unavailable", true))?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(forbidden("source_path_escape"));
    }
    Ok(canonical)
}

fn read_local_chunk(
    root: &str,
    relative: &str,
    offset: u64,
    max: u64,
) -> Result<(Vec<u8>, u64), LarmError> {
    let root =
        std::fs::canonicalize(root).map_err(|_| unavailable("source_root_unavailable", true))?;
    let path = safe_local_file(&root, relative)?;
    let mut file =
        std::fs::File::open(path).map_err(|_| unavailable("source_file_unavailable", true))?;
    let total = file
        .metadata()
        .map_err(|_| unavailable("source_file_unavailable", true))?
        .len();
    if offset > total {
        return Err(invalid("source_offset_past_eof"));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| unavailable("source_file_seek_failed", true))?;
    let mut bytes = vec![0; max.min(CHUNK_LIMIT) as usize];
    let read = file
        .read(&mut bytes)
        .map_err(|_| unavailable("source_file_read_failed", true))?;
    bytes.truncate(read);
    Ok((bytes, total))
}

#[cfg(windows)]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

fn parse_releases(repo: &str, releases: Vec<Value>) -> Result<Vec<Record>, LarmError> {
    let mut records = Vec::new();
    for release in releases {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let id = release.get("id").and_then(Value::as_u64).ok_or_else(|| {
            LarmError::new(
                ErrorCode::ProtocolError,
                "GitHub omitió el ID de publicación",
                "github_release_id_missing",
            )
        })?;
        let tag = release
            .get("tag_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ProtocolError,
                    "GitHub omitió la etiqueta de publicación",
                    "github_release_tag_missing",
                )
            })?;
        let name = release
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(tag)
            .to_string();
        let assets = release
            .get("assets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut artifacts = Vec::new();
        for asset in assets {
            if asset
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|v| v != "uploaded")
            {
                continue;
            }
            let path = asset
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ProtocolError,
                        "GitHub devolvió un activo sin nombre",
                        "github_asset_name_missing",
                    )
                })?
                .to_string();
            validate_relative(&path)?;
            let url = asset
                .get("browser_download_url")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ProtocolError,
                        "GitHub devolvió un activo sin URL",
                        "github_asset_url_missing",
                    )
                })?;
            let parsed = Url::parse(url).map_err(|_| forbidden("github_asset_url_invalid"))?;
            if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
                return Err(forbidden("github_asset_origin_invalid"));
            }
            let digest = asset
                .get("digest")
                .and_then(Value::as_str)
                .and_then(|v| v.strip_prefix("sha256:"))
                .filter(|v| is_sha256(v))
                .map(str::to_lowercase);
            let size = asset
                .get("size")
                .and_then(Value::as_u64)
                .filter(|n| *n <= i64::MAX as u64);
            artifacts.push(Artifact {
                path,
                size,
                sha256: digest,
                url: Some(url.to_string()),
                etag: None,
                last_modified: None,
            });
        }
        if artifacts.is_empty() {
            continue;
        }
        artifacts.sort_by(|a, b| a.path.cmp(&b.path));
        if artifacts
            .windows(2)
            .any(|v| v[0].path.eq_ignore_ascii_case(&v[1].path))
        {
            return Err(LarmError::new(
                ErrorCode::ProtocolError,
                "La publicación repite nombres de activos",
                "github_asset_name_duplicate",
            ));
        }
        let tag_key = if tag.chars().any(char::is_whitespace) {
            return Err(LarmError::new(
                ErrorCode::ProtocolError,
                "La etiqueta no se puede fijar de forma segura",
                "github_release_tag_invalid",
            ));
        } else {
            format!(
                "release:{id}:{}:{}",
                sha256(tag.as_bytes()),
                artifact_hash(&artifacts)?
            )
        };
        let _ = repo;
        records.push(Record {
            key: tag.to_string(),
            name,
            revision_key: tag_key,
            format: None,
            architecture: None,
            evidence: format!("github:release:{id}"),
            artifacts,
        });
    }
    Ok(records)
}

fn release_page_complete(page: u32, count: usize) -> Result<bool, LarmError> {
    if count < RELEASE_PAGE_SIZE {
        Ok(true)
    } else if page >= MAX_RELEASE_PAGES {
        Err(unavailable("github_release_page_limit", true))
    } else {
        Ok(false)
    }
}

fn source_artifacts(record: &Record) -> Result<Vec<SourceArtifact>, LarmError> {
    record
        .artifacts
        .iter()
        .map(|a| {
        let size_bytes = if a.sha256.is_some() {
            a.size.and_then(DecimalU64::new)
        } else {
            None
        };
        let size_estimate = Some(match (a.size, a.sha256.as_deref()) {
                (Some(size), Some(_)) => ArtifactSizeEstimate {
                    confidence: ArtifactSizeConfidence::Exact,
                    bytes: DecimalU64::new(size),
                    lower_bound_bytes: DecimalU64::new(size),
                    upper_bound_bytes: DecimalU64::new(size),
                    source: ArtifactSizeSource::Manifest,
                    evidence_refs: vec![record.evidence.clone()],
                    reason: Some("Tamaño y hash publicados o calculados para esta revisión".into()),
                },
            (Some(size), None) if record.evidence.starts_with("p017:") => ArtifactSizeEstimate {
                confidence: ArtifactSizeConfidence::Estimated,
                bytes: DecimalU64::new(size),
                lower_bound_bytes: None,
                upper_bound_bytes: DecimalU64::new(size),
                source: ArtifactSizeSource::LocalObservation,
                evidence_refs: vec![record.evidence.clone()],
                reason: Some("Tamaño observado por P017; el archivo puede cambiar antes de la lectura".into()),
            },
            (Some(size), None) => ArtifactSizeEstimate {
                    confidence: ArtifactSizeConfidence::High,
                    bytes: DecimalU64::new(size),
                    lower_bound_bytes: None,
                    upper_bound_bytes: DecimalU64::new(size),
                    source: size_source(record),
                    evidence_refs: vec![record.evidence.clone()],
                    reason: Some("Tamaño observado por la fuente; no hay hash utilizable".into()),
                },
                (None, _) => ArtifactSizeEstimate::unknown(
                    "La fuente no publica un tamaño verificable",
                    record.evidence.clone(),
                ),
            });
            Ok(SourceArtifact {
                relative_path: a.path.clone(),
                size_bytes,
                size_estimate,
                sha256: a.sha256.clone(),
                required: true,
                role: artifact_role(&a.path).into(),
            })
        })
        .collect()
}

fn size_source(record: &Record) -> ArtifactSizeSource {
    if record.evidence.starts_with("http:") {
        ArtifactSizeSource::HttpHead
    } else if record.evidence.starts_with("github:") {
        ArtifactSizeSource::SourceMetadata
    } else {
        ArtifactSizeSource::LocalObservation
    }
}

fn artifact_manifest(record: &Record) -> Result<ArtifactManifest, LarmError> {
    let artifacts = source_artifacts(record)?;
    let value = serde_json::to_value(&artifacts).map_err(|_| internal("manifest_serialize"))?;
    let manifest_hash =
        larm_domain::canonical::sha256_hex(&value).map_err(|_| internal("manifest_hash"))?;
    Ok(ArtifactManifest {
        revision_key: record.revision_key.clone(),
        manifest_hash,
        artifacts,
    })
}

fn model_inspection(record: &Record) -> Result<Value, LarmError> {
    // ModelInspection has a deliberately closed schema that predates the
    // manifest size-estimate extension. Keep estimates on get_manifest and
    // estimate_size, while sync/get_model only expose the compatible fields.
    let mut artifacts = source_artifacts(record)?;
    for artifact in &mut artifacts {
        artifact.size_estimate = None;
    }
    Ok(
        json!({"upstream_identity":record.key,"architecture":record.architecture,"format":record.format,"quantization":null,"capabilities":[],"limits":{},"artifacts":artifacts,"license":null,"evidence_refs":[record.evidence]}),
    )
}

fn model_variant(
    record: &Record,
    revision_id: &str,
    variant_id: &str,
    namespace: &str,
    source_id: &str,
) -> Result<Value, LarmError> {
    let mut metadata = Map::new();
    metadata.insert("display_name".into(), Value::String(record.name.clone()));
    metadata.insert("source_v1".into(), json!({"original":{"name":record.name},"normalized":{"repository_id":namespace,"model_id":record.key,"revision_key":record.revision_key,"catalog_reference":{"source_id":source_id,"upstream_namespace":namespace,"upstream_model_id":record.key,"revision_key":record.revision_key}},"limitations":["p048_source_adapter"],"rate_limit":null}));
    let timestamp = now();
    Ok(
        json!({"id":variant_id,"created_at":timestamp,"updated_at":timestamp,"row_version":1,"metadata":metadata,"revision_id":revision_id,"format":record.format,"quantization":null,"architecture":record.architecture,"bundle_hash":null,"lifecycle":"DISCOVERED","availability":"UNKNOWN","ownership":"EXTERNAL","promotion":"NONE","system_role":"GENERAL","capabilities":[],"model_limits":{"parameter_count":null,"layers":null,"kv_heads":null,"head_dim":null,"context_limit":null,"kv_bits":null,"model_max_memory_bytes":null,"architecture_metadata_hash":null},"system1_state":null,"update_available":false,"status":"DISCOVERED"}),
    )
}

fn estimate(record: &Record) -> Result<StorageEstimate, LarmError> {
    let known_sum = record
        .artifacts
        .iter()
        .filter_map(|a| a.size)
        .try_fold(0u64, u64::checked_add);
    let all_known = record.artifacts.iter().all(|a| a.size.is_some());
    let all_exact = all_known && record.artifacts.iter().all(|a| a.sha256.is_some());
    let local_observation = record.evidence.starts_with("p017:");
    let confidence = if all_exact {
        Confidence::Exact
    } else if all_known && local_observation {
        Confidence::Estimated
    } else if all_known {
        Confidence::High
    } else {
        Confidence::Unknown
    };
    let bytes = if all_known {
        known_sum.and_then(DecimalU64::new)
    } else {
        None
    };
    let upper_bound = if all_known {
        known_sum.and_then(DecimalU64::new)
    } else {
        None
    };
    Ok(StorageEstimate {
        download_bytes: bytes,
        final_artifact_bytes: bytes,
        installed_bytes: DecimalU64::new(0),
        derived_cache_bytes: DecimalU64::new(0),
        temporary_peak_bytes: DecimalU64::new(0),
        existing_bytes: DecimalU64::new(0),
        peak_extra_bytes: bytes,
        required_free_bytes: bytes,
        upper_bound_bytes: upper_bound,
        confidence,
        reason: if all_exact {
            "source_manifest_exact"
        } else if all_known && local_observation {
            "p017_local_observation_may_change"
        } else if all_known {
            "source_size_high"
        } else {
            "source_size_unknown"
        }
        .into(),
        evidence_refs: vec![record.evidence.clone()],
        per_volume: vec![],
    })
}

fn storage_estimate(record: &Record) -> Result<StorageEstimate, LarmError> {
    estimate(record)
}

fn artifact_hash(artifacts: &[Artifact]) -> Result<String, LarmError> {
    let value = artifacts
        .iter()
        .map(|a| json!({"path":a.path,"size":a.size,"sha256":a.sha256,"last_modified":a.last_modified}))
        .collect::<Vec<_>>();
    larm_domain::canonical::sha256_hex(&Value::Array(value)).map_err(|_| internal("artifact_hash"))
}

fn github_status_error(status: StatusCode, headers: &HeaderMap) -> LarmError {
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && header_text(headers, "x-ratelimit-remaining").as_deref() == Some("0"))
    {
        return github_rate_limit_error(headers);
    }
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::NOT_FOUND
    {
        return LarmError::new(
            ErrorCode::AuthRequired,
            "El repositorio requiere acceso público o una credencial admitida",
            "github_repository_access_required",
        );
    }
    http_status_error(status, headers)
}

pub fn github_rate_limit_error(headers: &HeaderMap) -> LarmError {
    let reset = header_text(headers, "x-ratelimit-reset")
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
        .map(larm_domain::bridge::timestamp);
    let retry = header_text(headers, RETRY_AFTER.as_str())
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(DecimalU64::new);
    let limit = header_text(headers, "x-ratelimit-limit")
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(DecimalU64::new);
    let remaining = header_text(headers, "x-ratelimit-remaining")
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(DecimalU64::new);
    larm_adapter_sdk::source::RateLimit {
        limit,
        remaining,
        reset_at: reset,
        retry_after_seconds: retry,
    }
    .to_error()
    .unwrap_or_else(|_| {
        LarmError::new(
            ErrorCode::UpstreamUnavailable,
            "GitHub limitó las solicitudes",
            "source_rate_limited",
        )
        .retryable(true)
    })
}

fn http_status_error(status: StatusCode, _headers: &HeaderMap) -> LarmError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        unavailable("source_upstream_unavailable", true)
    } else if status == StatusCode::NOT_FOUND {
        LarmError::new(
            ErrorCode::NotFound,
            "El recurso de origen no existe",
            "source_resource_not_found",
        )
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        LarmError::new(
            ErrorCode::AuthRequired,
            "El origen requiere autenticación",
            "source_auth_required",
        )
    } else {
        LarmError::new(
            ErrorCode::ProtocolError,
            "La fuente devolvió un estado no admitido",
            "source_http_status_invalid",
        )
    }
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, LarmError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(LarmError::new(
            ErrorCode::ProtocolError,
            "Respuesta de metadatos demasiado grande",
            "source_json_over_limit",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("source_body_read_failed", true))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(LarmError::new(
                ErrorCode::ProtocolError,
                "Respuesta de metadatos demasiado grande",
                "source_json_over_limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_chunk_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, bool), LarmError> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("source_body_read_failed", true))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, false))
}

fn unsatisfied_range_reply(
    headers: &HeaderMap,
    artifact: &Artifact,
    request: &SourceDownloadChunkRequest,
) -> Result<SourceDownloadChunk, LarmError> {
    let total = header_text(headers, "content-range")
        .and_then(|value| value.strip_prefix("bytes */").map(str::to_owned))
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
        .and_then(DecimalU64::new)
        .ok_or_else(|| {
            LarmError::new(
                ErrorCode::ProtocolError,
                "La fuente devolvió un Content-Range inválido",
                "source_content_range_invalid",
            )
        })?;
    if request.offset != total
        || artifact.size.is_some_and(|size| size != total.get())
        || artifact.sha256.is_none()
    {
        return Err(LarmError::new(
            ErrorCode::StaleRevision,
            "La fuente no validó la reanudación del bloque",
            "source_resume_validator_changed",
        ));
    }
    // 416 no entrega contenido. Sólo permite que el Core termine de verificar
    // el fichero completo que ya posee; no acredita su tamaño/hash por sí solo.
    let validator = strong_etag(headers)
        .map(|value| SourceDownloadResumeValidator::StrongEtag { value })
        .or_else(|| {
            header_text(headers, LAST_MODIFIED.as_str())
                .zip(artifact.sha256.clone())
                .map(|(last_modified, sha256)| {
                    SourceDownloadResumeValidator::LastModifiedAndSha256 {
                        last_modified,
                        sha256,
                    }
                })
        })
        .or_else(|| request.validator.clone());
    Ok(SourceDownloadChunk {
        offset: request.offset,
        data_base64: String::new(),
        eof: true,
        total_bytes: Some(total),
        validator,
    })
}

fn parse_content_range(headers: &HeaderMap) -> Option<(u64, u64, Option<u64>)> {
    let value = header_text(headers, "content-range")?;
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    (end >= start).then_some((start, end, total))
}
fn parse_u64_header(headers: &HeaderMap, name: &str) -> Option<u64> {
    header_text(headers, name)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v <= i64::MAX as u64)
}
fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}
fn strong_etag(headers: &HeaderMap) -> Option<String> {
    header_text(headers, ETAG.as_str())
        .filter(|v| v.starts_with('"') && v.ends_with('"') && !v.starts_with("W/"))
}
fn github_asset_origin(url: &Url) -> bool {
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "objects.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
            )
        )
}
fn filename_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(Iterator::last)
                .map(str::to_string)
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "download.bin".into())
}
fn artifact_role(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".gguf")
        || lower.ends_with(".safetensors")
        || lower.ends_with(".onnx")
        || lower.ends_with(".bin")
        || lower.ends_with(".pt")
        || lower.ends_with(".pth")
    {
        "weights"
    } else if lower.contains("tokenizer") {
        "tokenizer"
    } else {
        "other"
    }
}
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn now() -> String {
    larm_domain::bridge::timestamp(chrono::Utc::now())
}
fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Configuración o solicitud de fuente no válida",
        reason,
    )
}
fn forbidden(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Forbidden,
        "La fuente intentó acceder a una ruta u origen no autorizado",
        reason,
    )
}
fn unavailable(reason: &str, retryable: bool) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "La fuente no está disponible",
        reason,
    )
    .retryable(retryable)
}
fn unsupported(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::UnsupportedCapability,
        "La fuente no admite esta operación",
        reason,
    )
}
fn cancelled() -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "Operación cancelada",
        "operation_cancelled",
    )
}
fn internal(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Internal,
        "Fallo interno del adaptador de fuente",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use reqwest::header::HeaderValue;

    struct FixedCredentialReader {
        service: String,
        username: String,
        secret: Option<String>,
    }

    impl CredentialReader for FixedCredentialReader {
        fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
            assert_eq!(service, self.service);
            assert_eq!(username, self.username);
            Ok(self.secret.clone())
        }
    }

    async fn basic_probe_with_response(status: u16, redirect: Option<String>) -> AuthResult {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request
                .lines()
                .any(|line| line == "authorization: basic dxnlcjpzzwnyzxq="));
            let reason = match status {
                200 => "OK",
                302 => "Found",
                401 => "Unauthorized",
                403 => "Forbidden",
                _ => "Fixture",
            };
            let mut response = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n");
            if let Some(location) = redirect {
                response.push_str(&format!("Location: {location}\r\n"));
            }
            response.push_str("Connection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let source_id = "0192aaaa-0000-7000-8000-000000000001";
        let account_id = "0192aaaa-0000-7000-8000-000000000098";
        let profile = Profile {
            source_id: source_id.into(),
            source_type: SourceType::Http,
            root: None,
            url: Some(format!("http://{address}/private/model")),
            sha256: None,
            repository: None,
        };
        let reader = FixedCredentialReader {
            service: format!("larm/source/{source_id}"),
            username: account_id.into(),
            secret: Some(r#"{"username":"user","password":"secret"}"#.into()),
        };
        let work = tempfile::tempdir().unwrap();
        let adapter = P048Adapter::new_with_credential_reader(
            source_manifest(&format!("http://{address}"), false),
            profile,
            work.path(),
            std::sync::Arc::new(reader),
        )
        .unwrap();
        let request = AuthProbe {
            source_id: source_id.into(),
            auth_mode: larm_adapter_sdk::source::AuthMode::Basic,
            account_id: Some(account_id.into()),
            credential_reference_id: Some("0192aaaa-0000-7000-8000-000000000099".into()),
        };
        let SourceReply::AuthProbe(result) = adapter.auth_probe(request).await.unwrap() else {
            panic!("auth probe reply")
        };
        server.await.unwrap();
        result
    }

    fn source_manifest(network_origin: &str, searchable: bool) -> AdapterManifest {
        let table = larm_adapter_sdk::manifest::MethodTable::contract();
        let mut methods = table.mandatory_common.clone();
        methods.extend(
            table
                .required_for(larm_adapter_sdk::manifest::Family::Source)
                .into_iter()
                .map(str::to_string),
        );
        methods.push("source.download_chunk".into());
        let value = json!({
            "id":"larm.source.p048.test","version":"1.0.0","family":"source","protocol_version":"1.0","publisher":"LARM",
            "executable":{"relative_path":"bin/worker.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],
            "methods":methods,"permissions":{"read_roots":["p048-work"],"write_roots":["p048-work"],"network_origins":[network_origin],"process_allowlist":[],"secret_refs":["larm/source/0192aaaa-0000-7000-8000-000000000001"]},
            "capabilities":if searchable { vec!["source.search"] } else { vec![] },"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]
        });
        AdapterManifest::parse(&value.to_string()).unwrap()
    }

    #[test]
    fn traversal_and_windows_special_paths_are_rejected() {
        for path in [
            "../outside.gguf",
            "folder/../../outside.gguf",
            "C:/secret",
            "folder\\file.gguf",
            "file.gguf:secret",
            "/absolute.gguf",
        ] {
            assert_eq!(
                validate_relative(path).unwrap_err().details.reason,
                "artifact_path_invalid",
                "{path}"
            );
        }
    }

    #[test]
    fn unavailable_unc_root_maps_to_retryable_resource_error() {
        let missing = r"\\larm-test-host-does-not-exist\models";
        let error = scan_local_root(missing).unwrap_err();
        assert_eq!(error.details.reason, "source_root_unavailable");
        assert!(error.retryable);
    }

    #[test]
    fn http_head_without_content_length_stays_unknown() {
        let mut headers = HeaderMap::new();
        headers.insert(ETAG, HeaderValue::from_static("\"opaque-v1\""));
        assert_eq!(parse_u64_header(&headers, CONTENT_LENGTH.as_str()), None);
        let record = Record {
            key: "x".into(),
            name: "x".into(),
            revision_key: "url:abc".into(),
            format: None,
            architecture: None,
            artifacts: vec![Artifact {
                path: "x.bin".into(),
                size: None,
                sha256: None,
                url: Some("https://example.test/x.bin".into()),
                etag: strong_etag(&headers),
                last_modified: None,
            }],
            evidence: "http:head".into(),
        };
        let size = source_artifacts(&record)
            .unwrap()
            .remove(0)
            .size_estimate
            .unwrap();
        assert_eq!(size.confidence, ArtifactSizeConfidence::Unknown);
        assert_eq!(size.bytes, None);
    }

    #[tokio::test]
    async fn http_adapter_head_fixture_preserves_unknown_size() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let url = format!("http://{address}/weights.gguf");
        let profile = Profile {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            source_type: SourceType::Http,
            root: None,
            url: Some(url),
            sha256: None,
            repository: None,
        };
        let work = tempfile::tempdir().unwrap();
        let adapter = P048Adapter::new(
            source_manifest(&format!("http://{address}"), false),
            profile,
            work.path(),
        )
        .unwrap();
        let records = adapter.http_records().await.unwrap();
        server.await.unwrap();
        assert_eq!(records[0].artifacts[0].size, None);
        let estimate = source_artifacts(&records[0])
            .unwrap()
            .remove(0)
            .size_estimate
            .unwrap();
        assert_eq!(estimate.confidence, ArtifactSizeConfidence::Unknown);
    }

    #[tokio::test]
    async fn basic_auth_probe_sends_only_to_configured_loopback_and_redacts_result() {
        let result = basic_probe_with_response(200, None).await;
        assert!(result.authenticated);
        assert!(!result.auth_required && !result.license_required);
        let response = serde_json::to_string(&result).unwrap();
        assert!(!response.contains("user") && !response.contains("secret"));
    }

    #[tokio::test]
    async fn basic_auth_probe_classifies_401_and_gated_403_without_echoing_credentials() {
        let unauthorized = basic_probe_with_response(401, None).await;
        assert!(!unauthorized.authenticated && unauthorized.auth_required);
        assert!(!unauthorized.license_required);
        let forbidden = basic_probe_with_response(403, None).await;
        assert!(!forbidden.authenticated && !forbidden.auth_required);
        assert!(forbidden.license_required);
        let response = serde_json::to_string(&forbidden).unwrap();
        assert!(!response.contains("user") && !response.contains("secret"));
    }

    #[tokio::test]
    async fn basic_auth_probe_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        let main_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let main_address = main_listener.local_addr().unwrap();
        let redirect_location = format!("http://{redirect_address}/stolen");
        let server = tokio::spawn(async move {
            let (mut stream, _) = main_listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            stream
                .write_all(
                    format!("HTTP/1.1 302 Found\r\nLocation: {redirect_location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
        });
        let source_id = "0192aaaa-0000-7000-8000-000000000001";
        let account_id = "0192aaaa-0000-7000-8000-000000000098";
        let profile = Profile {
            source_id: source_id.into(),
            source_type: SourceType::Http,
            root: None,
            url: Some(format!("http://{main_address}/private/model")),
            sha256: None,
            repository: None,
        };
        let reader = FixedCredentialReader {
            service: format!("larm/source/{source_id}"),
            username: account_id.into(),
            secret: Some(r#"{"username":"user","password":"secret"}"#.into()),
        };
        let work = tempfile::tempdir().unwrap();
        let adapter = P048Adapter::new_with_credential_reader(
            source_manifest(&format!("http://{main_address}"), false),
            profile,
            work.path(),
            std::sync::Arc::new(reader),
        )
        .unwrap();
        let request = AuthProbe {
            source_id: source_id.into(),
            auth_mode: larm_adapter_sdk::source::AuthMode::Basic,
            account_id: Some(account_id.into()),
            credential_reference_id: Some("0192aaaa-0000-7000-8000-000000000099".into()),
        };
        let SourceReply::AuthProbe(result) = adapter.auth_probe(request).await.unwrap() else {
            panic!("auth probe reply")
        };
        server.await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason, "source_http_status_302");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), redirect_listener.accept())
                .await
                .is_err()
        );
    }

    async fn http_range_fixture(
        reply: &'static [u8],
        validator: Option<SourceDownloadResumeValidator>,
    ) -> Result<SourceDownloadChunk, LarmError> {
        http_range_fixture_at(reply, validator, 5, None).await
    }

    async fn http_range_fixture_at(
        reply: &'static [u8],
        validator: Option<SourceDownloadResumeValidator>,
        offset: u64,
        sha256: Option<String>,
    ) -> Result<SourceDownloadChunk, LarmError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected_if_range = validator.as_ref().map(|v| match v {
            SourceDownloadResumeValidator::StrongEtag { value } => value.clone(),
            SourceDownloadResumeValidator::LastModifiedAndSha256 { last_modified, .. } => {
                last_modified.clone()
            }
        });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut request_lines = Vec::new();
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                request_lines.push(line);
            }
            assert!(request_lines.iter().any(|line| {
                line.eq_ignore_ascii_case(&format!("range: bytes={offset}-{}\r\n", offset + 4))
            }));
            if let Some(value) = expected_if_range {
                assert!(request_lines
                    .iter()
                    .any(|line| line.eq_ignore_ascii_case(&format!("if-range: {value}\r\n"))));
            }
            stream.get_mut().write_all(reply).await.unwrap();
        });
        let url = format!("http://{address}/weights.gguf");
        let profile = Profile {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            source_type: SourceType::Http,
            root: None,
            url: Some(url.clone()),
            sha256: None,
            repository: None,
        };
        let work = tempfile::tempdir().unwrap();
        let adapter = P048Adapter::new(
            source_manifest(&format!("http://{address}"), false),
            profile,
            work.path(),
        )
        .unwrap();
        let source_id = "0192aaaa-0000-7000-8000-000000000001".to_string();
        let request = SourceDownloadChunkRequest {
            source_id: source_id.clone(),
            job_id: Uuid::now_v7().to_string(),
            plan_id: Uuid::now_v7().to_string(),
            plan_hash: "a".repeat(64),
            approval_id: None,
            revision: larm_adapter_sdk::source::RevisionRef {
                source_id,
                upstream_namespace: "direct-http".into(),
                upstream_model_id: "explicit-url".into(),
                revision_key: "url:fixture".into(),
            },
            relative_path: "weights.gguf".into(),
            offset: DecimalU64::new(offset).unwrap(),
            max_bytes: DecimalU64::new(5).unwrap(),
            validator,
        };
        let artifact = Artifact {
            path: "weights.gguf".into(),
            size: Some(12),
            sha256,
            url: Some(url),
            etag: None,
            last_modified: None,
        };
        let result = adapter.remote_chunk(artifact, request).await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn http_416_at_pinned_total_returns_eof_for_core_hash_verification() {
        let validator = Some(SourceDownloadResumeValidator::StrongEtag {
            value: "\"fixture-v1\"".into(),
        });
        let chunk = http_range_fixture_at(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */12\r\nConnection: close\r\n\r\n", validator.clone(), 12, Some("a".repeat(64))).await.unwrap();
        assert_eq!(chunk.offset.get(), 12);
        assert_eq!(chunk.total_bytes.unwrap().get(), 12);
        assert!(chunk.eof && chunk.decoded_bytes().unwrap().is_empty());
        assert_eq!(chunk.validator, validator);
    }

    #[tokio::test]
    async fn http_416_requires_matching_offset_size_and_a_pinned_hash() {
        let reply = b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */12\r\nConnection: close\r\n\r\n";
        for (offset, sha) in [(5, Some("a".repeat(64))), (12, None)] {
            let error = http_range_fixture_at(reply, None, offset, sha)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::StaleRevision);
            assert!(!error.retryable);
        }
        let error = http_range_fixture_at(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */11\r\nConnection: close\r\n\r\n",None,11,Some("a".repeat(64))).await.unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::StaleRevision,
            "la longitud del manifiesto permanece fijada en 12"
        );
    }

    #[tokio::test]
    async fn http_416_never_accepts_missing_malformed_or_overflowing_totals() {
        for reply in [
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */+12\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */9223372036854775808\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes 0-11/12\r\nConnection: close\r\n\r\n",
        ] {
            let error = http_range_fixture_at(reply,None,12,Some("a".repeat(64))).await.unwrap_err();
            assert_eq!(error.code,ErrorCode::ProtocolError);
            assert!(!error.retryable);
        }
    }

    #[tokio::test]
    async fn http_adapter_returns_only_the_requested_range_and_pins_etag() {
        let chunk = http_range_fixture(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/12\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\nworld", None).await.unwrap();
        assert_eq!(chunk.decoded_bytes().unwrap(), b"world");
        assert!(!chunk.eof);
        assert_eq!(chunk.total_bytes.unwrap().get(), 12);
        assert_eq!(
            chunk.validator,
            Some(SourceDownloadResumeValidator::StrongEtag {
                value: "\"fixture-v1\"".into()
            })
        );
    }

    #[tokio::test]
    async fn http_adapter_reports_changed_validator_without_mislabeling_range_bytes() {
        let chunk = http_range_fixture(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/12\r\nETag: \"fixture-v2\"\r\nConnection: close\r\n\r\nworld", Some(SourceDownloadResumeValidator::StrongEtag { value: "\"fixture-v1\"".into() })).await.unwrap();
        assert_eq!(chunk.offset.get(), 5);
        assert_eq!(chunk.decoded_bytes().unwrap(), b"world");
        assert_eq!(
            chunk.validator,
            Some(SourceDownloadResumeValidator::StrongEtag {
                value: "\"fixture-v2\"".into()
            })
        );
    }

    #[tokio::test]
    async fn http_adapter_does_not_label_a_full_response_as_a_resumed_chunk() {
        let error = http_range_fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nETag: \"fixture-v2\"\r\nConnection: close\r\n\r\nhello world!", Some(SourceDownloadResumeValidator::StrongEtag { value: "\"fixture-v1\"".into() })).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleRevision);
        assert_eq!(error.details.reason, "source_resume_validator_changed");
    }

    #[test]
    fn github_rate_limit_fixture_is_retryable_and_keeps_safe_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit", HeaderValue::from_static("60"));
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1790000000"));
        headers.insert(RETRY_AFTER, HeaderValue::from_static("15"));
        let error = github_status_error(StatusCode::FORBIDDEN, &headers);
        assert_eq!(error.details.reason, "source_rate_limited");
        assert!(error.retryable);
        assert!(larm_adapter_sdk::source::RateLimit::from_error(&error).is_some());
    }

    #[test]
    fn github_release_fixture_pins_release_and_uses_published_digest() {
        let digest = "a".repeat(64);
        let releases = vec![
            json!({"id":42,"tag_name":"v1.2.3","name":"Model v1.2.3","draft":false,"assets":[
                {"name":"weights.gguf","size":7,"digest":format!("sha256:{digest}"),"browser_download_url":"https://github.com/acme/model/releases/download/v1.2.3/weights.gguf","state":"uploaded"}
            ]}),
        ];
        let records = parse_releases("acme/model", releases).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].revision_key.starts_with("release:42:"));
        assert!(records[0].revision_key.len() <= 256);
        let artifact = source_artifacts(&records[0]).unwrap().remove(0);
        assert_eq!(artifact.sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(
            artifact.size_estimate.unwrap().confidence,
            ArtifactSizeConfidence::Exact
        );
        let variant = model_variant(
            &records[0],
            "0192aaaa-0000-7000-8000-000000000002",
            "0192aaaa-0000-7000-8000-000000000003",
            "acme/model",
            "0192aaaa-0000-7000-8000-000000000001",
        )
        .unwrap();
        let request = SourceRequest::Search(SourceSearch {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            q: String::new(),
            capabilities: vec![],
            cursor: None,
            limit: 1,
        });
        let page = json!({"items":[variant],"next_cursor":null,"snapshot_at":now()});
        assert!(SourceReply::parse(&request, page).is_ok());
    }

    #[test]
    fn malformed_or_mismatched_content_range_is_not_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert("content-range", HeaderValue::from_static("bytes 10-14/20"));
        assert_eq!(parse_content_range(&headers), Some((10, 14, Some(20))));
        headers.insert("content-range", HeaderValue::from_static("bytes 10-14/*"));
        assert_eq!(parse_content_range(&headers), Some((10, 14, None)));
        headers.insert("content-range", HeaderValue::from_static("bytes 15-14/20"));
        assert_eq!(parse_content_range(&headers), None);
    }

    #[test]
    fn github_release_page_limit_never_returns_a_silent_partial_catalog() {
        assert!(!release_page_complete(1, RELEASE_PAGE_SIZE).unwrap());
        assert!(release_page_complete(2, RELEASE_PAGE_SIZE - 1).unwrap());
        assert_eq!(
            release_page_complete(MAX_RELEASE_PAGES, RELEASE_PAGE_SIZE)
                .unwrap_err()
                .details
                .reason,
            "github_release_page_limit"
        );
    }

    #[test]
    fn local_index_uses_p017_without_hashing_or_writing_source_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("local");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("weights.gguf"), b"GGUF\0fixture").unwrap();
        let records = scan_local_root(&root.display().to_string()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].artifacts[0].size, Some(12));
        assert_eq!(records[0].artifacts[0].sha256, None);
        assert!(records[0].revision_key.starts_with("local:"));
        let estimate = source_artifacts(&records[0])
            .unwrap()
            .remove(0)
            .size_estimate
            .unwrap();
        assert_eq!(estimate.confidence, ArtifactSizeConfidence::Estimated);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn public_acquisition_selects_one_of_two_local_models_without_private_uuid() {
        use larm_adapter_sdk::source::{
            SourceAcquisitionArtifactBinding, SourceAcquisitionBinding, SourceCatalogReference,
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("models");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.gguf"), b"GGUF\0first").unwrap();
        std::fs::write(root.join("b.gguf"), b"GGUF\0second").unwrap();
        let records = scan_local_root(&root.display().to_string()).unwrap();
        assert_eq!(records.len(), 2);
        let record = records[1].clone();
        let manifest = artifact_manifest(&record).unwrap();
        let source_id = "0192aaaa-0000-7000-8000-000000000001";
        let adapter = P048Adapter::new(
            source_manifest("https://example.test", false),
            Profile {
                source_id: source_id.into(),
                source_type: SourceType::Local,
                root: Some(root.display().to_string()),
                url: None,
                sha256: None,
                repository: None,
            },
            temp.path(),
        )
        .unwrap();
        let binding = SourceAcquisitionBinding {
            revision: SourceCatalogReference {
                source_id: source_id.into(),
                upstream_namespace: "local".into(),
                upstream_model_id: record.key.clone(),
                revision_key: record.revision_key.clone(),
            },
            manifest_hash: manifest.manifest_hash.clone(),
            artifacts: manifest
                .artifacts
                .iter()
                .map(|a| SourceAcquisitionArtifactBinding {
                    artifact_id: Uuid::now_v7().to_string(),
                    relative_path: a.relative_path.clone(),
                    required: a.required,
                })
                .collect(),
        };
        let request = DownloadPlanRequest {
            variant_id: Uuid::now_v7().to_string(),
            storage_location_id: Uuid::now_v7().to_string(),
            hard_budget_bytes: None,
            source_id: Some(source_id.into()),
            priority: larm_adapter_sdk::source::DownloadPriority::Normal,
            optional_artifact_ids: Some(vec![]),
            acquisition: Some(binding),
        };
        let plan = adapter
            .plan_records(request.clone(), records.clone())
            .unwrap();
        let spec = larm_adapter_sdk::source::download_spec(&plan).unwrap();
        assert_eq!(spec.variant_id, request.variant_id);
        assert_eq!(spec.revision.revision_key, record.revision_key);
        assert_eq!(spec.artifacts.len(), record.artifacts.len());
        assert_eq!(spec.artifacts[0].relative_path, record.artifacts[0].path);
        assert!(
            !temp.path().join("p048-identities.json").exists(),
            "no private UUID lookup or registration"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        let mut changed = request;
        changed.acquisition.as_mut().unwrap().manifest_hash = "d".repeat(64);
        assert_eq!(
            adapter.plan_records(changed, records).unwrap_err().code,
            ErrorCode::StaleRevision
        );
    }

    #[test]
    fn high_confidence_plan_uses_observed_point_without_claiming_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let profile = Profile {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            source_type: SourceType::Http,
            root: None,
            url: Some("https://example.test/model.bin".into()),
            sha256: None,
            repository: None,
        };
        let adapter = P048Adapter::new(
            source_manifest("https://example.test:443", false),
            profile,
            temp.path(),
        )
        .unwrap();
        let record = Record {
            key: "model".into(),
            name: "Model".into(),
            revision_key: "r1".into(),
            format: None,
            architecture: None,
            artifacts: vec![Artifact {
                path: "weights.bin".into(),
                size: Some(10),
                sha256: None,
                url: Some("https://example.test/model.bin".into()),
                etag: None,
                last_modified: None,
            }],
            evidence: "http:head".into(),
        };
        let request = DownloadPlanRequest {
            variant_id: Uuid::now_v7().to_string(),
            storage_location_id: Uuid::now_v7().to_string(),
            hard_budget_bytes: None,
            source_id: Some(adapter.profile.source_id.clone()),
            priority: larm_adapter_sdk::source::DownloadPriority::Normal,
            optional_artifact_ids: None,
            acquisition: None,
        };
        let plan = adapter
            .plan_records(request.clone(), vec![record.clone()])
            .unwrap();
        assert_eq!(plan.storage_estimate.confidence, Confidence::High);
        assert_eq!(plan.storage_estimate.download_bytes, DecimalU64::new(10));
        let artifact = larm_adapter_sdk::source::download_spec(&plan)
            .unwrap()
            .artifacts
            .remove(0);
        assert!(artifact.size_bytes.is_none() && artifact.sha256.is_none());
        assert_eq!(
            artifact.size_estimate.unwrap().confidence,
            ArtifactSizeConfidence::High
        );
        let mut limited = request;
        limited.hard_budget_bytes = DecimalU64::new(9);
        assert_eq!(
            adapter
                .plan_records(limited, vec![record])
                .unwrap_err()
                .details
                .reason,
            "hard_budget_exceeded"
        );
    }

    #[test]
    fn http_profile_rejects_secret_query_and_unpermitted_origin() {
        let profile = Profile {
            source_id: "0192aaaa-0000-7000-8000-000000000001".into(),
            source_type: SourceType::Http,
            root: None,
            url: Some("https://example.test/model?access_token=hidden".into()),
            sha256: None,
            repository: None,
        };
        assert_eq!(
            validate_profile(&profile, &["https://example.test:443".into()])
                .unwrap_err()
                .details
                .reason,
            "http_url_secret_in_query"
        );
        let clean = Profile {
            url: Some("https://example.test/model".into()),
            ..profile
        };
        assert_eq!(
            validate_profile(&clean, &["https://other.test:443".into()])
                .unwrap_err()
                .details
                .reason,
            "http_network_origin_not_permitted"
        );
    }

    #[test]
    fn local_chunk_refuses_symlink_escape_and_reads_only_requested_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside.gguf");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape.gguf")).unwrap();
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    &root.join("escape.gguf").display().to_string(),
                    &outside.display().to_string(),
                ])
                .output();
        }
        if root.join("escape.gguf").exists()
            || std::fs::symlink_metadata(root.join("escape.gguf")).is_ok()
        {
            assert_eq!(
                safe_local_file(&root, "escape.gguf")
                    .unwrap_err()
                    .details
                    .reason,
                "source_file_reparse_point"
            );
        }
        std::fs::write(root.join("weights.gguf"), b"abcdefgh").unwrap();
        let (bytes, total) =
            read_local_chunk(&root.display().to_string(), "weights.gguf", 2, 3).unwrap();
        assert_eq!(bytes, b"cde");
        assert_eq!(total, 8);
    }
}
