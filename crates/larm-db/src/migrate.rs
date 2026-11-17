//! Migraciones idempotentes y atómicas con `schema_metadata` (P004).
//!
//! Las migraciones viajan dentro del binario (`include_str!`), con su sha256:
//! el daemon no depende de una carpeta externa y una migración ya aplicada
//! cuyo contenido cambie se detecta y bloquea el arranque. Cada migración se
//! aplica en una única transacción `BEGIN IMMEDIATE` junto con su registro en
//! `schema_metadata`: un fallo a mitad no deja tablas sueltas ni registro.

use larm_domain::error::{ErrorCode, LarmError};
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, SqlitePool};

/// Migración versionada.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// `NNNN_nombre`, orden lexicográfico = orden de aplicación.
    pub version: &'static str,
    pub sql: &'static str,
}

impl Migration {
    #[must_use]
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.sql.as_bytes()))
    }
}

/// Migraciones del producto, en orden.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "0001_bootstrap",
        sql: include_str!("../../../migrations/0001_bootstrap.sql"),
    },
    Migration {
        version: "0002_adapter_selection",
        sql: include_str!("../../../migrations/0002_adapter_selection.sql"),
    },
    Migration {
        version: "0003_runtime_channel_identity",
        sql: include_str!("../../../migrations/0003_runtime_channel_identity.sql"),
    },
    Migration {
        version: "0004_runtime_profile_locks",
        sql: include_str!("../../../migrations/0004_runtime_profile_locks.sql"),
    },
    Migration {
        version: "0005_runtime_profile_qualifications",
        sql: include_str!("../../../migrations/0005_runtime_profile_qualifications.sql"),
    },
    Migration {
        version: "0006_p052_artifact_size_estimate",
        sql: include_str!("../../../migrations/0006_p052_artifact_size_estimate.sql"),
    },
    Migration {
        version: "0007_p098_local_import_source",
        sql: include_str!("../../../migrations/0007_p098_local_import_source.sql"),
    },
    Migration {
        version: "0008_p093_failed_recoverable",
        sql: include_str!("../../../migrations/0008_p093_failed_recoverable.sql"),
    },
    Migration {
        version: "0009_p028_signed_adapter_registry",
        sql: include_str!("../../../migrations/0009_p028_signed_adapter_registry.sql"),
    },
    Migration {
        version: "0010_p028_registry_url_binding",
        sql: include_str!("../../../migrations/0010_p028_registry_url_binding.sql"),
    },
    Migration {
        version: "0011_p127_source_operational_snapshot",
        sql: include_str!("../../../migrations/0011_p127_source_operational_snapshot.sql"),
    },
    Migration {
        version: "0012_p128_low_disk_transition",
        sql: include_str!("../../../migrations/0012_p128_low_disk_transition.sql"),
    },
    Migration {
        version: "0013_p032_runtime_version_pinning_and_retention",
        sql: include_str!(
            "../../../migrations/0013_p032_runtime_version_pinning_and_retention.sql"
        ),
    },
    Migration {
        version: "0014_model_acquisition_manifest",
        sql: include_str!("../../../migrations/0014_model_acquisition_manifest.sql"),
    },
    Migration {
        version: "0015_p081_execution_host",
        sql: include_str!("../../../migrations/0015_p081_execution_host.sql"),
    },
    Migration {
        version: "0016_p119_download_admission",
        sql: include_str!("../../../migrations/0016_p119_download_admission.sql"),
    },
];

/// Migración registrada en `schema_metadata`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppliedMigration {
    pub version: String,
    pub sha256: String,
    pub applied_at: String,
}

/// Aplica las migraciones pendientes del producto. Idempotente.
///
/// # Errors
/// `CONFLICT` si una migración aplicada cambió de contenido; `INTERNAL` si falla el SQL.
pub async fn migrate(pool: &SqlitePool) -> Result<Vec<String>, LarmError> {
    migrate_with(pool, MIGRATIONS).await
}

/// Aplica `migrations` (inyectable para pruebas de fallo).
///
/// # Errors
/// Ver [`migrate`].
pub async fn migrate_with(
    pool: &SqlitePool,
    migrations: &[Migration],
) -> Result<Vec<String>, LarmError> {
    ensure_metadata_table(pool).await?;
    let recorded = applied(pool).await?;
    let mut ordered: Vec<&Migration> = migrations.iter().collect();
    ordered.sort_by_key(|m| m.version);

    let mut newly = Vec::new();
    for migration in ordered {
        let sha = migration.sha256();
        if let Some(done) = recorded.iter().find(|r| r.version == migration.version) {
            if done.sha256 != sha {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La base de datos se migró con una versión distinta de LARM",
                    "migration_checksum_mismatch",
                )
                .with_resource(migration.version)
                .with_technical(format!(
                    "migración {} aplicada con sha256 {} y ahora es {sha}",
                    migration.version, done.sha256
                )));
            }
            continue;
        }
        apply_one(pool, migration, &sha).await?;
        newly.push(migration.version.to_string());
    }
    Ok(newly)
}

async fn apply_one(pool: &SqlitePool, migration: &Migration, sha: &str) -> Result<(), LarmError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| super::pool::db_error("conexión para migrar", &e))?;
    // BEGIN IMMEDIATE toma el bloqueo de escritura antes de tocar el esquema.
    sqlx::raw_sql("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .map_err(|e| super::pool::db_error("BEGIN IMMEDIATE", &e))?;
    let outcome = async {
        // SQL versionado del propio binario, no entrada del usuario.
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql))
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                super::pool::db_error(&format!("aplicando migración {}", migration.version), &e)
            })?;
        sqlx::query(
            "INSERT INTO schema_metadata(version, name, sha256, applied_at) VALUES (?, ?, ?, ?)",
        )
        .bind(migration.version)
        .bind(format!("{}.sql", migration.version))
        .bind(sha)
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .execute(&mut *conn)
        .await
        .map_err(|e| super::pool::db_error("registrando migración", &e))?;
        Ok::<(), LarmError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            sqlx::raw_sql("COMMIT")
                .execute(&mut *conn)
                .await
                .map_err(|e| super::pool::db_error("COMMIT de migración", &e))?;
            Ok(())
        }
        Err(error) => {
            // Si el ROLLBACK también falla la conexión se descarta: no vuelve al pool.
            if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                let _ = conn.detach().close().await;
            }
            Err(error)
        }
    }
}

/// Migraciones registradas (ordenadas por versión).
///
/// # Errors
/// `INTERNAL` si falla la consulta.
pub async fn applied(pool: &SqlitePool) -> Result<Vec<AppliedMigration>, LarmError> {
    ensure_metadata_table(pool).await?;
    let rows =
        sqlx::query("SELECT version, sha256, applied_at FROM schema_metadata ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(|e| super::pool::db_error("consultando schema_metadata", &e))?;
    rows.iter()
        .map(|r| {
            Ok(AppliedMigration {
                version: r
                    .try_get(0)
                    .map_err(|e| super::pool::db_error("leyendo version", &e))?,
                sha256: r
                    .try_get(1)
                    .map_err(|e| super::pool::db_error("leyendo sha256", &e))?,
                applied_at: r
                    .try_get(2)
                    .map_err(|e| super::pool::db_error("leyendo applied_at", &e))?,
            })
        })
        .collect()
}

/// Versiones aplicadas (ordenadas).
///
/// # Errors
/// `INTERNAL` si falla la consulta.
pub async fn applied_versions(pool: &SqlitePool) -> Result<Vec<String>, LarmError> {
    Ok(applied(pool)
        .await?
        .into_iter()
        .map(|m| m.version)
        .collect())
}

async fn ensure_metadata_table(pool: &SqlitePool) -> Result<(), LarmError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_metadata(\
         id INTEGER PRIMARY KEY AUTOINCREMENT,\
         version TEXT NOT NULL UNIQUE,\
         name TEXT NOT NULL,\
         sha256 TEXT NOT NULL CHECK(length(sha256)=64),\
         applied_at TEXT NOT NULL\
         ) STRICT",
    )
    .execute(pool)
    .await
    .map_err(|e| super::pool::db_error("creando schema_metadata", &e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn p093_migration_preserves_jobs_and_admits_failed_recoverable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let db = crate::pool::Db::open(&directory.path().join("p093.sqlite"))
            .await
            .expect("database");
        migrate_with(&db.pool, &MIGRATIONS[..7])
            .await
            .expect("pre-P093 migrations");
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(&db.pool).await.expect("canonical model");
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','cm','0192f000-0000-7000-8000-00000000f103','ns','model')")
            .execute(&db.pool).await.expect("model distribution");
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('revision','t','t',1,'{}','distribution','r1','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','CURRENT')")
            .execute(&db.pool).await.expect("model revision");
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES('0192aaaa-0000-7000-8000-000000000001','t','t',1,'{}','revision','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .execute(&db.pool).await.expect("model variant");
        sqlx::query("INSERT INTO transformation_job(id,created_at,updated_at,row_version,metadata,input_variant_id,output_variant_id,adapter_id,tool_version,parameters,plan_hash,state,attempt,error) VALUES('0192aaaa-0000-7000-8000-000000000006','2026-10-23T11:00:00.000Z','2026-10-23T11:01:00.000Z',4,'{}','0192aaaa-0000-7000-8000-000000000001',NULL,'0192aaaa-0000-7000-8000-000000000004','1.0','{}','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','RUNNING',2,NULL)")
            .execute(&db.pool).await.expect("existing transformation job");
        migrate_with(&db.pool, &MIGRATIONS[7..8])
            .await
            .expect("P093 migration");
        let table_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='transformation_job'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("transformation_job schema");
        assert!(table_sql.contains("'FAILED_RECOVERABLE'"));
        let preserved: (String, i64, i64) = sqlx::query_as(
            "SELECT state,row_version,attempt FROM transformation_job WHERE id='0192aaaa-0000-7000-8000-000000000006'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("preserved job");
        assert_eq!(preserved, ("RUNNING".into(), 4, 2));
        sqlx::query("UPDATE transformation_job SET state='FAILED_RECOVERABLE' WHERE id='0192aaaa-0000-7000-8000-000000000006'")
            .execute(&db.pool).await.expect("new durable state");
        let index: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_transformation_job_recovery'",
        )
        .fetch_optional(&db.pool)
        .await
        .expect("recovery index");
        assert_eq!(index.as_deref(), Some("idx_transformation_job_recovery"));
        assert!(applied_versions(&db.pool)
            .await
            .expect("migration versions")
            .iter()
            .any(|version| version == "0008_p093_failed_recoverable"));
    }

    #[tokio::test]
    async fn p127_migration_seeds_existing_sources_and_defaults_future_sources() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let db = crate::pool::Db::open(&directory.path().join("p127.sqlite"))
            .await
            .expect("database");
        migrate_with(&db.pool, &MIGRATIONS[..10])
            .await
            .expect("pre-P127 migrations");
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('p127-install','t','t',1,'{}','fixture.source','1.0.0','source',?,'fixture','VERIFIED','{}','VALIDATED')")
            .bind("a".repeat(64))
            .execute(&db.pool)
            .await
            .expect("source adapter installation");
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('p127-adapter','t','t',1,'{}','p127-install','HTTP','[]')")
            .execute(&db.pool)
            .await
            .expect("source adapter binding");
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES('p127-existing','t','t',1,'{}','Existing','p127-adapter','https://example.invalid',0,0,'NONE',NULL,'{}')")
            .execute(&db.pool)
            .await
            .expect("pre-existing source");

        migrate_with(&db.pool, &MIGRATIONS[10..])
            .await
            .expect("P127 migration");
        let existing: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status,last_sync_at,rate_limit_json FROM model_source_operational_snapshot WHERE source_id='p127-existing'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("seeded source snapshot");
        assert_eq!(existing, ("UNKNOWN".into(), None, None));

        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES('p127-future','t','t',1,'{}','Future','p127-adapter','https://example.invalid/future',0,0,'NONE',NULL,'{}')")
            .execute(&db.pool)
            .await
            .expect("future source");
        let future: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status,last_sync_at,rate_limit_json FROM model_source_operational_snapshot WHERE source_id='p127-future'",
        )
        .fetch_one(&db.pool)
        .await
        .expect("triggered source snapshot");
        assert_eq!(future, ("UNKNOWN".into(), None, None));
    }
}
