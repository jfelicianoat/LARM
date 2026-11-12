//! Invalidación fail-closed de leases de procesos anteriores al arrancar LARM.
//!
//! Una generación nueva no revive `ACTIVE` solo por encontrar su fila en
//! SQLite. Las filas de otra generación expiran antes de montar las rutas del
//! Broker y sus secretos se borran de Credential Manager. Las filas terminales
//! también se recorren para reintentar una revocación que fallara en el arranque
//! anterior.

use larm_domain::{
    bridge::ApiError,
    ports::{MutationContext, RuntimeLeaseRepository, SecretStore},
};

/// Expira generaciones anteriores y revoca sus credenciales de endpoint.
///
/// # Errors
/// Devuelve error si SQLite no puede reconciliar el journal o si Credential
/// Manager no confirma la revocación; el daemon debe abortar el arranque para
/// no aceptar tráfico con una recuperación incompleta.
pub async fn invalidate_previous_generation(
    leases: &dyn RuntimeLeaseRepository,
    secrets: &dyn SecretStore,
    generation: u64,
    ctx: &MutationContext,
) -> Result<usize, ApiError> {
    if generation == 0 {
        return Err(ApiError::new(
            "VALIDATION_FAILED",
            "La generación del daemon debe ser positiva",
            "runtime_generation_invalid",
        ));
    }
    let stale = leases.expire_unreconciled(generation, ctx).await?;
    let count = stale.len();
    for lease in stale {
        let service = format!("larm/lease/{}", lease.id);
        secrets.delete(&service, &lease.owner_id).map_err(|_| {
            ApiError::new(
                "AUTH_BACKEND_UNAVAILABLE",
                "No se pudieron revocar todas las credenciales de leases anteriores",
                "lease_credential_revocation_failed",
            )
            .retryable(true)
            .with_resource(lease.id)
        })?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::{collections::HashMap, sync::Mutex};

    use super::*;
    use larm_db::{runtime_leases::SqlRuntimeLeases, testing::temp_migrated};
    use larm_domain::{
        bridge::{timestamp, Constraints, LeaseState, RuntimeLease},
        ports::{CreateLeaseOutcome, LeaseCreatePersistence, RuntimeLeaseRepository},
    };

    #[derive(Default)]
    struct MemorySecrets {
        entries: Mutex<HashMap<(String, String), String>>,
        fail_delete: Mutex<bool>,
    }

    impl SecretStore for MemorySecrets {
        fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String> {
            self.entries
                .lock()
                .unwrap()
                .insert((service.into(), username.into()), secret.into());
            Ok(())
        }

        fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&(service.into(), username.into()))
                .cloned())
        }

        fn delete(&self, service: &str, username: &str) -> Result<(), String> {
            if *self.fail_delete.lock().unwrap() {
                return Err("fixture failure".into());
            }
            self.entries
                .lock()
                .unwrap()
                .remove(&(service.into(), username.into()));
            Ok(())
        }
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "larm:daemon-recovery".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn lease(owner: &str, generation: u64) -> RuntimeLease {
        let now = timestamp(chrono::Utc::now());
        let id = uuid::Uuid::now_v7().to_string();
        let mut metadata = serde_json::Map::new();
        metadata.insert("job_id".into(), serde_json::Value::String(id.clone()));
        RuntimeLease {
            id,
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            owner_id: owner.into(),
            request_id: uuid::Uuid::now_v7().to_string(),
            instance_id: None,
            generation,
            state: LeaseState::Requested,
            expires_at: None,
            endpoint_id: None,
            constraints: Constraints {
                runtime_version_id: None,
                backend_id: None,
                execution_fingerprint: None,
                allow_backend_fallback: false,
                max_memory_bytes: None,
            },
        }
    }

    async fn insert(repo: &SqlRuntimeLeases, record: &RuntimeLease, ctx: &MutationContext) {
        let outcome = repo
            .create_requested(
                record,
                &"a".repeat(64),
                &format!("key-{}", record.id),
                &CreateLeaseOutcome {
                    status: 202,
                    body: serde_json::json!({
                        "job_id": record.id,
                        "status_url": format!("/api/v1/jobs/{}", record.id),
                        "state": "QUEUED"
                    }),
                },
                ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, LeaseCreatePersistence::Created(_)));
    }

    #[tokio::test]
    async fn startup_expires_old_generation_revokes_only_stale_secrets_and_is_repeatable() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let repo = SqlRuntimeLeases::new(db.pool);
        let secrets = MemorySecrets::default();
        let ctx = context();
        let stale = lease(&ctx.actor_id, 7);
        let current = lease(&ctx.actor_id, 8);
        insert(&repo, &stale, &ctx).await;
        insert(&repo, &current, &ctx).await;
        secrets
            .store(
                &format!("larm/lease/{}", stale.id),
                &stale.owner_id,
                "stale",
            )
            .unwrap();
        secrets
            .store(
                &format!("larm/lease/{}", current.id),
                &current.owner_id,
                "current",
            )
            .unwrap();

        assert_eq!(
            invalidate_previous_generation(&repo, &secrets, 8, &ctx)
                .await
                .unwrap(),
            1
        );
        assert!(secrets
            .read(&format!("larm/lease/{}", stale.id), &stale.owner_id)
            .unwrap()
            .is_none());
        assert_eq!(
            secrets
                .read(&format!("larm/lease/{}", current.id), &current.owner_id)
                .unwrap()
                .as_deref(),
            Some("current")
        );
        assert_eq!(
            repo.get_for_owner(&stale.id, &stale.owner_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            LeaseState::Expired
        );
        assert_eq!(
            repo.get_for_owner(&current.id, &current.owner_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            LeaseState::Requested
        );

        assert_eq!(
            invalidate_previous_generation(&repo, &secrets, 9, &ctx)
                .await
                .unwrap(),
            2
        );
        assert!(secrets
            .read(&format!("larm/lease/{}", current.id), &current.owner_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn fallo_del_almacen_de_secretos_falla_cerrado_y_se_reintenta_al_reinicio() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let repo = SqlRuntimeLeases::new(db.pool);
        let secrets = MemorySecrets::default();
        let ctx = context();
        let stale = lease(&ctx.actor_id, 7);
        insert(&repo, &stale, &ctx).await;
        secrets
            .store(
                &format!("larm/lease/{}", stale.id),
                &stale.owner_id,
                "stale",
            )
            .unwrap();
        *secrets.fail_delete.lock().unwrap() = true;

        let error = invalidate_previous_generation(&repo, &secrets, 8, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, "AUTH_BACKEND_UNAVAILABLE");
        assert_eq!(error.details.reason, "lease_credential_revocation_failed");
        assert!(error.to_string().find("stale").is_none());
        assert_eq!(
            repo.get_for_owner(&stale.id, &stale.owner_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            LeaseState::Expired
        );

        *secrets.fail_delete.lock().unwrap() = false;
        assert_eq!(
            invalidate_previous_generation(&repo, &secrets, 9, &ctx)
                .await
                .unwrap(),
            1
        );
        assert!(secrets
            .read(&format!("larm/lease/{}", stale.id), &stale.owner_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn rechaza_generacion_cero_sin_consultar_sqlite() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let repo = SqlRuntimeLeases::new(db.pool);
        let error = invalidate_previous_generation(&repo, &MemorySecrets::default(), 0, &context())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "runtime_generation_invalid");
    }
}
