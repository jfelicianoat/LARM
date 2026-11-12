//! SQLite/CODE_PASS fixture only; never qualifies a worker.
#![allow(dead_code)]
use larm_db::acquisition::SqlAcquisitionManifests;
use larm_domain::acquisition::{
    ModelAcquisitionArtifact, ModelAcquisitionManifest, SourceCatalogReference,
};
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{committed_installation_hash, Receipt, METADATA_KEY};
use larm_domain::model::ArtifactSizeEstimate;
use larm_domain::ports::{AdapterInstallationRepository, MutationContext};
use serde_json::json;
use sqlx::SqlitePool;

#[path = "../../../../tests/fixtures/adapters/conformance_persistence.rs"]
mod persistence;

pub fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
pub fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "acquisition-fixture".into(),
        correlation_id: id(),
    }
}

pub async fn seed(pool: &SqlitePool) -> ModelAcquisitionManifest {
    let now = "2026-10-27T16:00:00.000Z";
    let installation = AdapterInstallation {
        id: id(),
        created_at: now.into(),
        updated_at: now.into(),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "fixture.acquisition".into(),
        version: "1.0".into(),
        family: "source".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Verified,
        manifest: json!({"id":"fixture.acquisition","version":"1.0","family":"source",
            "publisher":"fixture","package_sha256":"a".repeat(64),"permissions":{},"capabilities":[]}),
        state: AdapterState::Installed,
    };
    larm_db::adapters::SqlAdapterInstallations { pool: pool.clone() }
        .insert(&installation, &ctx())
        .await
        .unwrap();
    let installation = persistence::qualify_fixture(pool, &installation, &ctx()).await;
    let receipt: Receipt =
        serde_json::from_value(installation.metadata[METADATA_KEY].clone()).unwrap();
    let source_id = id();
    let adapter_id = id();
    let model_id = id();
    let distribution_id = id();
    let revision_id = id();
    let variant_id = id();
    let artifact_id = id();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES(?,?,?,1,'{}',?,'HTTP','[]')")
        .bind(&adapter_id).bind(now).bind(now).bind(&installation.id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES(?,?,?,1,'{}','Fixture',?,'https://example.invalid',0,1,'NONE',NULL,'{}')")
        .bind(&source_id).bind(now).bind(now).bind(&adapter_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES(?,?,?,1,'{}','Remote model','[]','[]')")
        .bind(&model_id).bind(now).bind(now).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES(?,?,?,1,'{}',?,?,'fixture','model')")
        .bind(&distribution_id).bind(now).bind(now).bind(&model_id).bind(&source_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}',?,'r1',?,'CANDIDATE')")
        .bind(&revision_id).bind(now).bind(now).bind(&distribution_id).bind("b".repeat(64)).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,?,?,1,'{}',?,'DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
        .bind(&variant_id).bind(now).bind(now).bind(&revision_id).execute(pool).await.unwrap();
    let estimate = ArtifactSizeEstimate::unknown("not_reported", "fixture:source-manifest");
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,actual_size,sha256,required,size_estimate) VALUES(?,?,?,1,'{\"role\":\"weights\"}',?,'weights.bin',NULL,NULL,NULL,1,?)")
        .bind(&artifact_id).bind(now).bind(now).bind(&variant_id).bind(json!(estimate).to_string()).execute(pool).await.unwrap();
    ModelAcquisitionManifest {
        id: id(),
        variant_id,
        revision_id,
        source_id: source_id.clone(),
        source_reference: SourceCatalogReference {
            source_id,
            upstream_namespace: "fixture".into(),
            upstream_model_id: "model".into(),
            revision_key: "r1".into(),
        },
        upstream_manifest_hash: "b".repeat(64),
        snapshot_hash: String::new(),
        observed_at: now.into(),
        required_artifact_ids: vec![artifact_id.clone()],
        optional_artifact_ids: vec![],
        artifacts: vec![ModelAcquisitionArtifact {
            artifact_id,
            relative_path: "weights.bin".into(),
            size_bytes: None,
            sha256: None,
            required: true,
            role: "weights".into(),
            size_estimate: estimate,
        }],
        adapter_installation_id: installation.id.clone(),
        adapter_installation_revision: installation.row_version.to_string(),
        adapter_installation_sha256: committed_installation_hash(&installation).unwrap(),
        conformance_run_id: receipt.run_id,
        conformance_run_sha256: receipt.run_sha256,
    }
    .seal()
    .unwrap()
}

pub fn repo(pool: &SqlitePool) -> SqlAcquisitionManifests {
    SqlAcquisitionManifests {
        pool: pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    }
}
