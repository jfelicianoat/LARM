//! P131: seguridad de credenciales y autenticación REAL sobre el PC IA —
//! rate limit de fallos de autenticación sobre TCP real con la IP de origen,
//! rotación y revocación de tokens contra SQLite migrado con eventos de
//! auditoría y el backend seguro del sistema operativo (Windows Credential
//! Manager), sin secreto en la base de datos ni en el audit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::extract::Extension;
use larm_core::security::memory::{
    MemoryApprovals, MemoryCredentials, MemoryIdempotency, MemorySecrets,
};
use larm_core::security::{Approvals, Credentials, CursorSigner, Principals};
use larm_db::security::SqlCredentials;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::ports::{
    CredentialRepository, MutationContext, PrincipalAuthenticator, SecretStore,
};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// El backend seguro real del daemon: Windows Credential Manager (la misma
/// semántica de `apps/daemon/src/bridge.rs`, aquí contra el almacén del SO).
struct OsKeyring;
impl SecretStore for OsKeyring {
    fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(service, username)
            .and_then(|entry| entry.set_password(secret))
            .map_err(|e| format!("credential manager: {e}"))
    }

    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
        match keyring::Entry::new(service, username).and_then(|entry| entry.get_password()) {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("credential manager: {e}")),
        }
    }

    fn delete(&self, service: &str, username: &str) -> Result<(), String> {
        match keyring::Entry::new(service, username).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("credential manager: {e}")),
        }
    }
}

async fn get(client: &reqwest::Client, base: &str, token: &str) -> (u16, Value) {
    let response = client
        .get(format!("{base}/api/v1/credentials"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_real_sobre_tcp_bloquea_la_fuente_de_origen() {
    // 1) Router real de seguridad con el limiter del daemon, servido con
    //    connect info (igual que el daemon) en loopback del PC IA.
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p131-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, token) = principals
        .enroll(
            PrincipalKind::Operator,
            "p131-rl-admin",
            vec![ApiScope::Read, ApiScope::Admin],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let state = larm_api::security::SecurityState {
        auth: principals.clone(),
        credentials: Arc::new(Credentials {
            repo: Arc::new(MemoryCredentials::default()),
            secrets: Arc::new(MemorySecrets::default()),
        }),
        approvals: Arc::new(Approvals {
            repo: Arc::new(MemoryApprovals::default()),
            ttl: chrono::Duration::seconds(900),
        }),
        idempotency: Arc::new(MemoryIdempotency::default()),
        cursors: Arc::new(CursorSigner::default()),
    };
    let limiter = Arc::new(larm_api::auth::AuthRateLimiter::new());
    let app = larm_api::security::router(state).layer(Extension(limiter.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // 2) Diez credenciales inválidas desde la fuente real 127.0.0.1 llenan la
    //    ventana: cada una responde 401 bearer_invalid.
    for index in 0..10 {
        let (status, body) = get(&client, &base, &format!("token-malo-{index}")).await;
        assert_eq!(status, 401);
        assert_eq!(body["details"]["reason"], "bearer_invalid", "fallo {index}");
    }

    // 3) El onceavo intento con token VÁLIDO sale bloqueado: auth_rate_limited
    //    con Retry-After positivo (AUTH_REQUIRED del catálogo sellado).
    let (status, body) = get(&client, &base, &token).await;
    assert_eq!(status, 401);
    assert_eq!(body["details"]["reason"], "auth_rate_limited");
    let retry = body["details"].as_object().map(|d| d.len()).unwrap_or(0);
    assert!(retry > 0, "detalle presente");
    println!("REAL P131: rate limit bloquea la fuente real tras 10 fallos (auth_rate_limited)");

    // 4) Una autenticación correcta resetea el historial de la fuente (la
    //    ventana también libera al expirar).
    limiter.reset("127.0.0.1");
    let (status, _) = get(&client, &base, &token).await;
    assert_eq!(status, 200, "tras el reset la fuente vuelve a servir");
    println!("REAL P131: reset por éxito devuelve el servicio a la fuente");

    let _ = shutdown_tx.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotacion_y_revocacion_reales_con_sqlite_eventos_y_credential_manager() {
    // 1) SQLite migrado + repositorio real de credenciales + backend seguro
    //    del sistema operativo (Windows Credential Manager).
    let dir = TempDir::new().unwrap();
    let db = larm_db::pool::Db::open(&dir.path().join("p131.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let repo = Arc::new(SqlCredentials {
        pool: db.pool.clone(),
    });
    let secrets = Arc::new(OsKeyring);
    let principals = Arc::new(Principals::new(repo.clone(), secrets.clone()));
    let ctx = MutationContext {
        actor_id: "p131-rotation".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (p, t1) = principals
        .enroll(
            PrincipalKind::Operator,
            "p131-rotation-test",
            vec![ApiScope::Read],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();

    // 2) La referencia persistida no guarda el token; el secreto vive en el
    //    Credential Manager con el servicio larm/principal/<id>.
    let reference = repo.get(&p.id).await.unwrap().expect("referencia");
    let service = reference.service.clone();
    let username = reference.username.clone();
    assert!(!serde_json::to_string(&reference).unwrap().contains(&t1));
    assert_eq!(
        secrets.read(&service, &username).unwrap().as_deref(),
        Some(t1.as_str()),
        "el token vive en el Credential Manager"
    );
    println!("REAL P131: credencial con secreto en el Credential Manager y sin token en SQLite");

    // 3) Rotación REAL (el camino del CLI del operador): token nuevo, el
    //    anterior deja de valer inmediatamente y el Credential Manager se
    //    actualiza.
    let t2 = principals.rotate(&p.id, &ctx).await.unwrap();
    assert_ne!(t1, t2);
    assert!(principals.authenticate(&t1).await.unwrap().is_none());
    assert!(principals.authenticate(&t2).await.unwrap().is_some());
    assert_eq!(
        secrets.read(&service, &username).unwrap().as_deref(),
        Some(t2.as_str()),
        "el Credential Manager guarda el token rotado"
    );

    // 4) Auditoría: eventos credential.created y credential.rotated en la
    //    base real, y ningún secreto en ninguna fila de la base.
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM event WHERE resource_type='credential_reference' ORDER BY created_at",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(events.contains(&"credential.created".to_string()));
    assert!(events.contains(&"credential.rotated".to_string()));
    println!("REAL P131: rotacion con eventos credential.created/rotated en SQLite");
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT service || '|' || username || '|' || backend || '|' || COALESCE(metadata,'') FROM credential_reference",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let dump = rows.join("\n");
    assert!(
        !dump.contains(&t1) && !dump.contains(&t2),
        "sin secreto en la base"
    );
    println!("REAL P131: sin secreto en la base de datos");

    // 5) Revocación REAL: el token deja de autenticar y el Credential Manager
    //    borra la entrada; el evento credential.deleted queda en la base.
    principals.revoke(&p.id, &ctx).await.unwrap();
    assert!(principals.authenticate(&t2).await.unwrap().is_none());
    assert_eq!(secrets.read(&service, &username).unwrap(), None);
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM event WHERE resource_type='credential_reference' ORDER BY created_at",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(events.contains(&"credential.deleted".to_string()));
    println!("REAL P131: revocacion borra el secreto del SO y emite credential.deleted");

    // El cierre del temp dir borra la base; el Credential Manager ya quedó
    // limpio por la revocación.
    std::mem::drop(dir);
}
