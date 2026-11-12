//! Catálogo remoto inmutable, separado de la verificación física (3.12.0).

use crate::adapter_updates::is_sha256;
use crate::bridge::{is_uuid_v7, timestamp};
use crate::error::{ErrorCode, LarmError};
use crate::model::{bundle_path, ArtifactSizeConfidence, ArtifactSizeEstimate};
use crate::units::DecimalU64;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Referencia pública source; nunca un UUID privado interpretado por el Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCatalogReference {
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
    pub revision_key: String,
}

fn text_valid(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 4096
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "El manifiesto de adquisición no es válido",
        reason,
    )
}

impl SourceCatalogReference {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.source_id)
            || !text_valid(&self.upstream_namespace)
            || !text_valid(&self.upstream_model_id)
            || !text_valid(&self.revision_key)
        {
            return Err(invalid("source_catalog_reference_invalid"));
        }
        Ok(())
    }
}

/// Descriptor remoto; no contiene ubicación local ni afirma bytes verificados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAcquisitionArtifact {
    pub artifact_id: String,
    pub relative_path: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub size_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub sha256: Option<String>,
    pub required: bool,
    pub role: String,
    pub size_estimate: ArtifactSizeEstimate,
}

impl ModelAcquisitionArtifact {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.artifact_id)
            || !text_valid(&self.relative_path)
            || bundle_path(&self.relative_path).is_err()
            || !text_valid(&self.role)
            || self.sha256.as_deref().is_some_and(|h| !is_sha256(h))
        {
            return Err(invalid("acquisition_artifact_invalid"));
        }
        self.size_estimate.validate()?;
        let outside_bounds = self.size_bytes.is_some_and(|size| {
            self.size_estimate
                .lower_bound_bytes
                .is_some_and(|lower| size < lower)
                || self
                    .size_estimate
                    .upper_bound_bytes
                    .is_some_and(|upper| size > upper)
        });
        if matches!((self.size_bytes, self.size_estimate.bytes), (Some(a), Some(b)) if a != b)
            || outside_bounds
            || (self.size_estimate.confidence == ArtifactSizeConfidence::Exact
                && (self.sha256.is_none() || self.size_bytes != self.size_estimate.bytes))
        {
            return Err(invalid("acquisition_artifact_size_conflict"));
        }
        Ok(())
    }
}

/// Snapshot append-only: identidad central, descriptores y evidencia histórica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAcquisitionManifest {
    pub id: String,
    pub variant_id: String,
    pub revision_id: String,
    pub source_id: String,
    pub source_reference: SourceCatalogReference,
    pub upstream_manifest_hash: String,
    pub snapshot_hash: String,
    pub observed_at: String,
    pub required_artifact_ids: Vec<String>,
    pub optional_artifact_ids: Vec<String>,
    pub artifacts: Vec<ModelAcquisitionArtifact>,
    pub adapter_installation_id: String,
    pub adapter_installation_revision: String,
    pub adapter_installation_sha256: String,
    pub conformance_run_id: String,
    pub conformance_run_sha256: String,
}

/// Binding sellado en preview y plan; el hash upstream se conserva separado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionPlanBinding {
    pub manifest_id: String,
    pub snapshot_hash: String,
    pub upstream_manifest_hash: String,
}

fn ordered_ids(ids: &[String]) -> bool {
    ids.iter().all(|id| is_uuid_v7(id)) && ids.windows(2).all(|p| p[0] < p[1])
}

impl ModelAcquisitionManifest {
    fn validate_fields(&self) -> Result<(), LarmError> {
        if [
            &self.id,
            &self.variant_id,
            &self.revision_id,
            &self.source_id,
            &self.adapter_installation_id,
            &self.conformance_run_id,
        ]
        .iter()
        .any(|id| !is_uuid_v7(id))
            || [
                &self.upstream_manifest_hash,
                &self.adapter_installation_sha256,
                &self.conformance_run_sha256,
            ]
            .iter()
            .any(|h| !is_sha256(h))
            || self
                .adapter_installation_revision
                .parse::<i64>()
                .ok()
                .is_none_or(|v| v < 1 || v.to_string() != self.adapter_installation_revision)
            || chrono::DateTime::parse_from_rfc3339(&self.observed_at)
                .ok()
                .is_none_or(|v| timestamp(v.into()) != self.observed_at)
        {
            return Err(invalid("acquisition_snapshot_fields_invalid"));
        }
        self.source_reference.validate()?;
        if self.source_reference.source_id != self.source_id {
            return Err(invalid("acquisition_source_mismatch"));
        }
        if self.artifacts.is_empty()
            || self.artifacts.len() > 4096
            || self.required_artifact_ids.is_empty()
            || self.required_artifact_ids.len() > 4096
            || self.optional_artifact_ids.len() > 4096
            || !ordered_ids(&self.required_artifact_ids)
            || !ordered_ids(&self.optional_artifact_ids)
            || !self.artifacts.windows(2).all(|p| {
                (&p[0].relative_path, &p[0].artifact_id) < (&p[1].relative_path, &p[1].artifact_id)
            })
        {
            return Err(invalid("acquisition_artifact_set_invalid"));
        }
        let required: BTreeSet<_> = self.required_artifact_ids.iter().collect();
        let optional: BTreeSet<_> = self.optional_artifact_ids.iter().collect();
        if !required.is_disjoint(&optional)
            || required.len() + optional.len() != self.artifacts.len()
        {
            return Err(invalid("acquisition_artifact_set_invalid"));
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !ids.insert(&artifact.artifact_id)
                || !paths.insert(artifact.relative_path.to_lowercase())
                || (artifact.required && !required.contains(&artifact.artifact_id))
                || (!artifact.required && !optional.contains(&artifact.artifact_id))
            {
                return Err(invalid("acquisition_artifact_set_invalid"));
            }
        }
        Ok(())
    }

    fn digest(&self) -> Result<String, LarmError> {
        let mut payload = serde_json::to_value(self)
            .map_err(|_| invalid("acquisition_snapshot_encoding_invalid"))?;
        payload
            .as_object_mut()
            .ok_or_else(|| invalid("acquisition_snapshot_encoding_invalid"))?
            .remove("snapshot_hash");
        crate::canonical::sha256_hex(&payload)
            .map_err(|_| invalid("acquisition_snapshot_encoding_invalid"))
    }

    /// Ordena y sella una observación nueva; no concede cualificación ni VERIFIED.
    pub fn seal(mut self) -> Result<Self, LarmError> {
        self.required_artifact_ids.sort();
        self.optional_artifact_ids.sort();
        self.artifacts.sort_by(|a, b| {
            (&a.relative_path, &a.artifact_id).cmp(&(&b.relative_path, &b.artifact_id))
        });
        self.validate_fields()?;
        self.snapshot_hash = self.digest()?;
        Ok(self)
    }

    /// Comprueba forma, semántica y sello sin normalizar datos recibidos.
    pub fn validate(&self) -> Result<(), LarmError> {
        self.validate_fields()?;
        if !is_sha256(&self.snapshot_hash) || self.snapshot_hash != self.digest()? {
            return Err(invalid("acquisition_snapshot_hash_mismatch"));
        }
        Ok(())
    }

    #[must_use]
    pub fn binding(&self) -> AcquisitionPlanBinding {
        AcquisitionPlanBinding {
            manifest_id: self.id.clone(),
            snapshot_hash: self.snapshot_hash.clone(),
            upstream_manifest_hash: self.upstream_manifest_hash.clone(),
        }
    }

    /// Selección para el preflight: conserva las certezas declaradas y no crea
    /// un bundle físico. Rechaza IDs opcionales duplicados, requeridos o ajenos.
    pub fn download_estimates(
        &self,
        optional_ids: &[String],
    ) -> Result<Vec<crate::download::DownloadArtifactEstimate>, LarmError> {
        self.validate()?;
        let selected: BTreeSet<_> = optional_ids.iter().collect();
        if selected.len() != optional_ids.len()
            || selected
                .iter()
                .any(|id| !self.optional_artifact_ids.contains(id))
        {
            return Err(invalid("optional_artifact_selection_invalid"));
        }
        Ok(self
            .artifacts
            .iter()
            .map(|artifact| crate::download::DownloadArtifactEstimate {
                artifact_id: artifact.artifact_id.clone(),
                relative_path: artifact.relative_path.clone(),
                required: artifact.required,
                selected: artifact.required || selected.contains(&artifact.artifact_id),
                size_estimate: artifact.size_estimate.clone(),
                sha256: artifact.sha256.clone(),
            })
            .collect())
    }

    /// Vincula el record entero con este snapshot. No confunde el hash remoto
    /// con snapshot_hash ni con un manifest físico producido tras recibir bytes.
    pub fn validate_download_record(
        &self,
        record: &crate::download::DownloadPlanRecord,
    ) -> Result<(), LarmError> {
        let estimates = self.download_estimates(&record.request.optional_artifact_ids)?;
        let mut selected = record.request.optional_artifact_ids.clone();
        selected.sort();
        let response = &record.response;
        if response.acquisition.as_ref() != Some(&self.binding())
            || record.request.variant_id != self.variant_id
            || response.variant_id != self.variant_id
            || record.revision_id != self.revision_id
            || record.revision_key != self.source_reference.revision_key
            || record.manifest_hash != self.upstream_manifest_hash
            || response.source_id.as_deref() != Some(self.source_id.as_str())
            || record
                .request
                .source_id
                .as_deref()
                .is_some_and(|id| id != self.source_id)
            || response.optional_artifact_ids != selected
            || response.artifact_estimates != estimates
        {
            return Err(invalid("acquisition_plan_binding_mismatch"));
        }
        Ok(())
    }
}

/// Ausente es limitación explícita; presente y malformado es error.
pub fn catalog_reference(
    variant: &crate::model::ModelVariant,
) -> Result<Option<SourceCatalogReference>, LarmError> {
    let Some(value) = variant
        .metadata
        .get("source_v1")
        .and_then(|v| v.get("normalized"))
        .and_then(|v| v.get("catalog_reference"))
    else {
        return Ok(None);
    };
    let reference: SourceCatalogReference = serde_json::from_value(value.clone())
        .map_err(|_| invalid("source_catalog_reference_invalid"))?;
    reference.validate()?;
    Ok(Some(reference))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::{ArtifactSizeConfidence, ArtifactSizeSource};

    fn id() -> String {
        uuid::Uuid::now_v7().to_string()
    }
    fn fixture() -> ModelAcquisitionManifest {
        let source_id = id();
        let artifact_id = id();
        ModelAcquisitionManifest {
            id: id(),
            variant_id: id(),
            revision_id: id(),
            source_id: source_id.clone(),
            source_reference: SourceCatalogReference {
                source_id,
                upstream_namespace: "fixture".into(),
                upstream_model_id: "model".into(),
                revision_key: "r1".into(),
            },
            upstream_manifest_hash: "a".repeat(64),
            snapshot_hash: String::new(),
            observed_at: "2026-10-27T16:00:00.000Z".into(),
            required_artifact_ids: vec![artifact_id.clone()],
            optional_artifact_ids: vec![],
            artifacts: vec![ModelAcquisitionArtifact {
                artifact_id,
                relative_path: "weights.bin".into(),
                size_bytes: None,
                sha256: None,
                required: true,
                role: "weights".into(),
                size_estimate: ArtifactSizeEstimate::unknown("not_reported", "fixture:manifest"),
            }],
            adapter_installation_id: id(),
            adapter_installation_revision: "2".into(),
            adapter_installation_sha256: "b".repeat(64),
            conformance_run_id: id(),
            conformance_run_sha256: "c".repeat(64),
        }
        .seal()
        .unwrap()
    }

    fn download_record(snapshot: &ModelAcquisitionManifest) -> crate::download::DownloadPlanRecord {
        use crate::download::*;
        let request = DownloadPlanRequest {
            variant_id: snapshot.variant_id.clone(),
            storage_location_id: None,
            hard_budget_bytes: DecimalU64::new(8 << 20),
            source_id: None,
            priority: DownloadPriority::Normal,
            optional_artifact_ids: vec![],
        };
        DownloadPlanRecord {
            revision_id: snapshot.revision_id.clone(),
            revision_key: snapshot.source_reference.revision_key.clone(),
            manifest_hash: snapshot.upstream_manifest_hash.clone(),
            storage_observed_at: snapshot.observed_at.clone(),
            response: DownloadPlanResponse {
                acquisition: Some(snapshot.binding()),
                preview_id: id(),
                preview_hash: "d".repeat(64),
                variant_id: request.variant_id.clone(),
                optional_artifact_ids: vec![],
                artifact_estimates: snapshot.download_estimates(&[]).unwrap(),
                destination_options: vec![],
                selected_storage_location_id: None,
                plan_id: None,
                plan_hash: None,
                storage_estimate: None,
                source_id: Some(snapshot.source_id.clone()),
                source_candidates: vec![],
                hard_budget_bytes: request.hard_budget_bytes,
                priority: request.priority,
                expires_at: "2026-10-27T16:05:00.000Z".into(),
                operator_confirmation_required: true,
            },
            request,
        }
    }

    #[test]
    fn download_record_is_bound_to_snapshot_reference_and_all_declared_descriptors() {
        let snapshot = fixture();
        let record = download_record(&snapshot);
        snapshot.validate_download_record(&record).unwrap();
        assert_eq!(record.manifest_hash, snapshot.upstream_manifest_hash);
        assert_ne!(record.manifest_hash, snapshot.snapshot_hash);
        assert!(record.response.artifact_estimates[0]
            .size_estimate
            .bytes
            .is_none());
        let mutations = [
            ("/response/acquisition/manifest_id", serde_json::json!(id())),
            (
                "/response/acquisition/snapshot_hash",
                serde_json::json!("e".repeat(64)),
            ),
            (
                "/response/acquisition/upstream_manifest_hash",
                serde_json::json!("e".repeat(64)),
            ),
            ("/revision_id", serde_json::json!(id())),
            ("/revision_key", serde_json::json!("other-revision")),
            ("/manifest_hash", serde_json::json!("e".repeat(64))),
            ("/response/variant_id", serde_json::json!(id())),
            ("/response/source_id", serde_json::json!(id())),
            ("/request/source_id", serde_json::json!(id())),
            (
                "/response/artifact_estimates/0/relative_path",
                serde_json::json!("other.bin"),
            ),
            (
                "/response/artifact_estimates/0/selected",
                serde_json::json!(false),
            ),
            (
                "/response/artifact_estimates/0/sha256",
                serde_json::json!("e".repeat(64)),
            ),
        ];
        for (pointer, value) in mutations {
            let mut changed = serde_json::to_value(&record).unwrap();
            *changed.pointer_mut(pointer).unwrap() = value;
            let changed = serde_json::from_value(changed).unwrap();
            assert!(
                snapshot.validate_download_record(&changed).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn acquisition_optional_selection_preserves_unknown_and_rejects_aliased_ids() {
        let mut snapshot = fixture();
        let optional_id = id();
        let mut optional = snapshot.artifacts[0].clone();
        optional.artifact_id = optional_id.clone();
        optional.relative_path = "zz.optional".into();
        optional.required = false;
        snapshot.optional_artifact_ids.push(optional_id.clone());
        snapshot.artifacts.push(optional);
        let snapshot = snapshot.seal().unwrap();
        let unselected = snapshot.download_estimates(&[]).unwrap();
        assert!(!unselected[1].selected);
        let selected = snapshot
            .download_estimates(std::slice::from_ref(&optional_id))
            .unwrap();
        assert!(selected.iter().all(|item| item.selected
            && item.size_estimate.bytes.is_none()
            && item.sha256.is_none()));
        for bad in [
            vec![optional_id.clone(), optional_id],
            vec![snapshot.required_artifact_ids[0].clone()],
            vec![id()],
        ] {
            assert_eq!(
                snapshot
                    .download_estimates(&bad)
                    .unwrap_err()
                    .details
                    .reason,
                "optional_artifact_selection_invalid"
            );
        }
    }

    #[test]
    fn independent_utf8_digest_vector_preserves_bytes_above_javascript_integer_precision() {
        let text = include_str!("../../../tests/fixtures/models/acquisition-3.12.0.json");
        let snapshot: ModelAcquisitionManifest = serde_json::from_str(text).unwrap();
        snapshot.validate().unwrap();
        assert_eq!(
            snapshot.artifacts[0].size_bytes.unwrap().get(),
            9007199254740993
        );
        let encoded = serde_json::to_value(snapshot).unwrap();
        assert_eq!(encoded["artifacts"][0]["size_bytes"], "9007199254740993");
    }

    #[test]
    fn snapshot_unknown_roundtrips_without_fabricating_physical_state() {
        let s = fixture();
        s.validate().unwrap();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("state").is_none() && value.get("location").is_none());
        assert!(value["artifacts"][0]["size_bytes"].is_null());
        assert!(value["artifacts"][0]["size_estimate"]["bytes"].is_null());
        assert_eq!(
            serde_json::from_value::<ModelAcquisitionManifest>(value).unwrap(),
            s
        );
        assert_ne!(s.snapshot_hash, s.upstream_manifest_hash);
    }

    #[test]
    fn snapshot_seals_every_identity_descriptor_and_qualification() {
        let s = fixture();
        for field in [
            "id",
            "variant_id",
            "revision_id",
            "source_id",
            "upstream_manifest_hash",
            "observed_at",
            "adapter_installation_id",
            "adapter_installation_revision",
            "adapter_installation_sha256",
            "conformance_run_id",
            "conformance_run_sha256",
        ] {
            let mut v = serde_json::to_value(&s).unwrap();
            v[field] = serde_json::Value::String(if field.ends_with("_id") || field == "id" {
                id()
            } else if field.ends_with("hash") || field.ends_with("sha256") {
                "d".repeat(64)
            } else if field == "observed_at" {
                "2026-10-27T16:00:01.000Z".into()
            } else {
                "3".into()
            });
            assert!(
                serde_json::from_value::<ModelAcquisitionManifest>(v)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
        let mut changed = s.clone();
        changed.artifacts[0].role = "tokenizer".into();
        assert!(changed.validate().is_err());
        let mut extra = serde_json::to_value(s).unwrap();
        extra["verified"] = true.into();
        assert!(serde_json::from_value::<ModelAcquisitionManifest>(extra).is_err());
    }

    #[test]
    fn artifact_confidences_preserve_bounds_and_reject_conflicts() {
        let mut s = fixture();
        for estimate in [
            ArtifactSizeEstimate {
                confidence: ArtifactSizeConfidence::Exact,
                bytes: DecimalU64::new(10),
                lower_bound_bytes: DecimalU64::new(10),
                upper_bound_bytes: DecimalU64::new(10),
                source: ArtifactSizeSource::Manifest,
                evidence_refs: vec!["fixture:manifest".into()],
                reason: None,
            },
            ArtifactSizeEstimate {
                confidence: ArtifactSizeConfidence::High,
                bytes: DecimalU64::new(10),
                lower_bound_bytes: None,
                upper_bound_bytes: DecimalU64::new(10),
                source: ArtifactSizeSource::HttpHead,
                evidence_refs: vec!["fixture:head".into()],
                reason: None,
            },
            ArtifactSizeEstimate {
                confidence: ArtifactSizeConfidence::Estimated,
                bytes: None,
                lower_bound_bytes: DecimalU64::new(8),
                upper_bound_bytes: DecimalU64::new(12),
                source: ArtifactSizeSource::SourceMetadata,
                evidence_refs: vec!["fixture:range".into()],
                reason: None,
            },
        ] {
            s.artifacts[0].size_bytes = estimate.bytes;
            s.artifacts[0].sha256 = Some("d".repeat(64));
            s.artifacts[0].size_estimate = estimate.clone();
            let sealed = s.clone().seal().unwrap();
            sealed.validate().unwrap();
            assert_eq!(sealed.artifacts[0].size_estimate, estimate);
        }
        for declared in [7, 13] {
            s.artifacts[0].size_bytes = DecimalU64::new(declared);
            assert!(
                s.clone().seal().is_err(),
                "declared size outside the interval"
            );
        }
        s.artifacts[0].size_bytes = None;
        s.artifacts[0].size_estimate.confidence = ArtifactSizeConfidence::Exact;
        assert!(s.clone().seal().is_err());
        s.artifacts[0].size_estimate = ArtifactSizeEstimate::unknown("unknown", "fixture");
        s.artifacts[0].size_estimate.upper_bound_bytes = DecimalU64::new(0);
        assert!(s.seal().is_err());
    }

    #[test]
    fn acquisition_rejects_unsafe_duplicate_or_foreign_artifacts() {
        for path in [
            "../weights.bin",
            "C:/weights.bin",
            "folder/../weights.bin",
            "a:stream",
            "a\\b",
        ] {
            let mut s = fixture();
            s.artifacts[0].relative_path = path.into();
            assert!(s.seal().is_err(), "{path}");
        }
        let s = fixture();
        let mut d = s.clone();
        let mut a = d.artifacts[0].clone();
        a.artifact_id = id();
        a.relative_path = "WEIGHTS.BIN".into();
        d.required_artifact_ids.push(a.artifact_id.clone());
        d.artifacts.push(a);
        assert!(d.seal().is_err());
        let mut d = s.clone();
        d.optional_artifact_ids
            .push(d.required_artifact_ids[0].clone());
        assert!(d.seal().is_err());
        let mut d = s.clone();
        d.required_artifact_ids[0] = id();
        assert!(d.seal().is_err());
        let mut d = s;
        d.source_reference.source_id = id();
        assert!(d.seal().is_err());
    }

    #[test]
    fn acquisition_rejects_noncanonical_time_revision_and_reference() {
        for time in ["2026-10-27T16:00:00Z", "2026-10-27T17:00:00.000+01:00"] {
            let mut s = fixture();
            s.observed_at = time.into();
            assert!(s.seal().is_err());
        }
        for revision in ["0", "02", "-1", "9223372036854775808"] {
            let mut s = fixture();
            s.adapter_installation_revision = revision.into();
            assert!(s.seal().is_err());
        }
        for name in [" model", "model\n", ""] {
            let mut s = fixture();
            s.source_reference.upstream_model_id = name.into();
            assert!(s.seal().is_err());
        }
    }
}
