//! Lectura administrativa de registros de compatibilidad (P090, scope `read`).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Router};
use larm_domain::bridge::{timestamp, Page};
use larm_domain::ports::CompatibilityRepository;

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct CompatibilityState {
    pub repository: Arc<dyn CompatibilityRepository>,
}

pub fn routes(state: CompatibilityState) -> Router {
    Router::new()
        .route("/api/v1/compatibility", get(list))
        .with_state(state)
}

async fn list(
    State(state): State<CompatibilityState>,
    Extension(context): Extension<AuthContext>,
) -> Answer {
    let items = state
        .repository
        .list_all_compatibility()
        .await
        .map_err(|error| Failure::from(error).recorrelate(&context.correlation_id))?;
    let page = Page {
        items,
        next_cursor: None,
        snapshot_at: timestamp(chrono::Utc::now()),
    };
    Ok(json(StatusCode::OK, &page))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::Extension;
    use larm_domain::auth::{Principal, PrincipalKind};
    use larm_domain::bridge::ApiError;
    use larm_domain::compatibility::CompatibilityRecord;
    use larm_domain::ports::CompatibilityRepository;
    use tower::ServiceExt;

    use super::{routes, CompatibilityState};

    struct MemoryCompatibility(Vec<CompatibilityRecord>);

    #[async_trait::async_trait]
    impl CompatibilityRepository for MemoryCompatibility {
        async fn list_all_compatibility(&self) -> Result<Vec<CompatibilityRecord>, ApiError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn get_compatibility_returns_contract_page_with_persisted_records() {
        let principal = Principal {
            id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103".into(),
            name: "test".into(),
            kind: PrincipalKind::Operator,
            api_scopes: Default::default(),
            mcp_scopes: Default::default(),
        };
        let app = routes(CompatibilityState {
            repository: Arc::new(MemoryCompatibility(Vec::new())),
        })
        .layer(Extension(crate::security::AuthContext {
            principal,
            operation_id: "P090_get__compatibility".into(),
            correlation_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a104".into(),
        }));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/compatibility")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["items"], serde_json::json!([]));
        assert_eq!(value["next_cursor"], serde_json::Value::Null);
        assert!(value["snapshot_at"].as_str().is_some());
        assert_eq!(value.as_object().unwrap().len(), 3);
    }
}
