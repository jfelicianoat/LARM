//! Casos de uso P045 para la administración de cuentas de fuentes.

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, SourceAccountRepository};
use larm_domain::source_accounts::{
    CheckSourceLicense, ModelSourceAccount, ModelSourceAccountCreate,
};
use std::sync::Arc;

pub struct SourceAccounts {
    pub repo: Arc<dyn SourceAccountRepository>,
}

impl SourceAccounts {
    pub async fn create(
        &self,
        request: &ModelSourceAccountCreate,
        ctx: &MutationContext,
    ) -> Result<ModelSourceAccount, LarmError> {
        request.validate()?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let account = ModelSourceAccount {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: serde_json::Map::new(),
            source_id: request.source_id.clone(),
            credential_reference_id: request.credential_reference_id.clone(),
            display_name: request.display_name.clone(),
            accepted_license_hashes: request.accepted_license_hashes.clone(),
        };
        account.validate()?;
        self.repo.create_source_account(&account, ctx).await
    }

    pub async fn get(&self, id: &str) -> Result<ModelSourceAccount, LarmError> {
        self.repo.source_account(id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Cuenta de fuente desconocida",
                "not_found",
            )
            .with_resource(id)
        })
    }

    pub async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<ModelSourceAccount>, LarmError> {
        self.repo.page_source_accounts(after, limit).await
    }

    pub async fn delete(&self, id: &str, ctx: &MutationContext) -> Result<(), LarmError> {
        self.repo.delete_source_account(id, ctx).await
    }

    pub async fn check_license(
        &self,
        source_id: &str,
        request: &CheckSourceLicense,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<crate::security::CommandReceipt, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(source_id) {
            return Err(larm_domain::source_accounts::account_invalid("source_id"));
        }
        request.validate()?;
        let receipt = self
            .repo
            .set_source_license(source_id, request, idempotency_key, body_hash, ctx)
            .await?;
        Ok(crate::security::CommandReceipt {
            resource_id: receipt.resource_id,
            row_version: receipt.row_version,
            event_id: receipt.event_id,
        })
    }
}
