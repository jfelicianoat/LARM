//! Persistencia versionada de CUSTOM_RULES (P027).

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::{
    bridge::is_uuid_v7,
    custom_rules::{CustomRules, CustomRulesRepository, VersionedCustomRules},
    error::{ErrorCode, LarmError},
    ports::MutationContext,
};
use sqlx::{Row, SqliteConnection, SqlitePool};

const SETTINGS_ID: &str = "custom_rules";

#[derive(Clone)]
pub struct SqlCustomRules {
    pub pool: SqlitePool,
}

fn invalid() -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Las reglas personalizadas no son válidas",
        "custom_rules_invalid",
    )
}

fn stale() -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "La configuración cambió desde que se leyó",
        "stale_revision",
    )
}

fn corrupt() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "La configuración de reglas guardada está dañada",
        "custom_rules_corrupt",
    )
}

async fn read(conn: &mut SqliteConnection) -> Result<VersionedCustomRules, LarmError> {
    let row = sqlx::query("SELECT revision,payload FROM settings WHERE id=?")
        .bind(SETTINGS_ID)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|error| db_error("leyendo custom_rules", &error))?;
    let Some(row) = row else {
        return Ok(VersionedCustomRules {
            value: CustomRules::default(),
            revision: 0,
        });
    };

    let revision: i64 = row.try_get("revision").map_err(|_| corrupt())?;
    let payload: String = row.try_get("payload").map_err(|_| corrupt())?;
    let value: CustomRules = serde_json::from_str(&payload).map_err(|_| corrupt())?;
    if revision < 0 || value.validate().is_err() {
        return Err(corrupt());
    }
    Ok(VersionedCustomRules { value, revision })
}

#[async_trait]
impl CustomRulesRepository for SqlCustomRules {
    async fn get(&self) -> Result<VersionedCustomRules, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| db_error("abriendo lectura de custom_rules", &error))?;
        read(&mut conn).await
    }

    async fn replace(
        &self,
        expected_revision: i64,
        value: &CustomRules,
        ctx: &MutationContext,
    ) -> Result<VersionedCustomRules, LarmError> {
        if expected_revision < 0 || value.validate().is_err() {
            return Err(invalid());
        }
        if ctx.actor_id.trim().is_empty() || !is_uuid_v7(&ctx.correlation_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Contexto de cambio de reglas no válido",
                "custom_rules_actor_invalid",
            ));
        }
        let payload = serde_json::to_string(value).map_err(|_| invalid())?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando reemplazo de custom_rules", &error))?;

        // Reserva el escritor antes de leer revisión y payload. Así, dos PATCH
        // concurrentes no pueden confirmar una revisión basada en la misma fila.
        sqlx::query("UPDATE settings SET revision=revision WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("reservando escritura de custom_rules", &error))?;
        let current = read(&mut tx).await?;
        if current.revision != expected_revision {
            return Err(stale());
        }
        let revision = expected_revision.checked_add(1).ok_or_else(|| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "La revisión de reglas agotó su rango",
                "custom_rules_revision_overflow",
            )
        })?;
        let result = sqlx::query(
            "INSERT INTO settings(id,revision,payload) VALUES(?,?,?) \
             ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload \
             WHERE settings.revision=?",
        )
        .bind(SETTINGS_ID)
        .bind(revision)
        .bind(payload)
        .bind(expected_revision)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("guardando custom_rules", &error))?;
        if result.rows_affected() != 1 {
            return Err(stale());
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "settings.updated",
                resource_type: "settings",
                resource_id: SETTINGS_ID,
                resource_version: revision,
                data: serde_json::json!({}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando custom_rules", &error))?;
        Ok(VersionedCustomRules {
            value: value.clone(),
            revision,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;
    use larm_domain::custom_rules::{
        CustomRule, RuleEffect, RuleField, RuleOperator, RulePredicate,
    };
    use larm_domain::ports::MutationContext;
    use serde_json::json;

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103".into(),
            correlation_id: "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a104".into(),
        }
    }

    fn expect_error<T>(result: Result<T, LarmError>, message: &str) -> LarmError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("{message}"),
        }
    }

    fn rules() -> CustomRules {
        CustomRules {
            schema_version: "1.0".into(),
            rules: vec![CustomRule {
                id: "gguf-limit".into(),
                predicates: vec![RulePredicate {
                    field: RuleField::Format,
                    operator: RuleOperator::Eq,
                    value: json!("gguf"),
                }],
                effect: RuleEffect::Limit,
                limit_bytes: Some("4096".into()),
                target_storage_location_id: None,
            }],
        }
    }

    #[tokio::test]
    async fn persiste_custom_rules_con_revision_cas_y_lectura_validada() {
        let (_directory, db) = crate::testing::temp_migrated()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let repo = SqlCustomRules { pool: db.pool };

        let initial = repo.get().await.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(initial.revision, 0);
        assert_eq!(initial.value, CustomRules::default());

        let saved = repo
            .replace(0, &rules(), &context())
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.value, rules());
        assert_eq!(
            repo.get().await.unwrap_or_else(|error| panic!("{error}")),
            saved
        );
        let outbox_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_id=?",
        )
        .bind(SETTINGS_ID)
        .fetch_one(&repo.pool)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(outbox_events, 1);

        let error = expect_error(
            repo.replace(0, &CustomRules::default(), &context()).await,
            "la revisión antigua debe rechazarse",
        );
        assert_eq!(error.code, ErrorCode::StaleRevision);
        assert_eq!(
            repo.get().await.unwrap_or_else(|error| panic!("{error}")),
            saved
        );
        let outbox_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_id=?",
        )
        .bind(SETTINGS_ID)
        .fetch_one(&repo.pool)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(outbox_events, 1, "el PATCH obsoleto no añade evento");
    }

    #[tokio::test]
    async fn no_guarda_reglas_invalidas_y_detecta_payload_corrupto() {
        let (_directory, db) = crate::testing::temp_migrated()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let repo = SqlCustomRules {
            pool: db.pool.clone(),
        };
        let mut invalid_rules = rules();
        invalid_rules.rules[0].limit_bytes = None;
        assert_eq!(
            expect_error(
                repo.replace(0, &invalid_rules, &context()).await,
                "el límite es obligatorio",
            )
            .code,
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            repo.get()
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .revision,
            0
        );

        sqlx::query("INSERT INTO settings(id,revision,payload) VALUES(?,?,?)")
            .bind(SETTINGS_ID)
            .bind(1_i64)
            .bind("{}")
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            expect_error(repo.get().await, "payload corrupto").code,
            ErrorCode::IntegrityFailed
        );
    }

    #[tokio::test]
    async fn fallo_de_outbox_revierte_revision_y_payload() {
        let (_directory, db) = crate::testing::temp_migrated()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let repo = SqlCustomRules {
            pool: db.pool.clone(),
        };
        sqlx::query(
            "CREATE TRIGGER fail_custom_rules_outbox BEFORE INSERT ON event_outbox \
             BEGIN SELECT RAISE(ABORT,'fault injected'); END",
        )
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("{error}"));

        assert!(repo.replace(0, &rules(), &context()).await.is_err());
        assert_eq!(
            repo.get().await.unwrap_or_else(|error| panic!("{error}")),
            VersionedCustomRules::default()
        );
        let persisted: i64 = sqlx::query_scalar("SELECT count(*) FROM settings WHERE id=?")
            .bind(SETTINGS_ID)
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=?")
            .bind(SETTINGS_ID)
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(persisted, 0);
        assert_eq!(events, 0);
    }
}
