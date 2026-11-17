//! Tests de integración de persistencia (P004): pragmas, esquema contractual
//! completo, migraciones idempotentes y atómicas, rollback, FK, conformidad
//! H07/H08/H01/H02, bytes i64 como string y backup/restore verificado.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::migrate::{self, Migration};
use larm_db::pool::Db;
use larm_db::tx::with_transaction;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{ArtifactSizeConfidence, ArtifactSizeEstimate};
use larm_domain::ports::VariantRepository;
use larm_domain::units::DecimalU64;
use sqlx::Row;
use std::collections::BTreeSet;

const T: &str = "2026-10-11T00:00:00.000Z";

async fn prepare(path: &std::path::Path) -> Db {
    let db = Db::open(path).await.expect("abrir DB");
    let applied = migrate::migrate(&db.pool).await.expect("migrar");
    assert_eq!(
        applied,
        vec![
            "0001_bootstrap",
            "0002_adapter_selection",
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
    db
}

fn temp_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    (dir, path)
}

async fn tables(pool: &sqlx::SqlitePool) -> BTreeSet<String> {
    sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .fetch_all(pool)
        .await
        .expect("tablas")
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect()
}

async fn schema_sql(pool: &sqlx::SqlitePool) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT group_concat(type || ':' || name || ':' || coalesce(sql,''), '\n') FROM \
         (SELECT * FROM sqlite_master ORDER BY type, name)",
    )
    .fetch_one(pool)
    .await
    .expect("esquema")
}

async fn exec(pool: &sqlx::SqlitePool, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.replace("'t'", &format!("'{T}'"))))
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

/// Cadena adapter_installation → … → model_variant.
async fn seed_model_chain(pool: &sqlx::SqlitePool) {
    exec(pool, "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')").await;
    exec(pool, "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','{}')").await;
    exec(pool, "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','n','{}','[]')").await;
    exec(pool, "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','s','msa','http://x',0,0,'NONE','{}')").await;
    exec(pool, "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm','ms','ns','mid')").await;
    exec(pool, "INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1','h','CURRENT')").await;
    exec(pool, "INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES('mv','t','t',1,'{}','mr','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','{}','{}',0)").await;
}

#[tokio::test]
async fn p052_migra_tamanos_historicos_a_unknown_y_los_expone_conservadoramente() {
    let (_dir, path) = temp_path("p052-size-estimate.sqlite");
    let db = Db::open(&path).await.unwrap();
    migrate::migrate_with(&db.pool, &migrate::MIGRATIONS[..5])
        .await
        .unwrap();
    seed_model_chain(&db.pool).await;
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,required) VALUES('artifact-legacy','t','t',1,'{}','mv','weights.gguf',987654,1)")
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(
        migrate::migrate(&db.pool).await.unwrap(),
        vec![
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
    let (expected_size, persisted): (i64, String) = sqlx::query_as(
        "SELECT expected_size,size_estimate FROM model_artifact WHERE id='artifact-legacy'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(expected_size, 987654);
    let estimate: ArtifactSizeEstimate = serde_json::from_str(&persisted).unwrap();
    assert_eq!(estimate.confidence, ArtifactSizeConfidence::Unknown);
    assert_eq!(
        estimate.bytes, None,
        "expected_size no se promueve a conocido"
    );
    assert_eq!(estimate.evidence_refs, ["migration:0006"]);
    estimate.validate().unwrap();

    let repository = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let artifact = repository
        .get_artifact("artifact-legacy")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(artifact.size_estimate, estimate);
}

async fn seed_runtime_chain(pool: &sqlx::SqlitePool) {
    exec(pool, "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('rai','t','t',1,'{}','rt-adapter','1.0','runtime','ph','pub','UNTRUSTED','{}','INSTALLED')").await;
    exec(pool, "INSERT INTO runtime_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,runtime_family,capabilities) VALUES('rta','t','t',1,'{}','rai','llama.cpp','{}')").await;
    exec(pool, "INSERT INTO runtime(id,created_at,updated_at,row_version,metadata,channel_policy,name,adapter_id,capabilities) VALUES('rt','t','t',1,'{}','{}','llama.cpp','rta','{}')").await;
}

#[tokio::test]
async fn runtime_migra_canales_sin_reescribir_datos_y_conserva_identidad_por_canal() {
    let (_dir, path) = temp_path("runtime-channels.sqlite");
    let db = Db::open(&path).await.unwrap();
    migrate::migrate_with(&db.pool, &migrate::MIGRATIONS[..2])
        .await
        .unwrap();
    seed_runtime_chain(&db.pool).await;
    exec(&db.pool,"INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('stable-original','t','t',7,'{\"preserved\":true}','rt','b7142-local','stable','PINNED')").await;
    // El índice anterior confundía dos observaciones del mismo build en canales distintos.
    assert!(sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('nightly-before',?,?,1,'{}','rt','b7142-local','nightly','NONE')")
        .bind(T).bind(T).execute(&db.pool).await.is_err());
    let before: (i64, String, String) = sqlx::query_as(
        "SELECT row_version,metadata,promotion FROM runtime_version WHERE id='stable-original'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        migrate::migrate(&db.pool).await.unwrap(),
        vec![
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
    let after: (i64, String, String) = sqlx::query_as(
        "SELECT row_version,metadata,promotion FROM runtime_version WHERE id='stable-original'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    for channel in ["prerelease", "nightly", "commit", "custom"] {
        sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES(?,?,?,1,'{}','rt','b7142-local',?,'NONE')")
            .bind(channel).bind(T).bind(T).bind(channel).execute(&db.pool).await.unwrap();
    }
    assert!(sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('duplicate',?,?,1,'{}','rt','b7142-local','nightly','NONE')")
        .bind(T).bind(T).execute(&db.pool).await.is_err());
    assert!(sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('unknown',?,?,1,'{}','rt','b7142-local','experimental','NONE')")
        .bind(T).bind(T).execute(&db.pool).await.is_err());
    db.pool.close().await;
    let reopened = Db::open(&path).await.unwrap();
    assert!(migrate::migrate(&reopened.pool).await.unwrap().is_empty());
    let channels: Vec<String> = sqlx::query_scalar(
        "SELECT channel FROM runtime_version WHERE runtime_id='rt' ORDER BY channel",
    )
    .fetch_all(&reopened.pool)
    .await
    .unwrap();
    assert_eq!(
        channels,
        vec!["commit", "custom", "nightly", "prerelease", "stable"]
    );
    assert!(reopened.health().await.unwrap().is_healthy());
}

#[tokio::test]
async fn runtime_fallo_de_migracion_revierte_drop_index_y_no_avanza_metadata() {
    let (_dir, path) = temp_path("runtime-migration-fault.sqlite");
    let db = Db::open(&path).await.unwrap();
    migrate::migrate_with(&db.pool, &migrate::MIGRATIONS[..2])
        .await
        .unwrap();
    let before = schema_sql(&db.pool).await;
    let faulty=Migration {version:"0003_runtime_channel_identity",sql:"DROP INDEX runtime_version_identity; CREATE UNIQUE INDEX runtime_version_identity ON runtime_version(missing_column);"};
    assert!(migrate::migrate_with(&db.pool, &[faulty]).await.is_err());
    assert_eq!(schema_sql(&db.pool).await, before);
    assert_eq!(
        migrate::applied_versions(&db.pool).await.unwrap(),
        vec!["0001_bootstrap", "0002_adapter_selection"]
    );
    assert_eq!(
        migrate::migrate(&db.pool).await.unwrap(),
        vec![
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
}

async fn insert_location(
    pool: &sqlx::SqlitePool,
    id: &str,
    capacity: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,volume_guid,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,?,?,1,'{}','L1','C:/l1','C:/l1','fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3','NTFS','INTERNAL',?,0,0,0,1,'ONLINE','fb4f4fbc','NVME')")
        .bind(id)
        .bind(T)
        .bind(T)
        .bind(capacity)
        .execute(pool)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn pragmas_del_contrato_activos() {
    let (_d, path) = temp_path("wal_fk.sqlite");
    let db = prepare(&path).await;
    let health = db.health().await.expect("health");
    assert_eq!(health.journal_mode, "wal");
    assert!(health.foreign_keys_on);
    assert_eq!(health.busy_timeout_ms, 5000);
    assert_eq!(health.quick_check, "ok");
    assert!(health.is_healthy());
}

#[tokio::test]
async fn migracion_desde_vacia_crea_exactamente_las_tablas_del_ddl() {
    let (_d, path) = temp_path("empty.sqlite");
    let db = prepare(&path).await;
    let ddl = include_str!("../../../docs/contracts/persistence.sql");
    let mut expected: BTreeSet<String> = ddl
        .lines()
        .filter_map(|l| l.strip_prefix("CREATE TABLE "))
        .map(|l| {
            l.split(|c: char| c.is_whitespace() || c == '(')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        expected.len(),
        49,
        "DDL contractual: entidades, snapshots y pins/comparaciones P032"
    );
    let acceptance: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/contracts/acceptance.json")).unwrap();
    for entity in acceptance["required_entities"].as_array().unwrap() {
        assert!(expected.contains(entity.as_str().unwrap()), "{entity}");
    }
    expected.insert("schema_metadata".into());
    expected.insert("adapter_selection".into()); // auxiliar tipado de P024
    expected.insert("runtime_profile_lock".into()); // auxiliar inmutable de P099
    expected.insert("runtime_profile_qualification".into()); // evidencia inmutable separada de P099
    expected.insert("execution_host".into()); // registro read-only de hosts de ejecución (P081)
    expected.insert("download_admission_policy".into()); // política opt-in P119
    expected.insert("download_admission".into()); // historial inmutable de admisiones P119
    expected.insert("storage_volume_alert_state".into()); // nivel H09 duradero por volumen
    expected.insert("sqlite_sequence".into()); // AUTOINCREMENT
    let mut actual = tables(&db.pool).await;
    actual.insert("sqlite_sequence".into());
    assert_eq!(actual, expected);
    let applied = migrate::applied(&db.pool).await.unwrap();
    assert_eq!(applied.len(), 16);
    assert_eq!(applied[0].sha256, migrate::MIGRATIONS[0].sha256());
    assert_eq!(applied[1].version, "0002_adapter_selection");
    assert_eq!(applied[1].sha256, migrate::MIGRATIONS[1].sha256());
    assert_eq!(applied[2].version, "0003_runtime_channel_identity");
    assert_eq!(applied[2].sha256, migrate::MIGRATIONS[2].sha256());
    assert_eq!(applied[3].version, "0004_runtime_profile_locks");
    assert_eq!(applied[3].sha256, migrate::MIGRATIONS[3].sha256());
    assert_eq!(applied[4].version, "0005_runtime_profile_qualifications");
    assert_eq!(applied[4].sha256, migrate::MIGRATIONS[4].sha256());
    assert_eq!(applied[5].version, "0006_p052_artifact_size_estimate");
    assert_eq!(applied[5].sha256, migrate::MIGRATIONS[5].sha256());
    assert_eq!(applied[6].version, "0007_p098_local_import_source");
    assert_eq!(applied[6].sha256, migrate::MIGRATIONS[6].sha256());
    assert_eq!(applied[7].version, "0008_p093_failed_recoverable");
    assert_eq!(applied[7].sha256, migrate::MIGRATIONS[7].sha256());
    assert_eq!(applied[8].version, "0009_p028_signed_adapter_registry");
    assert_eq!(applied[8].sha256, migrate::MIGRATIONS[8].sha256());
    assert_eq!(applied[9].version, "0010_p028_registry_url_binding");
    assert_eq!(applied[9].sha256, migrate::MIGRATIONS[9].sha256());
    assert_eq!(applied[10].version, "0011_p127_source_operational_snapshot");
    assert_eq!(applied[10].sha256, migrate::MIGRATIONS[10].sha256());
    assert_eq!(applied[11].version, "0012_p128_low_disk_transition");
    assert_eq!(applied[11].sha256, migrate::MIGRATIONS[11].sha256());
    assert_eq!(
        applied[12].version,
        "0013_p032_runtime_version_pinning_and_retention"
    );
    assert_eq!(applied[12].sha256, migrate::MIGRATIONS[12].sha256());
    assert_eq!(applied[14].version, "0015_p081_execution_host");
    assert_eq!(applied[15].version, "0016_p119_download_admission");
    assert_eq!(applied[13].sha256, migrate::MIGRATIONS[13].sha256());
    let url_binding_index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='adapter_registry_checkpoint_url_unique'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        url_binding_index, 1,
        "registro URL se enlaza a identidad única"
    );
    assert!(
        regex_ts(&applied[0].applied_at),
        "{}",
        applied[0].applied_at
    );
}

fn regex_ts(s: &str) -> bool {
    s.len() == 24
        && s.ends_with('Z')
        && s.as_bytes()[19] == b'.'
        && chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

#[tokio::test]
async fn reaplicar_migraciones_no_cambia_estado() {
    let (_d, path) = temp_path("idem.sqlite");
    let db = prepare(&path).await;
    let before = schema_sql(&db.pool).await;
    let meta_before = migrate::applied(&db.pool).await.unwrap();
    assert!(migrate::migrate(&db.pool).await.unwrap().is_empty());
    assert_eq!(before, schema_sql(&db.pool).await);
    assert_eq!(meta_before, migrate::applied(&db.pool).await.unwrap());
    // reabrir el fichero y migrar de nuevo tampoco cambia nada
    db.pool.close().await;
    let db = Db::open(&path).await.unwrap();
    assert!(migrate::migrate(&db.pool).await.unwrap().is_empty());
    assert_eq!(before, schema_sql(&db.pool).await);
}

#[tokio::test]
async fn migracion_que_falla_a_mitad_no_deja_rastro() {
    let (_d, path) = temp_path("atomic.sqlite");
    let db = prepare(&path).await;
    let broken = Migration {
        version: "9999_rota",
        sql: "CREATE TABLE tabla_nueva(id TEXT PRIMARY KEY) STRICT;\n\
              INSERT INTO tabla_nueva(id) VALUES ('a');\n\
              CREATE TABLE tabla_rota(id TEXT PRIMARY KEY, x TIPO_INEXISTENTE) STRICT;",
    };
    let before = schema_sql(&db.pool).await;
    let err = migrate::migrate_with(&db.pool, &[migrate::MIGRATIONS[0], broken])
        .await
        .expect_err("la migración rota debe fallar");
    assert_eq!(err.code, ErrorCode::Internal);
    assert_eq!(
        before,
        schema_sql(&db.pool).await,
        "ni tabla_nueva ni tabla_rota"
    );
    assert_eq!(
        migrate::applied_versions(&db.pool).await.unwrap(),
        vec![
            "0001_bootstrap",
            "0002_adapter_selection",
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
    // corregida, se aplica entera y una sola vez
    let fixed = Migration {
        version: "9999_rota",
        sql: "CREATE TABLE tabla_nueva(id TEXT PRIMARY KEY) STRICT;",
    };
    let applied = migrate::migrate_with(&db.pool, &[migrate::MIGRATIONS[0], fixed])
        .await
        .unwrap();
    assert_eq!(applied, vec!["9999_rota"]);
    assert!(tables(&db.pool).await.contains("tabla_nueva"));
}

#[tokio::test]
async fn migracion_aplicada_modificada_bloquea() {
    let (_d, path) = temp_path("drift.sqlite");
    let db = prepare(&path).await;
    let tampered = Migration {
        version: "0001_bootstrap",
        sql: "-- otro contenido\nSELECT 1;",
    };
    let err = migrate::migrate_with(&db.pool, &[tampered])
        .await
        .expect_err("drift");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.details.reason, "migration_checksum_mismatch");
}

#[tokio::test]
async fn rollback_transaccional_en_fallo_inducido() {
    let (_d, path) = temp_path("tx.sqlite");
    let db = prepare(&path).await;
    let result = with_transaction(&db.pool, |conn| {
        Box::pin(async move {
            sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES ('m1',?,?,1,'{}','M','{}','[]')")
                .bind(T).bind(T)
                .execute(&mut *conn).await
                .map_err(|e| LarmError::new(ErrorCode::Internal, "x", e.to_string()))?;
            // fallo inducido: row_version=0 viola CHECK(row_version>=1)
            sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES ('m2',?,?,0,'{}','M','{}','[]')")
                .bind(T).bind(T)
                .execute(&mut *conn).await
                .map_err(|e| LarmError::new(ErrorCode::Conflict, "constraint", e.to_string()))?;
            Ok(())
        })
    })
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::Conflict);
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM canonical_model")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(c, 0, "el rollback deshizo también la inserción válida");
    // y una transacción correcta sí persiste
    with_transaction(&db.pool, |conn| {
        Box::pin(async move {
            sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES ('m3',?,?,1,'{}','M','{}','[]')")
                .bind(T).bind(T)
                .execute(&mut *conn).await
                .map_err(|e| LarmError::new(ErrorCode::Internal, "x", e.to_string()))?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM canonical_model")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(c, 1);
}

#[tokio::test]
async fn fk_diferida_se_rechaza_al_confirmar() {
    let (_d, path) = temp_path("fkdef.sqlite");
    let db = prepare(&path).await;
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES ('d1',?,?,1,'{}','noexiste','nosrc','ns','mid')")
        .bind(T).bind(T)
        .execute(&mut *tx)
        .await
        .expect("diferida: la sentencia se acepta dentro de la transacción");
    let commit = tx.commit().await;
    assert!(commit.is_err(), "el COMMIT debe rechazar la FK huérfana");
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_distribution")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(c, 0);
    // borrar un padre con hijos tampoco se confirma
    seed_model_chain(&db.pool).await;
    let delete = sqlx::query("DELETE FROM canonical_model WHERE id='cm'")
        .execute(&db.pool)
        .await;
    assert!(delete.is_err(), "borrar un padre referenciado debe fallar");
}

#[tokio::test]
async fn h07_transformation_job_acepta_failed_recoverable() {
    let (_d, path) = temp_path("h07.sqlite");
    let db = prepare(&path).await;
    seed_model_chain(&db.pool).await;
    exec(&db.pool, "INSERT INTO transformation_job(id,created_at,updated_at,row_version,metadata,input_variant_id,adapter_id,tool_version,parameters,plan_hash,state,attempt,error) VALUES('tj','t','t',1,'{}','mv','conv','1.0','{}','ph','FAILED_RECOVERABLE',1,'{\"cause\":\"interrumpido\",\"resumable\":false}')").await;
    let bad = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO transformation_job(id,created_at,updated_at,row_version,metadata,input_variant_id,adapter_id,tool_version,parameters,plan_hash,state,attempt) VALUES('tj2','{T}','{T}',1,'{{}}','mv','conv','1.0','{{}}','ph','ESTADO_FANTASMA',0)")))
        .execute(&db.pool)
        .await;
    assert!(
        bad.is_err(),
        "CHECK(state) rechaza estados fuera del contrato"
    );
    let row = sqlx::query("SELECT state, attempt, json_extract(error,'$.cause') FROM transformation_job WHERE id='tj'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "FAILED_RECOVERABLE");
    assert_eq!(row.get::<i64, _>(1), 1);
    assert_eq!(row.get::<String, _>(2), "interrumpido");
}

#[tokio::test]
async fn enums_del_ddl_canales_estrategias_prioridad() {
    let (_d, path) = temp_path("enums.sqlite");
    let db = prepare(&path).await;
    seed_runtime_chain(&db.pool).await;

    // H01: los cinco canales
    for channel in ["stable", "prerelease", "nightly", "commit", "custom"] {
        sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES(?,?,?,1,'{}','rt',?,?,'NONE')")
            .bind(format!("rv-{channel}"))
            .bind(T).bind(T)
            .bind(channel)
            .bind(channel)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("canal {channel} debe aceptarse: {e}"));
    }
    let channels: Vec<String> =
        sqlx::query_scalar("SELECT channel FROM runtime_version ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(channels.len(), 5);
    let bad = sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('rv-bad',?,?,1,'{}','rt','1','canal_fantasma','NONE')")
        .bind(T).bind(T)
        .execute(&db.pool)
        .await;
    assert!(bad.is_err(), "canal inválido debe rechazarse");

    // H02: las quince estrategias de instalación
    let strategies = [
        "PREBUILT_BINARY",
        "GITHUB_RELEASE",
        "SOURCE_BUILD",
        "WINGET",
        "PIP",
        "UV",
        "CONDA",
        "MAMBA",
        "NUGET",
        "WSL",
        "CONTAINER",
        "CUSTOM",
        "MSI_MANAGED",
        "PYTHON_VENV",
        "EXTERNAL_ADOPTION",
    ];
    for (i, strategy) in strategies.iter().enumerate() {
        sqlx::query("INSERT INTO runtime_installation(id,created_at,updated_at,row_version,metadata,registration_mode,stage,runtime_version_id,strategy,state,ownership) VALUES(?,?,?,1,'{}','MANAGED','INSTALLED','rv-stable',?,'ACTIVE','LARM_MANAGED')")
            .bind(format!("ri-{i}"))
            .bind(T).bind(T)
            .bind(strategy)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("estrategia {strategy} debe aceptarse: {e}"));
    }
    let stored: BTreeSet<String> = sqlx::query_scalar("SELECT strategy FROM runtime_installation")
        .fetch_all(&db.pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(stored.len(), 15, "las quince se conservan sin sustituirse");
    let bad_strategy = sqlx::query("INSERT INTO runtime_installation(id,created_at,updated_at,row_version,metadata,registration_mode,stage,runtime_version_id,strategy,state,ownership) VALUES('ri-bad',?,?,1,'{}','MANAGED','INSTALLED','rv-stable','TORRENT','ACTIVE','LARM_MANAGED')")
        .bind(T).bind(T)
        .execute(&db.pool)
        .await;
    assert!(bad_strategy.is_err());

    // H08: prioridad de descarga BACKGROUND/NORMAL/HIGH
    seed_model_chain(&db.pool).await;
    insert_location(&db.pool, "sl", 1)
        .await
        .expect("storage_location");
    for (id, prio) in [("dj-h", "HIGH"), ("dj-n", "NORMAL"), ("dj-b", "BACKGROUND")] {
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress) VALUES(?,?,?,1,'{}',?,'mv','sl','ph','PLANNED',0,'{}')")
            .bind(id).bind(T).bind(T).bind(prio)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("prioridad {prio}: {e}"));
    }
    let bad_prio = sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress) VALUES('dj-x',?,?,1,'{}','URGENT','mv','sl','ph','PLANNED',0,'{}')")
        .bind(T).bind(T)
        .execute(&db.pool)
        .await;
    assert!(
        bad_prio.is_err(),
        "prioridad fuera del enum debe rechazarse"
    );
}

#[tokio::test]
async fn bytes_hasta_i64_max_como_string_y_negativos_rechazados() {
    let (_d, path) = temp_path("bytes.sqlite");
    let db = prepare(&path).await;
    let max = DecimalU64::parse("9223372036854775807").unwrap();
    insert_location(&db.pool, "sl-max", larm_db::dto::bytes_to_sql(max))
        .await
        .unwrap();
    let raw: i64 =
        sqlx::query_scalar("SELECT capacity_bytes FROM storage_location WHERE id='sl-max'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let back = larm_db::dto::bytes_from_sql("capacity_bytes", raw).unwrap();
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        "\"9223372036854775807\""
    );
    assert!(
        insert_location(&db.pool, "sl-neg", -1).await.is_err(),
        "CHECK(capacity_bytes>=0)"
    );
}

#[tokio::test]
async fn backup_y_restore_verificado() {
    let (_d, path) = temp_path("live.sqlite");
    let db = prepare(&path).await;
    seed_model_chain(&db.pool).await;
    let backup_path = path.with_extension("backup.sqlite");
    larm_db::backup::backup(&db.pool, &backup_path)
        .await
        .expect("backup");
    let ver = larm_db::backup::verify_backup(&db.pool, &backup_path)
        .await
        .unwrap();
    assert!(ver.is_valid(), "{ver:?}");
    assert_eq!(ver.backup_rows["model_variant"], 1);

    // la base cambia después del backup: la verificación lo detecta
    exec(&db.pool, "DELETE FROM model_variant").await;
    assert!(!larm_db::backup::verify_backup(&db.pool, &backup_path)
        .await
        .unwrap()
        .is_valid());

    // restaurar con la base cerrada devuelve las filas del backup
    db.pool.close().await;
    let report = larm_db::backup::restore(&backup_path, &path)
        .await
        .expect("restore");
    assert_eq!(report.integrity, "ok");
    assert_eq!(report.rows["model_variant"], 1);
    assert!(
        report.previous.as_ref().is_some_and(|p| p.exists()),
        "la base previa se conserva"
    );
    let db = Db::open(&path).await.unwrap();
    assert!(db.health().await.unwrap().is_healthy());
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_variant")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(c, 1);
    assert!(
        migrate::migrate(&db.pool).await.unwrap().is_empty(),
        "esquema restaurado al día"
    );
}

#[tokio::test]
async fn h01_conserva_los_cinco_canales_en_backup_y_restore() {
    let (_dir, path) = temp_path("runtime-channels.sqlite");
    let db = prepare(&path).await;
    seed_runtime_chain(&db.pool).await;

    for channel in ["stable", "prerelease", "nightly", "commit", "custom"] {
        sqlx::query("INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES(?,?,?,1,'{}','rt',?,?, 'NONE')")
            .bind(format!("rv-{channel}"))
            .bind(T)
            .bind(T)
            .bind("same-upstream-build")
            .bind(channel)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("canal {channel} debe persistirse: {e}"));
    }

    let expected: Vec<(String, String)> = sqlx::query_as(
        "SELECT version_string,channel FROM runtime_version WHERE runtime_id='rt' ORDER BY channel",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(expected.len(), 5, "los cinco canales quedan consultables");

    let backup = path.with_extension("backup.sqlite");
    larm_db::backup::backup(&db.pool, &backup)
        .await
        .expect("backup");
    db.pool.close().await;

    let restored = larm_db::backup::restore(&backup, &path)
        .await
        .expect("restore");
    assert_eq!(restored.integrity, "ok");
    assert_eq!(restored.rows["runtime_version"], 5);

    let db = Db::open(&path).await.unwrap();
    let actual: Vec<(String, String)> = sqlx::query_as(
        "SELECT version_string,channel FROM runtime_version WHERE runtime_id='rt' ORDER BY channel",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        actual, expected,
        "el restore no fusiona ni renombra canales"
    );
    assert!(
        migrate::migrate(&db.pool).await.unwrap().is_empty(),
        "el esquema restaurado sigue al día"
    );
}

#[tokio::test]
async fn restore_de_un_fichero_no_valido_no_toca_la_base() {
    let (_d, path) = temp_path("live.sqlite");
    let db = prepare(&path).await;
    seed_model_chain(&db.pool).await;
    db.pool.close().await;
    let junk = path.with_extension("junk");
    std::fs::write(&junk, b"no es una base de datos").unwrap();
    assert!(larm_db::backup::restore(&junk, &path).await.is_err());
    let missing = path.with_extension("no-existe");
    assert_eq!(
        larm_db::backup::restore(&missing, &path)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let db = Db::open(&path).await.unwrap();
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_variant")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(c, 1, "la base viva sigue intacta");
    assert!(!path.with_extension("sqlite.pre-restore").exists());
}

#[tokio::test]
async fn helper_de_base_temporal_migrada() {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    assert!(db.health().await.unwrap().is_healthy());
    assert_eq!(
        migrate::applied_versions(&db.pool).await.unwrap(),
        vec![
            "0001_bootstrap",
            "0002_adapter_selection",
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest",
            "0015_p081_execution_host",
            "0016_p119_download_admission"
        ]
    );
    let root = dir.path().to_path_buf();
    db.pool.close().await;
    drop(dir);
    assert!(!root.exists(), "la carpeta temporal se elimina");
}

#[tokio::test]
async fn restore_en_una_instalacion_nueva_sin_carpeta() {
    // La base se perdió con su carpeta (spec §134): el restore la recrea.
    let (_d, path) = temp_path("origen.sqlite");
    let db = prepare(&path).await;
    seed_model_chain(&db.pool).await;
    let backup = path.with_extension("bak");
    larm_db::backup::backup(&db.pool, &backup).await.unwrap();
    db.pool.close().await;
    let fresh = path
        .parent()
        .unwrap()
        .join("instalacion-nueva")
        .join("state")
        .join("larm.sqlite");
    let report = larm_db::backup::restore(&backup, &fresh).await.unwrap();
    assert_eq!(report.integrity, "ok");
    let restored = Db::open(&fresh).await.unwrap();
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_variant")
        .fetch_one(&restored.pool)
        .await
        .unwrap();
    assert_eq!(c, 1);
}
