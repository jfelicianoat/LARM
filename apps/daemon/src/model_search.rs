//! Composición entre el motor federado de modelos y la frontera HTTP.

use std::sync::Arc;

use larm_core::search::{ModelSearchService, SearchResult, SearchWarning};
use larm_core::security::CursorSigner;
use larm_domain::bridge::ApiError;
use larm_domain::search::Search;
use larm_models::search::{
    FederatedSearch, FederatedSearchProvider, SearchConfig, SearchCursorSigner,
};

struct CursorSignerAdapter(Arc<CursorSigner>);

impl SearchCursorSigner for CursorSignerAdapter {
    fn sign(&self, sort_key: &str, id: &str, filter_digest: &str) -> String {
        self.0.sign(sort_key, id, filter_digest)
    }

    fn verify(&self, cursor: &str, filter_digest: &str) -> Result<(String, String), ApiError> {
        self.0.verify(cursor, filter_digest)
    }
}

struct ApiSearchAdapter(Arc<FederatedSearch>);

#[async_trait::async_trait]
impl ModelSearchService for ApiSearchAdapter {
    async fn search(&self, request: &Search) -> Result<SearchResult, ApiError> {
        let result = self.0.search(request).await?;
        Ok(SearchResult {
            page: result.page,
            warnings: result
                .warnings
                .into_iter()
                .map(|warning| SearchWarning {
                    source_id: warning.source_id,
                    reason: warning.reason,
                    limitations: warning.limitations,
                })
                .collect(),
        })
    }
}

pub fn api_service(
    provider: Arc<dyn FederatedSearchProvider>,
    cursors: Arc<CursorSigner>,
    config: SearchConfig,
) -> Arc<dyn ModelSearchService> {
    let engine = Arc::new(FederatedSearch::new(
        provider,
        Arc::new(CursorSignerAdapter(cursors)),
        config,
    ));
    Arc::new(ApiSearchAdapter(engine))
}
