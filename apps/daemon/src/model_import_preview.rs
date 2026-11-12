//! Integración de la vista previa read-only P098 con el escáner local.

use async_trait::async_trait;
use larm_api::import_preview::ModelImportPreviewer;
use larm_domain::bridge::ApiError;
use larm_domain::error::{ErrorCode, LarmError};
use std::path::PathBuf;

pub struct LocalModelImportPreview;

#[async_trait]
impl ModelImportPreviewer for LocalModelImportPreview {
    async fn preview(&self, source_path: &str) -> Result<serde_json::Value, ApiError> {
        let directory = PathBuf::from(source_path);
        let preview =
            tokio::task::spawn_blocking(move || larm_models::import::preview_dir(&directory))
                .await
                .map_err(|error| {
                    LarmError::new(
                        ErrorCode::Internal,
                        "No se pudo completar la vista previa",
                        "import_preview_worker_failed",
                    )
                    .with_technical(error.to_string())
                })??;
        serde_json::to_value(preview).map_err(|error| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo preparar la vista previa",
                "import_preview_serialize_failed",
            )
            .with_technical(error.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;

    #[tokio::test]
    async fn preview_llama_al_scanner_read_only_y_solo_devuelve_rutas_relativas() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("Qwen")).unwrap();
        fs::write(root.path().join("Qwen/model-Q4_K_M.gguf"), b"GGUFweights").unwrap();
        fs::write(root.path().join("Qwen/archive.zip"), b"PK\x03\x04ignored").unwrap();
        let before = fs::read(root.path().join("Qwen/model-Q4_K_M.gguf")).unwrap();

        let result = LocalModelImportPreview
            .preview(root.path().to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(result["recognized_files"], 1);
        assert_eq!(result["recognized_bytes"], "11");
        assert_eq!(result["ignored"][0]["relative_path"], "Qwen/archive.zip");
        assert_eq!(
            result["variants"][0]["files"][0]["relative_path"],
            "Qwen/model-Q4_K_M.gguf"
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains(root.path().to_str().unwrap()));
        assert_eq!(
            fs::read(root.path().join("Qwen/model-Q4_K_M.gguf")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn preview_de_carpeta_inexistente_devuelve_error_seguro() {
        let error = LocalModelImportPreview
            .preview("C:\\ruta-inexistente-larm-p098")
            .await
            .unwrap_err();
        assert_eq!(error.code.as_str(), "RESOURCE_UNAVAILABLE");
        assert_eq!(error.details.reason, "io_failed");
        assert!(!error.message.contains("ruta-inexistente-larm-p098"));
    }
}
