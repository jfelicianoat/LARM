//! P031: publicación sólo después de recoger todas las páginas y drenar el IO.
use crate::version_discovery::{collect, DiscoveryBatch};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::MutationContext;
use larm_domain::runtime::RuntimeVersion;
use larm_domain::runtime_execution::{RuntimeDiscoveryHost, RuntimeExecutionRepository};
use std::{num::NonZeroU32, sync::Arc};

pub struct DiscoveryService {
    repository: Arc<dyn RuntimeExecutionRepository>,
    host: Arc<dyn RuntimeDiscoveryHost>,
    max_pages: NonZeroU32,
}

#[derive(Debug, PartialEq, serde::Serialize)]
pub struct DiscoveryResult {
    pub operation_id: String,
    pub versions: Vec<RuntimeVersion>,
    pub page_snapshots: Vec<(larm_domain::runtime::RuntimeChannel, String)>,
}

impl DiscoveryService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn RuntimeExecutionRepository>,
        host: Arc<dyn RuntimeDiscoveryHost>,
        max_pages: NonZeroU32,
    ) -> Self {
        Self {
            repository,
            host,
            max_pages,
        }
    }

    /// No publica páginas parciales. Un cierre fallido impide publicar; un
    /// error del RPC conserva su causa aunque falle también la limpieza.
    pub async fn discover(
        &self,
        runtime: &str,
        ctx: &MutationContext,
    ) -> Result<DiscoveryResult, LarmError> {
        let context = self.repository.prepare_execution(runtime, ctx).await?;
        let expected_binding = context.binding()?;
        let session = self.host.open(&context, ctx).await?;
        let batch: Result<DiscoveryBatch, LarmError> = if session.binding() != &expected_binding {
            Err(LarmError::new(
                ErrorCode::Forbidden,
                "La sesión pertenece a otro runtime",
                "runtime_discovery_session_mismatch",
            ))
        } else {
            collect(session.as_ref(), &context.runtime, self.max_pages).await
        };
        // El guard sigue retenido mientras se cierra el proceso y confirma DB.
        let closed = session.close().await;
        let batch = batch?;
        closed?;
        let versions = self
            .repository
            .commit_discovery(&context, &batch.versions, session.as_ref(), ctx)
            .await?;
        Ok(DiscoveryResult {
            operation_id: context.operation_id,
            versions,
            page_snapshots: batch.page_snapshots,
        })
    }
}
