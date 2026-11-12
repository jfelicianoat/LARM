//! P026: entrada por instalación, nunca por un JSON con un veredicto PASS.
//! El ejecutor real vive fuera de Core y conserva P022/IO/procesos.
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_domain::adapters::{AdapterState, AdapterTrust};
use larm_domain::conformance::{Execution, Outcome, Receipt};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterConformanceRunner, AdapterInstallationRepository,
    MutationContext,
};
use std::sync::Arc;

pub struct AdapterConformance {
    pub installations: Arc<dyn AdapterInstallationRepository>,
    pub evidence: Arc<dyn AdapterConformanceRepository>,
    pub runner: Arc<dyn AdapterConformanceRunner>,
}
fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(code, "No se pudo validar el adaptador", reason)
}
impl AdapterConformance {
    /// Sólo un ID registrado. La autorización de ejecución y sus concesiones
    /// son responsabilidad del runner concreto; no las infiere de VALIDATED.
    async fn prepare_execution(
        &self,
        id: &str,
        ctx: &MutationContext,
        expected: Option<&str>,
    ) -> Result<Execution, LarmError> {
        if ctx.actor_id.is_empty()
            || ctx.actor_id.len() > 256
            || ctx.actor_id.chars().any(char::is_control)
            || !larm_domain::bridge::is_uuid_v7(&ctx.correlation_id)
        {
            return Err(error(
                ErrorCode::ValidationFailed,
                "conformance_context_invalid",
            ));
        }
        if !larm_domain::bridge::is_uuid_v7(id) {
            return Err(error(
                ErrorCode::ValidationFailed,
                "adapter_installation_id_invalid",
            ));
        }
        let installation = self
            .installations
            .get(id)
            .await?
            .ok_or_else(|| error(ErrorCode::NotFound, "adapter_installation_not_found"))?;
        if expected.is_some_and(|digest| {
            larm_domain::adapter_probes::snapshot(&installation).as_deref() != Ok(digest)
        }) {
            return Err(error(ErrorCode::StaleRevision, "probe_snapshot_stale"));
        }
        if installation.trust == AdapterTrust::Revoked {
            return Err(error(ErrorCode::Forbidden, "adapter_revoked"));
        }
        if !matches!(
            installation.state,
            AdapterState::Installed | AdapterState::Validated
        ) {
            return Err(error(
                ErrorCode::InvalidStateTransition,
                "adapter_not_installed",
            ));
        }
        let manifest = AdapterManifest::parse(
            &serde_json::to_string(&installation.manifest)
                .map_err(|_| error(ErrorCode::ValidationFailed, "conformance_manifest_invalid"))?,
        )?;
        if manifest.id != installation.adapter_id
            || manifest.version != installation.version
            || manifest.family.as_str() != installation.family
            || manifest.publisher != installation.publisher
            || manifest.package_sha256 != installation.package_hash
        {
            return Err(error(
                ErrorCode::ValidationFailed,
                "conformance_manifest_mismatch",
            ));
        }
        let execution = self.runner.execute_conformance(&installation, ctx).await?;
        if execution.run.actor_id != ctx.actor_id {
            return Err(error(ErrorCode::Forbidden, "conformance_actor_mismatch"));
        }
        if !execution.run.matches(&installation)? {
            return Err(error(
                ErrorCode::StaleRevision,
                "conformance_snapshot_stale",
            ));
        }
        Ok(execution)
    }
    async fn finish(
        &self,
        execution: Execution,
        ctx: &MutationContext,
        job: Option<&str>,
    ) -> Result<Receipt, LarmError> {
        let (outcome, failure) = match execution.proof {
            Ok(proof) => (Outcome::Passed { proof }, None),
            Err(error) => (Outcome::failed(&error.code), Some(error)),
        };
        let receipt = if let Some(job) = job {
            self.evidence
                .commit_probe_conformance(job, &execution.run, &outcome, ctx)
                .await?
        } else {
            self.evidence
                .commit_conformance(&execution.run, &outcome, ctx)
                .await?
        };
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(receipt)
        }
    }
    pub async fn validate(&self, id: &str, ctx: &MutationContext) -> Result<Receipt, LarmError> {
        self.finish(self.prepare_execution(id, ctx, None).await?, ctx, None)
            .await
    }
}
#[async_trait::async_trait]
impl larm_domain::ports::AdapterProbeExecutor for AdapterConformance {
    async fn execute_probe(
        &self,
        job: &larm_domain::adapter_probes::ProbeJob,
    ) -> Result<Receipt, larm_domain::adapter_probes::ProbeFailure> {
        use larm_domain::adapter_probes::ProbeFailure;
        if job.phase != larm_domain::bridge::JobPhase::Running {
            return Err(ProbeFailure::before(error(
                ErrorCode::InvalidStateTransition,
                "probe_not_running",
            )));
        }
        let ctx = job.context();
        let execution = self
            .prepare_execution(&job.target, &ctx, Some(&job.installation_sha256))
            .await
            .map_err(ProbeFailure::before)?;
        let outcome = match execution.proof {
            Ok(proof) => Outcome::Passed { proof },
            Err(error) => Outcome::failed(&error.code),
        };
        self.evidence
            .commit_probe_conformance(&job.id, &execution.run, &outcome, &ctx)
            .await
            .map_err(ProbeFailure::unconfirmed)
    }
    async fn wait_probe_io(&self, id: &str) -> Result<(), LarmError> {
        self.runner.wait_conformance(id).await
    }
}
