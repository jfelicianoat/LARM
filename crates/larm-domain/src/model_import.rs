//! Cola durable de importaciones locales P098.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::bridge::{ApiError, JobReceipt};
use crate::ports::{JobSource, MutationContext};

pub const IMPORT_JOB_KIND: &str = "model.import";
pub const IMPORT_OPERATION_ID: &str = "P098_post__models_import";

fn required_nullable<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModelImportMode {
    Adopt,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelImportRequest {
    pub source_path: String,
    pub storage_location_id: String,
    pub mode: ModelImportMode,
    #[serde(deserialize_with = "required_nullable")]
    pub format: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub license_id: Option<String>,
}

impl ModelImportRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.source_path.trim().is_empty() || self.source_path.contains('\0') {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "Indica una carpeta local válida",
                "import_source_path_invalid",
            ));
        }
        if !crate::bridge::is_uuid_v7(&self.storage_location_id) {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "La ubicación de destino no es válida",
                "import_location_id_invalid",
            ));
        }
        if self
            .format
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
            || self
                .license_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "Formato y licencia deben omitirse o contener un valor",
                "import_optional_field_invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelImportClaim {
    pub job_id: String,
    pub request: ModelImportRequest,
    pub actor_id: String,
    pub correlation_id: String,
}

#[async_trait::async_trait]
pub trait ModelImportSubmitter: Send + Sync {
    async fn enqueue_import(
        &self,
        request: &ModelImportRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<JobReceipt, ApiError>;
}

#[async_trait::async_trait]
pub trait ModelImportJobs: ModelImportSubmitter + JobSource {
    async fn claim_import(&self) -> Result<Option<ModelImportClaim>, ApiError>;
    async fn complete_import(
        &self,
        claim: &ModelImportClaim,
        result: Value,
        first_variant_id: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn fail_import(&self, claim: &ModelImportClaim, error: &ApiError)
        -> Result<(), ApiError>;
    /// Marks interrupted RUNNING work failed; QUEUED work remains available after restart.
    async fn recover_imports(&self) -> Result<u64, ApiError>;
}

#[async_trait::async_trait]
pub trait ModelImportSourceResolver: Send + Sync {
    /// Crea o reutiliza la fuente de la ruta local canónica con auditoría.
    async fn resolve_source_id(
        &self,
        request: &ModelImportRequest,
        ctx: &MutationContext,
    ) -> Result<String, ApiError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;

    const LOCATION: &str = "0192f000-0000-7000-8000-00000000c001";

    fn request() -> ModelImportRequest {
        ModelImportRequest {
            source_path: "C:\\Modelos\\qwen".into(),
            storage_location_id: LOCATION.into(),
            mode: ModelImportMode::Adopt,
            format: None,
            license_id: Some("apache-2.0".into()),
        }
    }

    #[test]
    fn solicitud_serializa_campos_nulos_exigidos_por_el_contrato() {
        let request = request();
        let value = serde_json::to_value(&request).unwrap();
        assert!(value.get("format").is_some_and(Value::is_null));
        assert_eq!(value["license_id"], "apache-2.0");
        assert!(serde_json::from_value::<ModelImportRequest>(value).is_ok());
    }

    #[test]
    fn deserializacion_rechaza_nulos_ausentes_y_campos_extra() {
        let mut value = serde_json::to_value(request()).unwrap();
        value.as_object_mut().unwrap().remove("format");
        assert!(serde_json::from_value::<ModelImportRequest>(value).is_err());
        let mut value = serde_json::to_value(request()).unwrap();
        value["unexpected"] = Value::Bool(true);
        assert!(serde_json::from_value::<ModelImportRequest>(value).is_err());
    }

    #[test]
    fn validacion_rechaza_ruta_vacia_ubicacion_invalida_y_opcionales_vacios() {
        let mut invalid_request = request();
        invalid_request.source_path = "  ".into();
        assert_eq!(
            invalid_request.validate().unwrap_err().details.reason,
            "import_source_path_invalid"
        );
        let mut invalid_request = request();
        invalid_request.storage_location_id = "not-a-uuid".into();
        assert_eq!(
            invalid_request.validate().unwrap_err().details.reason,
            "import_location_id_invalid"
        );
        let mut invalid_request = request();
        invalid_request.format = Some("  ".into());
        assert_eq!(
            invalid_request.validate().unwrap_err().details.reason,
            "import_optional_field_invalid"
        );
    }
}
