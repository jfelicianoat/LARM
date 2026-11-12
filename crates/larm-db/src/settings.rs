//! Settings singleton with optimistic concurrency and transactional outbox (P003).

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, SettingsRepository, SettingsSnapshot};
use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};

use crate::events::{append, NewEvent};
use crate::pool::db_error;

const SETTINGS_ID: &str = "global";

#[derive(Clone)]
pub struct SqlSettings {
    pub pool: SqlitePool,
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE settings SET revision=revision WHERE id=?")
        .bind(SETTINGS_ID)
        .execute(conn)
        .await
        .map_err(|error| db_error("bloqueando Settings", &error))?;
    Ok(())
}

fn decode(payload: String, revision: i64) -> Result<SettingsSnapshot, LarmError> {
    let payload = serde_json::from_str(&payload)
        .map_err(|error| db_error("decodificando Settings", &error))?;
    Ok(SettingsSnapshot { payload, revision })
}

#[async_trait]
impl SettingsRepository for SqlSettings {
    async fn get_or_initialize(
        &self,
        initial: serde_json::Value,
    ) -> Result<SettingsSnapshot, LarmError> {
        let encoded = serde_json::to_string(&initial)
            .map_err(|error| db_error("serializando Settings iniciales", &error))?;
        sqlx::query("INSERT OR IGNORE INTO settings(id,revision,payload) VALUES (?,1,?)")
            .bind(SETTINGS_ID)
            .bind(encoded)
            .execute(&self.pool)
            .await
            .map_err(|error| db_error("inicializando Settings", &error))?;
        let (payload, revision): (String, i64) =
            sqlx::query_as("SELECT payload,revision FROM settings WHERE id=?")
                .bind(SETTINGS_ID)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| db_error("leyendo Settings", &error))?;
        decode(payload, revision)
    }

    async fn update(
        &self,
        expected_revision: i64,
        payload: serde_json::Value,
        ctx: &MutationContext,
    ) -> Result<SettingsSnapshot, LarmError> {
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| db_error("serializando Settings", &error))?;
        let digest = hex::encode(Sha256::digest(encoded.as_bytes()));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN Settings", &error))?;
        lock(&mut tx).await?;
        let current_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM settings WHERE id=?")
                .bind(SETTINGS_ID)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| db_error("leyendo revisión Settings", &error))?;
        let Some(current_revision) = current_revision else {
            return Err(LarmError::new(
                ErrorCode::Internal,
                "Settings aún no está inicializado",
                "settings_missing",
            ));
        };
        if current_revision != expected_revision {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "Settings cambió desde la última lectura",
                "stale_revision",
            ));
        }
        let revision = current_revision.checked_add(1).ok_or_else(|| {
            LarmError::new(
                ErrorCode::Internal,
                "Se agotó el contador de revisión de Settings",
                "settings_revision_overflow",
            )
        })?;
        let changed =
            sqlx::query("UPDATE settings SET revision=?,payload=? WHERE id=? AND revision=?")
                .bind(revision)
                .bind(encoded)
                .bind(SETTINGS_ID)
                .bind(current_revision)
                .execute(&mut *tx)
                .await
                .map_err(|error| db_error("actualizando Settings", &error))?;
        if changed.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "Settings cambió desde la última lectura",
                "stale_revision",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "settings.changed",
                resource_type: "settings",
                resource_id: SETTINGS_ID,
                resource_version: revision,
                data: serde_json::json!({
                    "revision": revision,
                    "settings_hash": digest,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT Settings", &error))?;
        Ok(SettingsSnapshot { payload, revision })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::testing::temp_migrated;

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "operator".into(),
            correlation_id: "corr".into(),
        }
    }

    #[tokio::test]
    async fn initializes_persists_cas_and_writes_outbox_atomically() {
        let (_dir, db) = temp_migrated().await.unwrap();
        let repo = SqlSettings {
            pool: db.pool.clone(),
        };
        let initial =
            serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":null}});
        let first = repo.get_or_initialize(initial.clone()).await.unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.payload, initial);

        let changed =
            serde_json::json!({"storage":{"download_bandwidth_limit_bytes_per_second":"1024"}});
        let second = repo.update(1, changed.clone(), &context()).await.unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.payload, changed);
        assert_eq!(repo.get_or_initialize(initial).await.unwrap(), second);

        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event WHERE event_type='settings.changed' AND resource_version=2",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let outbox: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.event_type='settings.changed'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
        assert_eq!(outbox, 1);
    }

    #[tokio::test]
    async fn stale_revision_does_not_write_settings_or_event() {
        let (_dir, db) = temp_migrated().await.unwrap();
        let repo = SqlSettings {
            pool: db.pool.clone(),
        };
        repo.get_or_initialize(serde_json::json!({"v":1}))
            .await
            .unwrap();
        repo.update(1, serde_json::json!({"v":2}), &context())
            .await
            .unwrap();
        let error = repo
            .update(1, serde_json::json!({"v":3}), &context())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "stale_revision");
        let current: String = sqlx::query_scalar("SELECT payload FROM settings WHERE id='global'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(current, "{\"v\":2}");
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM event WHERE event_type='settings.changed'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(events, 1);
    }
}
