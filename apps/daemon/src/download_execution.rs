//! Composición P051: vuelve a resolver autorización, cualificación y acceso
//! antes de abrir una sesión source para consumir un job durable.

use async_trait::async_trait;
use larm_adapter_host::{conformance::NativeConformanceRunner, source::SourceWorker};
use larm_adapter_sdk::{
    permissions::Access,
    source::{SearchSupport, SourceReply, SourceRequest},
};
use larm_domain::{
    acquisition::ModelAcquisitionManifest,
    download::{DownloadArtifactEstimate, DownloadMirrorArtifact, DownloadSourceCandidate},
    error::{ErrorCode, LarmError},
    locations::{LocationState, StorageLocation},
    model::{ArtifactSizeConfidence, RevisionState},
    ports::{
        AcquisitionManifestCatalog, DownloadMirrorCatalog, ModelRepository, MutationContext,
        StorageLocationRepository, VariantFilter, VariantRepository,
    },
    sources::{AuthMode, ModelSource},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

const SOURCE_RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Catálogo de mirrors que revalida los gates de fuente y P026 antes de que
/// una alternativa aparezca en un plan confirmado.
pub struct QualifiedDownloadMirrorCatalog {
    pub variants: Arc<dyn VariantRepository>,
    pub models: Arc<dyn ModelRepository>,
    pub sources: larm_db::sources::SqlSources,
    pub conformance: larm_db::conformance::SqlAdapterConformance,
}

fn exact_selected_artifacts(
    selected: &[DownloadArtifactEstimate],
) -> Result<Vec<&DownloadArtifactEstimate>, LarmError> {
    let chosen = selected
        .iter()
        .filter(|artifact| artifact.selected)
        .collect::<Vec<_>>();
    if chosen.is_empty()
        || chosen.iter().any(|artifact| {
            artifact.sha256.is_none()
                || artifact.size_estimate.confidence != ArtifactSizeConfidence::Exact
                || artifact.size_estimate.bytes.is_none()
        })
    {
        return Err(unavailable("download_mirror_identity_not_exact"));
    }
    Ok(chosen)
}

#[async_trait]
impl DownloadMirrorCatalog for QualifiedDownloadMirrorCatalog {
    async fn eligible_candidates(
        &self,
        model_id: &str,
        selected_artifacts: &[DownloadArtifactEstimate],
    ) -> Result<Vec<DownloadSourceCandidate>, larm_domain::bridge::ApiError> {
        if !larm_domain::bridge::is_uuid_v7(model_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El modelo de referencia de los mirrors no es válido",
                "download_mirror_model_invalid",
            ));
        }
        let selected = match exact_selected_artifacts(selected_artifacts) {
            Ok(selected) => selected,
            Err(error) if error.details.reason == "download_mirror_identity_not_exact" => {
                return Ok(vec![]);
            }
            Err(error) => return Err(error),
        };
        let filter = VariantFilter {
            model_id: Some(model_id.to_owned()),
            ..VariantFilter::default()
        };
        let mut after = None;
        let mut visited = 0_usize;
        let mut by_source = std::collections::BTreeMap::new();
        loop {
            let page = self
                .variants
                .list_variants(after.clone(), 200, &filter)
                .await?;
            if page.is_empty() {
                break;
            }
            visited = visited.saturating_add(page.len());
            if visited > 10_000 {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El catálogo de variantes excede el límite de búsqueda de mirrors",
                    "download_mirror_catalog_limit",
                ));
            }
            after = page
                .last()
                .map(|variant| (variant.created_at.clone(), variant.id.clone()));
            for variant in page {
                let Some(revision) = self.models.get_revision(&variant.revision_id).await? else {
                    continue;
                };
                if revision.state != RevisionState::Current {
                    continue;
                }
                let Some(distribution) = self
                    .models
                    .get_distribution(&revision.distribution_id)
                    .await?
                else {
                    continue;
                };
                if distribution.canonical_model_id != model_id || distribution.license_id.is_some()
                {
                    continue;
                }
                let Some(bundle) = self.variants.bundle_of(&variant.id).await? else {
                    continue;
                };
                if bundle.variant_id != variant.id
                    || bundle.state != larm_domain::model::BundleState::Verified
                    || bundle.manifest_hash != revision.manifest_hash
                {
                    continue;
                }
                let artifacts = self.variants.artifacts_of(&variant.id).await?;
                let mut mapped = Vec::with_capacity(selected.len());
                let mut compatible = true;
                for target in &selected {
                    let Some(source_artifact) = artifacts
                        .iter()
                        .find(|artifact| artifact.filename == target.relative_path)
                    else {
                        compatible = false;
                        break;
                    };
                    let Some(target_hash) = target.sha256.as_deref() else {
                        compatible = false;
                        break;
                    };
                    if source_artifact.sha256.as_deref() != Some(target_hash)
                        || source_artifact.size_estimate.confidence != ArtifactSizeConfidence::Exact
                        || source_artifact.size_estimate.bytes != target.size_estimate.bytes
                    {
                        compatible = false;
                        break;
                    }
                    let Some(size_bytes) = source_artifact.size_estimate.bytes else {
                        compatible = false;
                        break;
                    };
                    mapped.push(DownloadMirrorArtifact {
                        target_artifact_id: target.artifact_id.clone(),
                        source_artifact_id: source_artifact.id.clone(),
                        relative_path: target.relative_path.clone(),
                        sha256: target_hash.to_owned(),
                        size_bytes,
                    });
                }
                if !compatible || mapped.len() != selected.len() {
                    continue;
                }
                let binding = match self
                    .sources
                    .execution_binding(&distribution.source_id)
                    .await
                {
                    Ok(binding) => binding,
                    Err(_) => continue,
                };
                if binding.source.auth_mode != AuthMode::None {
                    continue;
                }
                let current = match self
                    .conformance
                    .current_for_consumer(&binding.installation.id)
                    .await
                {
                    Ok(Some(current)) => current,
                    Ok(None) | Err(_) => continue,
                };
                if current.0 != binding.installation
                    || source_access(
                        &binding.source,
                        &binding.adapter.source_type,
                        &binding.installation,
                    )
                    .is_err()
                {
                    continue;
                }
                let https = url::Url::parse(&binding.source.base_url)
                    .is_ok_and(|url| url.scheme() == "https");
                let candidate = DownloadSourceCandidate {
                    source_id: binding.source.id,
                    variant_id: variant.id.clone(),
                    revision_id: revision.id,
                    revision_key: revision.revision_key,
                    manifest_hash: bundle.manifest_hash,
                    source_priority: binding.source.priority,
                    confidence: ArtifactSizeConfidence::Exact,
                    authenticated: false,
                    https,
                    artifacts: mapped,
                };
                let replace = by_source.get(&candidate.source_id).is_none_or(
                    |existing: &DownloadSourceCandidate| {
                        (&candidate.variant_id, &candidate.revision_key)
                            < (&existing.variant_id, &existing.revision_key)
                    },
                );
                if replace {
                    by_source.insert(candidate.source_id.clone(), candidate);
                }
            }
        }
        Ok(by_source.into_values().collect())
    }
}

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo autorizar la ejecución de la descarga",
        reason,
    )
}

fn origin(url: &url::Url) -> Option<String> {
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    Some(format!(
        "{scheme}://{}:{}",
        url.host_str()?.to_ascii_lowercase(),
        url.port_or_known_default()?
    ))
}

fn requires_source_auth(source: &ModelSource) -> bool {
    source.auth_mode != AuthMode::None
}

fn source_access(
    source: &ModelSource,
    source_type: &str,
    installation: &larm_domain::adapters::AdapterInstallation,
) -> Result<Access, LarmError> {
    if installation
        .metadata
        .get("execution")
        .and_then(serde_json::Value::as_str)
        == Some("none")
    {
        return Err(unavailable("download_source_execution_unavailable"));
    }
    let manifest =
        larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())?;
    if !manifest
        .methods
        .iter()
        .any(|method| method == "source.download_chunk")
        || !manifest
            .methods
            .iter()
            .any(|method| method == "source.download_plan")
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El adaptador no ofrece el protocolo de descarga por bloques",
            "download_source_chunk_protocol_unavailable",
        ));
    }

    match source_type {
        "LOCAL" | "UNC_PRIVATE" => {
            let path = if source.base_url.starts_with("file:") {
                url::Url::parse(&source.base_url)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .ok_or_else(|| unavailable("download_source_path_invalid"))?
            } else {
                PathBuf::from(&source.base_url)
            };
            if !path.is_absolute() || !path.is_dir() {
                return Err(unavailable("download_source_path_unavailable"));
            }
            Ok(Access {
                reads: vec![path],
                ..Access::default()
            })
        }
        "GIT_LFS" => Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "Git LFS requiere autorización de credenciales específica",
            "download_source_auth_unavailable",
        )),
        _ => {
            if requires_source_auth(source) {
                return Err(LarmError::new(
                    ErrorCode::AuthRequired,
                    "La fuente requiere credenciales que el consumidor aún no resuelve",
                    "download_source_auth_unavailable",
                ));
            }
            let url = url::Url::parse(&source.base_url)
                .map_err(|_| unavailable("download_source_url_invalid"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(unavailable("download_source_url_invalid"));
            }
            let source_origin =
                origin(&url).ok_or_else(|| unavailable("download_source_url_invalid"))?;
            let declared_origins = manifest
                .permissions
                .network_origins
                .iter()
                .filter_map(|candidate| url::Url::parse(candidate).ok())
                .collect::<Vec<_>>();
            if !declared_origins
                .iter()
                .any(|candidate| origin(candidate).as_deref() == Some(source_origin.as_str()))
            {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "El origen configurado no figura entre los permisos del adaptador",
                    "download_source_origin_not_declared",
                ));
            }
            // Los perfiles de fuentes remotas pueden usar hosts adicionales
            // (por ejemplo, CDN). El worker recibe solo orígenes ya declarados
            // en el manifiesto y cubiertos por la cualificación P022.
            let mut urls = declared_origins
                .into_iter()
                .filter(|candidate| origin(candidate).is_some())
                .map(|candidate| candidate.to_string())
                .collect::<Vec<_>>();
            urls.sort();
            urls.dedup();
            Ok(Access {
                urls,
                ..Access::default()
            })
        }
    }
}

pub struct QualifiedDownloadSourceResolver {
    pub sources: larm_db::sources::SqlSources,
    pub conformance: larm_db::conformance::SqlAdapterConformance,
    pub models: larm_db::models::SqlModels,
    pub variants: Arc<dyn VariantRepository>,
    pub runner: Arc<NativeConformanceRunner>,
}

struct ResolvedDownloadSource {
    acquisition: Option<ModelAcquisitionManifest>,
    installation: larm_domain::adapters::AdapterInstallation,
    run: larm_domain::conformance::Run,
    access: Access,
    support: SearchSupport,
    context: MutationContext,
}

struct QualifiedDownloadSourceSession {
    acquisition: Option<ModelAcquisitionManifest>,
    worker: SourceWorker,
    access: Access,
}

#[async_trait]
impl larm_download::consumer::DownloadSourceSession for QualifiedDownloadSourceSession {
    fn acquisition(&self) -> Option<&ModelAcquisitionManifest> {
        self.acquisition.as_ref()
    }
    async fn download_plan(
        &self,
        request: larm_adapter_sdk::source::DownloadPlanRequest,
    ) -> Result<larm_domain::artifacts::Plan, LarmError> {
        match self
            .worker
            .call(
                SourceRequest::DownloadPlan(request),
                &self.access,
                Some(SOURCE_RPC_TIMEOUT),
            )
            .await?
        {
            SourceReply::DownloadPlan(plan) => Ok(plan),
            _ => Err(LarmError::new(
                ErrorCode::ProtocolError,
                "El adaptador respondió con otro método al pedir el plan de descarga",
                "download_source_reply_mismatch",
            )),
        }
    }

    async fn download_chunk(
        &self,
        request: larm_adapter_sdk::source::SourceDownloadChunkRequest,
    ) -> Result<larm_adapter_sdk::source::SourceDownloadChunk, LarmError> {
        match self
            .worker
            .call(
                SourceRequest::DownloadChunk(request),
                &self.access,
                Some(SOURCE_RPC_TIMEOUT),
            )
            .await?
        {
            SourceReply::DownloadChunk(chunk) => Ok(chunk),
            _ => Err(LarmError::new(
                ErrorCode::ProtocolError,
                "El adaptador respondió con otro método al pedir un bloque",
                "download_source_reply_mismatch",
            )),
        }
    }

    async fn close(&self) -> Result<(), LarmError> {
        self.worker.close().await
    }
}

impl QualifiedDownloadSourceResolver {
    async fn resolve_planned(
        &self,
        record: &larm_domain::download::DownloadPlanRecord,
        candidate: &DownloadSourceCandidate,
        ctx: &MutationContext,
    ) -> Result<ResolvedDownloadSource, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id) {
            return Err(unavailable("download_job_context_invalid"));
        }
        let acquisitions = larm_db::acquisition::SqlAcquisitionManifests {
            pool: self.models.pool.clone(),
            qualification: Some(self.runner.clone()),
        };
        let target_acquisition = if let Some(binding) = &record.response.acquisition {
            let manifest = acquisitions
                .bound_current(&record.response.variant_id, binding, ctx)
                .await?;
            manifest.validate_download_record(record)?;
            Some(manifest)
        } else {
            None
        };
        let models = &self.models as &dyn larm_domain::ports::ModelRepository;
        let target_revision = models
            .get_revision(&record.revision_id)
            .await?
            .filter(|revision| revision.revision_key == record.revision_key)
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::StaleRevision,
                    "La revisión del modelo cambió desde el plan confirmado",
                    "download_model_revision_stale",
                )
            })?;
        let target_distribution = models
            .get_distribution(&target_revision.distribution_id)
            .await?
            .ok_or_else(|| unavailable("download_model_distribution_stale"))?;
        if target_distribution.license_id.is_some() {
            return Err(LarmError::new(
                ErrorCode::LicenseRequired,
                "La distribución del plan requiere una licencia que aún no se ha autorizado",
                "download_source_license_unavailable",
            ));
        }
        if !larm_domain::bridge::is_uuid_v7(&candidate.source_id)
            || !larm_domain::bridge::is_uuid_v7(&candidate.variant_id)
            || !larm_domain::bridge::is_uuid_v7(&candidate.revision_id)
            || candidate.authenticated
        {
            return Err(unavailable("download_source_candidate_invalid"));
        }
        let acquisition = if candidate.artifacts.is_empty() {
            target_acquisition
        } else {
            acquisitions
                .latest_current(&candidate.variant_id, ctx)
                .await?
        };
        if let Some(manifest) = &acquisition {
            if manifest.variant_id != candidate.variant_id
                || manifest.revision_id != candidate.revision_id
                || manifest.source_id != candidate.source_id
                || manifest.source_reference.revision_key != candidate.revision_key
                || manifest.upstream_manifest_hash != candidate.manifest_hash
            {
                return Err(unavailable("download_source_acquisition_stale"));
            }
            if manifest
                .source_reference
                .revision_key
                .starts_with("pending:")
            {
                return Err(LarmError::new(ErrorCode::UnsupportedCapability,
                    "La fuente debe proporcionar una revisión fijada antes de preparar esta descarga",
                    "download_source_revision_not_pinned"));
            }
        }
        let candidate_variant = self
            .variants
            .get_variant(&candidate.variant_id)
            .await?
            .filter(|variant| variant.revision_id == candidate.revision_id)
            .ok_or_else(|| unavailable("download_source_candidate_stale"))?;
        let revision = models
            .get_revision(&candidate.revision_id)
            .await?
            .filter(|revision| {
                revision.revision_key == candidate.revision_key
                    && revision.manifest_hash == candidate.manifest_hash
                    && (revision.state == RevisionState::Current
                        || (acquisition.is_some()
                            && matches!(
                                revision.state,
                                RevisionState::Candidate
                                    | RevisionState::Retired
                                    | RevisionState::Superseded
                            )))
            })
            .ok_or_else(|| unavailable("download_source_candidate_stale"))?;
        let distribution = models
            .get_distribution(&revision.distribution_id)
            .await?
            .filter(|distribution| {
                distribution.source_id == candidate.source_id
                    && distribution.canonical_model_id == target_distribution.canonical_model_id
                    && distribution.license_id.is_none()
            })
            .ok_or_else(|| unavailable("download_source_candidate_stale"))?;
        if acquisition.is_none() {
            self.variants
                .bundle_of(&candidate_variant.id)
                .await?
                .filter(|bundle| {
                    bundle.variant_id == candidate_variant.id
                        && bundle.state == larm_domain::model::BundleState::Verified
                        && bundle.manifest_hash == candidate.manifest_hash
                })
                .ok_or_else(|| unavailable("download_source_candidate_stale"))?;
        }
        let _ = distribution;
        let binding = self.sources.execution_binding(&candidate.source_id).await?;
        if binding.installation.family != "source" {
            return Err(unavailable("source_worker_family_mismatch"));
        }
        if requires_source_auth(&binding.source) {
            return Err(LarmError::new(
                ErrorCode::AuthRequired,
                "La fuente requiere credenciales que el consumidor aún no resuelve",
                "download_source_auth_unavailable",
            ));
        }
        let access = source_access(
            &binding.source,
            &binding.adapter.source_type,
            &binding.installation,
        )?;
        let current = self
            .conformance
            .current_for_consumer(&binding.installation.id)
            .await?
            .ok_or_else(|| unavailable("download_source_qualification_missing"))?;
        if current.0 != binding.installation {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instalación cambió desde su cualificación",
                "download_source_qualification_stale",
            ));
        }
        if let Some(manifest) = &acquisition {
            if manifest.adapter_installation_id != current.0.id
                || manifest.adapter_installation_revision != current.0.row_version.to_string()
                || manifest.adapter_installation_sha256
                    != larm_domain::conformance::committed_installation_hash(&current.0)?
                || manifest.conformance_run_id != current.1.id
                || manifest.conformance_run_sha256 != current.1.digest()?
            {
                return Err(unavailable("download_source_acquisition_stale"));
            }
        }
        let support: SearchSupport = binding
            .adapter
            .metadata
            .get("source_protocol_v1")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| unavailable("download_source_protocol_binding_missing"))?;
        let context = MutationContext {
            actor_id: current.1.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
        };
        Ok(ResolvedDownloadSource {
            acquisition,
            installation: current.0,
            run: current.1,
            access,
            support,
            context,
        })
    }

    async fn open_planned(
        &self,
        record: &larm_domain::download::DownloadPlanRecord,
        candidate: &DownloadSourceCandidate,
        ctx: &MutationContext,
    ) -> Result<Box<dyn larm_download::consumer::DownloadSourceSession>, LarmError> {
        let resolved = self.resolve_planned(record, candidate, ctx).await?;
        let worker = self
            .runner
            .source_worker(
                &resolved.installation,
                &resolved.run,
                &candidate.source_id,
                resolved.support,
                &resolved.context,
            )
            .await?;
        if !worker.supports_download_chunks() {
            let _ = worker.close().await;
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El worker cualificado no ofrece descarga por bloques",
                "download_source_chunk_protocol_unavailable",
            ));
        }
        Ok(Box::new(QualifiedDownloadSourceSession {
            acquisition: resolved.acquisition,
            worker,
            access: resolved.access,
        }))
    }
}

#[async_trait]
impl larm_download::consumer::DownloadSourceResolver for QualifiedDownloadSourceResolver {
    async fn open(
        &self,
        claim: &larm_db::download_jobs::DownloadClaim,
        record: &larm_domain::download::DownloadPlanRecord,
        candidate: &DownloadSourceCandidate,
    ) -> Result<Box<dyn larm_download::consumer::DownloadSourceSession>, LarmError> {
        self.open_planned(
            record,
            candidate,
            &MutationContext {
                actor_id: claim.actor_id.clone(),
                correlation_id: claim.correlation_id.clone(),
            },
        )
        .await
    }
}

/// Admisión P051 sin RPC: valida la misma fuente y cualificación física que
/// usará el consumer. Éste valida el plan source antes de transferir bytes.
pub struct QualifiedDownloadExecutionGate {
    pub sources: Arc<QualifiedDownloadSourceResolver>,
}

#[async_trait]
impl larm_domain::ports::DownloadPlanExecutionGate for QualifiedDownloadExecutionGate {
    async fn authorize(
        &self,
        record: &larm_domain::download::DownloadPlanRecord,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let candidate = larm_download::consumer::primary_source_candidate(record)?;
        let resolved = self
            .sources
            .resolve_planned(record, &candidate, ctx)
            .await?;
        self.sources
            .runner
            .verify_qualification_snapshot(&resolved.installation, &resolved.run, &resolved.context)
            .await
    }
}

pub struct OnlineDownloadStorageRootResolver {
    pub locations: larm_db::locations::SqlStorageLocations,
}

#[async_trait]
impl larm_download::consumer::DownloadStorageRootResolver for OnlineDownloadStorageRootResolver {
    async fn root(&self, location_id: &str) -> Result<PathBuf, LarmError> {
        self.resolve(location_id, false).await
    }

    async fn cleanup_root(&self, location_id: &str) -> Result<PathBuf, LarmError> {
        self.resolve(location_id, true).await
    }
}

impl OnlineDownloadStorageRootResolver {
    async fn resolve(&self, location_id: &str, cleanup: bool) -> Result<PathBuf, LarmError> {
        let location: StorageLocation = self
            .locations
            .get(location_id)
            .await?
            .ok_or_else(|| unavailable("download_storage_location_missing"))?;
        if location.state != LocationState::Online
            && !(cleanup && location.state == LocationState::LowSpace)
        {
            return Err(unavailable("download_storage_location_offline"));
        }
        let path = PathBuf::from(&location.canonical_path);
        if !path.is_absolute() || !path.is_dir() {
            return Err(unavailable("download_storage_root_unavailable"));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::Map;

    fn source(base_url: &str, auth_mode: AuthMode) -> ModelSource {
        ModelSource {
            id: "0192f000-0000-7000-8000-00000000f201".into(),
            created_at: "2026-10-24T00:00:00.000Z".into(),
            updated_at: "2026-10-24T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            name: "fixture".into(),
            adapter_id: "0192f000-0000-7000-8000-00000000f202".into(),
            base_url: base_url.into(),
            priority: 0,
            enabled: true,
            auth_mode,
            account_id: None,
            policy: Map::new(),
            operational: Default::default(),
        }
    }

    fn installation(manifest: serde_json::Value) -> larm_domain::adapters::AdapterInstallation {
        larm_domain::adapters::AdapterInstallation {
            id: "0192f000-0000-7000-8000-00000000f203".into(),
            created_at: "2026-10-24T00:00:00.000Z".into(),
            updated_at: "2026-10-24T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            adapter_id: "fixture.source".into(),
            version: "1.0.0".into(),
            family: "source".into(),
            package_hash: "a".repeat(64),
            publisher: "fixture".into(),
            trust: larm_domain::adapters::AdapterTrust::Verified,
            manifest,
            state: larm_domain::adapters::AdapterState::Validated,
        }
    }

    fn manifest(methods: &[&str], network_origins: &[&str]) -> serde_json::Value {
        let table = larm_adapter_sdk::manifest::MethodTable::contract();
        let required = table
            .required_for(larm_adapter_sdk::manifest::Family::Source)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut method_names = table
            .mandatory_common
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        method_names.extend(required);
        method_names.extend(methods.iter().map(|method| (*method).to_owned()));
        let package_hash = "a".repeat(64);
        serde_json::json!({
            "id":"fixture.source", "version":"1.0.0", "family":"source",
            "protocol_version":"1.0", "publisher":"fixture",
            "executable":{"relative_path":"bin/adapter.exe","args":[]},
            "platforms":["windows"], "architectures":["x86_64"],
            "methods":method_names, "permissions":{
                "read_roots":[], "write_roots":[], "network_origins":network_origins,
                "process_allowlist":[], "secret_refs":[]
            }, "capabilities":[], "package_sha256":package_hash, "signature":null,
            "installation_strategies":[]
        })
    }

    #[test]
    fn acceso_remoto_solo_concede_el_origen_configurado_y_rechaza_auth() {
        let installation = installation(manifest(
            &[
                "core.heartbeat",
                "source.download_plan",
                "source.download_chunk",
            ],
            &["https://models.example"],
        ));
        let access = source_access(
            &source("https://models.example/api", AuthMode::None),
            "HTTP",
            &installation,
        )
        .unwrap();
        assert_eq!(access.urls, ["https://models.example/"]);
        assert!(access.reads.is_empty());

        let error = source_access(
            &source("https://models.example/api", AuthMode::Bearer),
            "HTTP",
            &installation,
        )
        .unwrap_err();
        assert_eq!(error.details.reason, "download_source_auth_unavailable");
    }

    #[test]
    fn cuenta_asociada_no_exige_auth_si_la_fuente_es_publica() {
        let installation = installation(manifest(
            &[
                "core.heartbeat",
                "source.download_plan",
                "source.download_chunk",
            ],
            &["https://models.example"],
        ));
        let mut source = source("https://models.example/api", AuthMode::None);
        source.account_id = Some("0192f000-0000-7000-8000-00000000f204".into());

        let access = source_access(&source, "HTTP", &installation).unwrap();

        assert_eq!(access.urls, ["https://models.example/"]);
        assert!(!requires_source_auth(&source));
    }

    #[test]
    fn acceso_rechaza_worker_sin_chunk_y_built_in_sin_ejecucion() {
        let incomplete = installation(manifest(
            &["core.heartbeat", "source.download_plan"],
            &["https://models.example"],
        ));
        assert_eq!(
            source_access(
                &source("https://models.example", AuthMode::None),
                "HTTP",
                &incomplete
            )
            .unwrap_err()
            .details
            .reason,
            "download_source_chunk_protocol_unavailable"
        );
        let mut built_in = installation(serde_json::json!({}));
        built_in
            .metadata
            .insert("execution".into(), serde_json::Value::String("none".into()));
        assert_eq!(
            source_access(&source("file:///", AuthMode::None), "LOCAL", &built_in)
                .unwrap_err()
                .details
                .reason,
            "download_source_execution_unavailable"
        );
    }
}
