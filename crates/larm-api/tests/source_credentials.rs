#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::credential::{CredentialReference, SecretType, BACKEND};
use larm_domain::error::LarmError;
use larm_domain::ports::{MutationContext, PrincipalAuthenticator, SourceBasicCredentialWriter};
use larm_domain::source_credentials::BasicCredentialWrite;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

#[derive(Default)]
struct Writer {
    requests: Mutex<Vec<BasicCredentialWrite>>,
}

#[async_trait]
impl SourceBasicCredentialWriter for Writer {
    async fn write_basic_credential(
        &self,
        _source_id: &str,
        request: &BasicCredentialWrite,
        _ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        self.requests.lock().push(request.clone());
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        Ok(CredentialReference {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            service: format!("larm/source/{}", request.source_id),
            username: request.account_id.clone(),
            backend: BACKEND.into(),
            secret_type: SecretType::Password,
            scopes: vec![],
            expires_at: None,
        })
    }
}

async fn harness() -> (axum::Router, String, String, Arc<Writer>) {
    let rows = Arc::new(MemoryCredentials::default());
    let secrets = Arc::new(MemorySecrets::default());
    let principals = Arc::new(Principals::new(rows, secrets));
    let ctx = MutationContext {
        actor_id: "source-credentials-test".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, writer_token) = principals
        .enroll(
            PrincipalKind::Operator,
            "credential-writer",
            vec![ApiScope::CredentialsWrite],
            vec![],
            Some("writer-token-for-source-credentials-test-0001".into()),
            &ctx,
        )
        .await
        .unwrap();
    let (_, reader_token) = principals
        .enroll(
            PrincipalKind::Operator,
            "credential-reader",
            vec![ApiScope::Read],
            vec![],
            Some("reader-token-for-source-credentials-test-0001".into()),
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let writer = Arc::new(Writer::default());
    let app = larm_api::source_credentials::routes(writer.clone()).layer(
        axum::middleware::from_fn_with_state(auth, larm_api::security::guard),
    );
    (app, writer_token, reader_token, writer)
}

fn valid_body(source: &str, account: &str) -> Value {
    json!({
        "source_id": source,
        "account_id": account,
        "username": "private-user-sentinel",
        "password": "private-password-sentinel"
    })
}

async fn post(
    app: &axum::Router,
    token: &str,
    source: &str,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(format!("/api/v1/sources/{source}/basic-credential"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn guarda_la_credencial_solo_con_scope_y_responde_sin_campos_secretos() {
    let (app, writer_token, reader_token, writer) = harness().await;
    let source = uuid::Uuid::now_v7().to_string();
    let account = uuid::Uuid::now_v7().to_string();
    let denied = post(&app, &reader_token, &source, valid_body(&source, &account)).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(writer.requests.lock().is_empty());

    let response = post(&app, &writer_token, &source, valid_body(&source, &account)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let reference: Value = serde_json::from_slice(&bytes).unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(reference["username"], account);
    assert_eq!(reference["secret_type"], "PASSWORD");
    assert!(!text.contains("private-user-sentinel"));
    assert!(!text.contains("private-password-sentinel"));
    assert_eq!(writer.requests.lock().len(), 1);
}

#[tokio::test]
async fn rechaza_cuerpo_abierto_y_source_id_distinto_de_la_ruta() {
    let (app, writer_token, _, writer) = harness().await;
    let source = uuid::Uuid::now_v7().to_string();
    let other = uuid::Uuid::now_v7().to_string();
    let account = uuid::Uuid::now_v7().to_string();

    let mismatch = post(&app, &writer_token, &source, valid_body(&other, &account)).await;
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
    let mut extra = valid_body(&source, &account);
    extra["access_token"] = json!("must-not-be-accepted");
    let open = post(&app, &writer_token, &source, extra).await;
    assert_eq!(open.status(), StatusCode::BAD_REQUEST);
    assert!(writer.requests.lock().is_empty());
}
