#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::{middleware::from_fn_with_state, Router};
use larm_core::security::{
    memory::MemoryCredentials, memory::MemorySecrets, CursorSigner, Principals,
};
use larm_domain::{
    auth::{ApiScope, PrincipalKind},
    bridge::JobReceipt,
    correlation::CORRELATION_HEADER,
    error::{ErrorCode, LarmError},
    ports::{MutationContext, PrincipalAuthenticator},
    runtime::{Runtime, RuntimeChannelPolicy, RuntimeCreate},
    runtime_install::{InstallRequest, RuntimeInstallSubmitter},
    runtime_ports::{RuntimeCatalogService, RuntimeFilter},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::oneshot;

struct CatalogUnavailable;

#[async_trait]
impl RuntimeCatalogService for CatalogUnavailable {
    async fn create(
        &self,
        _request: &RuntimeCreate,
        _key: &str,
        _body_hash: &str,
        _ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        Err(LarmError::new(
            ErrorCode::Internal,
            "No usado",
            "test_unused",
        ))
    }

    async fn change_policy(
        &self,
        _id: &str,
        _if_match: i64,
        _policy: &RuntimeChannelPolicy,
        _ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        Err(LarmError::new(
            ErrorCode::Internal,
            "No usado",
            "test_unused",
        ))
    }
}

struct InstallUnavailable;

#[async_trait]
impl RuntimeInstallSubmitter for InstallUnavailable {
    async fn submit(
        &self,
        _request: &InstallRequest,
        _principal_id: &str,
        _idempotency_key: &str,
        _request_hash: &str,
        _ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        Err(LarmError::new(
            ErrorCode::Internal,
            "No usado",
            "test_unused",
        ))
    }

    async fn cancel(
        &self,
        _job_id: &str,
        _principal_id: &str,
        _ctx: &MutationContext,
    ) -> Result<JobReceipt, LarmError> {
        Err(LarmError::new(
            ErrorCode::Internal,
            "No usado",
            "test_unused",
        ))
    }
}

async fn principals() -> (Arc<Principals>, String, String) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p037-test-setup".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "writer",
            vec![ApiScope::Read, ApiScope::RuntimesWrite],
            vec![],
            None,
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
            None,
            &ctx,
        )
        .await
        .unwrap();
    (principals, writer, reader)
}

async fn setup() -> (
    TempDir,
    Arc<larm_db::runtimes::SqlRuntimes>,
    Router,
    String,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = larm_db::pool::Db::open(&dir.path().join("p037.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let repo = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let (principals, writer, reader) = principals().await;
    let auth: Arc<dyn PrincipalAuthenticator> = principals.clone();
    let router = larm_api::runtimes::routes(larm_api::runtimes::RuntimesState {
        repo: repo.clone(),
        service: Arc::new(CatalogUnavailable),
        cursors: Arc::new(CursorSigner::default()),
        install_submitter: Arc::new(InstallUnavailable),
    })
    .layer(from_fn_with_state(auth, larm_api::security::guard));
    (dir, repo, router, writer, reader)
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_real_loopback_external_host_registration_sqlite_and_scope() {
    use larm_domain::runtime_ports::RuntimeRepository;
    use tokio::net::TcpListener;

    let (_dir, repo, router, writer, reader) = setup().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let correlation_id = uuid::Uuid::now_v7().to_string();
    let response = client
        .post(format!("http://{address}/api/v1/external-hosts"))
        .bearer_auth(&writer)
        .header(CORRELATION_HEADER, &correlation_id)
        .json(&json!({
            "name":"P037 local host fixture",
            "base_url":format!("http://{address}/v1"),
            "host_type":"fixture.openai-compatible",
            "credential_reference_id":null,
            "registration_mode":"EXTERNAL"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    assert_eq!(created["state"], "UNKNOWN");
    assert_eq!(created["ownership"], "EXTERNAL");
    assert_eq!(created["base_url"], format!("http://{address}/v1"));
    let id = created["id"].as_str().unwrap().to_owned();

    let invalid = client
        .post(format!("http://{address}/api/v1/external-hosts"))
        .bearer_auth(&writer)
        .header(CORRELATION_HEADER, uuid::Uuid::now_v7().to_string())
        .header("content-type", "application/json")
        .body(r#"{"name":"bad","base_url":"https://user:secret@host.example","host_type":"fixture","credential_reference_id":null,"registration_mode":"EXTERNAL"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let forbidden = client
        .post(format!("http://{address}/api/v1/external-hosts"))
        .bearer_auth(&reader)
        .header(CORRELATION_HEADER, uuid::Uuid::now_v7().to_string())
        .header("content-type", "application/json")
        .body("not-json")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let read_back = repo.external_host(&id).await.unwrap().unwrap();
    assert_eq!(
        read_back.state,
        larm_domain::runtime::ExternalHostState::Unknown
    );
    assert_eq!(
        read_back.registration_mode,
        larm_domain::runtime::RegistrationMode::External
    );
    assert_eq!(
        repo.external_hosts(None, 10, &RuntimeFilter::default())
            .await
            .unwrap()
            .len(),
        1
    );

    let _ = shutdown_tx.send(());
    server.await.unwrap();
}
