//! Rutas de seguridad (P009) a través del router real con puertos en memoria.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_core::security::memory::{
    MemoryApprovals, MemoryCredentials, MemoryIdempotency, MemorySecrets,
};
use larm_core::security::{Approvals, Credentials, CursorSigner, Principals};
use larm_domain::auth::{ApiScope, McpPermission, PrincipalKind};
use larm_domain::ports::{MutationContext, SecretStore};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tower::ServiceExt;

struct Harness {
    app: axum::Router,
    secrets: Arc<MemorySecrets>,
    principals: Arc<Principals>,
    admin: String,
    reader: String,
    creds: String,
    broker: String,
    mcp: String,
}

async fn harness() -> Harness {
    let repo = Arc::new(MemoryCredentials::default());
    let secrets = Arc::new(MemorySecrets::default());
    let principals = Arc::new(Principals::new(repo.clone(), secrets.clone()));
    let ctx = MutationContext {
        actor_id: "t".into(),
        correlation_id: "c".into(),
    };
    let enroll = |kind, name: &'static str, api: Vec<ApiScope>, mcp: Vec<McpPermission>| {
        let principals = principals.clone();
        let ctx = ctx.clone();
        async move {
            principals
                .enroll(kind, name, api, mcp, None, &ctx)
                .await
                .unwrap()
                .1
        }
    };
    let admin = enroll(
        PrincipalKind::Operator,
        "admin",
        vec![ApiScope::Admin],
        vec![],
    )
    .await;
    let reader = enroll(
        PrincipalKind::Operator,
        "lector",
        vec![ApiScope::Read],
        vec![],
    )
    .await;
    let creds = enroll(
        PrincipalKind::Operator,
        "creds",
        vec![ApiScope::CredentialsWrite, ApiScope::CredentialsRead],
        vec![],
    )
    .await;
    let broker = enroll(
        PrincipalKind::Broker,
        "ai-broker",
        vec![ApiScope::Read, ApiScope::LeasesManage],
        vec![],
    )
    .await;
    let mcp = enroll(
        PrincipalKind::Mcp,
        "athena",
        vec![ApiScope::Admin],
        vec![McpPermission::Admin],
    )
    .await;
    let state = larm_api::security::SecurityState {
        auth: principals.clone(),
        credentials: Arc::new(Credentials {
            repo,
            secrets: secrets.clone(),
        }),
        approvals: Arc::new(Approvals {
            repo: Arc::new(MemoryApprovals::default()),
            ttl: chrono::Duration::seconds(900),
        }),
        idempotency: Arc::new(MemoryIdempotency::default()),
        cursors: Arc::new(CursorSigner::default()),
    };
    Harness {
        app: larm_api::security::router(state),
        secrets,
        principals,
        admin,
        reader,
        creds,
        broker,
        mcp,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = req
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value, headers)
}

fn cred_body(service: &str) -> String {
    json!({"service": service, "username": "acc", "secret_type": "TOKEN", "secret": "hf_supersecretovalor", "scopes": []}).to_string()
}

#[tokio::test]
async fn sin_bearer_token_malo_o_scope_insuficiente() {
    let h = harness().await;
    let (s, v, _) = call(&h.app, "GET", "/api/v1/credentials", None, &[], None).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AUTH_REQUIRED"))
    );
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        Some("token-inventado"),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("bearer_invalid"))
    );
    // read del Broker no es credentials:read
    for token in [&h.reader, &h.broker] {
        let (s, v, _) = call(&h.app, "GET", "/api/v1/credentials", Some(token), &[], None).await;
        assert_eq!(
            (s, v["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("FORBIDDEN"))
        );
    }
    // un principal MCP con admin+admin no alcanza operaciones sin herramienta MCP
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        Some(&h.mcp),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::FORBIDDEN, Some("mcp_operation_unmapped"))
    );
    // cabeceras del cliente no cambian la audiencia
    let (s, _, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        Some(&h.mcp),
        &[("x-larm-audience", "larm-api"), ("x-mcp", "false")],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        Some(&h.creds),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn autentica_antes_de_leer_el_cuerpo() {
    let h = harness().await;
    // cuerpo inválido y sin token: 401, no 400 — el cuerpo ni se mira
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        None,
        &[("idempotency-key", "k")],
        Some("{basura"),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.reader),
        &[("idempotency-key", "k")],
        Some("{basura"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "k")],
        Some("{basura"),
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("VALIDATION_FAILED"))
    );
}

#[tokio::test]
async fn crear_es_idempotente_y_nunca_devuelve_el_secreto() {
    let h = harness().await;
    let body = cred_body("larm/source/hf");
    let (s, first, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "alta-1")],
        Some(&body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{first}");
    assert!(!first.to_string().contains("hf_supersecretovalor"));
    assert_eq!(first["backend"], "WINDOWS_CREDENTIAL_MANAGER");
    let (s, replay, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "alta-1")],
        Some(&body),
    )
    .await;
    assert_eq!(
        (s, &replay),
        (StatusCode::CREATED, &first),
        "replay devuelve la respuesta original"
    );
    let other = cred_body("larm/source/otro");
    let (s, v, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "alta-1")],
        Some(&other),
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("IDEMPOTENCY_CONFLICT"))
    );
    let (s, v, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[],
        Some(&body),
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("idempotency_key_invalid"))
    );
    // GET tampoco lo muestra
    let id = first["id"].as_str().unwrap();
    let (s, got, _) = call(
        &h.app,
        "GET",
        &format!("/api/v1/credentials/{id}"),
        Some(&h.creds),
        &[],
        None,
    )
    .await;
    assert_eq!((s, got), (StatusCode::OK, first));
    // espacio reservado: no se fabrican principales por /credentials
    let (s, v, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "p")],
        Some(&cred_body("larm/principal/x")),
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::FORBIDDEN, Some("reserved_credential_namespace"))
    );
}

#[tokio::test]
async fn borrar_con_if_match() {
    let h = harness().await;
    let (_, created, _) = call(
        &h.app,
        "POST",
        "/api/v1/credentials",
        Some(&h.creds),
        &[("idempotency-key", "a")],
        Some(&cred_body("larm/source/hf")),
    )
    .await;
    let uri = format!("/api/v1/credentials/{}", created["id"].as_str().unwrap());
    let (s, v, _) = call(&h.app, "DELETE", &uri, Some(&h.creds), &[], None).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            Some("PRECONDITION_REQUIRED")
        )
    );
    let (s, v, _) = call(
        &h.app,
        "DELETE",
        &uri,
        Some(&h.creds),
        &[("if-match", "9")],
        None,
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("STALE_REVISION"))
    );
    let (s, _, _) = call(
        &h.app,
        "DELETE",
        &uri,
        Some(&h.creds),
        &[("if-match", "1")],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(h.secrets.read("larm/source/hf", "acc").unwrap().is_none());
    let (s, _, _) = call(&h.app, "GET", &uri, Some(&h.creds), &[], None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials/no-es-uuid",
        Some(&h.creds),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("path_id_invalid"))
    );
}

#[tokio::test]
async fn lista_paginada_y_filtros_ajenos_rechazados() {
    let h = harness().await;
    let (s, page, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials?limit=2",
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // los 5 principales del arnés están en credential_reference: la lista los enseña sin secreto
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let (s, next, _) = call(
        &h.app,
        "GET",
        &format!("/api/v1/credentials?limit=2&cursor={cursor}"),
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(next["items"][0]["id"], page["items"][0]["id"]);
    for tok in [&h.admin, &h.reader, &h.creds, &h.broker, &h.mcp] {
        assert!(
            !page.to_string().contains(tok.as_str()) && !next.to_string().contains(tok.as_str())
        );
    }
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials?q=x",
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("filter_not_allowed"))
    );
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials?cursor=manipulado",
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_cursor"))
    );
}

#[tokio::test]
async fn aprobacion_exige_admin_y_devuelve_recibo() {
    let h = harness().await;
    let body = json!({"operation_digest": "b".repeat(64), "resource_ids": ["v"], "code_hash": null, "permissions": {}, "approved": true}).to_string();
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/api/v1/approvals",
        Some(&h.creds),
        &[("idempotency-key", "a")],
        Some(&body),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, receipt, _) = call(
        &h.app,
        "POST",
        "/api/v1/approvals",
        Some(&h.admin),
        &[("idempotency-key", "a")],
        Some(&body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{receipt}");
    assert!(larm_domain::bridge::is_uuid_v7(
        receipt["resource_id"].as_str().unwrap()
    ));
    assert_eq!(receipt["row_version"], 1);
    assert!(receipt["event_id"].as_str().unwrap().parse::<u64>().is_ok());
    let rejected = json!({"operation_digest": "b".repeat(64), "resource_ids": ["v"], "code_hash": null, "permissions": {}, "approved": false}).to_string();
    let (s, v, _) = call(
        &h.app,
        "POST",
        "/api/v1/approvals",
        Some(&h.admin),
        &[("idempotency-key", "b")],
        Some(&rejected),
    )
    .await;
    assert_eq!(
        (s, v["details"]["reason"].as_str()),
        (StatusCode::BAD_REQUEST, Some("approval_not_granted"))
    );
}

#[tokio::test]
async fn almacen_caido_responde_503_y_ruta_desconocida_404() {
    let h = harness().await;
    h.principals.invalidate();
    h.secrets.fail.store(true, Ordering::SeqCst);
    let (s, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str(), v["retryable"].as_bool()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("AUTH_BACKEND_UNAVAILABLE"),
            Some(true)
        )
    );
    assert!(
        !v.to_string().contains("credential manager no disponible"),
        "el detalle técnico no sale"
    );
    h.secrets.fail.store(false, Ordering::SeqCst);
    let (s, _, _) = call(
        &h.app,
        "PATCH",
        "/api/v1/credentials",
        Some(&h.admin),
        &[],
        None,
    )
    .await;
    assert!(s == StatusCode::NOT_FOUND || s == StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn correlacion_v7_se_conserva_y_otra_se_sustituye() {
    let h = harness().await;
    let mine = uuid::Uuid::now_v7().to_string();
    let (_, v, headers) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        None,
        &[("x-correlation-id", &mine)],
        None,
    )
    .await;
    assert_eq!(v["correlation_id"].as_str(), Some(mine.as_str()));
    assert_eq!(headers["x-correlation-id"], mine.as_str());
    let (_, v, _) = call(
        &h.app,
        "GET",
        "/api/v1/credentials",
        None,
        &[("x-correlation-id", "8f14e45f-ceea-4c3b-9e1d-2a7b5c3d4e5f")],
        None,
    )
    .await;
    let got = v["correlation_id"].as_str().unwrap();
    assert!(larm_domain::bridge::is_uuid_v7(got) && got != "8f14e45f-ceea-4c3b-9e1d-2a7b5c3d4e5f");
}
