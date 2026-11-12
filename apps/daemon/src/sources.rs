//! Composición del catálogo y alta local de bindings. No ejecuta adaptadores.
use larm_core::sources::SourceCatalog;
use larm_domain::error::{ErrorCode, LarmError};
use std::io::Read;
use std::sync::Arc;
pub fn catalog(
    db: &larm_db::pool::Db,
    qualification: Option<Arc<dyn larm_domain::ports::AdapterQualification>>,
) -> Arc<SourceCatalog> {
    Arc::new(SourceCatalog {
        repo: Arc::new(larm_db::sources::SqlSources {
            pool: db.pool.clone(),
            qualification,
        }),
        installations: Arc::new(larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone(),
        }),
    })
}

/// El catálogo P043 aún no ofrece un lanzador de búsqueda source que vuelva a
/// autorizar P022 y recupere credenciales por cuenta. Fallar explícitamente
/// evita presentar un catálogo local vacío como si fuera una búsqueda remota.
#[cfg(test)]
pub struct UnqualifiedSourceSearch;

#[cfg(test)]
#[async_trait::async_trait]
impl larm_models::search::FederatedSearchProvider for UnqualifiedSourceSearch {
    async fn sources(
        &self,
        _source_ids: &[String],
    ) -> Result<Vec<larm_models::search::SearchSource>, larm_domain::bridge::ApiError> {
        Err(larm_domain::bridge::ApiError::new(
            larm_domain::error::ErrorCode::UpstreamUnavailable,
            "La búsqueda de fuentes requiere el host de adaptadores cualificado",
            "source_search_backend_unavailable",
        )
        .retryable(true))
    }

    async fn search_page(
        &self,
        _source_id: &str,
        _request: larm_adapter_sdk::source::SourceSearch,
    ) -> Result<
        larm_domain::bridge::Page<larm_models::search::SearchCandidate>,
        larm_domain::bridge::ApiError,
    > {
        Err(larm_domain::bridge::ApiError::new(
            larm_domain::error::ErrorCode::UpstreamUnavailable,
            "La búsqueda de fuentes requiere el host de adaptadores cualificado",
            "source_search_backend_unavailable",
        )
        .retryable(true))
    }
}

pub async fn register(
    db: &larm_db::pool::Db,
    installation: &str,
    source_type: &str,
    descriptor: &std::path::Path,
) -> Result<larm_domain::sources::ModelSourceAdapter, LarmError> {
    let file = std::fs::File::open(descriptor).map_err(|_| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo leer el descriptor de fuente",
            "source_descriptor_unreadable",
        )
    })?;
    let mut bytes = Vec::new();
    file.take((1 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo leer el descriptor de fuente",
                "source_descriptor_unreadable",
            )
        })?;
    if bytes.len() > 1 << 20 {
        return Err(larm_domain::sources::source_invalid("descriptor_too_large"));
    }
    let description = serde_json::from_slice(&bytes)
        .map_err(|_| larm_domain::sources::source_invalid("descriptor"))?;
    catalog(db, None)
        .register_adapter(
            installation,
            source_type,
            description,
            &larm_domain::ports::MutationContext {
                actor_id: "local-operator".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn busqueda_no_cualificada_devuelve_error_y_no_catalogo_vacio() {
        let search = crate::model_search::api_service(
            Arc::new(UnqualifiedSourceSearch),
            Arc::new(larm_core::security::CursorSigner::default()),
            larm_models::search::SearchConfig::default(),
        );
        let request: larm_domain::search::Search = serde_json::from_value(serde_json::json!({
            "q":"", "capabilities":[], "source_ids":[], "compatibility":"ALL",
            "architectures":[], "formats":[], "quantizations":[], "licenses":[],
            "runtime_ids":[], "parameter_count":{"minimum":null,"maximum":null},
            "size_bytes":{"minimum":null,"maximum":null},
            "trust_score":{"minimum":null,"maximum":null}, "certification_statuses":[],
            "only_executable":false,"only_recommended":false,"show_incompatible":true,
            "cursor":null,"limit":50
        }))
        .expect("Search con schema válido");
        let error = search
            .search(&request)
            .await
            .expect_err("backend no cualificado");
        assert_eq!(error.code, "UPSTREAM_UNAVAILABLE");
        assert_eq!(error.details.reason, "source_search_backend_unavailable");
    }
}
