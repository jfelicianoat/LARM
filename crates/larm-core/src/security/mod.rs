//! Casos de uso de seguridad (P009): principales, credenciales, aprobaciones
//! y cursores firmados. Los puertos (repositorios y almacén de secretos) los
//! implementan `larm-db` y el daemon (Credential Manager).

pub mod memory;

use async_trait::async_trait;
use larm_domain::approval::{ApprovalRecord, ApprovalRequest, ApprovalUse};
use larm_domain::auth::{ApiScope, McpPermission, Principal, PrincipalKind};
use larm_domain::bridge::{timestamp, Page};
use larm_domain::credential::{
    is_reserved_service, CredentialReference, CredentialWrite, SecretType, BACKEND,
    PRINCIPAL_PREFIX,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    ApprovalRepository, CredentialRepository, MutationContext, PrincipalAuthenticator, SecretStore,
};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Vida máxima de la caché de principales: una rotación hecha desde otro
/// proceso deja de valer como mucho en este tiempo.
pub const PRINCIPAL_CACHE_TTL: Duration = Duration::from_secs(5);

fn backend_unavailable(detail: &str) -> LarmError {
    LarmError::new(
        ErrorCode::AuthBackendUnavailable,
        "El almacén de credenciales no está disponible",
        "credential_backend_unavailable",
    )
    .retryable(true)
    .with_technical(detail.to_string())
}

/// Token nuevo: 32 bytes de un CSPRNG en hexadecimal.
#[must_use]
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[derive(Default)]
struct Cache {
    loaded: Option<Instant>,
    by_digest: HashMap<[u8; 32], Principal>,
}

/// Registro de principales: referencia en SQLite (`larm/principal/{id}`) y
/// token en Credential Manager. Nunca guarda el token en claro en memoria:
/// solo su sha256.
pub struct Principals {
    repo: Arc<dyn CredentialRepository>,
    secrets: Arc<dyn SecretStore>,
    cache: Mutex<Cache>,
}

/// Principal reconstruido desde su `credential_reference`.
///
/// # Errors
/// `INTERNAL` si la referencia no describe un principal válido.
pub fn principal_from_reference(reference: &CredentialReference) -> Result<Principal, LarmError> {
    let corrupt = |why: String| {
        LarmError::new(
            ErrorCode::Internal,
            "Principal mal registrado",
            "principal_corrupt",
        )
        .with_resource(reference.id.clone())
        .with_technical(why)
    };
    let kind = reference
        .metadata
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(PrincipalKind::parse)
        .ok_or_else(|| corrupt("metadata.kind ausente o desconocido".into()))?;
    let api = reference
        .scopes
        .iter()
        .map(|s| ApiScope::parse(s).ok_or_else(|| corrupt(format!("scope {s}"))))
        .collect::<Result<Vec<_>, _>>()?;
    let mcp = reference
        .metadata
        .get("mcp_scopes")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|v| {
                    v.as_str()
                        .and_then(McpPermission::parse)
                        .ok_or_else(|| corrupt(format!("mcp_scope {v}")))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Principal::new(
        reference.id.clone(),
        reference.username.clone(),
        kind,
        api,
        mcp,
    )
    .map_err(|e| corrupt(e.technical_detail().to_string()))
}

impl Principals {
    #[must_use]
    pub fn new(repo: Arc<dyn CredentialRepository>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repo,
            secrets,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Vacía la caché: la siguiente autenticación relee referencias y secretos.
    pub fn invalidate(&self) {
        *self.cache.lock() = Cache::default();
    }

    async fn reload(&self) -> Result<(), LarmError> {
        let references = self.repo.by_service_prefix(PRINCIPAL_PREFIX).await?;
        let now = chrono::Utc::now();
        let mut by_digest = HashMap::new();
        for reference in references {
            if reference
                .expires_at
                .as_deref()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .is_some_and(|t| t <= now)
            {
                continue;
            }
            let principal = match principal_from_reference(&reference) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(principal = %reference.id, detail = %e.technical_detail(), "principal ignorado");
                    continue;
                }
            };
            match self.secrets.read(&reference.service, &reference.username) {
                Ok(Some(token)) if !token.is_empty() => {
                    by_digest.insert(digest(&token), principal);
                }
                Ok(_) => {
                    tracing::warn!(principal = %reference.id, "principal sin token en Credential Manager")
                }
                Err(e) => return Err(backend_unavailable(&e)),
            }
        }
        *self.cache.lock() = Cache {
            loaded: Some(Instant::now()),
            by_digest,
        };
        Ok(())
    }

    /// Da de alta un principal y devuelve su token, que solo se entrega aquí.
    /// Con `token` se usa ese (p. ej. el que ya tiene configurado el Broker);
    /// debe tener al menos 32 caracteres.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` si los scopes no respetan su tipo; `CONFLICT` si ya
    /// existe un principal con ese nombre; `AUTH_BACKEND_UNAVAILABLE` si
    /// Credential Manager falla (no queda nada a medias).
    pub async fn enroll(
        &self,
        kind: PrincipalKind,
        name: &str,
        api_scopes: Vec<ApiScope>,
        mcp_scopes: Vec<McpPermission>,
        token: Option<String>,
        ctx: &MutationContext,
    ) -> Result<(Principal, String), LarmError> {
        if token
            .as_deref()
            .is_some_and(|t| t.trim().len() < 32 || t.trim() != t)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El token debe tener al menos 32 caracteres sin espacios",
                "token_too_short",
            ));
        }
        let id = uuid::Uuid::now_v7().to_string();
        let principal = Principal::new(id.clone(), name, kind, api_scopes, mcp_scopes)?;
        if self
            .repo
            .by_service_prefix(PRINCIPAL_PREFIX)
            .await?
            .iter()
            .any(|r| r.username == name)
        {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe un principal con ese nombre",
                "principal_exists",
            ));
        }
        let now = timestamp(chrono::Utc::now());
        let mut metadata = serde_json::Map::new();
        metadata.insert("kind".into(), kind.as_str().into());
        metadata.insert(
            "mcp_scopes".into(),
            principal
                .mcp_scopes
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .into(),
        );
        metadata.insert("token_audience".into(), principal.audience().into());
        let reference = CredentialReference {
            id: id.clone(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            service: format!("{PRINCIPAL_PREFIX}{id}"),
            username: name.to_string(),
            backend: BACKEND.into(),
            secret_type: SecretType::Token,
            scopes: principal
                .api_scopes
                .iter()
                .map(|s| s.as_str().to_string())
                .collect(),
            expires_at: None,
        };
        let token = token.unwrap_or_else(new_token);
        self.secrets
            .store(&reference.service, &reference.username, &token)
            .map_err(|e| backend_unavailable(&e))?;
        if let Err(error) = self.repo.insert(&reference, ctx).await {
            let _ = self.secrets.delete(&reference.service, &reference.username);
            return Err(error);
        }
        self.invalidate();
        Ok((principal, token))
    }

    /// Emite un token nuevo para el principal: el anterior deja de valer ya
    /// en este proceso y, en otro, en cuanto caduque su caché.
    ///
    /// # Errors
    /// `NOT_FOUND` si no existe; `AUTH_BACKEND_UNAVAILABLE` si Credential Manager falla.
    pub async fn rotate(&self, id: &str, ctx: &MutationContext) -> Result<String, LarmError> {
        let reference = self
            .repo
            .get(id)
            .await?
            .filter(|r| r.service.starts_with(PRINCIPAL_PREFIX))
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Principal desconocido", "not_found")
                    .with_resource(id)
            })?;
        let token = new_token();
        self.secrets
            .store(&reference.service, &reference.username, &token)
            .map_err(|e| backend_unavailable(&e))?;
        self.invalidate();
        self.repo.touch(id, ctx).await?;
        Ok(token)
    }

    /// Revoca un principal: borra su token de Credential Manager y su referencia.
    ///
    /// # Errors
    /// `NOT_FOUND` si no existe; `AUTH_BACKEND_UNAVAILABLE` si Credential Manager falla.
    pub async fn revoke(&self, id: &str, ctx: &MutationContext) -> Result<(), LarmError> {
        let reference = self
            .repo
            .get(id)
            .await?
            .filter(|r| r.service.starts_with(PRINCIPAL_PREFIX))
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Principal desconocido", "not_found")
                    .with_resource(id)
            })?;
        // Primero el secreto: si Credential Manager falla, el principal sigue entero.
        self.secrets
            .delete(&reference.service, &reference.username)
            .map_err(|e| backend_unavailable(&e))?;
        self.invalidate();
        self.repo.delete(id, reference.row_version, ctx).await?;
        Ok(())
    }

    /// Principal por nombre (sin token).
    ///
    /// # Errors
    /// Errores del repositorio.
    pub async fn find(&self, name: &str) -> Result<Option<Principal>, LarmError> {
        self.repo
            .by_service_prefix(PRINCIPAL_PREFIX)
            .await?
            .iter()
            .find(|r| r.username == name)
            .map(principal_from_reference)
            .transpose()
    }
}

#[async_trait]
impl PrincipalAuthenticator for Principals {
    async fn authenticate(&self, bearer: &str) -> Result<Option<Principal>, LarmError> {
        let fresh = self
            .cache
            .lock()
            .loaded
            .is_some_and(|at| at.elapsed() < PRINCIPAL_CACHE_TTL);
        if !fresh {
            self.reload().await?;
        }
        Ok(self.cache.lock().by_digest.get(&digest(bearer)).cloned())
    }
}

/// CRUD de credenciales de `/credentials` (nunca devuelve secretos).
pub struct Credentials {
    pub repo: Arc<dyn CredentialRepository>,
    pub secrets: Arc<dyn SecretStore>,
}

impl Credentials {
    /// Guarda el secreto en Credential Manager y su referencia en la base.
    ///
    /// # Errors
    /// Validación del DTO; `CONFLICT` si ya existe; `AUTH_BACKEND_UNAVAILABLE`.
    pub async fn create(
        &self,
        write: CredentialWrite,
        ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        write.validate()?;
        // No sobrescribir el secreto de una referencia existente.
        if self
            .repo
            .by_service_prefix(&write.service)
            .await?
            .iter()
            .any(|r| r.service == write.service && r.username == write.username)
        {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe una credencial con ese servicio y usuario",
                "credential_exists",
            ));
        }
        let now = timestamp(chrono::Utc::now());
        let reference = CredentialReference {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            service: write.service.clone(),
            username: write.username.clone(),
            backend: BACKEND.into(),
            secret_type: write.secret_type,
            scopes: write.scopes.clone(),
            expires_at: None,
        };
        self.secrets
            .store(&write.service, &write.username, &write.secret)
            .map_err(|e| backend_unavailable(&e))?;
        if let Err(error) = self.repo.insert(&reference, ctx).await {
            let _ = self.secrets.delete(&write.service, &write.username);
            return Err(error);
        }
        Ok(reference)
    }

    /// # Errors
    /// `NOT_FOUND`.
    pub async fn get(&self, id: &str) -> Result<CredentialReference, LarmError> {
        self.repo.get(id).await?.ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Credencial desconocida", "not_found")
                .with_resource(id)
        })
    }

    /// # Errors
    /// Errores del repositorio.
    pub async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<CredentialReference>, LarmError> {
        self.repo.list(after, limit).await
    }

    /// Borra referencia y secreto con `If-Match`.
    ///
    /// # Errors
    /// `NOT_FOUND`, `STALE_REVISION`; `FORBIDDEN` en espacios reservados.
    pub async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let reference = self.get(id).await?;
        if is_reserved_service(&reference.service) {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Ese espacio de credenciales lo gestiona LARM",
                "reserved_credential_namespace",
            )
            .with_resource(id));
        }
        self.repo.delete(id, if_match, ctx).await?;
        if let Err(e) = self.secrets.delete(&reference.service, &reference.username) {
            tracing::error!(credential = %id, detail = %e, "referencia borrada pero el secreto sigue en Credential Manager");
        }
        Ok(())
    }
}

/// Recibo de una mutación síncrona (schema `CommandReceipt`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommandReceipt {
    pub resource_id: String,
    pub row_version: i64,
    pub event_id: String,
}

/// Aprobaciones ligadas a hash.
pub struct Approvals {
    pub repo: Arc<dyn ApprovalRepository>,
    pub ttl: chrono::Duration,
}

impl Approvals {
    /// Concede la aprobación en nombre de `ctx.actor_id`.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` si la petición no es válida.
    pub async fn grant(
        &self,
        request: &ApprovalRequest,
        ctx: &MutationContext,
    ) -> Result<CommandReceipt, LarmError> {
        let record = request.grant(
            uuid::Uuid::now_v7().to_string(),
            ctx.actor_id.clone(),
            chrono::Utc::now(),
            self.ttl,
        )?;
        let event_id = self.repo.insert(&record, ctx).await?;
        Ok(CommandReceipt {
            resource_id: record.id,
            row_version: 1,
            event_id,
        })
    }

    /// Valida y consume la aprobación para esta operación (un solo uso).
    ///
    /// # Errors
    /// `APPROVAL_REQUIRED` si no existe, caducó, ya se usó o no cubre la operación.
    pub async fn consume(
        &self,
        approval_id: &str,
        usage: &ApprovalUse<'_>,
    ) -> Result<ApprovalRecord, LarmError> {
        let missing = || {
            LarmError::new(
                ErrorCode::ApprovalRequired,
                "Aprobación desconocida",
                "approval_missing",
            )
            .with_resource(approval_id)
        };
        let record = self.repo.get(approval_id).await?.ok_or_else(missing)?;
        let now = chrono::Utc::now();
        record.check(usage, now)?;
        if !self.repo.consume(approval_id, &timestamp(now)).await? {
            return Err(LarmError::new(
                ErrorCode::ApprovalRequired,
                "La aprobación ya se usó",
                "approval_consumed",
            )
            .with_resource(approval_id));
        }
        Ok(record)
    }
}

/// Cursores opacos y firmados (contrato 07): base64url de la clave de orden y
/// del resumen del filtro, más un HMAC-SHA256 con clave del proceso. Un
/// cursor manipulado, de otra lista o de otro arranque se rechaza con 400.
pub struct CursorSigner {
    key: [u8; 32],
}

/// Tamaño de página por defecto y máximo del contrato 07.
pub const PAGE_DEFAULT: u32 = 50;
pub const PAGE_MAX: u32 = 200;

fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

fn bad_cursor() -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Cursor no válido",
        "bad_cursor",
    )
}

impl Default for CursorSigner {
    fn default() -> Self {
        let mut key = [0u8; 32];
        rand::fill(&mut key);
        Self { key }
    }
}

impl CursorSigner {
    #[must_use]
    pub fn sign(&self, sort_key: &str, id: &str, filter_digest: &str) -> String {
        use base64::Engine as _;
        let payload = serde_json::json!([sort_key, id, filter_digest]).to_string();
        let mac = hmac_sha256(&self.key, payload.as_bytes());
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}", engine.encode(payload), engine.encode(mac))
    }

    /// # Errors
    /// `VALIDATION_FAILED` (`bad_cursor`) si no es nuestro, está alterado o es de otro filtro.
    pub fn verify(&self, cursor: &str, filter_digest: &str) -> Result<(String, String), LarmError> {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let (payload_b64, mac_b64) = cursor.split_once('.').ok_or_else(bad_cursor)?;
        let payload = engine.decode(payload_b64).map_err(|_| bad_cursor())?;
        let mac = engine.decode(mac_b64).map_err(|_| bad_cursor())?;
        let expected = hmac_sha256(&self.key, &payload);
        // Comparación en tiempo constante.
        if mac.len() != expected.len()
            || mac
                .iter()
                .zip(expected)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                != 0
        {
            return Err(bad_cursor());
        }
        let parts: (String, String, String) =
            serde_json::from_slice(&payload).map_err(|_| bad_cursor())?;
        if parts.2 != filter_digest {
            return Err(bad_cursor());
        }
        Ok((parts.0, parts.1))
    }

    /// Página de credenciales con cursor.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` por límite fuera de 1..=200 o cursor inválido.
    pub async fn page_credentials(
        &self,
        service: &Credentials,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<CredentialReference>, LarmError> {
        self.page(
            "credentials:all",
            cursor,
            limit,
            |c: &CredentialReference| (c.created_at.clone(), c.id.clone()),
            |after, n| service.list(after, n),
        )
        .await
    }

    /// Página genérica ordenada por (`created_at`, `id`) con cursor firmado
    /// ligado a `filter` (contrato 07: default 50, máximo 200).
    ///
    /// # Errors
    /// `VALIDATION_FAILED` por límite fuera de 1..=200 o cursor inválido; los
    /// errores de `fetch` se propagan.
    pub async fn page<T, F, Fut>(
        &self,
        filter: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
        key: impl Fn(&T) -> (String, String),
        fetch: F,
    ) -> Result<Page<T>, LarmError>
    where
        F: FnOnce(Option<(String, String)>, u32) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<T>, LarmError>>,
    {
        let limit = limit.unwrap_or(PAGE_DEFAULT);
        if !(1..=PAGE_MAX).contains(&limit) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "limit debe estar en 1..=200",
                "limit_invalid",
            ));
        }
        let after = cursor.map(|c| self.verify(c, filter)).transpose()?;
        let snapshot_at = timestamp(chrono::Utc::now());
        let mut items = fetch(after, limit + 1).await?;
        let next_cursor = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|last| {
                let (sort_key, id) = key(last);
                self.sign(&sort_key, &id, filter)
            })
        } else {
            None
        };
        Ok(Page {
            items,
            next_cursor,
            snapshot_at,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn hmac_coincide_con_rfc4231_caso_2() {
        // RFC 4231, caso 2: clave "Jefe" (rellena a 32 bytes con ceros equivale a la clave de 4 bytes).
        let mut key = [0u8; 32];
        key[..4].copy_from_slice(b"Jefe");
        let mac = hmac_sha256(&key, b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn cursor_firmado_roundtrip_y_manipulaciones_rechazadas() {
        let s = CursorSigner::default();
        let c = s.sign("2026-10-11T00:00:00.000Z", "id-1", "f");
        assert_eq!(
            s.verify(&c, "f").unwrap(),
            ("2026-10-11T00:00:00.000Z".into(), "id-1".into())
        );
        assert!(s.verify(&c, "otro-filtro").is_err());
        let mut tampered = c.clone();
        tampered.insert(3, 'A');
        assert!(s.verify(&tampered, "f").is_err());
        assert!(
            CursorSigner::default().verify(&c, "f").is_err(),
            "cursor de otro arranque"
        );
        assert!(s.verify("basura", "f").is_err());
    }

    #[test]
    fn tokens_aleatorios_de_64_hex() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    use super::memory::{MemoryApprovals, MemoryCredentials, MemorySecrets};
    use std::sync::atomic::Ordering;

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: "operator".into(),
            correlation_id: "0192aaaa-0000-7000-8000-000000000000".into(),
        }
    }

    fn registry() -> (Arc<MemoryCredentials>, Arc<MemorySecrets>, Principals) {
        let repo = Arc::new(MemoryCredentials::default());
        let secrets = Arc::new(MemorySecrets::default());
        let principals = Principals::new(repo.clone(), secrets.clone());
        (repo, secrets, principals)
    }

    #[tokio::test]
    async fn alta_autentica_y_el_token_no_se_guarda_en_la_referencia() {
        let (repo, secrets, principals) = registry();
        let (p, token) = principals
            .enroll(
                PrincipalKind::Broker,
                "ai-broker",
                vec![ApiScope::Read, ApiScope::LeasesManage],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        let found = principals.authenticate(&token).await.unwrap().unwrap();
        assert_eq!(found, p);
        assert!(principals
            .authenticate("otro-token")
            .await
            .unwrap()
            .is_none());
        let reference = repo.get(&p.id).await.unwrap().unwrap();
        assert_eq!(reference.service, format!("larm/principal/{}", p.id));
        assert!(!serde_json::to_string(&reference).unwrap().contains(&token));
        assert_eq!(
            secrets
                .read(&reference.service, "ai-broker")
                .unwrap()
                .as_deref(),
            Some(token.as_str())
        );
        assert_eq!(repo.events.lock()[0].0, "credential.created");
    }

    #[tokio::test]
    async fn rotar_revoca_el_token_anterior_inmediatamente() {
        let (_, _, principals) = registry();
        let (p, old) = principals
            .enroll(
                PrincipalKind::Operator,
                "operator",
                vec![ApiScope::Admin],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        assert!(principals.authenticate(&old).await.unwrap().is_some());
        let new = principals.rotate(&p.id, &ctx()).await.unwrap();
        assert_ne!(old, new);
        assert!(
            principals.authenticate(&old).await.unwrap().is_none(),
            "el token rotado ya no vale"
        );
        assert!(principals.authenticate(&new).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn revocar_borra_token_y_referencia() {
        let (repo, secrets, principals) = registry();
        let (p, token) = principals
            .enroll(
                PrincipalKind::Operator,
                "temporal",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        principals.revoke(&p.id, &ctx()).await.unwrap();
        assert!(principals.authenticate(&token).await.unwrap().is_none());
        assert!(repo.get(&p.id).await.unwrap().is_none());
        assert!(secrets.entries.lock().is_empty());
        assert_eq!(
            principals.revoke(&p.id, &ctx()).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn rotacion_desde_otro_proceso_revoca_al_caducar_la_cache() {
        let (repo, secrets, principals) = registry();
        let (p, old) = principals
            .enroll(
                PrincipalKind::Operator,
                "operator",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        assert!(principals.authenticate(&old).await.unwrap().is_some());
        // Otro proceso (otro registro sobre el mismo almacén) rota el token.
        let other = Principals::new(repo, secrets);
        let new = other.rotate(&p.id, &ctx()).await.unwrap();
        tokio::time::sleep(PRINCIPAL_CACHE_TTL + Duration::from_millis(200)).await;
        assert!(principals.authenticate(&old).await.unwrap().is_none());
        assert!(principals.authenticate(&new).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn almacen_caido_falla_cerrado_con_503() {
        let (_, secrets, principals) = registry();
        let (_, token) = principals
            .enroll(
                PrincipalKind::Operator,
                "operator",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        principals.invalidate();
        secrets.fail.store(true, Ordering::SeqCst);
        let e = principals.authenticate(&token).await.unwrap_err();
        assert_eq!(
            (e.code.clone(), e.http_status(), e.retryable),
            (ErrorCode::AuthBackendUnavailable, 503, true)
        );
        // y un alta con el almacén caído no deja la referencia
        let e = principals
            .enroll(
                PrincipalKind::Operator,
                "otro",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::AuthBackendUnavailable);
        secrets.fail.store(false, Ordering::SeqCst);
        assert!(principals.find("otro").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn alta_valida_tipo_nombre_y_token_dado() {
        let (_, _, principals) = registry();
        let e = principals
            .enroll(
                PrincipalKind::Broker,
                "ai-broker",
                vec![ApiScope::Admin],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap_err();
        assert_eq!(e.details.reason, "broker_scope_not_allowed");
        let e = principals
            .enroll(
                PrincipalKind::Operator,
                "op",
                vec![ApiScope::Read],
                vec![],
                Some("corto".into()),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert_eq!(e.details.reason, "token_too_short");
        let given = "b".repeat(64);
        let (_, token) = principals
            .enroll(
                PrincipalKind::Operator,
                "op",
                vec![ApiScope::Read],
                vec![],
                Some(given.clone()),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(token, given);
        let e = principals
            .enroll(
                PrincipalKind::Operator,
                "op",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Conflict);
    }

    fn credentials() -> (Arc<MemoryCredentials>, Arc<MemorySecrets>, Credentials) {
        let repo = Arc::new(MemoryCredentials::default());
        let secrets = Arc::new(MemorySecrets::default());
        (repo.clone(), secrets.clone(), Credentials { repo, secrets })
    }

    fn write(service: &str) -> CredentialWrite {
        serde_json::from_value(serde_json::json!({"service": service, "username": "acc",
            "secret_type": "TOKEN", "secret": "hf_supersecreto", "scopes": ["read"]}))
        .unwrap()
    }

    #[tokio::test]
    async fn credencial_guarda_secreto_aparte_y_compensa_fallos() {
        let (repo, secrets, svc) = credentials();
        let r = svc.create(write("larm/source/hf"), &ctx()).await.unwrap();
        assert!(!serde_json::to_string(&r)
            .unwrap()
            .contains("hf_supersecreto"));
        assert_eq!(
            secrets.read("larm/source/hf", "acc").unwrap().as_deref(),
            Some("hf_supersecreto")
        );
        // duplicado: CONFLICT sin sobrescribir el secreto existente
        let mut dup = write("larm/source/hf");
        dup.secret = "otro".into();
        assert_eq!(
            svc.create(dup, &ctx()).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        assert_eq!(
            secrets.read("larm/source/hf", "acc").unwrap().as_deref(),
            Some("hf_supersecreto")
        );
        // la base falla tras guardar el secreto: el secreto se retira
        repo.fail_insert.store(true, Ordering::SeqCst);
        assert!(svc.create(write("larm/source/ms"), &ctx()).await.is_err());
        assert!(secrets.read("larm/source/ms", "acc").unwrap().is_none());
        repo.fail_insert.store(false, Ordering::SeqCst);
        // Credential Manager caído: 503 y nada en la base
        secrets.fail.store(true, Ordering::SeqCst);
        assert_eq!(
            svc.create(write("larm/source/k"), &ctx())
                .await
                .unwrap_err()
                .code,
            ErrorCode::AuthBackendUnavailable
        );
        secrets.fail.store(false, Ordering::SeqCst);
        assert!(repo
            .by_service_prefix("larm/source/k")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn borrar_exige_revision_y_respeta_espacios_reservados() {
        let (repo, secrets, svc) = credentials();
        let r = svc.create(write("larm/source/hf"), &ctx()).await.unwrap();
        assert_eq!(
            svc.delete(&r.id, 7, &ctx()).await.unwrap_err().code,
            ErrorCode::StaleRevision
        );
        svc.delete(&r.id, 1, &ctx()).await.unwrap();
        assert!(secrets.read("larm/source/hf", "acc").unwrap().is_none());
        assert_eq!(svc.get(&r.id).await.unwrap_err().code, ErrorCode::NotFound);
        // un principal no se borra por /credentials
        let principals = Principals::new(repo.clone(), secrets.clone());
        let (p, _) = principals
            .enroll(
                PrincipalKind::Operator,
                "op",
                vec![ApiScope::Read],
                vec![],
                None,
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(
            svc.delete(&p.id, 1, &ctx()).await.unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[tokio::test]
    async fn aprobacion_se_concede_y_consume_una_sola_vez() {
        let svc = Approvals {
            repo: Arc::new(MemoryApprovals::default()),
            ttl: chrono::Duration::seconds(900),
        };
        let digest = "a".repeat(64);
        let request: ApprovalRequest = serde_json::from_value(serde_json::json!({
            "operation_digest": digest, "resource_ids": ["v1"], "code_hash": null,
            "permissions": {}, "approved": true}))
        .unwrap();
        let receipt = svc.grant(&request, &ctx()).await.unwrap();
        assert_eq!(receipt.row_version, 1);
        let ids = vec!["v1".to_string()];
        let usage = ApprovalUse {
            actor_id: "operator",
            operation_digest: &digest,
            resource_ids: &ids,
            code_hash: None,
        };
        svc.consume(&receipt.resource_id, &usage).await.unwrap();
        let again = svc.consume(&receipt.resource_id, &usage).await.unwrap_err();
        assert_eq!(
            (again.code, again.details.reason.as_str()),
            (ErrorCode::ApprovalRequired, "approval_consumed")
        );
        let missing = svc.consume("no-existe", &usage).await.unwrap_err();
        assert_eq!(missing.details.reason, "approval_missing");
    }

    #[tokio::test]
    async fn paginacion_con_cursor_firmado() {
        let (_, _, svc) = credentials();
        for i in 0..5 {
            svc.create(write(&format!("larm/source/s{i}")), &ctx())
                .await
                .unwrap();
        }
        let signer = CursorSigner::default();
        let first = signer.page_credentials(&svc, None, Some(2)).await.unwrap();
        assert_eq!(first.items.len(), 2);
        let second = signer
            .page_credentials(&svc, first.next_cursor.as_deref(), Some(2))
            .await
            .unwrap();
        let third = signer
            .page_credentials(&svc, second.next_cursor.as_deref(), Some(2))
            .await
            .unwrap();
        assert_eq!(third.items.len(), 1);
        assert!(third.next_cursor.is_none());
        let mut ids: Vec<_> = [first.items, second.items, third.items]
            .concat()
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.dedup();
        assert_eq!(ids.len(), 5, "sin repeticiones ni huecos");
        assert!(signer.page_credentials(&svc, None, Some(0)).await.is_err());
        assert!(signer
            .page_credentials(&svc, None, Some(201))
            .await
            .is_err());
        assert!(signer
            .page_credentials(&svc, Some("x.y"), None)
            .await
            .is_err());
    }
}
