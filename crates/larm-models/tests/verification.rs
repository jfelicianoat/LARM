//! Verificación completa sobre bytes reales y SQLite real (P042).
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use larm_db::artifacts::SqlArtifactLocations;
use larm_db::locations::SqlStorageLocations;
use larm_db::model_verification::SqlModelVerificationJobs;
use larm_db::testing::temp_migrated;
use larm_db::variants::SqlVariants;
use larm_domain::model::{bundle_hash, BundleEntry};
use larm_domain::model_verification::{ModelVerificationSubmitter, VERIFY_JOB_KIND};
use larm_domain::ports::{MutationContext, StorageProber};
use larm_domain::storage::StorageObservation;
use larm_models::verification::{ModelVerificationWorker, VerificationPorts};
use sha2::Digest;
use std::sync::Arc;

struct EmptyObservation;

#[async_trait]
impl StorageProber for EmptyObservation {
    async fn probe(
        &self,
        _: &[String],
    ) -> Result<StorageObservation, larm_domain::bridge::ApiError> {
        Ok(StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        })
    }
}

async fn seed(pool: &sqlx::SqlitePool, root: &str, bytes: &[u8]) -> (String, String, String) {
    let variant = uuid::Uuid::now_v7().to_string();
    let artifact = uuid::Uuid::now_v7().to_string();
    let bundle = uuid::Uuid::now_v7().to_string();
    let storage = uuid::Uuid::now_v7().to_string();
    let location = uuid::Uuid::now_v7().to_string();
    let sha256 = hex::encode(sha2::Sha256::digest(bytes));
    let manifest_hash = bundle_hash(&[BundleEntry {
        relative_path: "data.bin".into(),
        sha256: sha256.clone(),
        size_bytes: bytes.len() as u64,
        required: true,
    }])
    .unwrap();
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','{}')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','model','[]','[]')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','source','msa','https://example.invalid',0,1,'NONE','{}')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm','ms','ns','model')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1',?,'CURRENT')")
        .bind(&manifest_hash).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,format,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available,bundle_hash) VALUES(?,'t','t',1,'{}','mr','GGUF','DOWNLOADED','DOWNLOADED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[\"text.generate\"]','{}',0,?)")
        .bind(&variant).bind(&manifest_hash).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,actual_size,sha256,required) VALUES(?,'t','t',1,'{\"role\":\"weights\"}',?,'data.bin',?,?,?,1)")
        .bind(&artifact).bind(&variant).bind(bytes.len() as i64).bind(bytes.len() as i64).bind(&sha256).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,'t','t',1,'{}',?,?,'[]',?,'INCOMPLETE')")
        .bind(&bundle).bind(&variant).bind(serde_json::json!([artifact]).to_string()).bind(&manifest_hash).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','models',?,?, 'NTFS','LOCAL',100,100,0,0,0,'ONLINE','test-volume','UNKNOWN')")
        .bind(&storage).bind(root).bind(root).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership) VALUES(?,'t','t',1,'{}',?,?, 'data.bin',?,?, 'UNKNOWN','LARM_MANAGED')")
        .bind(&location).bind(&artifact).bind(&storage).bind(bytes.len() as i64).bind(&sha256).execute(pool).await.unwrap();
    (variant, bundle, location)
}

#[tokio::test]
async fn worker_verifies_bytes_and_marks_a_later_mismatch_corrupt() {
    let (_db_dir, db) = temp_migrated().await.unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("data.bin"), b"good").unwrap();
    let (variant, bundle, location) =
        seed(&db.pool, &root.path().display().to_string(), b"good").await;
    let jobs = Arc::new(SqlModelVerificationJobs {
        pool: db.pool.clone(),
    });
    let worker = ModelVerificationWorker::new(VerificationPorts {
        jobs: jobs.clone(),
        variants: Arc::new(SqlVariants {
            pool: db.pool.clone(),
        }),
        artifact_locations: Arc::new(SqlArtifactLocations {
            pool: db.pool.clone(),
        }),
        storage_locations: Arc::new(SqlStorageLocations {
            pool: db.pool.clone(),
        }),
        prober: Arc::new(EmptyObservation),
    });
    let ctx = MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let first = jobs.enqueue_verification(&variant, &ctx).await.unwrap();
    assert_eq!(first.state, larm_domain::bridge::JobPhase::Queued);
    assert!(worker.run_once().await.unwrap());
    let verified: (String, String, String) = sqlx::query_as(
        "SELECT v.lifecycle,v.availability,b.state FROM model_variant v JOIN model_bundle b ON b.variant_id=v.id WHERE v.id=?",
    ).bind(&variant).fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        verified,
        ("VERIFIED".into(), "UNKNOWN".into(), "VERIFIED".into())
    );
    let online: String = sqlx::query_scalar("SELECT state FROM artifact_location WHERE id=?")
        .bind(&location)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(online, "ONLINE");

    std::fs::write(root.path().join("data.bin"), b"evil").unwrap();
    let second = jobs.enqueue_verification(&variant, &ctx).await.unwrap();
    assert_ne!(first.job_id, second.job_id);
    assert!(worker.run_once().await.unwrap());
    let corrupt: (String, String, String) = sqlx::query_as(
        "SELECT v.lifecycle,v.availability,b.state FROM model_variant v JOIN model_bundle b ON b.variant_id=v.id WHERE v.id=?",
    ).bind(&variant).fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        corrupt,
        ("CORRUPT".into(), "CORRUPT".into(), "CORRUPT".into())
    );
    let location_state: String =
        sqlx::query_scalar("SELECT state FROM artifact_location WHERE id=?")
            .bind(&location)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(location_state, "CORRUPT");
    let result: String = sqlx::query_scalar("SELECT json_extract(payload,'$.result.bundle_state') FROM operation_journal WHERE kind=? AND id=?")
        .bind(VERIFY_JOB_KIND).bind(second.job_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(result, "CORRUPT");
    assert_eq!(
        bundle,
        sqlx::query_scalar::<_, String>("SELECT id FROM model_bundle WHERE variant_id=?")
            .bind(&variant)
            .fetch_one(&db.pool)
            .await
            .unwrap()
    );
}
