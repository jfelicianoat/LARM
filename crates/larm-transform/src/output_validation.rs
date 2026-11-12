//! Server-owned format validators. A worker's verify reply cannot grant
//! technical validation; registered hooks must load, health-check and smoke-test.
use crate::output::VerifiedTransformationOutput;
use larm_domain::error::{ErrorCode, LarmError};
use std::{collections::BTreeMap, sync::Arc};

#[async_trait::async_trait]
pub trait TransformationRuntimeValidator: Send + Sync {
    async fn load_health_smoke(
        &self,
        output: &VerifiedTransformationOutput,
    ) -> Result<(), LarmError>;
}

struct Registration {
    id: String,
    version: String,
    validator: Arc<dyn TransformationRuntimeValidator>,
}

pub use larm_domain::transformation_publication::TransformationRuntimeValidation;

/// Not deserializable. It owns the same immutable file handles across any
/// runtime hook, and records absence explicitly instead of inventing success.
pub struct CheckedTransformationOutput {
    output: VerifiedTransformationOutput,
    runtime: TransformationRuntimeValidation,
}
impl CheckedTransformationOutput {
    pub fn output(&self) -> &VerifiedTransformationOutput {
        &self.output
    }
    pub fn runtime(&self) -> &TransformationRuntimeValidation {
        &self.runtime
    }
}

#[async_trait::async_trait]
impl larm_domain::transformation_publication::TransformationMeasuredOutputGuard
    for CheckedTransformationOutput
{
    fn measured(&self) -> larm_domain::transformation_publication::MeasuredTransformationOutput {
        use larm_domain::transformation_publication::{
            MeasuredTransformationArtifact, MeasuredTransformationOutput,
        };
        let output = &self.output;
        MeasuredTransformationOutput {
            fence: output.fence().clone(),
            binding: output.binding().clone(),
            worker_job_id: output.worker_job_id().into(),
            worker_manifest_hash: output.worker_manifest_hash().into(),
            target_format: output.target_format().into(),
            policy: output.policy().clone(),
            runtime: self.runtime.clone(),
            artifacts: output
                .entries()
                .into_iter()
                .enumerate()
                .map(|(i, entry)| MeasuredTransformationArtifact {
                    entry,
                    worker_relative_path: output.source_relative_path(i).unwrap_or_default().into(),
                    worker_role: output.role(i).unwrap_or_default().into(),
                })
                .collect(),
        }
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.output.recheck()
    }
}

#[derive(Default)]
pub struct TransformationOutputValidators {
    formats: BTreeMap<String, Registration>,
}
impl TransformationOutputValidators {
    pub fn register(
        &mut self,
        format: &str,
        id: &str,
        version: &str,
        validator: Arc<dyn TransformationRuntimeValidator>,
    ) -> Result<(), LarmError> {
        if [format, id, version]
            .iter()
            .any(|value| value.is_empty() || value.trim() != *value || value.len() > 256)
            || self.formats.contains_key(&format.to_ascii_lowercase())
        {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El validador de salida no se pudo registrar",
                "transformation_output_validator_invalid",
            ));
        }
        self.formats.insert(
            format.to_ascii_lowercase(),
            Registration {
                id: id.into(),
                version: version.into(),
                validator,
            },
        );
        Ok(())
    }

    pub async fn validate(
        &self,
        output: VerifiedTransformationOutput,
    ) -> Result<CheckedTransformationOutput, LarmError> {
        output.recheck()?;
        let runtime = if let Some(registration) = self
            .formats
            .get(&output.target_format().to_ascii_lowercase())
        {
            registration.validator.load_health_smoke(&output).await?;
            output.recheck()?;
            TransformationRuntimeValidation::Passed {
                validator_id: registration.id.clone(),
                validator_version: registration.version.clone(),
            }
        } else {
            TransformationRuntimeValidation::NotRegistered
        };
        Ok(CheckedTransformationOutput { output, runtime })
    }
}
