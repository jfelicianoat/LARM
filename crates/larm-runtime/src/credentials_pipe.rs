//! Canal local de credenciales de endpoint (contrato 04).
//!
//! `\\.\pipe\LARM-{sha256(SID)[0:16]}-credentials`, solo clientes locales
//! (`PIPE_REJECT_REMOTE_CLIENTS`), mensajes NDJSON de 4 KiB como máximo y un
//! nonce de 32 bytes de un solo uso que caduca a los 60 s. El token de
//! endpoint sale solo por aquí y solo hacia el principal propietario de la
//! lease, en su generación ACTIVE.
//!
//! El daemon crea cada instancia con una DACL que solo admite el SID del
//! usuario y SYSTEM (`PipeFactory`, ADR-0009); además el Bearer del principal
//! autoriza cada petición.

use std::sync::Arc;
use std::time::Duration;

use larm_domain::bridge::{timestamp, ApiError};
use larm_domain::ports::{ApiScope, BrokerBridge, PrincipalAuthenticator};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Tamaño máximo de un mensaje del canal.
pub const MESSAGE_MAX_BYTES: usize = 4096;
/// Vida del nonce de sesión.
pub const NONCE_TTL: Duration = Duration::from_secs(60);

/// Nombre del pipe para un SID.
#[must_use]
pub fn pipe_name(sid: &str) -> String {
    let digest = hex::encode(Sha256::digest(sid.as_bytes()));
    format!(r"\\.\pipe\LARM-{}-credentials", &digest[..16])
}

/// Petición del cliente (estricta).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequest {
    pub request_id: String,
    pub lease_id: String,
    pub generation: u64,
    pub credential_reference_id: String,
    pub bearer_token: String,
    pub nonce: String,
}

/// Procesa una petición ya leída: valida nonce, principal y lease. Devuelve
/// la línea de respuesta (sin el salto final).
pub async fn answer(
    raw: &[u8],
    expected_nonce: &str,
    issued: tokio::time::Instant,
    auth: &dyn PrincipalAuthenticator,
    bridge: &dyn BrokerBridge,
) -> String {
    let refuse = |error: ApiError| {
        json!({"code": error.code, "message": error.message, "retryable": error.retryable})
            .to_string()
    };
    if raw.len() > MESSAGE_MAX_BYTES {
        return refuse(ApiError::new(
            "VALIDATION_FAILED",
            "Mensaje de más de 4 KiB",
            "message_too_large",
        ));
    }
    let request: CredentialRequest = match serde_json::from_slice(raw) {
        Ok(request) => request,
        Err(_) => {
            return refuse(ApiError::new(
                "VALIDATION_FAILED",
                "Petición inválida",
                "invalid_request",
            ))
        }
    };
    if request.nonce != expected_nonce || issued.elapsed() > NONCE_TTL {
        return refuse(ApiError::new(
            "FORBIDDEN",
            "Nonce inválido o caducado",
            "nonce_invalid",
        ));
    }
    let principal = match auth.authenticate(&request.bearer_token).await {
        Ok(Some(principal)) => principal,
        Ok(None) => {
            return refuse(ApiError::new(
                "AUTH_REQUIRED",
                "Principal no autenticado",
                "bearer_invalid",
            ))
        }
        Err(error) => return refuse(error),
    };
    if !principal.has_api_scope(ApiScope::LeasesManage) {
        return refuse(ApiError::new(
            "FORBIDDEN",
            "El principal no tiene leases:manage",
            "scope_missing",
        ));
    }
    match bridge
        .endpoint_credential(
            &principal,
            &request.lease_id,
            request.generation,
            &request.credential_reference_id,
        )
        .await
    {
        Ok(credential) => json!({
            "request_id": request.request_id,
            "token": credential.token,
            "expires_at": credential.expires_at,
        })
        .to_string(),
        Err(error) => refuse(error),
    }
}

/// Nonce de 32 bytes de un CSPRNG, en hexadecimal.
#[must_use]
pub fn new_nonce() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

#[cfg(windows)]
mod server {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::NamedPipeServer;

    /// Crea una instancia del pipe; `true` en la primera. El daemon la
    /// implementa con la DACL restringida al SID del usuario (ADR-0009).
    pub type PipeFactory = Arc<dyn Fn(bool) -> std::io::Result<NamedPipeServer> + Send + Sync>;

    /// Sirve el pipe hasta que falle su creación. Cada conexión se atiende en
    /// su tarea; el siguiente servidor se crea antes de atenderla.
    pub async fn serve(
        create: PipeFactory,
        auth: Arc<dyn PrincipalAuthenticator>,
        bridge: Arc<dyn BrokerBridge>,
    ) -> std::io::Result<()> {
        let mut server = create(true)?;
        loop {
            server.connect().await?;
            let connected = server;
            server = create(false)?;
            let auth = auth.clone();
            let bridge = bridge.clone();
            tokio::spawn(async move {
                if let Err(error) = handle(connected, auth.as_ref(), bridge.as_ref()).await {
                    tracing::warn!(error = %error, "conexión del canal de credenciales cerrada con error");
                }
            });
        }
    }

    async fn handle(
        mut pipe: NamedPipeServer,
        auth: &dyn PrincipalAuthenticator,
        bridge: &dyn BrokerBridge,
    ) -> std::io::Result<()> {
        let nonce = new_nonce();
        let issued = tokio::time::Instant::now();
        let expires = chrono::Utc::now() + chrono::Duration::seconds(NONCE_TTL.as_secs() as i64);
        let hello = json!({"nonce": nonce, "expires_at": timestamp(expires)}).to_string() + "\n";
        pipe.write_all(hello.as_bytes()).await?;
        let mut line = Vec::with_capacity(512);
        let read = tokio::time::timeout(NONCE_TTL, async {
            let mut byte = [0u8; 1];
            while line.len() <= MESSAGE_MAX_BYTES {
                if pipe.read(&mut byte).await? == 0 || byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Ok::<(), std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            return Ok(());
        }
        let reply = answer(&line, &nonce, issued, auth, bridge).await + "\n";
        pipe.write_all(reply.as_bytes()).await?;
        pipe.flush().await?;
        // El nonce era de esta conexión y muere con ella: no se reutiliza.
        Ok(())
    }
}

#[cfg(windows)]
pub use server::{serve, PipeFactory};

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]
    use super::*;
    use async_trait::async_trait;
    use larm_domain::bridge::{BrokerCatalog, JobState, LeaseRequest, Page, Renew, RuntimeLease};
    use larm_domain::ports::{CreateLeaseOutcome, EndpointCredential, GatewayResponse, Principal};
    use serde_json::Value;

    struct Tokens;
    #[async_trait]
    impl PrincipalAuthenticator for Tokens {
        async fn authenticate(&self, bearer: &str) -> Result<Option<Principal>, ApiError> {
            Ok(match bearer {
                "broker" => Some(
                    larm_domain::auth::Principal::new(
                        "0192aaaa-0000-7000-8000-000000000001",
                        "ai-broker",
                        larm_domain::auth::PrincipalKind::Broker,
                        [
                            larm_domain::auth::ApiScope::Read,
                            larm_domain::auth::ApiScope::LeasesManage,
                        ],
                        [],
                    )
                    .unwrap_or_else(|e| panic!("{e}")),
                ),
                "reader" => Some(
                    larm_domain::auth::Principal::new(
                        "0192aaaa-0000-7000-8000-000000000002",
                        "lector",
                        larm_domain::auth::PrincipalKind::Operator,
                        [larm_domain::auth::ApiScope::Read],
                        [],
                    )
                    .unwrap_or_else(|e| panic!("{e}")),
                ),
                _ => None,
            })
        }
    }

    struct OneLease;
    #[async_trait]
    impl BrokerBridge for OneLease {
        async fn catalog(&self) -> Result<BrokerCatalog, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn launch_profiles(&self) -> Result<Page<Value>, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn create_lease(
            &self,
            _: &Principal,
            _: LeaseRequest,
            _: &str,
        ) -> Result<CreateLeaseOutcome, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn get_lease(&self, _: &Principal, _: &str) -> Result<RuntimeLease, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn list_leases(&self, _: &Principal) -> Result<Page<RuntimeLease>, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn renew_lease(
            &self,
            _: &Principal,
            _: &str,
            _: u64,
            _: &str,
            _: Renew,
        ) -> Result<RuntimeLease, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn release_lease(&self, _: &Principal, _: &str, _: u64) -> Result<(), ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn get_job(&self, _: &Principal, _: &str) -> Result<JobState, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn gateway(
            &self,
            _: &str,
            _: Option<&str>,
            _: &str,
            _: Value,
        ) -> Result<GatewayResponse, ApiError> {
            Err(ApiError::new("NOT_FOUND", "x", "x"))
        }
        async fn endpoint_credential(
            &self,
            p: &Principal,
            lease: &str,
            generation: u64,
            reference: &str,
        ) -> Result<EndpointCredential, ApiError> {
            if p.name == "ai-broker" && lease == "L" && generation == 3 && reference == "R" {
                Ok(EndpointCredential {
                    token: "secreto".into(),
                    expires_at: "2099-01-01T00:00:00.000Z".into(),
                })
            } else {
                Err(ApiError::new("FORBIDDEN", "ajena", "not_owner"))
            }
        }
    }

    fn request(bearer: &str, nonce: &str, generation: u64) -> Vec<u8> {
        json!({"request_id": "q", "lease_id": "L", "generation": generation, "credential_reference_id": "R",
               "bearer_token": bearer, "nonce": nonce})
        .to_string()
        .into_bytes()
    }

    async fn reply(raw: &[u8], nonce: &str) -> Value {
        let text = answer(raw, nonce, tokio::time::Instant::now(), &Tokens, &OneLease).await;
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[tokio::test]
    async fn owner_with_valid_nonce_gets_the_token() {
        let value = reply(&request("broker", "n1", 3), "n1").await;
        assert_eq!(value["token"], "secreto");
        assert_eq!(value["request_id"], "q");
    }

    #[tokio::test]
    async fn every_refusal_is_explicit_and_never_carries_a_token() {
        let cases = [
            (request("broker", "otro", 3), "FORBIDDEN"),
            (request("nadie", "n1", 3), "AUTH_REQUIRED"),
            (request("reader", "n1", 3), "FORBIDDEN"),
            (request("broker", "n1", 4), "FORBIDDEN"),
            (b"{no json".to_vec(), "VALIDATION_FAILED"),
            (vec![b' '; MESSAGE_MAX_BYTES + 1], "VALIDATION_FAILED"),
        ];
        for (raw, code) in cases {
            let value = reply(&raw, "n1").await;
            assert_eq!(value["code"], code);
            assert!(value.get("token").is_none());
        }
    }

    #[test]
    fn pipe_name_matches_the_broker_derivation() {
        // Mismo cálculo que app.larm.default_credentials_pipe en el Broker.
        let name = pipe_name("S-1-5-21-1-2-3-1001");
        let digest = hex::encode(Sha256::digest(b"S-1-5-21-1-2-3-1001"));
        assert_eq!(
            name,
            format!(r"\\.\pipe\LARM-{}-credentials", &digest[..16])
        );
    }

    #[test]
    fn nonces_are_long_and_unique() {
        let (a, b) = (new_nonce(), new_nonce());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
