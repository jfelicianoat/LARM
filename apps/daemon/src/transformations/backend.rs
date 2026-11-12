use super::*;
use larm_adapter_host::{
    conformance::profiles::{LocalProfile, Recipe},
    transformation::SessionIdentity,
};
use larm_adapter_sdk::permissions::Access;
use larm_domain::{
    adapters::AdapterInstallation,
    conformance::Run,
    ports::{ConformanceProfileRepository, StorageLocationRepository},
    transformations::TransformationInputGuard,
};
use larm_transform::environment::WorkspaceRoot;

pub(super) struct Backend {
    pub installation: AdapterInstallation,
    pub planner_id: &'static str,
    pub run: Run,
    pub identity: SessionIdentity,
    pub backend_context: MutationContext,
    pub local: LocalProfile,
    pub profile: Value,
    pub access: Access,
}
impl Backend {
    pub async fn select(
        s: &Service,
        request: &TransformationRequest,
        ctx: &MutationContext,
    ) -> Result<Self, LarmError> {
        request.validate().map_err(|_| {
            error(
                ErrorCode::ValidationFailed,
                "transformation_request_invalid",
            )
        })?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM adapter_installation WHERE adapter_id=? AND family='transformation' AND state IN ('INSTALLED','VALIDATED') ORDER BY id LIMIT 2")
            .bind(&request.adapter_id).fetch_all(&s.inner.db.pool).await.map_err(db_error)?;
        if ids.is_empty() {
            return Err(error(ErrorCode::NotFound, "transformation_adapter_missing"));
        }
        if ids.len() != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "transformation_adapter_selection_ambiguous",
            ));
        }
        Self::load(s, &ids[0], ctx).await
    }
    async fn load(
        s: &Service,
        installation_id: &str,
        ctx: &MutationContext,
    ) -> Result<Self, LarmError> {
        let (installation, run) = larm_db::conformance::SqlAdapterConformance {
            pool: s.inner.db.qualification_reader(),
        }
        .current_for_consumer(installation_id)
        .await?
        .ok_or_else(|| error(ErrorCode::NotFound, "transformation_adapter_missing"))?;
        let record = larm_db::conformance_profiles::SqlConformanceProfiles {
            pool: s.inner.db.qualification_reader(),
        }
        .conformance_profile(installation_id)
        .await?
        .ok_or_else(|| {
            error(
                ErrorCode::InvalidStateTransition,
                "transformation_profile_missing",
            )
        })?;
        let local: LocalProfile = serde_json::from_value(record.configuration.clone())
            .map_err(|_| error(ErrorCode::IntegrityFailed, "transformation_profile_invalid"))?;
        // El planner id sella el paso de publicación y lo verifica
        // check_destination: cada receta registrada aporta el suyo.
        let planner = match &local.suite {
            Recipe::Transformation(reference) => (reference.profile.clone(), "reference.base64.v1"),
            Recipe::Gguf(gguf_recipe) => (gguf_recipe.profile.clone(), "product.gguf.v1"),
            Recipe::Onnx(onnx_recipe) => (onnx_recipe.profile.clone(), "product.onnx.v1"),
            _ => {
                return Err(error(
                    ErrorCode::UnsupportedCapability,
                    "transformation_planner_not_registered",
                ));
            }
        };
        let profile = planner.0;
        let identity = SessionIdentity {
            adapter_id: installation.adapter_id.clone(),
            installation_id: installation.id.clone(),
            run_id: run.id.clone(),
            run_sha256: run.digest()?,
            profile_sha256: larm_domain::conformance::hash(&record)?,
        };
        identity.validate()?;
        let backend_context = MutationContext {
            actor_id: record.actor_id,
            correlation_id: ctx.correlation_id.clone(),
        };
        let access = Access {
            reads: vec![
                local.package_dir.clone(),
                local.work_dir.join("input"),
                local.work_dir.join("profile"),
                local.work_dir.join("state"),
                local.work_dir.join("output"),
            ],
            writes: vec![local.work_dir.join("state"), local.work_dir.join("output")],
            ..Access::default()
        };
        Ok(Self {
            planner_id: planner.1,
            installation,
            run,
            identity,
            backend_context,
            local,
            profile,
            access,
        })
    }
    pub async fn for_plan(s: &Service, plan: &SealedTransformationPlan) -> Result<Self, LarmError> {
        plan.verify_seal().map_err(|_| {
            error(
                ErrorCode::IntegrityFailed,
                "transformation_plan_seal_invalid",
            )
        })?;
        let binding = plan
            .execution_binding
            .as_ref()
            .ok_or_else(|| error(ErrorCode::IntegrityFailed, "transformation_binding_missing"))?;
        let ctx = MutationContext {
            actor_id: String::new(),
            correlation_id: id(),
        };
        let backend = Self::load(s, &binding.adapter_installation_id, &ctx).await?;
        if backend.identity.adapter_id != plan.request.adapter_id
            || backend.identity.run_id != binding.conformance_run_id
            || backend.identity.run_sha256 != binding.conformance_run_sha256
            || backend.identity.profile_sha256 != binding.profile_sha256
        {
            return Err(error(
                ErrorCode::StaleRevision,
                "transformation_execution_binding_stale",
            ));
        }
        Ok(backend)
    }
    pub fn check_input(
        &self,
        request: &TransformationRequest,
        input: &dyn TransformationInputGuard,
    ) -> Result<(), LarmError> {
        let files = input.artifacts();
        if files.len() != 1
            || self.profile["input_variant_id"] != request.input_variant_id
            || self.profile["storage_location_id"] != request.storage_location_id
            || self.profile["input"]["relative_path"] != files[0].relative_path
            || self.profile["input"]["sha256"] != files[0].sha256
            || self.profile["input"]["size_bytes"] != files[0].size_bytes.to_string()
        {
            return Err(error(
                ErrorCode::Incompatible,
                "transformation_backend_input_not_bound",
            ));
        }
        Ok(())
    }
    pub fn workspaces(
        &self,
        plan: &SealedTransformationPlan,
    ) -> Result<Vec<WorkspaceRoot>, LarmError> {
        let p = plan.output_policy.as_ref().ok_or_else(|| {
            error(
                ErrorCode::IntegrityFailed,
                "transformation_output_policy_missing",
            )
        })?;
        Ok(["state", "output"]
            .into_iter()
            .map(|name| WorkspaceRoot {
                path: self.local.work_dir.join(name),
                volume_key: p.worker_volume_key.clone(),
            })
            .collect())
    }
    pub async fn check_destination(
        &self,
        s: &Service,
        plan: &SealedTransformationPlan,
    ) -> Result<(), LarmError> {
        let step = plan
            .response
            .steps
            .last()
            .filter(|step| step.phase == "register_managed_output" && step.adapter_id.is_none())
            .ok_or_else(|| {
                error(
                    ErrorCode::IntegrityFailed,
                    "transformation_public_plan_missing",
                )
            })?;
        let destination = s
            .inner
            .env
            .locations
            .get(&plan.request.storage_location_id)
            .await?
            .ok_or_else(|| error(ErrorCode::NotFound, "transformation_destination_missing"))?;
        let expected = serde_json::json!({"planner":self.planner_id,"storage_location_id":destination.id,"destination_row_version":destination.row_version,"profile_sha256":self.identity.profile_sha256});
        if step.request != expected {
            return Err(error(
                ErrorCode::StaleRevision,
                "transformation_destination_changed",
            ));
        }
        Ok(())
    }
}
