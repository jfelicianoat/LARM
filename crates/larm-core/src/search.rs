//! Frontera de búsqueda HTTP para el motor de modelos federado.

use async_trait::async_trait;
use larm_domain::bridge::{ApiError, Page};
use larm_domain::model::CanonicalModel;
use larm_domain::search::Search;
use serde::Serialize;

/// Advertencia de una fuente que no pudo completar su consulta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchWarning {
    pub source_id: String,
    pub reason: String,
    pub limitations: Vec<String>,
}

/// Respuesta paginada y diagnósticos internos. Las advertencias se adjuntan
/// a los metadatos de búsqueda de los modelos sin ampliar el schema de Page.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub page: Page<CanonicalModel>,
    pub warnings: Vec<SearchWarning>,
}

/// Puerta de entrada de las dos operaciones HTTP de búsqueda.
#[async_trait]
pub trait ModelSearchService: Send + Sync {
    async fn search(&self, request: &Search) -> Result<SearchResult, ApiError>;
}
