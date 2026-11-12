//! P031: plan previo y binding de solicitud. No hace IO ni concede ejecución.
//! El coordinador debe resolver P022/P026/P099, reservar con P019 y consumir la
//! aprobación dentro del journal antes de usar este plan. Los comandos concretos
//! siguen siendo los del adaptador/perfil cualificado, nunca flags inventados aquí.
use larm_domain::adapter_updates::is_sha256;
use larm_domain::approval::{ApprovalRecord, ApprovalUse};
use larm_domain::artifacts::{normalize_relative, Plan, PlanStep};
use larm_domain::bridge::{is_uuid_v7, timestamp};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::runtime::{InstallationStrategy, RegistrationMode, RuntimeOwnership};
use larm_domain::runtime_install::{
    InstallRequest, INSTALL_OPERATION as DOMAIN_INSTALL_OPERATION,
    INSTALL_SPEC as DOMAIN_INSTALL_SPEC,
};
use serde::{Deserialize, Serialize};
use serde_json::Map;

pub const INSTALL_SPEC: &str = DOMAIN_INSTALL_SPEC;
pub const INSTALL_OPERATION: &str = DOMAIN_INSTALL_OPERATION;

/// Documento interno en Plan.steps[].request. No añade campos a InstallRequest
/// ni reemplaza el profile lock P099. Se guarda íntegro bajo plan_hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallSpec {
    pub request: InstallRequest,
    pub runtime_version_id: String,
    /// Hash del plan original que el adaptador retiene en su propio journal.
    /// `plan_hash` externo se recalcula después de añadir el binding de LARM.
    #[serde(default)]
    pub adapter_plan_hash: String,
    pub adapter_installation_id: String,
    pub adapter_package_hash: String,
    pub registration_mode: RegistrationMode,
    pub ownership: RuntimeOwnership,
    #[serde(deserialize_with = "Option::deserialize")]
    pub relative_destination: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub environment_prefix: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub dependencies_lock_hash: Option<String>,
    pub global_effects: bool,
    pub reboot_required: bool,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Plan de instalación no válido",
        reason,
    )
}
fn isolated(strategy: InstallationStrategy) -> bool {
    matches!(
        strategy,
        InstallationStrategy::Pip
            | InstallationStrategy::Uv
            | InstallationStrategy::Conda
            | InstallationStrategy::Mamba
            | InstallationStrategy::PythonVenv
    )
}

fn requires_dependency_lock(strategy: InstallationStrategy) -> bool {
    isolated(strategy) || strategy == InstallationStrategy::Nuget
}

fn shape(
    plan: &Plan,
    request: &InstallRequest,
    version: &str,
    require_version: bool,
) -> Result<(), LarmError> {
    if !is_uuid_v7(&plan.plan_id)
        || plan.operation != INSTALL_OPERATION
        || !plan.resource_ids.contains(&request.runtime_id)
        || !plan.resource_ids.contains(&request.storage_location_id)
        || (require_version && !plan.resource_ids.iter().any(|id| id == version))
        || plan.steps.iter().any(|step| {
            step.phase.is_empty() || step.adapter_id.as_deref().is_some_and(str::is_empty)
        })
    {
        return Err(invalid("runtime_install_plan_invalid"));
    }
    let expires = chrono::DateTime::parse_from_rfc3339(&plan.expires_at)
        .map_err(|_| invalid("runtime_install_expiry_invalid"))?;
    if timestamp(expires.with_timezone(&chrono::Utc)) != plan.expires_at {
        return Err(invalid("runtime_install_expiry_invalid"));
    }
    Ok(())
}

impl InstallSpec {
    pub fn validate_for(&self, request: &InstallRequest) -> Result<(), LarmError> {
        request.validate()?;
        if self.request != *request {
            return Err(invalid("runtime_install_request_mismatch"));
        }
        if self.adapter_plan_hash.is_empty() {
            return Err(invalid("runtime_install_adapter_plan_hash_missing"));
        }
        if !is_uuid_v7(&self.runtime_version_id)
            || !is_sha256(&self.adapter_plan_hash)
            || !is_uuid_v7(&self.adapter_installation_id)
            || !is_sha256(&self.adapter_package_hash)
        {
            return Err(invalid("runtime_install_binding_invalid"));
        }
        if self
            .dependencies_lock_hash
            .as_deref()
            .is_some_and(|h| !is_sha256(h))
        {
            return Err(invalid("runtime_dependencies_lock_invalid"));
        }
        if request.strategy == InstallationStrategy::ExternalAdoption {
            if self.registration_mode != RegistrationMode::Adopted
                || self.ownership != RuntimeOwnership::External
                || self.relative_destination.is_some()
                || self.environment_prefix.is_some()
                || self.global_effects
                || self.reboot_required
            {
                return Err(invalid("runtime_adoption_must_be_read_only"));
            }
        } else {
            if self.registration_mode != RegistrationMode::Managed
                || self.ownership != RuntimeOwnership::LarmManaged
            {
                return Err(invalid("runtime_managed_ownership_invalid"));
            }
            let destination = self
                .relative_destination
                .as_deref()
                .ok_or_else(|| invalid("runtime_destination_missing"))?;
            // IDs y lock inmutables; una versión/perfil nuevo obtiene otra carpeta.
            let expected = format!(
                "runtimes/{}/{}/{}",
                request.runtime_id, self.runtime_version_id, request.profile_lock_hash
            );
            if normalize_relative(destination)? != expected || destination != expected {
                return Err(invalid("runtime_destination_not_side_by_side"));
            }
            if isolated(request.strategy) {
                if self.environment_prefix.as_deref() != Some(format!("{destination}/env").as_str())
                    || self.dependencies_lock_hash.is_none()
                {
                    return Err(invalid("runtime_isolated_environment_required"));
                }
            } else if self.environment_prefix.is_some() {
                return Err(invalid("runtime_environment_not_applicable"));
            } else if requires_dependency_lock(request.strategy)
                && self.dependencies_lock_hash.is_none()
            {
                return Err(invalid("runtime_dependencies_lock_required"));
            }
        }
        Ok(())
    }

    /// Sella un Plan contractual del adaptador sin ejecutar sus pasos ni
    /// fabricar estimaciones, source hashes, permisos o comandos. Una solicitud
    /// no anunciada se rechaza antes de aceptar el plan.
    pub fn bind(
        &self,
        request: &InstallRequest,
        declared: &[InstallationStrategy],
        mut plan: Plan,
        adapter_id: &str,
    ) -> Result<Plan, LarmError> {
        request.require_advertised(declared)?;
        self.validate_for(request)?;
        shape(&plan, request, &self.runtime_version_id, false)?;
        if plan.plan_hash != self.adapter_plan_hash
            || plan_hash(&plan)? != self.adapter_plan_hash
            || adapter_id.is_empty()
            || !is_uuid_v7(&plan.plan_id)
            || plan.operation != INSTALL_OPERATION
            || !plan.resource_ids.contains(&request.runtime_id)
            || !plan.resource_ids.contains(&request.storage_location_id)
            || plan
                .steps
                .iter()
                .any(|s| s.phase.is_empty() || s.request.contains_key(INSTALL_SPEC))
        {
            return Err(invalid("runtime_install_plan_invalid"));
        }
        if !plan.resource_ids.contains(&self.runtime_version_id) {
            plan.resource_ids.push(self.runtime_version_id.clone());
            plan.resource_ids.sort();
        }
        shape(&plan, request, &self.runtime_version_id, true)?;
        let mut payload = Map::new();
        payload.insert(
            INSTALL_SPEC.into(),
            serde_json::to_value(self).map_err(|_| invalid("runtime_install_serialize"))?,
        );
        plan.steps.push(PlanStep {
            phase: "BIND_INSTALLATION".into(),
            adapter_id: Some(adapter_id.into()),
            request: payload,
        });
        plan.approval_required |= self.global_effects || self.reboot_required;
        plan.plan_hash = plan_hash(&plan)?;
        Ok(plan)
    }
}

pub fn plan_hash(plan: &Plan) -> Result<String, LarmError> {
    larm_domain::runtime_install::plan_hash(plan)
}

pub fn spec(plan: &Plan) -> Result<InstallSpec, LarmError> {
    let mut found = plan
        .steps
        .iter()
        .filter_map(|s| s.request.get(INSTALL_SPEC).map(|value| (s, value)));
    let (step, value) = found
        .next()
        .ok_or_else(|| invalid("runtime_install_spec_missing"))?;
    if found.next().is_some()
        || step.phase != "BIND_INSTALLATION"
        || step.adapter_id.as_deref().is_none_or(str::is_empty)
    {
        return Err(invalid("runtime_install_spec_ambiguous"));
    }
    serde_json::from_value(value.clone()).map_err(|_| invalid("runtime_install_spec_invalid"))
}

/// Validación pura previa al consumidor; no devuelve una autoridad de IO.
/// El journal concreto vuelve a comprobar y consumir approval_id atómicamente.
pub fn check_execution(
    plan: &Plan,
    request: &InstallRequest,
    actor: &str,
    approval: Option<&ApprovalRecord>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LarmError> {
    let binding = spec(plan)?;
    binding.validate_for(request)?;
    shape(plan, request, &binding.runtime_version_id, true)?;
    if (binding.global_effects || binding.reboot_required) && !plan.approval_required {
        return Err(invalid("runtime_install_approval_flag_mismatch"));
    }
    if plan_hash(plan)? != plan.plan_hash
        || plan.operation != INSTALL_OPERATION
        || !plan.resource_ids.contains(&request.runtime_id)
        || !plan.resource_ids.contains(&request.storage_location_id)
    {
        return Err(invalid("runtime_install_plan_hash_mismatch"));
    }
    let expires = chrono::DateTime::parse_from_rfc3339(&plan.expires_at)
        .map_err(|_| invalid("runtime_install_expiry_invalid"))?;
    if timestamp(expires.with_timezone(&chrono::Utc)) != plan.expires_at {
        return Err(invalid("runtime_install_expiry_invalid"));
    }
    if now >= expires {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "El plan de instalación ha caducado",
            "runtime_install_plan_expired",
        ));
    }
    if binding.global_effects || binding.reboot_required || plan.approval_required {
        let approval = approval.ok_or_else(|| {
            LarmError::new(
                ErrorCode::ApprovalRequired,
                "La instalación requiere una aprobación explícita",
                "runtime_install_approval_required",
            )
        })?;
        approval.check(
            &ApprovalUse {
                actor_id: actor,
                operation_digest: &plan.plan_hash,
                resource_ids: &plan.resource_ids,
                code_hash: Some(&binding.adapter_package_hash),
            },
            now,
        )?;
    }
    Ok(())
}
