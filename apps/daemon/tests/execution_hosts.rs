//! P081: aceptación real del registro de hosts de ejecución sobre este equipo
//! Windows: sondeo read-only de `wsl.exe`/`docker.exe` reales (WSL2 Ubuntu y
//! motor de contenedores del PC IA), persistencia con journal/evento/outbox y
//! lectura pública `GET /api/v1/execution-hosts` con scopes y filtros. La
//! prueba observa conducta real del sistema anfitrión; no instala ni muta
//! nada del subsistema WSL o del motor de contenedores.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use axum::{middleware::from_fn_with_state, Router};
use larm_core::security::{
    memory::MemoryCredentials, memory::MemorySecrets, CursorSigner, Principals,
};
use larm_domain::{
    auth::{ApiScope, McpPermission, PrincipalKind},
    error::{ErrorCode, LarmError},
    ports::{MutationContext, PrincipalAuthenticator},
    runtime::{
        ExecutionHost, ExecutionHostKind, ExternalHostState, Runtime, RuntimeChannelPolicy,
        RuntimeCreate,
    },
    runtime_install::{InstallRequest, RuntimeInstallSubmitter},
    runtime_ports::RuntimeCatalogService,
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
        _key: &str,
        _hash: &str,
        _ctx: &MutationContext,
    ) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
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
    ) -> Result<larm_domain::bridge::JobReceipt, LarmError> {
        Err(LarmError::new(
            ErrorCode::Internal,
            "No usado",
            "test_unused",
        ))
    }
}

async fn principals() -> (Arc<Principals>, String, String, String) {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p081-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let mut tokens = Vec::new();
    for (name, scopes, mcp) in [
        (
            "writer",
            vec![ApiScope::Read, ApiScope::RuntimesWrite],
            vec![],
        ),
        ("reader", vec![ApiScope::Read], vec![]),
        ("mcp-reader", vec![], vec![McpPermission::Install]),
    ] {
        let (_principal, token) = principals
            .enroll(
                if name.starts_with("mcp") {
                    PrincipalKind::Mcp
                } else {
                    PrincipalKind::Operator
                },
                name,
                scopes,
                mcp,
                None,
                &ctx,
            )
            .await
            .unwrap();
        tokens.push(token);
    }
    (
        principals,
        tokens[0].clone(),
        tokens[1].clone(),
        tokens[2].clone(),
    )
}

async fn setup() -> (
    TempDir,
    Arc<larm_db::runtimes::SqlRuntimes>,
    Router,
    String,
    String,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = larm_db::pool::Db::open(&dir.path().join("p081.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let repo = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let (principals, writer, reader, mcp) = principals().await;
    let auth: Arc<dyn PrincipalAuthenticator> = principals.clone();
    let router = larm_api::runtimes::routes(larm_api::runtimes::RuntimesState {
        repo: repo.clone(),
        service: Arc::new(CatalogUnavailable),
        cursors: Arc::new(CursorSigner::default()),
        install_submitter: Arc::new(InstallUnavailable),
    })
    .layer(from_fn_with_state(auth, larm_api::security::guard));
    (dir, repo, router, writer, reader, mcp)
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_sondeo_real_wsl2_y_docker_registro_y_lectura_publica() {
    use larm_domain::runtime_ports::RuntimeRepository;
    use std::time::Duration;
    use tokio::net::TcpListener;

    // 1) Sondeo read-only real del host (wsl.exe/docker.exe reales).
    let observed =
        larm_hardware::virtualization::probe("wsl.exe", "docker.exe", Duration::from_secs(30))
            .await;
    // Este equipo tiene WSL2 con Ubuntu y docker-desktop; el motor de
    // contenedores puede no tener daemon vivo: el cliente sí está.
    assert!(
        observed.wsl.available,
        "wsl.exe debe estar disponible en el PC IA"
    );
    assert!(
        observed.wsl.distros.iter().any(|d| d.wsl_version == 2),
        "al menos una distro WSL2: {:?}",
        observed.wsl.distros
    );
    assert!(
        observed.container.client_available,
        "docker.exe debe estar disponible: {:?}",
        observed.container.errors
    );
    let docker_version = observed
        .container
        .client_version
        .clone()
        .expect("versión del cliente");
    println!(
        "REAL wsl default={:?} distros={:?} docker={docker_version} build={:?} daemon={:?}",
        observed.wsl.default_distro,
        observed.wsl.distros,
        observed.container.build,
        observed.container.daemon_running
    );

    // 2) Persistencia de la observación estructurada.
    let (_dir, repo, router, writer, reader, mcp) = setup().await;
    let at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let ctx = MutationContext {
        actor_id: "p081-real".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let default_distro = observed.wsl.default_distro.clone().unwrap_or_else(|| {
        observed
            .wsl
            .distros
            .first()
            .expect("distro WSL")
            .name
            .clone()
    });
    let wsl_host = ExecutionHost {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: at.clone(),
        updated_at: at.clone(),
        row_version: 1,
        metadata: serde_json::from_value(
            json!({"source": "virtualization::probe", "tool": "wsl.exe"}),
        )
        .unwrap(),
        kind: ExecutionHostKind::Wsl2,
        name: default_distro,
        engine_version: None,
        wsl_version: Some(2),
        default: true,
        status: ExternalHostState::Online,
        evidence_refs: vec!["P081-REAL-wsl-probe".into()],
    };
    let container_host = ExecutionHost {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: at.clone(),
        updated_at: at.clone(),
        row_version: 1,
        metadata: serde_json::from_value(
            json!({"source": "virtualization::probe", "tool": "docker.exe"}),
        )
        .unwrap(),
        kind: ExecutionHostKind::Container,
        name: "docker".into(),
        engine_version: Some(docker_version.clone()),
        wsl_version: None,
        default: false,
        status: if observed.container.daemon_running == Some(true) {
            ExternalHostState::Online
        } else {
            ExternalHostState::Unknown
        },
        evidence_refs: vec!["P081-REAL-docker-probe".into()],
    };
    repo.record_execution_host(&wsl_host, &ctx).await.unwrap();
    repo.record_execution_host(&container_host, &ctx)
        .await
        .unwrap();

    // 3) Lectura pública con scopes reales.
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

    let response = client
        .get(format!("http://{address}/api/v1/execution-hosts"))
        .bearer_auth(&reader)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let page: Value = response.json().await.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "dos hosts registrados: {page}");
    let kinds: Vec<&str> = items
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"WSL2"));
    assert!(kinds.contains(&"CONTAINER"));
    let wsl_item = items.iter().find(|i| i["kind"] == "WSL2").unwrap();
    assert_eq!(wsl_item["wsl_version"], 2);
    assert_eq!(wsl_item["status"], "ONLINE");
    assert_eq!(wsl_item["default"], true);
    let container_item = items.iter().find(|i| i["kind"] == "CONTAINER").unwrap();
    assert_eq!(container_item["engine_version"], json!(docker_version));
    assert!(container_item["wsl_version"].is_null());

    // Filtro por nombre.
    let filtered = client
        .get(format!("http://{address}/api/v1/execution-hosts?q=dock"))
        .bearer_auth(&reader)
        .send()
        .await
        .unwrap();
    assert_eq!(filtered.status(), reqwest::StatusCode::OK);
    let filtered: Value = filtered.json().await.unwrap();
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);

    // Filtro inválido → 400.
    let bad = client
        .get(format!(
            "http://{address}/api/v1/execution-hosts?state=PAUSED"
        ))
        .bearer_auth(&reader)
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);

    // Sin token → 401; token MCP sin scope read → 403.
    let anonymous = client
        .get(format!("http://{address}/api/v1/execution-hosts"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
    let mcp = client
        .get(format!("http://{address}/api/v1/execution-hosts"))
        .bearer_auth(&mcp)
        .send()
        .await
        .unwrap();
    assert_eq!(mcp.status(), reqwest::StatusCode::FORBIDDEN);

    // El registro nunca permite escritura por la API: no existe POST.
    let post = client
        .post(format!("http://{address}/api/v1/execution-hosts"))
        .bearer_auth(&writer)
        .json(&json!({"kind":"WSL2","name":"intruder"}))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);

    let _ = shutdown_tx.send(());
    server.await.unwrap();
    let _ = repo;
}
