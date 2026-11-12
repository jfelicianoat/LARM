//! Persistencia y finalización transaccional de jobs P042.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::model_verification::SqlModelVerificationJobs;
use larm_db::testing::temp_migrated;
use larm_domain::artifacts::ArtifactState;
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::bridge::{JobPhase, JobState};
use larm_domain::model::BundleState;
use larm_domain::model_verification::{
    LocationVerification, ModelVerificationJobs, ModelVerificationSubmitter, VerificationCommit,
};
use larm_domain::ports::{JobSource, MutationContext};
use std::collections::BTreeSet;

const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn principal(id: String) -> Principal {
    Principal {
        id,
        name: "operator".into(),
        kind: PrincipalKind::Operator,
        api_scopes: BTreeSet::from([ApiScope::ModelsWrite, ApiScope::Read]),
        mcp_scopes: BTreeSet::new(),
    }
}

async fn seed(pool: &sqlx::SqlitePool, root: &str) -> (String, String, String) {
    let variant = uuid::Uuid::now_v7().to_string();
    let artifact = uuid::Uuid::now_v7().to_string();
    let bundle = uuid::Uuid::now_v7().to_string();
    let storage = uuid::Uuid::now_v7().to_string();
    let location = uuid::Uuid::now_v7().to_string();
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
        .bind(HASH).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,format,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr','GGUF','DOWNLOADED','DOWNLOADED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[\"text.generate\"]','{}',0)")
        .bind(&variant).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,sha256,required) VALUES(?,'t','t',1,'{}',?,'model.bin',4,?,1)")
        .bind(&artifact).bind(&variant).bind(HASH).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,'t','t',1,'{}',?,'[]','[]',?,'INCOMPLETE')")
        .bind(&bundle).bind(&variant).bind(HASH).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','models',?,?, 'NTFS','LOCAL',100,100,0,0,0,'ONLINE','test-volume','UNKNOWN')")
        .bind(&storage).bind(root).bind(root).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership) VALUES(?,'t','t',1,'{}',?,?, 'model.bin',4,?,'UNKNOWN','LARM_MANAGED')")
        .bind(&location).bind(&artifact).bind(&storage).bind(HASH).execute(pool).await.unwrap();
    (variant, bundle, location)
}

fn commit(variant: &str, bundle: &str, location: &str) -> VerificationCommit {
    VerificationCommit {
        variant_id: variant.into(),
        expected_variant_row_version: 1,
        bundle_id: bundle.into(),
        expected_bundle_row_version: 1,
        bundle_state: BundleState::Verified,
        evidence: serde_json::json!({"verified_at":"2026-10-22T00:00:00Z","missing":[],"mismatched":[]}),
        result: serde_json::json!({"bundle_state":"VERIFIED"}),
        locations: vec![LocationVerification {
            id: location.into(),
            expected_row_version: 1,
            state: ArtifactState::Online,
            sha256: Some(HASH.into()),
            size_bytes: Some(4),
            verified_at: Some("2026-10-22T00:00:00Z".into()),
        }],
    }
}

#[tokio::test]
async fn job_deduplicates_and_commits_all_verification_state_atomically() {
    let (_dir, db) = temp_migrated().await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let (variant, bundle, location) = seed(&db.pool, &root.path().display().to_string()).await;
    let jobs = SqlModelVerificationJobs {
        pool: db.pool.clone(),
    };
    let ctx = ctx();
    let receipt = jobs.enqueue_verification(&variant, &ctx).await.unwrap();
    let duplicate = jobs.enqueue_verification(&variant, &ctx).await.unwrap();
    assert_eq!(receipt, duplicate);
    assert_eq!(receipt.state, JobPhase::Queued);
    let claim = jobs.claim_verification().await.unwrap().unwrap();
    assert_eq!(claim.job_id, receipt.job_id);

    sqlx::query("CREATE TRIGGER reject_bundle_update BEFORE UPDATE ON model_bundle BEGIN SELECT RAISE(ABORT,'test rollback'); END")
        .execute(&db.pool).await.unwrap();
    assert!(jobs
        .complete_verification(&claim, &commit(&variant, &bundle, &location))
        .await
        .is_err());
    let untouched: (String, i64, String) = sqlx::query_as(
        "SELECT l.state,l.row_version,j.phase FROM artifact_location l JOIN operation_journal j ON j.id=? WHERE l.id=?",
    )
    .bind(&receipt.job_id).bind(&location).fetch_one(&db.pool).await.unwrap();
    assert_eq!(untouched, ("UNKNOWN".into(), 1, "RUNNING".into()));
    sqlx::query("DROP TRIGGER reject_bundle_update")
        .execute(&db.pool)
        .await
        .unwrap();

    jobs.complete_verification(&claim, &commit(&variant, &bundle, &location))
        .await
        .unwrap();
    let variant_state: (String, String, String) =
        sqlx::query_as("SELECT lifecycle,availability,status FROM model_variant WHERE id=?")
            .bind(&variant)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        variant_state,
        ("VERIFIED".into(), "UNKNOWN".into(), "VERIFIED".into())
    );
    let bundle_state: String = sqlx::query_scalar("SELECT state FROM model_bundle WHERE id=?")
        .bind(&bundle)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(bundle_state, "VERIFIED");
    let location_state: (String, i64, Option<String>) =
        sqlx::query_as("SELECT state,row_version,verified_at FROM artifact_location WHERE id=?")
            .bind(&location)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(location_state.0, "ONLINE");
    assert_eq!(location_state.1, 2);
    assert!(location_state.2.is_some());
    let job: Option<JobState> = jobs
        .job(&principal(ctx.actor_id.clone()), &receipt.job_id)
        .await
        .unwrap();
    assert_eq!(job.unwrap().state, JobPhase::Completed);
}
