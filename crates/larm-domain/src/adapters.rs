//! Instalaciones de adaptadores (entidad `adapter_installation`; contrato 03,
//! P022–P024). El manifiesto se guarda como JSON: su tipo vive en el SDK.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Confianza persistida (`adapter_installation.trust`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdapterTrust {
    Untrusted,
    Verified,
    Revoked,
}

impl AdapterTrust {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "UNTRUSTED",
            Self::Verified => "VERIFIED",
            Self::Revoked => "REVOKED",
        }
    }
}

/// Estado de la instalación (independiente del proceso worker).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdapterState {
    Staged,
    Verified,
    Installed,
    Validated,
    Retired,
    Failed,
}

impl AdapterState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "STAGED",
            Self::Verified => "VERIFIED",
            Self::Installed => "INSTALLED",
            Self::Validated => "VALIDATED",
            Self::Retired => "RETIRED",
            Self::Failed => "FAILED",
        }
    }
}

/// Fila `adapter_installation` (schema cerrado).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterInstallation {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub adapter_id: String,
    pub version: String,
    /// `runtime`, `source`, `protocol`, `transformation` o `benchmark`.
    pub family: String,
    pub package_hash: String,
    pub publisher: String,
    pub trust: AdapterTrust,
    pub manifest: Value,
    pub state: AdapterState,
}

/// Filtros del listado de instalaciones; no confunden firma con lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterFilter {
    pub q: Option<String>,
    pub state: Option<AdapterState>,
}

/// Selección operativa interna de P024. No amplía `adapter_installation` ni
/// confunde INSTALLED con VALIDATED. Cambiar selección nunca lanza un worker:
/// sus permisos y confianza se vuelven a comprobar al ejecutarlo (P022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterSelection {
    pub adapter_id: String,
    pub active_installation_id: Option<String>,
    pub previous_installation_id: Option<String>,
    pub enabled: bool,
    pub row_version: i64,
    pub updated_at: String,
}

/// Una selección nueva empieza deshabilitada. Activarla es una decisión
/// explícita posterior. Rollback consume el anterior para no alternar
/// silenciosamente entre dos versiones al reintentarlo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum AdapterSelectionChange {
    Promote { installation_id: String },
    Rollback,
    SetEnabled { enabled: bool },
}
