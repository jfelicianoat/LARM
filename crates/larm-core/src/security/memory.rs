//! Implementaciones en memoria de los puertos de seguridad para pruebas de
//! casos de uso y de la API sin Credential Manager ni SQLite. La conducta
//! (conflictos, If-Match, consumo único, fallos del almacén) imita la de
//! producción; las pruebas contra SQLite y Credential Manager reales están en
//! `larm-db` y en las pruebas reales de P009.

use async_trait::async_trait;
use larm_domain::approval::ApprovalRecord;
use larm_domain::credential::CredentialReference;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    ApprovalRepository, CredentialRepository, IdempotencyStore, MutationContext, SecretStore,
    StoredResponse,
};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Secretos en memoria; `fail` simula Credential Manager caído.
#[derive(Default)]
pub struct MemorySecrets {
    pub entries: Mutex<HashMap<(String, String), String>>,
    pub fail: AtomicBool,
}

impl SecretStore for MemorySecrets {
    fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("credential manager no disponible".into());
        }
        self.entries
            .lock()
            .insert((service.into(), username.into()), secret.into());
        Ok(())
    }

    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("credential manager no disponible".into());
        }
        Ok(self
            .entries
            .lock()
            .get(&(service.into(), username.into()))
            .cloned())
    }

    fn delete(&self, service: &str, username: &str) -> Result<(), String> {
        self.entries
            .lock()
            .remove(&(service.to_string(), username.to_string()));
        Ok(())
    }
}

/// `credential_reference` en memoria; `fail_insert` simula un fallo de la base.
#[derive(Default)]
pub struct MemoryCredentials {
    pub rows: Mutex<BTreeMap<String, CredentialReference>>,
    pub events: Mutex<Vec<(String, MutationContext)>>,
    pub fail_insert: AtomicBool,
    sequence: AtomicU64,
}

impl MemoryCredentials {
    fn event(&self, kind: &str, ctx: &MutationContext) -> String {
        self.events.lock().push((kind.into(), ctx.clone()));
        (self.sequence.fetch_add(1, Ordering::SeqCst) + 1).to_string()
    }
}

#[async_trait]
impl CredentialRepository for MemoryCredentials {
    async fn insert(
        &self,
        c: &CredentialReference,
        ctx: &MutationContext,
    ) -> Result<String, LarmError> {
        if self.fail_insert.load(Ordering::SeqCst) {
            return Err(LarmError::new(
                ErrorCode::Internal,
                "Fallo de base de datos",
                "database_error",
            ));
        }
        {
            let mut rows = self.rows.lock();
            if rows
                .values()
                .any(|r| r.service == c.service && r.username == c.username)
            {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya existe",
                    "credential_exists",
                ));
            }
            rows.insert(c.id.clone(), c.clone());
        }
        Ok(self.event("credential.created", ctx))
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialReference>, LarmError> {
        Ok(self.rows.lock().get(id).cloned())
    }

    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<CredentialReference>, LarmError> {
        let mut all: Vec<_> = self.rows.lock().values().cloned().collect();
        all.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        Ok(all
            .into_iter()
            .filter(|r| {
                after
                    .as_ref()
                    .is_none_or(|(c, i)| (&r.created_at, &r.id) > (c, i))
            })
            .take(limit as usize)
            .collect())
    }

    async fn by_service_prefix(&self, prefix: &str) -> Result<Vec<CredentialReference>, LarmError> {
        Ok(self
            .rows
            .lock()
            .values()
            .filter(|r| r.service.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<String, LarmError> {
        {
            let mut rows = self.rows.lock();
            match rows.get(id) {
                None => {
                    return Err(LarmError::new(
                        ErrorCode::NotFound,
                        "Credencial desconocida",
                        "not_found",
                    ))
                }
                Some(r) if r.row_version != if_match => {
                    return Err(LarmError::new(
                        ErrorCode::StaleRevision,
                        "Cambió",
                        "row_version_mismatch",
                    ))
                }
                Some(_) => {
                    rows.remove(id);
                }
            }
        }
        Ok(self.event("credential.deleted", ctx))
    }

    async fn touch(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<CredentialReference, LarmError> {
        let updated = {
            let mut rows = self.rows.lock();
            let r = rows.get_mut(id).ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Credencial desconocida", "not_found")
            })?;
            r.row_version += 1;
            r.clone()
        };
        self.event("credential.rotated", ctx);
        Ok(updated)
    }
}

/// Aprobaciones en memoria con consumo único.
#[derive(Default)]
pub struct MemoryApprovals {
    pub rows: Mutex<HashMap<String, ApprovalRecord>>,
    sequence: AtomicU64,
}

#[async_trait]
impl ApprovalRepository for MemoryApprovals {
    async fn insert(
        &self,
        a: &ApprovalRecord,
        _ctx: &MutationContext,
    ) -> Result<String, LarmError> {
        self.rows.lock().insert(a.id.clone(), a.clone());
        Ok((self.sequence.fetch_add(1, Ordering::SeqCst) + 1).to_string())
    }

    async fn get(&self, id: &str) -> Result<Option<ApprovalRecord>, LarmError> {
        Ok(self.rows.lock().get(id).cloned())
    }

    async fn consume(&self, id: &str, at: &str) -> Result<bool, LarmError> {
        let mut rows = self.rows.lock();
        match rows.get_mut(id) {
            Some(r) if r.consumed_at.is_none() => {
                r.consumed_at = Some(at.to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// Idempotencia en memoria (sin caducidad: las pruebas son cortas).
#[derive(Default)]
pub struct MemoryIdempotency {
    pub rows: Mutex<HashMap<(String, String, String), StoredResponse>>,
}

#[async_trait]
impl IdempotencyStore for MemoryIdempotency {
    async fn lookup(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
    ) -> Result<Option<StoredResponse>, LarmError> {
        Ok(self
            .rows
            .lock()
            .get(&(principal_id.into(), operation.into(), key.into()))
            .cloned())
    }

    async fn save(
        &self,
        principal_id: &str,
        operation: &str,
        key: &str,
        stored: &StoredResponse,
        _expires_at: &str,
    ) -> Result<(), LarmError> {
        self.rows.lock().insert(
            (principal_id.into(), operation.into(), key.into()),
            stored.clone(),
        );
        Ok(())
    }
}
