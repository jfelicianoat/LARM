//! Binding público entre IDs del Core y un manifiesto source fijado (3.12.1).

use super::{
    invalid, ArtifactManifest, DownloadPlanRequest, SourceArtifact, SourceCatalogReference,
};
use larm_domain::adapter_updates::is_sha256;
use larm_domain::bridge::is_uuid_v7;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::bundle_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionArtifactBinding {
    pub artifact_id: String,
    pub relative_path: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionBinding {
    pub revision: SourceCatalogReference,
    pub manifest_hash: String,
    pub artifacts: Vec<SourceAcquisitionArtifactBinding>,
}

impl SourceAcquisitionBinding {
    /// Forma y selección del request; no consulta el catálogo ni concede permisos.
    pub fn validate_for(&self, request: &DownloadPlanRequest) -> Result<(), LarmError> {
        self.revision.validate()?;
        if request.source_id.as_deref() != Some(self.revision.source_id.as_str())
            || self.revision.revision_key.starts_with("pending:")
            || !is_sha256(&self.manifest_hash)
            || self.artifacts.is_empty()
            || self.artifacts.len() > 4096
            || !self.artifacts.iter().any(|a| a.required)
            || !self.artifacts.windows(2).all(|p| {
                (&p[0].relative_path, &p[0].artifact_id) < (&p[1].relative_path, &p[1].artifact_id)
            })
        {
            return Err(invalid("source_acquisition_binding_invalid"));
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut optional = BTreeSet::new();
        for artifact in &self.artifacts {
            if !is_uuid_v7(&artifact.artifact_id)
                || artifact.relative_path.chars().count() > 4096
                || bundle_path(&artifact.relative_path).is_err()
                || !ids.insert(artifact.artifact_id.as_str())
                || !paths.insert(artifact.relative_path.to_lowercase())
            {
                return Err(invalid("source_acquisition_artifacts_invalid"));
            }
            if !artifact.required {
                optional.insert(artifact.artifact_id.as_str());
            }
        }
        let selected = request.optional_artifact_ids.as_deref().unwrap_or_default();
        let set: BTreeSet<_> = selected.iter().map(String::as_str).collect();
        if set.len() != selected.len() || !set.is_subset(&optional) {
            return Err(invalid("source_acquisition_optional_selection_invalid"));
        }
        Ok(())
    }

    /// Comprueba el manifiesto actual completo y devuelve sólo los archivos
    /// elegidos. Ninguna identidad se busca a partir del UUID privado del worker.
    pub fn select(
        &self,
        request: &DownloadPlanRequest,
        manifest: &ArtifactManifest,
    ) -> Result<Vec<SourceArtifact>, LarmError> {
        self.validate_for(request)?;
        manifest.validate()?;
        if manifest.revision_key != self.revision.revision_key
            || manifest.manifest_hash != self.manifest_hash
            || manifest.artifacts.len() != self.artifacts.len()
        {
            return Err(changed());
        }
        let selected: BTreeSet<_> = request
            .optional_artifact_ids
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect();
        let mut result = Vec::new();
        for binding in &self.artifacts {
            let artifact = manifest
                .artifacts
                .iter()
                .find(|a| a.relative_path == binding.relative_path)
                .filter(|a| a.required == binding.required)
                .ok_or_else(changed)?;
            if binding.required || selected.contains(binding.artifact_id.as_str()) {
                result.push(artifact.clone());
            }
        }
        Ok(result)
    }

    pub(crate) fn validate_selection(
        &self,
        request: &DownloadPlanRequest,
        artifacts: &[SourceArtifact],
    ) -> Result<(), LarmError> {
        self.validate_for(request)?;
        let selected: BTreeSet<_> = request
            .optional_artifact_ids
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<_> = self
            .artifacts
            .iter()
            .filter(|a| a.required || selected.contains(a.artifact_id.as_str()))
            .map(|a| (&a.relative_path, a.required))
            .collect();
        let actual: BTreeSet<_> = artifacts
            .iter()
            .map(|a| (&a.relative_path, a.required))
            .collect();
        if actual.len() != artifacts.len() || expected != actual {
            return Err(invalid("source_acquisition_selection_changed"));
        }
        Ok(())
    }
}

fn changed() -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "El manifiesto de la fuente cambió",
        "source_acquisition_manifest_changed",
    )
}
