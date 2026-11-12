//! Checkpoint y revocaciones durables del registro P028.
use larm_domain::adapter_registry::{
    RegistryCheckpoint, RegistryRevocationRecord, RegistryRevocationTargetType, RegistryStatus,
    RegistrySyncReceipt,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::MutationContext;
use sqlx::{SqliteConnection, SqlitePool};

use crate::pool::db_error;

type CheckpointRow = (String, String, i64, String, String, String, String, String);

fn decode_checkpoint(row: CheckpointRow) -> Result<RegistryCheckpoint, LarmError> {
    let sequence = u64::try_from(row.2).map_err(|e| db_error("secuencia del registro", &e))?;
    Ok(RegistryCheckpoint {
        registry_id: row.0,
        registry_url: row.1,
        sequence,
        payload_sha256: row.3,
        issued_at: row.4,
        expires_at: row.5,
        accepted_at: row.6,
        envelope: serde_json::from_str(&row.7)
            .map_err(|e| db_error("envelope del registro", &e))?,
    })
}

fn conflict(reason: &'static str, message: &'static str) -> LarmError {
    LarmError::new(ErrorCode::Conflict, message, reason)
}

/// Recupera el checkpoint asociado a la URL configurada para que el verificador
/// pueda aplicar el freno anti-rollback antes de aceptar otra firma.
pub async fn checkpoint_by_url(
    pool: &SqlitePool,
    registry_url: &str,
) -> Result<Option<RegistryCheckpoint>, LarmError> {
    let row: Option<CheckpointRow> = sqlx::query_as(
        "SELECT registry_id,registry_url,sequence,payload_sha256,issued_at,expires_at,accepted_at,envelope_json \
         FROM adapter_registry_checkpoint WHERE registry_url=?",
    )
    .bind(registry_url)
    .fetch_optional(pool)
    .await
    .map_err(|e| db_error("leyendo checkpoint del registro", &e))?;
    row.map(decode_checkpoint).transpose()
}

/// Estado de solo lectura para la pantalla de registro. Las revocaciones se
/// devuelven en orden estable y sobreviven aunque no aparezcan en índices nuevos.
pub async fn status(
    pool: &SqlitePool,
    registry_url: Option<&str>,
) -> Result<RegistryStatus, LarmError> {
    let Some(registry_url) = registry_url else {
        return Ok(RegistryStatus {
            enabled: false,
            registry_url: None,
            checkpoint: None,
            revocations: Vec::new(),
        });
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| db_error("BEGIN estado del registro", &e))?;
    let row: Option<CheckpointRow> = sqlx::query_as(
        "SELECT registry_id,registry_url,sequence,payload_sha256,issued_at,expires_at,accepted_at,envelope_json \
         FROM adapter_registry_checkpoint WHERE registry_url=?",
    )
    .bind(registry_url)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error("leyendo estado del registro", &e))?;
    let checkpoint = row.map(decode_checkpoint).transpose()?;
    let mut revocations = Vec::new();
    if let Some(checkpoint) = &checkpoint {
        let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
            "SELECT target_type,target,revoked_at,reason_code,source_sequence \
             FROM adapter_registry_revocation WHERE registry_id=? ORDER BY target_type,target",
        )
        .bind(&checkpoint.registry_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo revocaciones del registro", &e))?;
        revocations = rows
            .into_iter()
            .map(
                |(target_type, target, revoked_at, reason_code, source_sequence)| {
                    let target_type = match target_type.as_str() {
                        "PACKAGE_SHA256" => RegistryRevocationTargetType::PackageSha256,
                        "PUBLISHER_KEY" => RegistryRevocationTargetType::PublisherKey,
                        _ => return Err(db_error("tipo de revocación", &target_type)),
                    };
                    Ok(RegistryRevocationRecord {
                        registry_id: checkpoint.registry_id.clone(),
                        target_type,
                        target,
                        revoked_at,
                        reason_code,
                        source_sequence: u64::try_from(source_sequence)
                            .map_err(|e| db_error("secuencia de revocación", &e))?,
                    })
                },
            )
            .collect::<Result<Vec<_>, LarmError>>()?;
    }
    tx.commit()
        .await
        .map_err(|e| db_error("COMMIT estado del registro", &e))?;
    Ok(RegistryStatus {
        enabled: true,
        registry_url: Some(registry_url.to_owned()),
        checkpoint,
        revocations,
    })
}

/// Persiste de forma atómica el índice ya verificado, sus nuevas revocaciones y
/// el evento de auditoría. `BEGIN IMMEDIATE` serializa aceptaciones concurrentes.
#[allow(clippy::too_many_arguments)]
pub async fn accept_index(
    pool: &SqlitePool,
    registry_url: &str,
    registry_id: &str,
    sequence: u64,
    payload_sha256: &str,
    issued_at: &str,
    expires_at: &str,
    accepted_at: &str,
    envelope_json: &str,
    package_count: u64,
    revocations: &[RegistryRevocationRecord],
    ctx: &MutationContext,
) -> Result<RegistrySyncReceipt, LarmError> {
    let sequence_sql = i64::try_from(sequence).map_err(|e| db_error("secuencia", &e))?;
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| db_error("pool del registro", &e))?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("BEGIN IMMEDIATE del registro", &e))?;
    let result = accept_in_transaction(
        &mut conn,
        registry_url,
        registry_id,
        sequence_sql,
        payload_sha256,
        issued_at,
        expires_at,
        accepted_at,
        envelope_json,
        package_count,
        revocations,
        ctx,
    )
    .await;
    match result {
        Ok(receipt) => {
            sqlx::query("COMMIT")
                .execute(&mut *conn)
                .await
                .map_err(|e| db_error("COMMIT checkpoint del registro", &e))?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_in_transaction(
    conn: &mut SqliteConnection,
    registry_url: &str,
    registry_id: &str,
    sequence: i64,
    payload_sha256: &str,
    issued_at: &str,
    expires_at: &str,
    accepted_at: &str,
    envelope_json: &str,
    package_count: u64,
    revocations: &[RegistryRevocationRecord],
    ctx: &MutationContext,
) -> Result<RegistrySyncReceipt, LarmError> {
    let current: Option<(String, i64, String, String)> = sqlx::query_as(
        "SELECT registry_id,sequence,payload_sha256,accepted_at FROM adapter_registry_checkpoint WHERE registry_url=?",
    )
    .bind(registry_url)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("comprobando checkpoint concurrente", &e))?;
    if let Some((current_id, current_sequence, current_hash, current_accepted_at)) = current {
        if current_id != registry_id {
            return Err(conflict(
                "registry_identity_changed",
                "La identidad del registro cambió para la URL configurada",
            ));
        }
        if current_sequence > sequence {
            return Err(conflict(
                "registry_sequence_rollback",
                "El registro devolvió una secuencia anterior",
            ));
        }
        if current_sequence == sequence {
            if current_hash != payload_sha256 {
                return Err(conflict(
                    "registry_sequence_conflict",
                    "La secuencia del registro ya existe con otro contenido",
                ));
            }
            return Ok(RegistrySyncReceipt {
                registry_id: registry_id.to_owned(),
                sequence: u64::try_from(sequence).map_err(|e| db_error("secuencia", &e))?,
                payload_sha256: payload_sha256.to_owned(),
                accepted_at: current_accepted_at,
                expires_at: expires_at.to_owned(),
                package_count,
                revocation_count: u64::try_from(revocations.len())
                    .map_err(|e| db_error("cantidad de revocaciones", &e))?,
                replay: true,
            });
        }
    }
    let bound_url: Option<String> = sqlx::query_scalar(
        "SELECT registry_url FROM adapter_registry_checkpoint WHERE registry_id=?",
    )
    .bind(registry_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("comprobando identidad del registro", &e))?;
    if bound_url
        .as_deref()
        .is_some_and(|current| current != registry_url)
    {
        return Err(conflict(
            "registry_identity_changed",
            "La identidad firmada ya está asociada a otra URL",
        ));
    }
    let updated = sqlx::query(
        "INSERT INTO adapter_registry_checkpoint(registry_id,registry_url,sequence,payload_sha256,issued_at,expires_at,accepted_at,envelope_json,updated_at) \
         VALUES (?,?,?,?,?,?,?,?,?) \
         ON CONFLICT(registry_id) DO UPDATE SET registry_url=excluded.registry_url,sequence=excluded.sequence, \
         payload_sha256=excluded.payload_sha256,issued_at=excluded.issued_at,expires_at=excluded.expires_at, \
         accepted_at=excluded.accepted_at,envelope_json=excluded.envelope_json,updated_at=excluded.updated_at \
         WHERE adapter_registry_checkpoint.registry_url=excluded.registry_url AND adapter_registry_checkpoint.sequence<excluded.sequence",
    )
    .bind(registry_id)
    .bind(registry_url)
    .bind(sequence)
    .bind(payload_sha256)
    .bind(issued_at)
    .bind(expires_at)
    .bind(accepted_at)
    .bind(envelope_json)
    .bind(accepted_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("guardando checkpoint", &e))?;
    if updated.rows_affected() != 1 {
        return Err(conflict(
            "registry_checkpoint_race",
            "El checkpoint cambió durante la sincronización",
        ));
    }
    for revocation in revocations {
        let target_type = match revocation.target_type {
            RegistryRevocationTargetType::PackageSha256 => "PACKAGE_SHA256",
            RegistryRevocationTargetType::PublisherKey => "PUBLISHER_KEY",
        };
        sqlx::query(
            "INSERT OR IGNORE INTO adapter_registry_revocation(registry_id,target_type,target,revoked_at,reason_code,source_sequence) VALUES (?,?,?,?,?,?)",
        )
        .bind(registry_id)
        .bind(target_type)
        .bind(&revocation.target)
        .bind(&revocation.revoked_at)
        .bind(&revocation.reason_code)
        .bind(i64::try_from(revocation.source_sequence).map_err(|e| db_error("source_sequence", &e))?)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("guardando revocación", &e))?;
    }
    let event_id = crate::events::append(
        conn,
        &crate::events::NewEvent {
            event_type: "adapter.registry.synced",
            resource_type: "adapter_registry",
            resource_id: registry_id,
            resource_version: sequence,
            data: serde_json::json!({
                "sequence": sequence,
                "payload_sha256": payload_sha256,
                "package_count": package_count,
                "revocation_count": revocations.len(),
            }),
        },
        ctx,
    )
    .await?;
    let _ = event_id;
    Ok(RegistrySyncReceipt {
        registry_id: registry_id.to_owned(),
        sequence: u64::try_from(sequence).map_err(|e| db_error("secuencia", &e))?,
        payload_sha256: payload_sha256.to_owned(),
        accepted_at: accepted_at.to_owned(),
        expires_at: expires_at.to_owned(),
        package_count,
        revocation_count: u64::try_from(revocations.len())
            .map_err(|e| db_error("cantidad de revocaciones", &e))?,
        replay: false,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use larm_domain::adapter_registry::RegistryRevocationTargetType;

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000001".into(),
        }
    }

    fn revocation(sequence: u64) -> RegistryRevocationRecord {
        RegistryRevocationRecord {
            registry_id: "fixture-v1".into(),
            target_type: RegistryRevocationTargetType::PackageSha256,
            target: "a".repeat(64),
            revoked_at: "2026-10-24T10:00:00.000Z".into(),
            reason_code: "MALICIOUS_PACKAGE".into(),
            source_sequence: sequence,
        }
    }

    async fn accept(
        db: &crate::pool::Db,
        sequence: u64,
        hash: &str,
        revocations: &[RegistryRevocationRecord],
    ) -> Result<RegistrySyncReceipt, LarmError> {
        accept_index(
            &db.pool,
            "https://registry.example.test/index.json",
            "fixture-v1",
            sequence,
            hash,
            "2026-10-24T09:00:00.000Z",
            "2026-10-25T09:00:00.000Z",
            "2026-10-24T10:00:00.000Z",
            &serde_json::json!({"sequence":sequence}).to_string(),
            2,
            revocations,
            &context(),
        )
        .await
    }

    #[tokio::test]
    async fn checkpoint_replay_y_revocaciones_acumulativas_sobreviven_al_reinicio() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        let first = accept(&db, 1, &"1".repeat(64), &[revocation(1)])
            .await
            .unwrap();
        assert!(!first.replay);

        let second = accept(&db, 2, &"2".repeat(64), &[]).await.unwrap();
        assert!(!second.replay);
        let replay = accept(&db, 2, &"2".repeat(64), &[]).await.unwrap();
        assert!(replay.replay);

        let status = status(&db.pool, Some("https://registry.example.test/index.json"))
            .await
            .unwrap();
        assert_eq!(status.checkpoint.unwrap().sequence, 2);
        assert_eq!(status.revocations.len(), 1);
        assert_eq!(status.revocations[0].source_sequence, 1);
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event WHERE event_type='adapter.registry.synced'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(events, 2);
    }

    #[tokio::test]
    async fn rechaza_rollback_conflicto_de_secuencia_y_cambio_de_identidad() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        accept(&db, 3, &"3".repeat(64), &[]).await.unwrap();
        assert_eq!(
            accept(&db, 2, &"2".repeat(64), &[])
                .await
                .unwrap_err()
                .details
                .reason,
            "registry_sequence_rollback"
        );
        assert_eq!(
            accept(&db, 3, &"4".repeat(64), &[])
                .await
                .unwrap_err()
                .details
                .reason,
            "registry_sequence_conflict"
        );
        let changed_identity = accept_index(
            &db.pool,
            "https://registry.example.test/index.json",
            "other-registry",
            1,
            &"5".repeat(64),
            "2026-10-24T09:00:00.000Z",
            "2026-10-25T09:00:00.000Z",
            "2026-10-24T10:00:00.000Z",
            "{\"sequence\":1}",
            0,
            &[],
            &context(),
        )
        .await
        .unwrap_err();
        assert_eq!(changed_identity.details.reason, "registry_identity_changed");
    }

    #[tokio::test]
    async fn misma_identidad_no_puede_moverse_a_otra_url() {
        let (_dir, db) = crate::testing::temp_migrated().await.unwrap();
        accept(&db, 1, &"1".repeat(64), &[]).await.unwrap();
        let error = accept_index(
            &db.pool,
            "https://other.example.test/index.json",
            "fixture-v1",
            2,
            &"2".repeat(64),
            "2026-10-24T09:00:00.000Z",
            "2026-10-25T09:00:00.000Z",
            "2026-10-24T10:00:00.000Z",
            "{\"sequence\":2}",
            0,
            &[],
            &context(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.details.reason, "registry_identity_changed");
    }
}
