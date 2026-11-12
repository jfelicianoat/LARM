//! Alta de adaptadores declarativos (P023; spec §13; contrato 03; contrato 11
//! H04). Orden: validar el documento (sin IO), comprobar rutas reales dentro
//! de las raíces, comprobar que la versión no existe, crear el paquete y
//! registrar `adapter_installation` con su evento en una transacción. Si el
//! registro falla, el paquete se borra.
//!
//! Un declarativo no lleva firma de publicador: `CUSTOM`, confianza
//! `UNTRUSTED` y, para ejecutar, concesión TRUSTED_ONLY (P022). Importar un
//! documento exportado es otra alta: nunca hereda confianza.

use std::path::PathBuf;
use std::sync::Arc;

use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::{timestamp, ApiError};
use larm_domain::error::ErrorCode;
use larm_domain::ports::{AdapterInstallationRepository, MutationContext};
use serde_json::{json, Value};

pub use larm_adapter_sdk::declarative::DeclarativeAdapter;

/// Caso de uso `POST /adapters/declarative`.
pub struct DeclarativeRegistry {
    pub repo: Arc<dyn AdapterInstallationRepository>,
    /// Carpeta de paquetes (`<data_root>/adapters`).
    pub packages_root: PathBuf,
    /// `larm-generic-adapter.exe` que se copia en cada paquete.
    pub worker_exe: PathBuf,
}

impl DeclarativeRegistry {
    /// Registra un documento `DeclarativeAdapter`.
    ///
    /// # Errors
    /// `VALIDATION_FAILED`/`UNSUPPORTED_CAPABILITY` del documento,
    /// `FORBIDDEN` `read_denied` (ruta fuera de las raíces),
    /// `RESOURCE_UNAVAILABLE` (ruta inexistente o worker ausente) y
    /// `CONFLICT` `adapter_version_exists`.
    pub async fn register(
        &self,
        body: &Value,
        ctx: &MutationContext,
    ) -> Result<AdapterInstallation, ApiError> {
        let doc = DeclarativeAdapter::from_value(body)?;
        doc.check_paths()?;
        let m = &doc.manifest;
        if self.repo.find(&m.id, &m.version).await?.is_some() {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Ya existe esa versión del adaptador",
                "adapter_version_exists",
            ));
        }
        let document_hash = doc.document_hash()?;
        let package = doc.materialize(&self.packages_root, &self.worker_exe)?;
        let now = timestamp(chrono::Utc::now());
        let mut metadata = serde_json::Map::new();
        for (k, v) in [
            ("kind", json!("DECLARATIVE")),
            ("signature_state", json!("CUSTOM")),
            ("execution_mode", json!("TRUSTED_ONLY")),
            ("document_hash", json!(document_hash)),
            ("package_path", json!(format!("{}/{}", m.id, m.version))),
            ("protocol_profile_id", json!(doc.protocol_profile_id)),
            ("formats", json!(doc.formats)),
        ] {
            metadata.insert(k.into(), v);
        }
        let installation = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            adapter_id: m.id.clone(),
            version: m.version.clone(),
            family: m.family.as_str().into(),
            package_hash: package.manifest.package_sha256.clone(),
            publisher: m.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            manifest: serde_json::to_value(&package.manifest).unwrap_or_default(),
            state: AdapterState::Installed,
        };
        if let Err(e) = self.repo.insert(&installation, ctx).await {
            let _ = std::fs::remove_dir_all(&package.dir);
            return Err(e);
        }
        Ok(installation)
    }
}
