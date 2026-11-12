//! API de configuración y previsualización de CUSTOM_RULES (P027).

use crate::http::{correlated, if_match, json, strict, Answer, Failure};
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::get,
    Extension, Router,
};
use larm_domain::{
    custom_rules::{CustomRules, CustomRulesRepository, VersionedCustomRules},
    error::{ErrorCode, LarmError},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct CustomRulesState {
    pub repo: Arc<dyn CustomRulesRepository>,
}

pub fn routes(state: CustomRulesState) -> Router {
    Router::new()
        .route("/api/v1/custom-rules", get(read_rules).patch(replace_rules))
        .with_state(state)
}

fn failure(error: LarmError, headers: &HeaderMap) -> Failure {
    Failure::from(correlated(error, headers))
}

fn versioned_json(value: &VersionedCustomRules) -> Response {
    let mut response = json(StatusCode::OK, &value.value);
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", value.revision)) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response
}

async fn read_rules(State(state): State<CustomRulesState>, headers: HeaderMap) -> Answer {
    let current = state
        .repo
        .get()
        .await
        .map_err(|error| failure(error, &headers))?;
    Ok(versioned_json(&current))
}

async fn replace_rules(
    State(state): State<CustomRulesState>,
    Extension(ctx): Extension<crate::security::AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let expected_revision = if_match(&headers)?;
    let expected_revision = i64::try_from(expected_revision).map_err(|_| {
        failure(
            LarmError::new(
                ErrorCode::ValidationFailed,
                "If-Match debe ser la row_version",
                "if_match_invalid",
            ),
            &headers,
        )
    })?;
    let value: CustomRules = strict(&body, &headers)?;
    value.validate().map_err(|_| {
        failure(
            LarmError::new(
                ErrorCode::ValidationFailed,
                "Las reglas personalizadas no son válidas",
                "custom_rules_invalid",
            ),
            &headers,
        )
    })?;
    let updated = state
        .repo
        .replace(expected_revision, &value, &ctx.mutation())
        .await
        .map_err(|error| failure(error, &headers))?;
    Ok(versioned_json(&updated))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;
    use axum::{body::Body, http::Request};
    use larm_domain::custom_rules::{CustomRulesError, VersionedCustomRules};
    use larm_domain::{
        auth::{Principal, PrincipalKind},
        ports::MutationContext,
    };
    use std::{collections::BTreeSet, sync::Mutex};
    use tower::ServiceExt;

    #[derive(Default)]
    struct MemoryCustomRules(Mutex<VersionedCustomRules>);

    #[async_trait::async_trait]
    impl CustomRulesRepository for MemoryCustomRules {
        async fn get(&self) -> Result<VersionedCustomRules, LarmError> {
            self.0
                .lock()
                .map(|value| value.clone())
                .map_err(|_| LarmError::new(ErrorCode::Internal, "test mutex", "test_mutex"))
        }

        async fn replace(
            &self,
            expected_revision: i64,
            value: &CustomRules,
            _ctx: &MutationContext,
        ) -> Result<VersionedCustomRules, LarmError> {
            value.validate().map_err(|_: CustomRulesError| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "reglas inválidas",
                    "custom_rules_invalid",
                )
            })?;
            let mut current = self
                .0
                .lock()
                .map_err(|_| LarmError::new(ErrorCode::Internal, "test mutex", "test_mutex"))?;
            if current.revision != expected_revision {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "revisión antigua",
                    "stale_revision",
                ));
            }
            current.revision += 1;
            current.value.clone_from(value);
            Ok(current.clone())
        }
    }

    fn app() -> Router {
        routes(CustomRulesState {
            repo: Arc::new(MemoryCustomRules::default()),
        })
        .layer(Extension(crate::security::AuthContext {
            principal: Principal {
                id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103".into(),
                name: "test".into(),
                kind: PrincipalKind::Operator,
                api_scopes: BTreeSet::new(),
                mcp_scopes: BTreeSet::new(),
            },
            operation_id: "P027_patch__custom_rules".into(),
            correlation_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a104".into(),
        }))
    }

    fn rules_json() -> String {
        serde_json::json!({
            "schema_version":"1.0",
            "rules":[{
                "id":"gguf-only",
                "predicates":[{"field":"format","operator":"eq","value":"gguf"}],
                "effect":"require",
                "limit_bytes":null,
                "target_storage_location_id":null
            }]
        })
        .to_string()
    }

    async fn request(
        app: Router,
        method: &str,
        if_match: Option<&str>,
        body: Option<String>,
    ) -> Response {
        let mut builder = Request::builder()
            .method(method)
            .uri("/api/v1/custom-rules");
        if let Some(revision) = if_match {
            builder = builder.header(header::IF_MATCH, revision);
        }
        app.oneshot(
            builder
                .body(body.map_or_else(Body::empty, Body::from))
                .unwrap_or_else(|error| panic!("{error}")),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[tokio::test]
    async fn get_y_patch_publican_etag_y_aplican_cas() {
        let app = app();
        let initial = request(app.clone(), "GET", None, None).await;
        assert_eq!(initial.status(), StatusCode::OK);
        assert_eq!(
            initial
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"0\"")
        );

        let saved = request(app.clone(), "PATCH", Some("\"0\""), Some(rules_json())).await;
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            saved
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"1\"")
        );

        let stale = request(app, "PATCH", Some("0"), Some(rules_json())).await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(stale.into_body(), 4096)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let error: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(error["code"], "STALE_REVISION");
    }

    #[tokio::test]
    async fn exige_if_match_y_rechaza_schema_y_semantica_invalidos() {
        let app = app();
        let missing = request(app.clone(), "PATCH", None, Some(rules_json())).await;
        assert_eq!(missing.status(), StatusCode::PRECONDITION_REQUIRED);

        let unknown = r#"{"schema_version":"1.0","rules":[],"script":"run"}"#.to_string();
        assert_eq!(
            request(app.clone(), "PATCH", Some("0"), Some(unknown))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );

        let mut invalid = serde_json::from_str::<serde_json::Value>(&rules_json())
            .unwrap_or_else(|error| panic!("{error}"));
        invalid["rules"][0]["effect"] = serde_json::json!("limit");
        assert_eq!(
            request(app, "PATCH", Some("0"), Some(invalid.to_string()))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}
