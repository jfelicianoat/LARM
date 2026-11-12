#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_core::source_accounts::SourceAccounts;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::credential::CredentialReference;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, PrincipalAuthenticator, SourceAccountRepository};
use larm_domain::source_accounts::{CheckSourceLicense, ModelSourceAccount, SourceLicenseReceipt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

#[derive(Default)]
struct MemorySourceAccounts {
    accounts: Mutex<BTreeMap<String, ModelSourceAccount>>,
    license_idempotency: Mutex<BTreeMap<(String, String), (String, SourceLicenseReceipt)>>,
}

#[async_trait]
impl SourceAccountRepository for MemorySourceAccounts {
    async fn create_source_account(
        &self,
        account: &ModelSourceAccount,
        _ctx: &MutationContext,
    ) -> Result<ModelSourceAccount, LarmError> {
        if self.accounts.lock().contains_key(&account.id) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Duplicada",
                "duplicate",
            ));
        }
        self.accounts
            .lock()
            .insert(account.id.clone(), account.clone());
        Ok(account.clone())
    }

    async fn source_account(&self, id: &str) -> Result<Option<ModelSourceAccount>, LarmError> {
        Ok(self.accounts.lock().get(id).cloned())
    }

    async fn page_source_accounts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<ModelSourceAccount>, LarmError> {
        let mut rows: Vec<_> = self.accounts.lock().values().cloned().collect();
        rows.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        if let Some(after) = after {
            rows.retain(|item| (&item.created_at, &item.id) > (&after.0, &after.1));
        }
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn set_basic_credential(
        &self,
        account_id: &str,
        credential: &CredentialReference,
        _ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        let mut accounts = self.accounts.lock();
        let account = accounts
            .get_mut(account_id)
            .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "No existe", "not_found"))?;
        if credential.service != format!("larm/source/{}", account.source_id)
            || credential.username != account_id
        {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Cuenta ajena",
                "source_credential_scope_mismatch",
            ));
        }
        account.credential_reference_id = Some(credential.id.clone());
        account.row_version += 1;
        Ok(credential.clone())
    }

    async fn set_source_license(
        &self,
        source_id: &str,
        request: &CheckSourceLicense,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<SourceLicenseReceipt, LarmError> {
        let key = (ctx.actor_id.clone(), idempotency_key.to_string());
        if let Some((previous_hash, receipt)) = self.license_idempotency.lock().get(&key).cloned() {
            if previous_hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Clave ya utilizada",
                    "idempotency_body_mismatch",
                ));
            }
            return Ok(receipt);
        }
        let mut rows = self.accounts.lock();
        let account = rows
            .get_mut(&request.source_account_id)
            .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "No existe", "not_found"))?;
        if account.source_id != source_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Cuenta ajena",
                "source_account_mismatch",
            ));
        }
        if request.accepted {
            if !account
                .accepted_license_hashes
                .contains(&request.license_hash)
            {
                account
                    .accepted_license_hashes
                    .push(request.license_hash.clone());
            }
        } else {
            account
                .accepted_license_hashes
                .retain(|hash| hash != &request.license_hash);
        }
        account.row_version += 1;
        let receipt = SourceLicenseReceipt {
            resource_id: account.id.clone(),
            row_version: account.row_version,
            event_id: "license-event".into(),
        };
        self.license_idempotency
            .lock()
            .insert(key, (body_hash.to_string(), receipt.clone()));
        Ok(receipt)
    }

    async fn delete_source_account(
        &self,
        id: &str,
        _ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.accounts
            .lock()
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "No existe", "not_found"))
    }
}

async fn harness() -> (axum::Router, String, String) {
    let credential_rows = Arc::new(MemoryCredentials::default());
    let secrets = Arc::new(MemorySecrets::default());
    let principals = Arc::new(Principals::new(credential_rows, secrets));
    let ctx = MutationContext {
        actor_id: "test-actor".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "writer",
            vec![ApiScope::Read, ApiScope::CredentialsWrite],
            vec![],
            Some("writer-token-source-accounts-123456".into()),
            &ctx,
        )
        .await
        .unwrap();
    let (_, reader) = principals
        .enroll(
            PrincipalKind::Operator,
            "reader",
            vec![ApiScope::Read],
            vec![],
            Some("reader-token-source-accounts-123456".into()),
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals.clone();
    let router =
        larm_api::source_accounts::routes(larm_api::source_accounts::SourceAccountsState {
            service: Arc::new(SourceAccounts {
                repo: Arc::new(MemorySourceAccounts::default()),
            }),
            cursors: Arc::new(CursorSigner::default()),
        })
        .layer(axum::middleware::from_fn_with_state(
            auth,
            larm_api::security::guard,
        ));
    (router, writer, reader)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    call_with_key(
        app,
        method,
        uri,
        token,
        body,
        Some(&uuid::Uuid::now_v7().to_string()),
    )
    .await
}

async fn call_with_key(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&str>,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(key) = idempotency_key {
        req = req.header("idempotency-key", key);
    }
    let req = req
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_owned())))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn rutas_respetan_scopes_dto_y_serializan_solo_la_referencia() {
    let (app, writer, reader) = harness().await;
    let body = json!({
        "source_id":"0192aaaa-0000-7000-8000-000000000001",
        "credential_reference_id":null,
        "display_name":"Cuenta local",
        "accepted_license_hashes":["a".repeat(64)]
    })
    .to_string();
    let (status, _value) = call(
        &app,
        "POST",
        "/api/v1/source-accounts",
        &reader,
        Some(&body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, account) = call(
        &app,
        "POST",
        "/api/v1/source-accounts",
        &writer,
        Some(&body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{account}");
    assert_eq!(account["credential_reference_id"], Value::Null);
    assert!(account.get("secret").is_none());
    assert!(account.get("username").is_none());
    assert!(account.get("service").is_none());

    let id = account["id"].as_str().unwrap();
    let (status, value) = call(
        &app,
        "GET",
        &format!("/api/v1/source-accounts/{id}"),
        &reader,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, account);

    let (status, page) = call(
        &app,
        "GET",
        "/api/v1/source-accounts?limit=1",
        &reader,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0], account);

    let source_id = account["source_id"].as_str().unwrap();
    let license_hash = "a".repeat(64);
    let accept = json!({
        "license_hash":license_hash,
        "source_account_id":id,
        "revision_key":"fixture:revision-1",
        "accepted":true
    })
    .to_string();
    let (status, receipt) = call_with_key(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &writer,
        Some(&accept),
        Some("license-accept-once"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(receipt["resource_id"], account["id"]);
    assert_eq!(receipt["row_version"], 2);
    let (status, replay) = call_with_key(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &writer,
        Some(&accept),
        Some("license-accept-once"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay, receipt);

    let (status, _) = call_with_key(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &reader,
        Some(&accept),
        Some("reader-license-attempt"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut changed_body: Value = serde_json::from_str(&accept).unwrap();
    changed_body["revision_key"] = json!("fixture:revision-2");
    let (status, _) = call_with_key(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &writer,
        Some(&changed_body.to_string()),
        Some("license-accept-once"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = call_with_key(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &writer,
        Some(&accept),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let revoke = json!({
        "license_hash":"a".repeat(64),
        "source_account_id":account["id"],
        "revision_key":"fixture:revision-1",
        "accepted":false
    })
    .to_string();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/sources/{source_id}/license"),
        &writer,
        Some(&revoke),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, current) = call(
        &app,
        "GET",
        &format!("/api/v1/source-accounts/{id}"),
        &reader,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["accepted_license_hashes"], json!([]));

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/source-accounts",
        &writer,
        Some(r#"{"source_id":"0192aaaa-0000-7000-8000-000000000001","credential_reference_id":null,"display_name":"x","accepted_license_hashes":[],"extra":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/source-accounts/{id}"),
        &writer,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
