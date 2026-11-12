//! Documento interno de Plan.steps[].request, cubierto por plan_hash.
//! No sustituye admisión/locks/journal ni el ejecutor de descargas de P050.
use super::{
    invalid, validate_artifacts, wire, AuthResult, DownloadPlanRequest, DownloadPriority,
    RevisionRef, SourceArtifact,
};
use larm_domain::artifacts::{Plan, PlanStep};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::Confidence;
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SOURCE_DOWNLOAD_SPEC: &str = "source_download_v1";
pub const SOURCE_ACQUISITION_SPEC: &str = "source_acquisition_v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDownloadSpec {
    pub variant_id: String,
    pub storage_location_id: String,
    pub revision: RevisionRef,
    pub artifacts: Vec<SourceArtifact>,
    pub auth: AuthResult,
    pub resumable: bool,
    pub priority: DownloadPriority,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_budget_bytes: Option<DecimalU64>,
}
impl SourceDownloadSpec {
    pub fn validate_for(&self, request: &DownloadPlanRequest) -> Result<(), LarmError> {
        self.revision.validate()?;
        validate_artifacts(&self.artifacts)?;
        if self.artifacts.is_empty() || self.auth.reason.is_empty() {
            return Err(invalid("download_spec"));
        }
        if self.variant_id != request.variant_id
            || self.storage_location_id != request.storage_location_id
            || self.priority != request.priority
            || self.hard_budget_bytes != request.hard_budget_bytes
            || request
                .source_id
                .as_ref()
                .is_some_and(|s| *s != self.revision.source_id)
        {
            return Err(invalid("download_request_mismatch"));
        }
        if let Some(binding) = &request.acquisition {
            if serde_json::to_value(&self.revision).map_err(|_| invalid("serialize"))?
                != serde_json::to_value(&binding.revision).map_err(|_| invalid("serialize"))?
            {
                return Err(invalid("source_acquisition_revision_changed"));
            }
            binding.validate_selection(request, &self.artifacts)?;
        }
        Ok(())
    }
    /// Suma comprobada de puntos declarados; un archivo sin punto conserva null.
    pub fn download_bytes(&self) -> Result<Option<DecimalU64>, LarmError> {
        let mut sum = 0u64;
        let mut unknown = false;
        for artifact in &self.artifacts {
            let point = artifact.size_bytes.or_else(|| {
                artifact
                    .size_estimate
                    .as_ref()
                    .and_then(|estimate| estimate.bytes)
            });
            match point {
                Some(size) => {
                    sum = sum
                        .checked_add(size.get())
                        .filter(|s| *s <= i64::MAX as u64)
                        .ok_or_else(|| invalid("size_overflow"))?
                }
                None => unknown = true,
            }
        }
        Ok(if unknown { None } else { DecimalU64::new(sum) })
    }
    pub fn check_execution_requirements(&self, confidence: Confidence) -> Result<(), LarmError> {
        if self.auth.auth_required && !self.auth.authenticated {
            return Err(LarmError::new(
                ErrorCode::AuthRequired,
                "La fuente requiere autenticación",
                "source_auth_required",
            ));
        }
        if self.auth.license_required {
            return Err(LarmError::new(
                ErrorCode::LicenseRequired,
                "La fuente requiere aceptar la licencia",
                "source_license_required",
            ));
        }
        if confidence == Confidence::Unknown && self.hard_budget_bytes.is_none() {
            return Err(LarmError::new(
                ErrorCode::UnknownSize,
                "Se necesita una cota o presupuesto explícito",
                "source_size_unknown",
            ));
        }
        Ok(())
    }
    /// Añadir la especificación al único paso de fuente y sellar el Plan.
    /// El estimate físico procede del preflight del propietario; no se inventa.
    pub fn bind(
        &self,
        request: &DownloadPlanRequest,
        mut plan: Plan,
        adapter_id: &str,
    ) -> Result<Plan, LarmError> {
        self.validate_for(request)?;
        if adapter_id.is_empty() {
            return Err(invalid("adapter_id"));
        }
        let mut payload = Map::new();
        payload.insert(
            SOURCE_DOWNLOAD_SPEC.into(),
            serde_json::to_value(self).map_err(|_| invalid("serialize"))?,
        );
        if let Some(binding) = &request.acquisition {
            payload.insert(
                SOURCE_ACQUISITION_SPEC.into(),
                serde_json::to_value(binding).map_err(|_| invalid("serialize"))?,
            );
        }
        plan.steps.push(PlanStep {
            phase: "DOWNLOAD".into(),
            adapter_id: Some(adapter_id.into()),
            request: payload,
        });
        plan.plan_hash = plan_hash(&plan)?;
        validate_source_plan(request, &plan)?;
        Ok(plan)
    }
}

pub fn plan_hash(plan: &Plan) -> Result<String, LarmError> {
    let mut value = serde_json::to_value(plan).map_err(|_| invalid("serialize"))?;
    if let Value::Object(map) = &mut value {
        map.remove("plan_hash");
    }
    larm_domain::canonical::sha256_hex(&value).map_err(|_| invalid("plan_canonical"))
}
pub fn download_spec(plan: &Plan) -> Result<SourceDownloadSpec, LarmError> {
    let mut specs = plan
        .steps
        .iter()
        .filter_map(|step| step.request.get(SOURCE_DOWNLOAD_SPEC).map(|v| (step, v)));
    let (step, value) = specs
        .next()
        .ok_or_else(|| invalid("download_spec_missing"))?;
    if specs.next().is_some() || step.adapter_id.as_deref().is_none_or(str::is_empty) {
        return Err(invalid("download_spec_ambiguous"));
    }
    wire::decode(value.clone())
}
pub fn validate_source_plan(request: &DownloadPlanRequest, plan: &Plan) -> Result<(), LarmError> {
    let spec = download_spec(plan)?;
    spec.validate_for(request)?;
    if request.acquisition.is_some()
        && (!plan.resource_ids.contains(&request.variant_id)
            || !plan.resource_ids.contains(&request.storage_location_id))
    {
        return Err(invalid("source_acquisition_resources_changed"));
    }
    let received = plan
        .steps
        .iter()
        .filter_map(|s| s.request.get(SOURCE_ACQUISITION_SPEC))
        .collect::<Vec<_>>();
    match (&request.acquisition, received.as_slice()) {
        (None, []) => {}
        (Some(binding), [value])
            if **value == serde_json::to_value(binding).map_err(|_| invalid("serialize"))? => {}
        _ => return Err(invalid("source_acquisition_binding_changed")),
    }
    if plan_hash(plan)? != plan.plan_hash {
        return Err(invalid("plan_hash_mismatch"));
    }
    let sizes = spec.download_bytes()?;
    if plan.storage_estimate.download_bytes != sizes {
        return Err(invalid("download_size_mismatch"));
    }
    match plan.storage_estimate.confidence {
        Confidence::Exact
            if spec
                .artifacts
                .iter()
                .any(|a| a.size_bytes.is_none() || a.sha256.is_none()) =>
        {
            return Err(invalid("exact_requires_manifest"))
        }
        Confidence::High if sizes.is_none() => return Err(invalid("high_requires_sizes")),
        Confidence::Estimated if plan.storage_estimate.upper_bound_bytes.is_none() => {
            return Err(invalid("estimated_requires_bound"))
        }
        _ => {}
    }
    Ok(())
}
