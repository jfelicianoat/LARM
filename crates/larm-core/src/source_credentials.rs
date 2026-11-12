//! Escritura de credenciales Basic para fuentes P141.

use larm_domain::credential::{CredentialReference, SecretType, BACKEND};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    CredentialRepository, MutationContext, SecretStore, SourceAccountRepository,
    SourceBasicCredentialWriter, SourceCatalogRepository,
};
use larm_domain::source_credentials::BasicCredentialWrite;
use larm_domain::sources::AuthMode;
use std::sync::Arc;

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "La configuración de credenciales Basic no es válida",
        reason,
    )
}

fn store_unavailable() -> LarmError {
    LarmError::new(
        ErrorCode::AuthBackendUnavailable,
        "El almacén de credenciales no está disponible",
        "credential_backend_unavailable",
    )
    .retryable(true)
}

/// Guarda el usuario y la contraseña de Basic como un secreto opaco en el
/// almacén del sistema. La base solo recibe CredentialReference.
pub struct SourceBasicCredentials {
    pub sources: Arc<dyn SourceCatalogRepository>,
    pub accounts: Arc<dyn SourceAccountRepository>,
    pub credentials: Arc<dyn CredentialRepository>,
    pub secrets: Arc<dyn SecretStore>,
    write_lock: tokio::sync::Mutex<()>,
}

impl SourceBasicCredentials {
    #[must_use]
    pub fn new(
        sources: Arc<dyn SourceCatalogRepository>,
        accounts: Arc<dyn SourceAccountRepository>,
        credentials: Arc<dyn CredentialRepository>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            sources,
            accounts,
            credentials,
            secrets,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn committed_after_error(
        &self,
        account_id: &str,
        reference: &CredentialReference,
    ) -> Option<CredentialReference> {
        let account = self.accounts.source_account(account_id).await.ok()??;
        if account.credential_reference_id.as_deref() != Some(reference.id.as_str()) {
            return None;
        }
        let current = self.credentials.get(&reference.id).await.ok()??;
        (current.service == reference.service
            && current.username == reference.username
            && current.secret_type == SecretType::Password
            && current.row_version >= reference.row_version)
            .then_some(current)
    }

    fn restore_secret(
        &self,
        service: &str,
        username: &str,
        previous: Option<&str>,
    ) -> Result<(), String> {
        match previous {
            Some(value) => self.secrets.store(service, username, value),
            None => self.secrets.delete(service, username),
        }
    }
}

#[async_trait::async_trait]
impl SourceBasicCredentialWriter for SourceBasicCredentials {
    async fn write_basic_credential(
        &self,
        source_id: &str,
        request: &BasicCredentialWrite,
        ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        request.validate()?;
        if !larm_domain::bridge::is_uuid_v7(source_id) || request.source_id != source_id {
            return Err(invalid("basic_credential_source_mismatch"));
        }

        let _guard = self.write_lock.lock().await;
        let source = self.sources.source(source_id).await?.ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Fuente desconocida", "not_found")
                .with_resource(source_id)
        })?;
        if source.auth_mode != AuthMode::Basic {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "La fuente no admite autenticación Basic",
                "unsupported_auth",
            )
            .with_resource(source_id));
        }
        let base = url::Url::parse(&source.base_url)
            .map_err(|_| invalid("basic_credential_source_url_invalid"))?;
        if base.scheme() != "https" {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "Las credenciales Basic requieren una fuente HTTPS",
                "basic_requires_https",
            )
            .with_resource(source_id));
        }
        if source.account_id.as_deref() != Some(request.account_id.as_str()) {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La cuenta no está seleccionada en esta fuente",
                "source_account_mismatch",
            )
            .with_resource(&request.account_id));
        }

        let account = self
            .accounts
            .source_account(&request.account_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Cuenta de fuente desconocida",
                    "not_found",
                )
                .with_resource(&request.account_id)
            })?;
        if account.source_id != source_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La cuenta pertenece a otra fuente",
                "source_account_mismatch",
            )
            .with_resource(&request.account_id));
        }

        let service = format!("larm/source/{source_id}");
        let reference = if let Some(reference_id) = &account.credential_reference_id {
            let current = self.credentials.get(reference_id).await?.ok_or_else(|| {
                LarmError::new(
                    ErrorCode::AuthBackendUnavailable,
                    "La referencia de credencial de la cuenta no está disponible",
                    "source_credential_reference_missing",
                )
            })?;
            if current.service != service || current.username != request.account_id {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "La credencial vinculada no pertenece a esta cuenta",
                    "source_credential_scope_mismatch",
                )
                .with_resource(reference_id));
            }
            CredentialReference {
                updated_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
                row_version: current.row_version.checked_add(1).ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::Conflict,
                        "La referencia de credencial alcanzó su límite de revisiones",
                        "credential_version_exhausted",
                    )
                })?,
                secret_type: SecretType::Password,
                scopes: vec![],
                expires_at: None,
                ..current
            }
        } else {
            if self
                .credentials
                .by_service_prefix(&service)
                .await?
                .iter()
                .any(|candidate| {
                    candidate.service == service && candidate.username == request.account_id
                })
            {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya existe una credencial sin vincular para esta cuenta",
                    "credential_exists",
                )
                .with_resource(&request.account_id));
            }
            let now = larm_domain::bridge::timestamp(chrono::Utc::now());
            CredentialReference {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: now.clone(),
                updated_at: now,
                row_version: 1,
                metadata: serde_json::Map::new(),
                service: service.clone(),
                username: request.account_id.clone(),
                backend: BACKEND.into(),
                secret_type: SecretType::Password,
                scopes: vec![],
                expires_at: None,
            }
        };

        let previous = self
            .secrets
            .read(&service, &request.account_id)
            .map_err(|_| store_unavailable())?;
        let secret = serde_json::to_string(&serde_json::json!({
            "username": request.username,
            "password": request.password,
        }))
        .map_err(|_| invalid("basic_credential_serialize_failed"))?;
        self.secrets
            .store(&service, &request.account_id, &secret)
            .map_err(|_| store_unavailable())?;

        match self
            .accounts
            .set_basic_credential(&request.account_id, &reference, ctx)
            .await
        {
            Ok(saved) => Ok(saved),
            Err(error) => {
                if let Some(saved) = self
                    .committed_after_error(&request.account_id, &reference)
                    .await
                {
                    return Ok(saved);
                }
                if self
                    .restore_secret(&service, &request.account_id, previous.as_deref())
                    .is_err()
                {
                    tracing::error!(credential_reference_id = %reference.id, "falló la compensación de una escritura de credencial Basic");
                    return Err(store_unavailable());
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::security::memory::{MemoryCredentials, MemorySecrets};
    use async_trait::async_trait;
    use larm_domain::ports::{SourceAccountRepository, SourceCatalogRepository};
    use larm_domain::source_accounts::{
        CheckSourceLicense, ModelSourceAccount, SourceLicenseReceipt,
    };
    use larm_domain::sources::{ModelSource, ModelSourceAdapter, ModelSourceCreate, SourceFilter};
    use parking_lot::Mutex;

    const SOURCE_ID: &str = "0192aaaa-0000-7000-8000-000000000001";
    const ACCOUNT_ID: &str = "0192aaaa-0000-7000-8000-000000000002";

    fn error() -> LarmError {
        LarmError::new(
            ErrorCode::UnsupportedCapability,
            "No implementado en el fixture",
            "fixture_unsupported",
        )
    }

    fn fixture_source(auth_mode: AuthMode, base_url: &str) -> ModelSource {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        ModelSource {
            id: SOURCE_ID.into(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: "Fuente Basic de prueba".into(),
            adapter_id: "0192aaaa-0000-7000-8000-000000000003".into(),
            base_url: base_url.into(),
            priority: 0,
            enabled: true,
            auth_mode,
            account_id: Some(ACCOUNT_ID.into()),
            policy: serde_json::Map::new(),
            operational: Default::default(),
        }
    }

    fn fixture_account() -> ModelSourceAccount {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        ModelSourceAccount {
            id: ACCOUNT_ID.into(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            source_id: SOURCE_ID.into(),
            credential_reference_id: None,
            display_name: "Cuenta técnica".into(),
            accepted_license_hashes: vec![],
        }
    }

    struct Sources(ModelSource);

    #[async_trait]
    impl SourceCatalogRepository for Sources {
        async fn source_replay(
            &self,
            _: &str,
            _: &str,
            _: &MutationContext,
        ) -> Result<Option<ModelSource>, LarmError> {
            Ok(None)
        }

        async fn register_source_adapter(
            &self,
            _: &str,
            _: &str,
            _: &serde_json::Map<String, serde_json::Value>,
            _: &MutationContext,
        ) -> Result<ModelSourceAdapter, LarmError> {
            Err(error())
        }

        async fn source_adapter(&self, _: &str) -> Result<Option<ModelSourceAdapter>, LarmError> {
            Ok(None)
        }

        async fn create_source(
            &self,
            _: &ModelSourceCreate,
            _: &str,
            _: &str,
            _: &MutationContext,
        ) -> Result<ModelSource, LarmError> {
            Err(error())
        }

        async fn source(&self, id: &str) -> Result<Option<ModelSource>, LarmError> {
            Ok((self.0.id == id).then(|| self.0.clone()))
        }

        async fn page_sources(
            &self,
            _: Option<(String, String)>,
            _: u32,
            _: &SourceFilter,
        ) -> Result<Vec<ModelSource>, LarmError> {
            Ok(vec![self.0.clone()])
        }

        async fn update_source(
            &self,
            _: &str,
            _: i64,
            _: &ModelSourceCreate,
            _: &MutationContext,
        ) -> Result<ModelSource, LarmError> {
            Err(error())
        }

        async fn delete_source(
            &self,
            _: &str,
            _: i64,
            _: &MutationContext,
        ) -> Result<(), LarmError> {
            Ok(())
        }
    }

    struct Accounts {
        account: Mutex<ModelSourceAccount>,
        credentials: Arc<MemoryCredentials>,
        fail_write: bool,
    }

    #[async_trait]
    impl SourceAccountRepository for Accounts {
        async fn create_source_account(
            &self,
            account: &ModelSourceAccount,
            _: &MutationContext,
        ) -> Result<ModelSourceAccount, LarmError> {
            Ok(account.clone())
        }

        async fn source_account(&self, id: &str) -> Result<Option<ModelSourceAccount>, LarmError> {
            let account = self.account.lock().clone();
            Ok((account.id == id).then_some(account))
        }

        async fn page_source_accounts(
            &self,
            _: Option<(String, String)>,
            _: u32,
        ) -> Result<Vec<ModelSourceAccount>, LarmError> {
            Ok(vec![self.account.lock().clone()])
        }

        async fn set_basic_credential(
            &self,
            account_id: &str,
            credential: &CredentialReference,
            ctx: &MutationContext,
        ) -> Result<CredentialReference, LarmError> {
            if self.fail_write {
                return Err(LarmError::new(
                    ErrorCode::Internal,
                    "Fallo inyectado",
                    "database_error",
                ));
            }
            if self.account.lock().credential_reference_id.is_some() {
                return Err(error());
            }
            self.credentials.insert(credential, ctx).await?;
            let mut account = self.account.lock();
            if account.id != account_id {
                return Err(LarmError::new(
                    ErrorCode::NotFound,
                    "Cuenta desconocida",
                    "not_found",
                ));
            }
            account.credential_reference_id = Some(credential.id.clone());
            account.row_version += 1;
            Ok(credential.clone())
        }

        async fn set_source_license(
            &self,
            _: &str,
            _: &CheckSourceLicense,
            _: &str,
            _: &str,
            _: &MutationContext,
        ) -> Result<SourceLicenseReceipt, LarmError> {
            Err(error())
        }

        async fn delete_source_account(
            &self,
            _: &str,
            _: &MutationContext,
        ) -> Result<(), LarmError> {
            Ok(())
        }
    }

    fn request() -> BasicCredentialWrite {
        BasicCredentialWrite {
            source_id: SOURCE_ID.into(),
            account_id: ACCOUNT_ID.into(),
            username: "private-user-sentinel".into(),
            password: "private-password-sentinel".into(),
        }
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn service(
        source: ModelSource,
        fail_write: bool,
    ) -> (
        SourceBasicCredentials,
        Arc<MemorySecrets>,
        Arc<MemoryCredentials>,
    ) {
        let secrets = Arc::new(MemorySecrets::default());
        let credentials = Arc::new(MemoryCredentials::default());
        let accounts = Arc::new(Accounts {
            account: Mutex::new(fixture_account()),
            credentials: credentials.clone(),
            fail_write,
        });
        (
            SourceBasicCredentials::new(
                Arc::new(Sources(source)),
                accounts,
                credentials.clone(),
                secrets.clone(),
            ),
            secrets,
            credentials,
        )
    }

    #[tokio::test]
    async fn guarda_basic_en_secreto_protegido_y_sql_solo_recibe_referencia() {
        let (service, secrets, credentials) = service(
            fixture_source(AuthMode::Basic, "https://private.example/api"),
            false,
        );
        let reference = service
            .write_basic_credential(SOURCE_ID, &request(), &context())
            .await
            .unwrap();
        assert_eq!(reference.secret_type, SecretType::Password);
        assert_eq!(reference.username, ACCOUNT_ID);
        let secret = secrets
            .read(&reference.service, ACCOUNT_ID)
            .unwrap()
            .unwrap();
        let secret: serde_json::Value = serde_json::from_str(&secret).unwrap();
        assert_eq!(secret["username"], "private-user-sentinel");
        assert_eq!(secret["password"], "private-password-sentinel");
        let stored = credentials.get(&reference.id).await.unwrap().unwrap();
        let public = serde_json::to_string(&stored).unwrap();
        assert!(!public.contains("private-user-sentinel"));
        assert!(!public.contains("private-password-sentinel"));
    }

    #[tokio::test]
    async fn restaura_el_almacen_si_falla_el_vinculo_sqlite() {
        let (service, secrets, credentials) = service(
            fixture_source(AuthMode::Basic, "https://private.example/api"),
            true,
        );
        assert!(service
            .write_basic_credential(SOURCE_ID, &request(), &context())
            .await
            .is_err());
        assert!(secrets
            .read(
                "larm/source/0192aaaa-0000-7000-8000-000000000001",
                ACCOUNT_ID
            )
            .unwrap()
            .is_none());
        assert!(credentials
            .get("0192aaaa-0000-7000-8000-000000000004")
            .await
            .unwrap()
            .is_none());
        assert!(credentials.rows.lock().is_empty());
    }

    #[tokio::test]
    async fn rechaza_http_y_cuenta_no_seleccionada_antes_de_guardar() {
        let (writer, secrets, _) = service(
            fixture_source(AuthMode::Basic, "http://private.example/api"),
            false,
        );
        let error = writer
            .write_basic_credential(SOURCE_ID, &request(), &context())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "basic_requires_https");
        assert!(secrets.entries.lock().is_empty());

        let (writer, secrets, _) = service(
            fixture_source(AuthMode::None, "https://private.example/api"),
            false,
        );
        assert!(writer
            .write_basic_credential(SOURCE_ID, &request(), &context())
            .await
            .is_err());
        assert!(secrets.entries.lock().is_empty());
    }
}
