//! Formas públicas P028 para estado y sincronización del registro firmado.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::ApiError;
use crate::ports::MutationContext;

/// Cuerpo vacío cerrado: la URL y las claves de firma proceden de Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySyncRequest {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RegistryRevocationTargetType {
    PackageSha256,
    PublisherKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryRevocationRecord {
    pub registry_id: String,
    pub target_type: RegistryRevocationTargetType,
    pub target: String,
    pub revoked_at: String,
    pub reason_code: String,
    pub source_sequence: u64,
}

/// Checkpoint confirmado. `envelope` conserva el índice firmado exacto que
/// permitió reconstruir el catálogo tras reiniciar el daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCheckpoint {
    pub registry_id: String,
    pub registry_url: String,
    pub sequence: u64,
    pub payload_sha256: String,
    pub issued_at: String,
    pub expires_at: String,
    pub accepted_at: String,
    pub envelope: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySyncReceipt {
    pub registry_id: String,
    pub sequence: u64,
    pub payload_sha256: String,
    pub accepted_at: String,
    pub expires_at: String,
    pub package_count: u64,
    pub revocation_count: u64,
    pub replay: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryStatus {
    pub enabled: bool,
    pub registry_url: Option<String>,
    pub checkpoint: Option<RegistryCheckpoint>,
    pub revocations: Vec<RegistryRevocationRecord>,
}

#[async_trait]
pub trait AdapterRegistryManager: Send + Sync {
    async fn status(&self) -> Result<RegistryStatus, ApiError>;
    async fn sync(&self, ctx: &MutationContext) -> Result<RegistrySyncReceipt, ApiError>;
}
