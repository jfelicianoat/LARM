//! Composición del scheduler P050 con persistencia y workers cualificados.

use async_trait::async_trait;
use larm_adapter_host::conformance::NativeConformanceRunner;
use larm_adapter_sdk::{
    permissions::Access,
    source::{SearchSupport, SourceReply, SourceRequest, SyncRequest, SyncResult},
};
use larm_domain::{
    bridge::timestamp,
    error::{ErrorCode, LarmError},
    ports::{MutationContext, SourceWatchRepository},
    sources::{AuthMode, ModelSource},
    watchers::{SourceWatch, SourceWatchSyncFailure, SourceWatchSyncSuccess},
};
use larm_models::watcher::{SourceSyncExecutor, SourceWatchStore, SourceWatchWorker};
use std::{path::PathBuf, sync::Arc, time::Duration};

const SOURCE_SYNC_TIMEOUT: Duration = Duration::from_secs(60);
const WATCH_BATCH: u32 = 1;
const WATCH_POLL: Duration = Duration::from_secs(15);

fn sync_context(qualified_actor: &str) -> MutationContext {
    MutationContext {
        actor_id: qualified_actor.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn sync_method(manifest: &larm_adapter_sdk::manifest::AdapterManifest) -> Result<(), LarmError> {
    if !manifest
        .methods
        .iter()
        .any(|method| method == "source.sync")
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El adaptador no declara source.sync",
            "source_sync_method_unavailable",
        ));
    }
    Ok(())
}

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo sincronizar la fuente vigilada",
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

fn local_path(source: &ModelSource) -> Result<PathBuf, LarmError> {
    let path = if source.base_url.starts_with("file:") {
        url::Url::parse(&source.base_url)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| unavailable("source_watch_path_invalid"))?
    } else {
        PathBuf::from(&source.base_url)
    };
    if !path.is_absolute() || !path.is_dir() {
        return Err(unavailable("source_watch_path_unavailable"));
    }
    Ok(path)
}

fn source_sync_access(
    source: &ModelSource,
    source_type: &str,
    installation: &larm_domain::adapters::AdapterInstallation,
) -> Result<Access, LarmError> {
    let manifest =
        larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())?;
    sync_method(&manifest)?;
    match source_type {
        "LOCAL" if source.auth_mode == AuthMode::None => Ok(Access {
            reads: vec![local_path(source)?],
            ..Access::default()
        }),
        "UNC_PRIVATE" if source.auth_mode == AuthMode::WindowsIntegrated => Ok(Access {
            reads: vec![local_path(source)?],
            ..Access::default()
        }),
        "LOCAL" | "UNC_PRIVATE" => Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "La vigilancia local no admite este modo de autenticación",
            "source_watch_auth_unavailable",
        )),
        _ if source.auth_mode != AuthMode::None => Err(LarmError::new(
            ErrorCode::AuthRequired,
            "La fuente requiere credenciales que el watcher aún no resuelve",
            "source_watch_auth_unavailable",
        )),
        _ => {
            let url = url::Url::parse(&source.base_url)
                .map_err(|_| unavailable("source_watch_url_invalid"))?;
            let source_origin =
                origin(&url).ok_or_else(|| unavailable("source_watch_url_invalid"))?;
            let declared = manifest
                .permissions
                .network_origins
                .iter()
                .filter_map(|candidate| url::Url::parse(candidate).ok())
                .filter_map(|candidate| origin(&candidate))
                .collect::<Vec<_>>();
            if !declared.iter().any(|candidate| candidate == &source_origin) {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "El origen configurado no figura entre los permisos del adaptador",
                    "source_watch_origin_not_declared",
                ));
            }
            let mut urls = manifest
                .permissions
                .network_origins
                .iter()
                .filter_map(|candidate| url::Url::parse(candidate).ok())
                .filter_map(|candidate| origin(&candidate))
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

pub struct QualifiedSourceSyncExecutor {
    sources: larm_db::sources::SqlSources,
    conformance: larm_db::conformance::SqlAdapterConformance,
    runner: Arc<NativeConformanceRunner>,
}

impl QualifiedSourceSyncExecutor {
    #[must_use]
    pub fn new(db: &larm_db::pool::Db, runner: Arc<NativeConformanceRunner>) -> Self {
        Self {
            sources: larm_db::sources::SqlSources {
                pool: db.pool.clone(),
                qualification: Some(runner.clone()),
            },
            conformance: larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            },
            runner,
        }
    }
}

#[async_trait]
impl SourceSyncExecutor for QualifiedSourceSyncExecutor {
    async fn sync(&self, request: SyncRequest) -> Result<SyncResult, LarmError> {
        let binding = self.sources.execution_binding(&request.source_id).await?;
        if !binding.source.enabled {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La fuente está deshabilitada",
                "source_disabled",
            )
            .with_resource(&request.source_id));
        }
        let current = self
            .conformance
            .current_for_consumer(&binding.installation.id)
            .await?
            .ok_or_else(|| unavailable("source_watch_qualification_missing"))?;
        if current.0 != binding.installation {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instalación cambió desde su cualificación",
                "source_watch_qualification_stale",
            ));
        }
        let support: SearchSupport = binding
            .adapter
            .metadata
            .get("source_protocol_v1")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| unavailable("source_watch_protocol_binding_missing"))?;
        let access = source_sync_access(
            &binding.source,
            &binding.adapter.source_type,
            &binding.installation,
        )?;
        let context = sync_context(&current.1.actor_id);
        let worker = self
            .runner
            .source_worker(
                &current.0,
                &current.1,
                &request.source_id,
                support,
                &context,
            )
            .await?;
        let response = worker
            .call(
                SourceRequest::Sync(request),
                &access,
                Some(SOURCE_SYNC_TIMEOUT),
            )
            .await;
        worker.close().await?;
        match response? {
            SourceReply::Sync(result) => Ok(result),
            _ => Err(LarmError::new(
                ErrorCode::ProtocolError,
                "El adaptador respondió con otro método a source.sync",
                "source_watch_reply_mismatch",
            )),
        }
    }
}

struct DatabaseSourceWatchStore(larm_db::source_watches::SqlSourceWatches);

#[async_trait]
impl SourceWatchStore for DatabaseSourceWatchStore {
    async fn due(
        &self,
        now: &str,
        limit: u32,
    ) -> Result<Vec<SourceWatch>, larm_domain::bridge::ApiError> {
        self.0.due_source_watches(now, limit).await
    }

    async fn recover_interrupted(
        &self,
        recovered_at: &str,
        ctx: &MutationContext,
    ) -> Result<u64, larm_domain::bridge::ApiError> {
        self.0
            .recover_interrupted_source_syncs(recovered_at, ctx)
            .await
    }

    async fn started(
        &self,
        watch: &SourceWatch,
        started_at: &str,
        ctx: &MutationContext,
    ) -> Result<(), larm_domain::bridge::ApiError> {
        self.0
            .start_source_watch_sync(&watch.id, watch.row_version, started_at, ctx)
            .await
    }

    async fn succeeded(
        &self,
        watch: &SourceWatch,
        success: &SourceWatchSyncSuccess,
        ctx: &MutationContext,
    ) -> Result<(), larm_domain::bridge::ApiError> {
        self.0
            .record_source_watch_success(&watch.id, watch.row_version, success, ctx)
            .await
            .map(|_| ())
    }

    async fn failed(
        &self,
        watch: &SourceWatch,
        failure: &SourceWatchSyncFailure,
        ctx: &MutationContext,
    ) -> Result<(), larm_domain::bridge::ApiError> {
        self.0
            .record_source_watch_failure(&watch.id, watch.row_version, failure, ctx)
            .await
            .map(|_| ())
    }
}

pub fn worker(
    db: &larm_db::pool::Db,
    runner: Arc<NativeConformanceRunner>,
    settings: &larm_core::config::WatcherSettings,
) -> SourceWatchWorker {
    let repository = larm_db::source_watches::SqlSourceWatches {
        pool: db.pool.clone(),
    };
    SourceWatchWorker::new(
        Arc::new(DatabaseSourceWatchStore(repository)),
        Arc::new(QualifiedSourceSyncExecutor::new(db, runner)),
        Arc::new(larm_models::watcher::UnknownWatchCompatibility),
    )
    .with_schedule_limits(settings.jitter_max_seconds, settings.max_backoff_seconds)
}

pub fn spawn_consumer(
    worker: Arc<SourceWatchWorker>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let recovery_context = MutationContext {
            actor_id: "system:source-watcher".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        match worker
            .recover_interrupted(&timestamp(chrono::Utc::now()), &recovery_context)
            .await
        {
            Ok(recovered) if recovered > 0 => tracing::warn!(
                recovered,
                "sincronizaciones de fuentes interrumpidas al reiniciar se marcaron como error"
            ),
            Ok(_) => {}
            Err(error) => tracing::error!(
                reason = %error.details.reason,
                "no se pudieron recuperar sincronizaciones de fuentes interrumpidas"
            ),
        }
        loop {
            let context = MutationContext {
                actor_id: "system:source-watcher".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            };
            match worker
                .run_due_once(chrono::Utc::now(), WATCH_BATCH, &context)
                .await
            {
                Ok(summary) if summary.failed > 0 => tracing::warn!(
                    due = summary.due,
                    failed = summary.failed,
                    stale = summary.stale,
                    "algunas vigilancias de fuentes fallaron; se reintentarán con backoff"
                ),
                Ok(_) => {}
                Err(error) => tracing::error!(
                    reason = %error.details.reason,
                    "no se pudo consultar la cola de vigilancias de fuentes"
                ),
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(WATCH_POLL) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn manifest() -> larm_adapter_sdk::manifest::AdapterManifest {
        serde_json::from_value(json!({"id":"test.source","version":"1.0.0","family":"source","protocol_version":"1.0","publisher":"owned test","executable":{"relative_path":"worker.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":["source.sync"],"permissions":{"read_roots":[],"write_roots":[],"network_origins":["http://127.0.0.1:12345"],"process_allowlist":[],"secret_refs":[]},"capabilities":[],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]})).unwrap()
    }

    #[test]
    fn sync_declared_method_does_not_need_a_duplicate_capability() {
        let manifest = manifest();
        assert!(sync_method(&manifest).is_ok());
    }

    #[test]
    fn sync_without_declared_method_stays_closed() {
        let mut manifest = manifest();
        manifest.methods.clear();
        manifest.capabilities.push("source.sync".into());
        let error = sync_method(&manifest).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_CAPABILITY");
        assert_eq!(error.details.reason, "source_sync_method_unavailable");
    }

    #[test]
    fn sync_execution_preserves_the_approved_actor_and_new_correlation() {
        let actor = "0190f5c0-7a00-7000-8000-000000000001";
        let context = sync_context(actor);
        assert_eq!(context.actor_id, actor);
        assert!(larm_domain::bridge::is_uuid_v7(&context.correlation_id));
    }
}
