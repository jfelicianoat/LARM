//! `POST /storage-locations/{id}/probe` como job (P014): vuelve a sondear el
//! almacenamiento y aplica el último estado a las ubicaciones. Como el
//! refresco de hardware, un solo sondeo a la vez.

use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::bridge::{JobReceipt, JobState};
use larm_domain::error::LarmError;
use larm_domain::ports::{
    ApiScope, JobSource, JournalJobRepository, LocationService, MutationContext, Principal,
};

use crate::hardware::{RefreshTask, StorageRefresh};

#[derive(Clone)]
pub struct LocationProbe {
    pub storage: StorageRefresh,
    pub jobs: Arc<dyn JournalJobRepository>,
    pub locations: Arc<dyn LocationService>,
}

impl LocationProbe {
    /// Encola el sondeo de la ubicación `id` (debe existir).
    ///
    /// # Errors
    /// `NOT_FOUND` o errores del repositorio de jobs.
    pub async fn start(
        &self,
        ctx: &MutationContext,
        id: &str,
    ) -> Result<(JobReceipt, Option<RefreshTask>), LarmError> {
        self.locations.get(id).await?;
        let (job, created) = self.jobs.enqueue(ctx, Some(id)).await?;
        let receipt = JobReceipt {
            status_url: format!("/api/v1/jobs/{}", job.state.job_id),
            job_id: job.state.job_id.clone(),
            state: job.state.state,
        };
        if !created {
            return Ok((receipt, None));
        }
        let service = self.clone();
        let (job_id, location_id, ctx) = (job.state.job_id, id.to_string(), ctx.clone());
        let task: RefreshTask =
            Box::pin(async move { service.run(&job_id, &location_id, &ctx).await });
        Ok((receipt, Some(task)))
    }

    async fn run(&self, job_id: &str, location_id: &str, ctx: &MutationContext) {
        if let Err(error) = self.jobs.running(job_id).await {
            tracing::error!(job_id = %job_id, detail = %error.technical_detail(), "el sondeo de ubicación no pudo empezar");
            return;
        }
        let outcome = async {
            let (observation, changes) = self.storage.refresh(ctx).await?;
            let location = self.locations.get(location_id).await?;
            Ok::<_, LarmError>(serde_json::json!({
                "storage_location_id": location.id,
                "state": location.state,
                "free_bytes": location.free_bytes,
                "capacity_bytes": location.capacity_bytes,
                "volume_key": location.volume_key,
                "changes": changes.len(),
                "observed_at": observation.observed_at,
            }))
        }
        .await;
        let stored = match outcome {
            Ok(result) => self.jobs.complete(job_id, location_id, result, &[]).await,
            Err(mut error) => {
                if let Ok(c) = larm_domain::correlation::CorrelationId::parse(&ctx.correlation_id) {
                    error = error.with_correlation(c);
                }
                tracing::error!(job_id = %job_id, detail = %error.technical_detail(), "el sondeo de ubicación falló");
                self.jobs.fail(job_id, &error).await
            }
        };
        if let Err(error) = stored {
            tracing::error!(job_id = %job_id, detail = %error.technical_detail(), "no se pudo cerrar el job de ubicación");
        }
    }
}

#[async_trait]
impl JobSource for LocationProbe {
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        Ok(self.jobs.get(id).await?.and_then(|job| {
            (job.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
                .then_some(job.state)
        }))
    }
}
