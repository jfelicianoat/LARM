//! SQLite and synthetic completed-acquisition evidence only. No physical bytes,
//! native worker, qualification, conversion or P093 acceptance is claimed here.
#![allow(clippy::unwrap_used)]
use larm_db::{pool::Db, transformation_inputs::SqlTransformationInputs};
use larm_domain::{
    download::{DownloadPlanRecord, DownloadPlanRequest, DownloadPlanResponse, DownloadPriority},
    model::{bundle_hash, required_hash, BundleEntry, ModelLimits},
    units::DecimalU64,
};
use serde_json::{json, Value};

#[path = "support/acquisition_fixture.rs"]
mod fixture;
use fixture::id;

struct Fixture {
    directory: tempfile::TempDir,
    db: Db,
    owner: String,
    child: String,
    artifact: String,
    plan: String,
    job: String,
    payload: Value,
    repo: SqlTransformationInputs,
}
impl Fixture {
    async fn new() -> Self {
        let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let snapshot = fixture::seed(&db.pool).await;
        fixture::repo(&db.pool)
            .record(&snapshot, &fixture::ctx())
            .await
            .unwrap();
        let root = id();
        let plan = id();
        let job = id();
        let child = id();
        let artifact = snapshot.artifacts[0].artifact_id.clone();
        let own = id();
        let at = snapshot.observed_at.clone();
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,?,?,1,'{}','Fixture','C:/fixture','C:/fixture','NTFS','LOCAL',1000,900,0,0,0,'ONLINE','fixture-volume','UNKNOWN')")
            .bind(&root).bind(&at).bind(&at).execute(&db.pool).await.unwrap();
        let request = DownloadPlanRequest {
            variant_id: snapshot.variant_id.clone(),
            storage_location_id: Some(root.clone()),
            hard_budget_bytes: DecimalU64::new(100),
            source_id: Some(snapshot.source_id.clone()),
            priority: DownloadPriority::Normal,
            optional_artifact_ids: vec![],
        };
        let mut record = DownloadPlanRecord {
            revision_id: snapshot.revision_id.clone(),
            revision_key: snapshot.source_reference.revision_key.clone(),
            manifest_hash: snapshot.upstream_manifest_hash.clone(),
            storage_observed_at: at.clone(),
            response: DownloadPlanResponse {
                acquisition: Some(snapshot.binding()),
                preview_id: id(),
                preview_hash: "d".repeat(64),
                variant_id: snapshot.variant_id.clone(),
                optional_artifact_ids: vec![],
                artifact_estimates: snapshot.download_estimates(&[]).unwrap(),
                destination_options: vec![],
                selected_storage_location_id: Some(root),
                plan_id: Some(plan.clone()),
                plan_hash: None,
                storage_estimate: None,
                source_id: Some(snapshot.source_id.clone()),
                source_candidates: vec![],
                hard_budget_bytes: request.hard_budget_bytes,
                priority: request.priority,
                expires_at: "2026-10-27T16:05:00.000Z".into(),
                operator_confirmation_required: true,
            },
            request,
        };
        let seal = record.sealed_hash(true).unwrap();
        record.response.plan_hash = Some(seal.clone());
        snapshot.validate_download_record(&record).unwrap();
        let payload = json!({"record":record,"execution":{"job_id":job,"result":{"artifacts":[{
            "artifact_id":artifact,"sha256":"a".repeat(64),"size_bytes":"19"
        }]}}});
        sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'download.plan','[]','COMPLETED',?,?,?)")
            .bind(&plan).bind(&seal).bind(payload.to_string()).bind(&at).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,1,?,'NORMAL',?,? ,?,'COMPLETED',1,'{}',NULL)")
            .bind(&job).bind(&at).bind(&at).bind(json!({"plan_id":plan}).to_string())
            .bind(&snapshot.variant_id).bind(record.request.storage_location_id.as_ref().unwrap())
            .bind(&seal).execute(&db.pool).await.unwrap();
        let entry = BundleEntry {
            relative_path: "weights.bin".into(),
            sha256: "a".repeat(64),
            size_bytes: 19,
            required: true,
        };
        let original_hash = bundle_hash(std::slice::from_ref(&entry)).unwrap();
        sqlx::query("UPDATE model_variant SET lifecycle='VERIFIED',status='VERIFIED',bundle_hash=?,model_limits=? WHERE id=?")
            .bind(&original_hash).bind(json!(ModelLimits::default()).to_string()).bind(&snapshot.variant_id).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
            .bind(id()).bind(&at).bind(&at).bind(&snapshot.variant_id).bind(json!([artifact]).to_string())
            .bind(&original_hash).execute(&db.pool).await.unwrap();
        let entries = [
            BundleEntry {
                required: false,
                ..entry
            },
            BundleEntry {
                relative_path: "config.bin".into(),
                sha256: "c".repeat(64),
                size_bytes: 7,
                required: true,
            },
        ];
        sqlx::query("INSERT INTO model_variant SELECT update_available,status,?,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,?,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits FROM model_variant WHERE id=?")
            .bind(&child).bind(required_hash(&entries).unwrap()).bind(&snapshot.variant_id).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,sha256,required) VALUES(?,?,?,1,'{}',?,'config.bin',7,?,1)")
            .bind(&own).bind(&at).bind(&at).bind(&child).bind("c".repeat(64)).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO artifact_reference(artifact_id,variant_id) VALUES(?,?)")
            .bind(&artifact)
            .bind(&child)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,?, 'VERIFIED')")
            .bind(id()).bind(&at).bind(&at).bind(&child).bind(json!([own]).to_string()).bind(json!([artifact]).to_string())
            .bind(bundle_hash(&entries).unwrap()).execute(&db.pool).await.unwrap();
        let repo = SqlTransformationInputs {
            pool: db.pool.clone(),
        };
        Self {
            directory,
            db,
            owner: snapshot.variant_id,
            child,
            artifact,
            plan,
            job,
            payload,
            repo,
        }
    }
    async fn set_payload(&self, payload: &Value) {
        sqlx::query("UPDATE operation_journal SET payload=? WHERE id=?")
            .bind(payload.to_string())
            .bind(&self.plan)
            .execute(&self.db.pool)
            .await
            .unwrap();
    }
    async fn close(self) {
        self.db.pool.close().await;
        self.directory.close().unwrap();
    }
}

#[tokio::test]
async fn unknown_shared_descriptor_uses_completed_owner_evidence_and_the_consumers_role() {
    let f = Fixture::new().await;
    let owner = f.repo.load(&f.owner).await.unwrap();
    assert_eq!(owner.artifacts[0].sha256, "a".repeat(64));
    let child = f.repo.load(&f.child).await.unwrap();
    assert_eq!(child.artifacts.len(), 2);
    let shared = child
        .artifacts
        .iter()
        .find(|a| a.artifact_id == f.artifact)
        .unwrap();
    assert_eq!(shared.sha256, "a".repeat(64));
    assert_eq!(shared.size_bytes.get(), 19);
    assert_eq!(
        child.bundle.optional_artifact_ids.as_slice(),
        std::slice::from_ref(&f.artifact)
    );
    let declaration: (Option<String>, Option<i64>) =
        sqlx::query_as("SELECT sha256,expected_size FROM model_artifact WHERE id=?")
            .bind(&f.artifact)
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
    assert_eq!(declaration, (None, None));
    let effects:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)")
        .fetch_one(&f.db.pool).await.unwrap();
    assert_eq!(effects, (0, 0, 0));
    f.close().await;
}

#[tokio::test]
async fn incomplete_or_tampered_owner_evidence_cannot_authorize_a_reference() {
    let f = Fixture::new().await;
    for (pointer, value) in [
        ("/execution/job_id", json!(id())),
        ("/execution/result/artifacts", json!([])),
        (
            "/execution/result/artifacts/0/sha256",
            json!("b".repeat(64)),
        ),
        ("/execution/result/artifacts/0/artifact_id", json!(id())),
        ("/record/manifest_hash", json!("f".repeat(64))),
    ] {
        let mut changed = f.payload.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        f.set_payload(&changed).await;
        assert!(f.repo.load(&f.child).await.is_err(), "{pointer}");
    }
    f.set_payload(&f.payload).await;
    sqlx::query("UPDATE download_job SET state='RUNNING' WHERE id=?")
        .bind(&f.job)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.repo.load(&f.child).await.is_err());
    sqlx::query("UPDATE download_job SET state='COMPLETED' WHERE id=?")
        .bind(&f.job)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.repo.load(&f.child).await.is_ok());
    sqlx::query("DELETE FROM artifact_reference WHERE variant_id=?")
        .bind(&f.child)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.repo.load(&f.child).await.is_err());
    f.close().await;
}

#[tokio::test]
async fn conflicting_completed_descriptors_for_one_unknown_artifact_remain_ambiguous() {
    let f = Fixture::new().await;
    let job = id();
    let plan = id();
    let mut other = f.payload.clone();
    other["record"]["response"]["plan_id"] = json!(plan);
    other["execution"]["job_id"] = json!(job);
    other["execution"]["result"]["artifacts"][0]["sha256"] = json!("b".repeat(64));
    sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) SELECT ?,kind,resource_ids,phase,plan_hash,?,updated_at FROM operation_journal WHERE id=?")
        .bind(&plan).bind(other.to_string()).bind(&f.plan).execute(&f.db.pool).await.unwrap();
    sqlx::query("INSERT INTO download_job SELECT ?,created_at,updated_at,row_version,?,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error FROM download_job WHERE id=?")
        .bind(&job).bind(json!({"plan_id":plan}).to_string()).bind(&f.job).execute(&f.db.pool).await.unwrap();
    let different = BundleEntry {
        relative_path: "weights.bin".into(),
        sha256: "b".repeat(64),
        size_bytes: 19,
        required: true,
    };
    sqlx::query("INSERT INTO model_bundle SELECT ?,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,?,state FROM model_bundle WHERE variant_id=?")
        .bind(id()).bind(bundle_hash(&[different]).unwrap()).bind(&f.owner).execute(&f.db.pool).await.unwrap();
    assert_eq!(
        f.repo.load(&f.child).await.err().unwrap().details.reason,
        "transformation_input_evidence_ambiguous"
    );
    f.close().await;
}
