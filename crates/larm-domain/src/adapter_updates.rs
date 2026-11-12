//! P024: DTO público AdapterImport y journal interno de actualización.
//! Un candidato INSTALLED no se promueve ni recibe VALIDATED por instalarlo.
use serde::{Deserialize, Serialize};

use crate::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use crate::bridge::{is_uuid_v7, JobPhase, JobReceipt, JobState};
use crate::error::{ErrorCode, LarmError};
use crate::guard::{LockMode, Resource, ResourceKind};
use crate::units::DecimalU64;

pub const UPDATE_KIND: &str = "adapter.update";
pub const UPDATE_OPERATION: &str = "P024_post__adapters_id_update";
pub const UPDATE_GENERATION: i64 = 1;

fn required_nullable<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d)
}

/// Forma exacta de larm.schema.json: approval_id es obligatorio aun siendo null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterImport {
    pub package_path: String,
    pub expected_hash: String,
    #[serde(deserialize_with = "required_nullable")]
    pub approval_id: Option<String>,
}

pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl AdapterImport {
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.package_path.is_empty()
            || !is_sha256(&self.expected_hash)
            || self
                .approval_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de paquete no válida",
                "adapter_import_invalid",
            ));
        }
        Ok(())
    }
}

/// Pasos internos; el estado público continúa usando sólo JobPhase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpdateStep {
    Queued,
    Inspecting,
    Reserved,
    Extracting,
    Staged,
    Publishing,
    Completed,
    Failed,
}

impl UpdateStep {
    pub fn phase(self) -> JobPhase {
        match self {
            Self::Queued => JobPhase::Queued,
            Self::Completed => JobPhase::Completed,
            Self::Failed => JobPhase::Failed,
            _ => JobPhase::Running,
        }
    }
    pub fn next(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Queued, Self::Inspecting)
                | (Self::Reserved, Self::Extracting)
                | (Self::Extracting, Self::Staged)
                | (Self::Staged, Self::Publishing)
        )
    }
}

/// Prueba que el coordinador P022 presenta para consumir una aprobación.
/// El repositorio vuelve a validarla y consumirla dentro de la reserva.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateApproval {
    pub id: String,
    pub operation_digest: String,
    pub resource_ids: Vec<String>,
    pub code_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlan {
    pub candidate: AdapterInstallation,
    pub workspace_path: String,
    pub destination_path: String,
    pub volume_key: String,
    pub archive_bytes: DecimalU64,
    pub unpacked_bytes: DecimalU64,
    pub required_bytes: DecimalU64,
    pub approval: Option<UpdateApproval>,
}

impl UpdatePlan {
    /// El SDK/host comprueban manifiesto, permisos, firma y rutas físicas.
    /// Este límite protege la transacción ante un plan interno incoherente.
    pub fn validate(&self, target: &AdapterInstallation) -> Result<(), LarmError> {
        let peak = self
            .archive_bytes
            .get()
            .checked_add(self.unpacked_bytes.get());
        let required = peak.and_then(|p| p.checked_add((1 << 30).max(p.div_ceil(20))));
        if !is_uuid_v7(&self.candidate.id)
            || self.candidate.id == target.id
            || !self
                .candidate
                .adapter_id
                .eq_ignore_ascii_case(&target.adapter_id)
            || self.candidate.version.eq_ignore_ascii_case(&target.version)
            || self.candidate.family != target.family
            || self.candidate.row_version != 1
            || self.candidate.state != AdapterState::Installed
            || self.candidate.trust == AdapterTrust::Revoked
            || !is_sha256(&self.candidate.package_hash)
            || self.workspace_path.is_empty()
            || self.destination_path.is_empty()
            || self.volume_key.is_empty()
            || required != Some(self.required_bytes.get())
            || self.archive_bytes.get() > (1 << 30)
            || self.unpacked_bytes.get() > (1 << 30)
            || self.approval.as_ref().is_some_and(|a| {
                !is_uuid_v7(&a.id)
                    || !is_sha256(&a.operation_digest)
                    || a.resource_ids.is_empty()
                    || a.code_hash != self.candidate.package_hash
            })
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Plan de actualización incoherente",
                "adapter_update_plan_invalid",
            ));
        }
        Ok(())
    }

    pub fn locks(&self) -> Vec<(Resource, LockMode)> {
        vec![
            (
                Resource::new(ResourceKind::Volume, &self.volume_key),
                LockMode::Shared,
            ),
            (
                Resource::new(
                    ResourceKind::Artifact,
                    format!(
                        "adapter-package:{}/{}",
                        self.candidate.adapter_id.to_lowercase(),
                        self.candidate.version.to_lowercase()
                    ),
                ),
                LockMode::Exclusive,
            ),
        ]
    }
}

/// Persistido sólo en operation_journal; paths y request nunca van a JobState.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterUpdate {
    pub job_id: String,
    pub actor_id: String,
    pub correlation_id: String,
    pub target_installation_id: String,
    pub target_row_version: i64,
    pub request: AdapterImport,
    pub step: UpdateStep,
    pub plan: Option<UpdatePlan>,
    pub error: Option<LarmError>,
}

/// Plan durable de P028 para instalar un paquete sin instalación previa.
/// La instalación queda side-by-side en estado INSTALLED y no se promueve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterImportPlan {
    pub candidate: AdapterInstallation,
    pub workspace_path: String,
    pub destination_path: String,
    pub volume_key: String,
    pub archive_bytes: DecimalU64,
    pub unpacked_bytes: DecimalU64,
    pub required_bytes: DecimalU64,
    #[serde(deserialize_with = "required_nullable")]
    pub approval_id: Option<String>,
}

impl AdapterImportPlan {
    pub fn validate(&self) -> Result<(), LarmError> {
        let peak = self
            .archive_bytes
            .get()
            .checked_add(self.unpacked_bytes.get());
        let required = peak.and_then(|p| p.checked_add((1 << 30).max(p.div_ceil(20))));
        if !is_uuid_v7(&self.candidate.id)
            || self.candidate.row_version != 1
            || self.candidate.state != AdapterState::Installed
            || self.candidate.trust == AdapterTrust::Revoked
            || !is_sha256(&self.candidate.package_hash)
            || self.workspace_path.is_empty()
            || self.destination_path.is_empty()
            || self.volume_key.is_empty()
            || required != Some(self.required_bytes.get())
            || self.archive_bytes.get() > (1 << 30)
            || self.unpacked_bytes.get() > (1 << 30)
            || self
                .approval_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Plan de importación incoherente",
                "adapter_import_plan_invalid",
            ));
        }
        Ok(())
    }

    pub fn locks(&self) -> Vec<(Resource, LockMode)> {
        vec![
            (
                Resource::new(ResourceKind::Volume, &self.volume_key),
                LockMode::Shared,
            ),
            (
                Resource::new(
                    ResourceKind::Artifact,
                    format!(
                        "adapter-package:{}/{}",
                        self.candidate.adapter_id.to_lowercase(),
                        self.candidate.version.to_lowercase()
                    ),
                ),
                LockMode::Exclusive,
            ),
        ]
    }
}

/// Journal P028: contiene rutas únicamente en SQLite y nunca en JobState.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterImportJournal {
    pub job_id: String,
    pub actor_id: String,
    pub correlation_id: String,
    pub request: AdapterImport,
    pub step: UpdateStep,
    #[serde(deserialize_with = "required_nullable")]
    pub plan: Option<AdapterImportPlan>,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<LarmError>,
}

impl AdapterImportJournal {
    pub fn receipt(&self) -> JobReceipt {
        JobReceipt {
            job_id: self.job_id.clone(),
            status_url: format!("/api/v1/jobs/{}", self.job_id),
            state: JobPhase::Queued,
        }
    }

    pub fn public_state(&self) -> JobState {
        JobState {
            job_id: self.job_id.clone(),
            state: self.step.phase(),
            resource_type: "adapter_installation".into(),
            resource_id: self.plan.as_ref().map(|p| p.candidate.id.clone()),
            progress: serde_json::json!({"step":self.step})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            result: (self.step == UpdateStep::Completed).then(|| {
                serde_json::json!({
                    "adapter_installation_id": self.plan.as_ref().map(|p| &p.candidate.id),
                    "promoted": false
                })
            }),
            error: self.error.clone(),
            evidence_refs: Vec::new(),
        }
    }
}

impl AdapterUpdate {
    pub fn receipt(&self) -> JobReceipt {
        JobReceipt {
            job_id: self.job_id.clone(),
            status_url: format!("/api/v1/jobs/{}", self.job_id),
            state: JobPhase::Queued,
        }
    }
    pub fn public_state(&self) -> JobState {
        JobState {
            job_id: self.job_id.clone(),
            state: self.step.phase(),
            resource_type: "adapter_installation".into(),
            resource_id: Some(
                if self.step == UpdateStep::Completed {
                    self.plan
                        .as_ref()
                        .map_or(&self.target_installation_id, |p| &p.candidate.id)
                } else {
                    &self.target_installation_id
                }
                .clone(),
            ),
            progress: serde_json::json!({"step":self.step})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            result: (self.step == UpdateStep::Completed).then(|| {
                serde_json::json!({
                    "adapter_installation_id": self.plan.as_ref().map(|p| &p.candidate.id),
                    "promoted": false
                })
            }),
            error: self.error.clone(),
            evidence_refs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn adapter_import_cerrado_y_nullable_obligatorio_como_el_schema() {
        let value = serde_json::json!({"package_path":"C:/fixtures/package.zip", "expected_hash":"a".repeat(64),"approval_id":null});
        let request: AdapterImport = serde_json::from_value(value.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        for field in ["package_path", "expected_hash", "approval_id"] {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<AdapterImport>(bad).is_err(),
                "{field} es obligatorio"
            );
        }
        let mut extra = value;
        extra["trust"] = serde_json::json!("VERIFIED");
        assert!(serde_json::from_value::<AdapterImport>(extra).is_err());
        for (path, hash, approval) in [
            ("", "a".repeat(64), None),
            ("x", "A".repeat(64), None),
            ("x", "a".repeat(63), None),
            ("x", "a".repeat(64), Some(uuid::Uuid::new_v4().to_string())),
        ] {
            assert!(AdapterImport {
                package_path: path.into(),
                expected_hash: hash,
                approval_id: approval
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn journal_de_importacion_no_filtra_rutas_en_job_state() {
        let now = crate::bridge::timestamp(chrono::Utc::now());
        let candidate = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            adapter_id: "acme.import-fixture".into(),
            version: "1.0".into(),
            family: "runtime".into(),
            package_hash: "a".repeat(64),
            publisher: "fixture".into(),
            trust: AdapterTrust::Untrusted,
            manifest: serde_json::json!({}),
            state: AdapterState::Installed,
        };
        let journal = AdapterImportJournal {
            job_id: uuid::Uuid::now_v7().to_string(),
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
            request: AdapterImport {
                package_path: "C:/private/package.zip".into(),
                expected_hash: "b".repeat(64),
                approval_id: None,
            },
            step: UpdateStep::Queued,
            plan: Some(AdapterImportPlan {
                candidate,
                workspace_path: "C:/private/adapters/.staging/job".into(),
                destination_path: "C:/private/adapters/acme.import-fixture/1.0".into(),
                volume_key: "volume".into(),
                archive_bytes: DecimalU64::new(10).unwrap(),
                unpacked_bytes: DecimalU64::new(20).unwrap(),
                required_bytes: DecimalU64::new(10 + 20 + (1 << 30)).unwrap(),
                approval_id: None,
            }),
            error: None,
        };
        let public = serde_json::to_string(&journal.public_state()).unwrap();
        assert!(!public.contains("C:/private"));
        assert!(!public.contains("package.zip"));
    }
}
