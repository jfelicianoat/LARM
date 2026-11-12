//! P030: casos de uso del inventario; descubrir no equivale a instalar o promover.
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::MutationContext;
use larm_domain::runtime::{ObservedChannel, Runtime, RuntimeChannelPolicy, RuntimeCreate};
use larm_domain::runtime_ports::RuntimeRepository;
use std::sync::Arc;

#[derive(Clone)]
pub struct RuntimeCatalog {
    pub repo: Arc<dyn RuntimeRepository>,
}

#[async_trait::async_trait]
impl larm_domain::runtime_ports::RuntimeCatalogService for RuntimeCatalog {
    async fn create(
        &self,
        request: &RuntimeCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        RuntimeCatalog::create(self, request, key, body_hash, ctx).await
    }
    async fn change_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeChannelPolicy,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        RuntimeCatalog::change_policy(self, id, if_match, policy, ctx).await
    }
}

impl RuntimeCatalog {
    pub async fn create(
        &self,
        request: &RuntimeCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        request.validate()?;
        self.repo.create_runtime(request, key, body_hash, ctx).await
    }
    pub async fn change_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeChannelPolicy,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        policy.validate()?;
        self.repo
            .change_channel_policy(id, if_match, policy, ctx)
            .await
    }
    /// Filtra observaciones del upstream con la política actual. No filtra la
    /// consulta del inventario, modifica pins/defaults ni devuelve autoridad IO.
    pub async fn discovery_channels(
        &self,
        id: &str,
        upstream: &[String],
    ) -> Result<Vec<ObservedChannel>, LarmError> {
        let runtime = self.repo.runtime(id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Runtime desconocido",
                "runtime_not_found",
            )
            .with_resource(id)
        })?;
        runtime.channel_policy.validate()?;
        Ok(upstream
            .iter()
            .map(|name| ObservedChannel::from_upstream(name))
            .filter(|channel| channel.automatic_selection_allowed(&runtime.channel_policy))
            .collect())
    }
}
