#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use larm_db::artifacts::SqlArtifactLocations;
use larm_db::locations::SqlStorageLocations;
use larm_db::model_import::SqlModelImportJobs;
use larm_db::models::SqlModels;
use larm_db::variants::SqlVariants;
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::bridge::JobPhase;
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::model_import::{
    ModelImportMode, ModelImportRequest, ModelImportSourceResolver, ModelImportSubmitter,
};
use larm_domain::ports::{
    ArtifactLocationRepository, JobSource, ModelRepository, MutationContext,
    StorageLocationRepository, VariantRepository,
};
use larm_domain::storage::PerformanceClass;
use larm_domain::units::DecimalU64;
use larm_models::import_worker::{ModelImportPorts, ModelImportWorker};

const ACTOR: &str = "0192f000-0000-7000-8000-000000000001";
const LOCATION: &str = "0192f000-0000-7000-8000-00000000c001";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: ACTOR.into(),
        correlation_id: "0192f000-0000-7000-8000-000000000002".into(),
    }
}

fn gguf(name: &str) -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(2u64.to_le_bytes());
    for (key, value) in [("general.architecture", "qwen2"), ("general.name", name)] {
        bytes.extend((key.len() as u64).to_le_bytes());
        bytes.extend(key.as_bytes());
        bytes.extend(8u32.to_le_bytes());
        bytes.extend((value.len() as u64).to_le_bytes());
        bytes.extend(value.as_bytes());
    }
    bytes.extend([1u8; 16]);
    bytes
}

fn location(root: &Path) -> StorageLocation {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    StorageLocation {
        id: LOCATION.into(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        name: "Modelos locales".into(),
        root_path: root.display().to_string(),
        canonical_path: std::fs::canonicalize(root).unwrap().display().to_string(),
        volume_guid: None,
        drive_letter: None,
        unc_path: None,
        filesystem: "NTFS".into(),
        storage_type: "LOCAL".into(),
        capacity_bytes: DecimalU64::new(1 << 40).unwrap(),
        free_bytes: DecimalU64::new(1 << 39).unwrap(),
        reserved_bytes: DecimalU64::default(),
        priority: 0,
        auto_placement: true,
        state: LocationState::Online,
        last_seen_at: None,
        volume_key: "volume-local-import".into(),
        performance_class: PerformanceClass::Ssd,
    }
}

fn principal() -> Principal {
    Principal::new(
        ACTOR,
        "local-import-test",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap()
}

#[tokio::test]
async fn job_persistido_importa_una_carpeta_verificada_y_devuelve_su_resultado() {
    let (_db_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let files = tempfile::tempdir().unwrap();
    let root = files.path().join("modelos");
    let source_dir = root.join("qwen");
    std::fs::create_dir_all(&source_dir).unwrap();
    let model_path = source_dir.join("model-Q4_K_M.gguf");
    let model_bytes = gguf("Qwen Import Worker");
    std::fs::write(&model_path, &model_bytes).unwrap();

    let ct2_dir = source_dir.join("ct2");
    std::fs::create_dir_all(&ct2_dir).unwrap();
    std::fs::write(ct2_dir.join("config.json"), br#"{"model_type":"nllb"}"#).unwrap();
    let mut ct2_model = 6u32.to_le_bytes().to_vec();
    ct2_model.extend(12u16.to_le_bytes());
    ct2_model.extend(b"transformer\0");
    ct2_model.extend(1u32.to_le_bytes());
    ct2_model.extend(1u32.to_le_bytes());
    std::fs::write(ct2_dir.join("model.bin"), ct2_model).unwrap();
    std::fs::write(ct2_dir.join("source_vocabulary.json"), b"{}").unwrap();

    let ncnn_dir = source_dir.join("ncnn");
    std::fs::create_dir_all(&ncnn_dir).unwrap();
    std::fs::write(
        ncnn_dir.join("net.param"),
        b"7767517\n1 1\nInput input 0 1 data\n",
    )
    .unwrap();
    std::fs::write(ncnn_dir.join("net.bin"), b"raw ncnn weights").unwrap();

    let mnn_dir = source_dir.join("mnn");
    std::fs::create_dir_all(&mnn_dir).unwrap();
    std::fs::write(mnn_dir.join("config.json"), b"{}").unwrap();
    std::fs::write(
        mnn_dir.join("llm.mnn"),
        [12, 0, 0, 0, 6, 0, 4, 0, 0, 0, 0, 0, 8, 0, 0, 0],
    )
    .unwrap();
    std::fs::write(mnn_dir.join("llm.mnn.weight"), b"external weights").unwrap();
    std::fs::write(mnn_dir.join("llm_config.json"), b"{}").unwrap();
    std::fs::write(mnn_dir.join("tokenizer.txt"), b"tokens").unwrap();
    std::fs::write(mnn_dir.join("embeddings_bf16.bin"), b"embeddings").unwrap();

    let location = location(&root);
    SqlStorageLocations {
        pool: db.pool.clone(),
    }
    .insert(&location, &ctx())
    .await
    .unwrap();

    let jobs = Arc::new(SqlModelImportJobs {
        pool: db.pool.clone(),
    });
    let request = ModelImportRequest {
        source_path: source_dir.display().to_string(),
        storage_location_id: LOCATION.into(),
        mode: ModelImportMode::Adopt,
        format: None,
        license_id: Some("license-local".into()),
    };
    let receipt = jobs
        .enqueue_import(&request, "import-worker-1", &"e".repeat(64), &ctx())
        .await
        .unwrap();
    let source_bytes_before = std::fs::read(&model_path).unwrap();
    let worker = ModelImportWorker::new(ModelImportPorts {
        jobs: jobs.clone(),
        source: jobs.clone() as Arc<dyn ModelImportSourceResolver>,
        models: Arc::new(SqlModels {
            pool: db.pool.clone(),
        }) as Arc<dyn ModelRepository>,
        variants: Arc::new(SqlVariants {
            pool: db.pool.clone(),
        }) as Arc<dyn VariantRepository>,
        artifact_locations: Arc::new(SqlArtifactLocations {
            pool: db.pool.clone(),
        }) as Arc<dyn ArtifactLocationRepository>,
        storage_locations: Arc::new(SqlStorageLocations {
            pool: db.pool.clone(),
        }) as Arc<dyn StorageLocationRepository>,
    });

    assert!(worker.run_once().await.unwrap());
    assert!(!worker.run_once().await.unwrap());
    let job = jobs
        .job(&principal(), &receipt.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.state, JobPhase::Completed);
    let variants = job.result.as_ref().unwrap()["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 4);
    let mut formats: Vec<_> = variants
        .iter()
        .map(|variant| variant["format"].as_str().unwrap())
        .collect();
    formats.sort_unstable();
    assert_eq!(formats, ["CTranslate2", "GGUF", "MNN", "NCNN"]);
    assert_eq!(job.result.as_ref().unwrap()["hashed"], 12);
    assert_eq!(std::fs::read(&model_path).unwrap(), source_bytes_before);
    let canonical_source = std::fs::canonicalize(&source_dir).unwrap();
    let source_url = url::Url::from_file_path(canonical_source)
        .unwrap()
        .to_string();
    let source_id: String = sqlx::query_scalar(
        "SELECT id FROM model_source WHERE base_url=? AND json_extract(policy,'$.automatic')=1",
    )
    .bind(source_url)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let distribution_sources: Vec<String> =
        sqlx::query_scalar("SELECT source_id FROM model_distribution")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(distribution_sources.len(), 4);
    assert!(distribution_sources.iter().all(|id| id == &source_id));
    let licenses: Vec<Option<String>> =
        sqlx::query_scalar("SELECT license_id FROM model_distribution WHERE source_id=?")
            .bind(&source_id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(licenses.len(), 4);
    assert!(licenses
        .iter()
        .all(|license| license.as_deref() == Some("license-local")));
}
