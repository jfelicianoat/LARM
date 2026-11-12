//! P028: replay, reservation, job sanitization and atomic import completion.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::adapter_imports::SqlAdapterImports;
use larm_domain::adapter_updates::{AdapterImport, AdapterImportPlan, UpdateStep as Step};
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::auth::{ApiScope, Principal, PrincipalKind};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterImportRepository, JobSource, MutationContext, VolumeDemand};
use larm_domain::units::DecimalU64;
use serde_json::json;
use sqlx::SqlitePool;

const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "adapter-import-operator".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn request() -> AdapterImport {
    AdapterImport {
        package_path: "C:/private/package.zip".into(),
        expected_hash: HASH.into(),
        approval_id: None,
    }
}

fn candidate(version: &str) -> AdapterInstallation {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: "acme.initial-import-fixture".into(),
        version: version.into(),
        family: "runtime".into(),
        package_hash: HASH.into(),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        manifest: json!({"fixture_only":"not qualified"}),
        state: AdapterState::Installed,
    }
}

fn plan(candidate: AdapterInstallation) -> AdapterImportPlan {
    AdapterImportPlan {
        candidate,
        workspace_path: "C:/managed/adapters/.staging/job".into(),
        destination_path: "C:/managed/adapters/acme.initial-import-fixture/1.0".into(),
        volume_key: "fixture-volume".into(),
        archive_bytes: DecimalU64::new(1024).unwrap(),
        unpacked_bytes: DecimalU64::new(2048).unwrap(),
        required_bytes: DecimalU64::new(3072 + (1 << 30)).unwrap(),
        approval_id: None,
    }
}

fn demand(plan: &AdapterImportPlan) -> VolumeDemand {
    VolumeDemand {
        volume_key: plan.volume_key.clone(),
        required_bytes: plan.required_bytes.get(),
        os_free: 10 << 30,
        policy_minimum: 0,
    }
}

async fn setup() -> (tempfile::TempDir, larm_db::pool::Db, SqlAdapterImports) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterImports {
        pool: db.pool.clone(),
    };
    (dir, db, repo)
}

async fn enqueue(repo: &SqlAdapterImports, key: &str) -> String {
    repo.enqueue_import(&request(), HASH, key, &ctx())
        .await
        .unwrap()
        .0
        .job_id
}

async fn transition_to_publishing(repo: &SqlAdapterImports, id: &str) {
    repo.advance_import(id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let p = plan(candidate("1.0"));
    repo.prepare_import(id, &p, &demand(&p), chrono::Utc::now())
        .await
        .unwrap();
    for (from, to) in [
        (Step::Reserved, Step::Extracting),
        (Step::Extracting, Step::Staged),
        (Step::Staged, Step::Publishing),
    ] {
        repo.advance_import(id, from, to).await.unwrap();
    }
}

async fn sql_counts(pool: &SqlitePool) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM adapter_installation), (SELECT count(*) FROM operation_journal WHERE kind='adapter.import'), (SELECT count(*) FROM volume_reservation), (SELECT count(*) FROM resource_lock)")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn importacion_persiste_replay_y_evento_de_aceptacion_atomicos() {
    let (_dir, db, repo) = setup().await;
    let mutation = ctx();
    let (receipt, created) = repo
        .enqueue_import(&request(), HASH, "import-key", &mutation)
        .await
        .unwrap();
    assert!(created);
    let (replay, created) = repo
        .enqueue_import(&request(), HASH, "import-key", &mutation)
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(receipt, replay);

    let events: Vec<(String, String)> =
        sqlx::query_as("SELECT event_type,correlation_id FROM event ORDER BY event_id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "adapter.import.queued");
    assert_eq!(events[0].1, mutation.correlation_id);

    let job = repo.import_job(&receipt.job_id).await.unwrap().unwrap();
    let public = serde_json::to_string(&job.public_state()).unwrap();
    assert!(!public.contains("C:/private"));
    assert!(!public.contains("package.zip"));
    assert_eq!(
        job.public_state().state,
        larm_domain::bridge::JobPhase::Queued
    );

    let different = AdapterImport {
        package_path: "C:/private/other.zip".into(),
        ..request()
    };
    let error = repo
        .enqueue_import(&different, HASH, "import-key", &mutation)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdempotencyConflict);
    assert_eq!(sql_counts(&db.pool).await, (1, 1, 0, 0));
}

#[tokio::test]
async fn commit_publica_instalacion_installed_y_evento_imported_con_limpieza_atomica() {
    let (_dir, db, repo) = setup().await;
    let id = enqueue(&repo, "complete-key").await;
    let p = plan(candidate("1.0"));
    let target_id = p.candidate.id.clone();
    // El fixture de plan debe referirse al mismo workspace que el job.
    let mut p = p;
    p.workspace_path = format!("C:/managed/adapters/.staging/{id}");
    p.destination_path = "C:/managed/adapters/acme.initial-import-fixture/1.0".into();
    repo.advance_import(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    repo.prepare_import(&id, &p, &demand(&p), chrono::Utc::now())
        .await
        .unwrap();
    for (from, to) in [
        (Step::Reserved, Step::Extracting),
        (Step::Extracting, Step::Staged),
        (Step::Staged, Step::Publishing),
    ] {
        repo.advance_import(&id, from, to).await.unwrap();
    }
    repo.complete_import(&id).await.unwrap();
    repo.complete_import(&id).await.unwrap();

    let row: (String, String) =
        sqlx::query_as("SELECT state,trust FROM adapter_installation WHERE id=?")
            .bind(&target_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(row, ("INSTALLED".into(), "UNTRUSTED".into()));
    let record = repo.import_job(&id).await.unwrap().unwrap();
    assert_eq!(record.step, Step::Completed);
    assert_eq!(
        record.public_state().resource_id.as_deref(),
        Some(target_id.as_str())
    );
    assert_eq!(sql_counts(&db.pool).await, (2, 1, 0, 0));
    let event_types: Vec<String> =
        sqlx::query_scalar("SELECT event_type FROM event ORDER BY event_id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_types, ["adapter.import.queued", "adapter.imported"]);
    let pending: Option<i64> = sqlx::query_scalar(
        "SELECT event_id FROM event_outbox WHERE delivered_at IS NULL ORDER BY event_id DESC LIMIT 1",
    )
    .fetch_optional(&db.pool)
    .await
    .unwrap();
    assert!(pending.is_some());
}

#[tokio::test]
async fn fallo_de_insert_conserva_journal_publishing_y_no_emite_imported() {
    let (_dir, db, repo) = setup().await;
    let id = enqueue(&repo, "fault-key").await;
    transition_to_publishing(&repo, &id).await;
    sqlx::raw_sql("CREATE TRIGGER reject_import BEFORE INSERT ON adapter_installation BEGIN SELECT RAISE(ABORT,'P028 fixture'); END;")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo.complete_import(&id).await.is_err());
    let record = repo.import_job(&id).await.unwrap().unwrap();
    assert_eq!(record.step, Step::Publishing);
    assert_eq!(sql_counts(&db.pool).await, (1, 1, 1, 2));
    let events: Vec<String> = sqlx::query_scalar("SELECT event_type FROM event ORDER BY event_id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, ["adapter.import.queued"]);
}

#[tokio::test]
async fn fallo_prepublicacion_marca_failed_y_libera_reservas_y_locks() {
    let (_dir, db, repo) = setup().await;
    let id = enqueue(&repo, "failure-key").await;
    repo.advance_import(&id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let error = LarmError::new(ErrorCode::IntegrityFailed, "ZIP inválido", "zip_invalid");
    repo.fail_import(&id, &error).await.unwrap();
    repo.fail_import(&id, &error).await.unwrap();
    assert_eq!(
        repo.import_job(&id).await.unwrap().unwrap().step,
        Step::Failed
    );
    assert_eq!(sql_counts(&db.pool).await, (1, 1, 0, 0));
    let event: String =
        sqlx::query_scalar("SELECT event_type FROM event ORDER BY event_id DESC LIMIT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event, "adapter.import.failed");
}

#[tokio::test]
async fn solo_propietario_o_admin_puede_leer_el_job() {
    let (_dir, _db, repo) = setup().await;
    let mutation = ctx();
    let (receipt, _) = repo
        .enqueue_import(&request(), HASH, "owner-key", &mutation)
        .await
        .unwrap();
    let owner = Principal {
        id: mutation.actor_id,
        name: "owner".into(),
        kind: PrincipalKind::Operator,
        api_scopes: Default::default(),
        mcp_scopes: Default::default(),
    };
    let stranger = Principal {
        id: "another-actor".into(),
        ..owner.clone()
    };
    let admin = Principal {
        api_scopes: [ApiScope::Admin].into_iter().collect(),
        ..stranger.clone()
    };
    assert!(repo.job(&owner, &receipt.job_id).await.unwrap().is_some());
    assert!(repo
        .job(&stranger, &receipt.job_id)
        .await
        .unwrap()
        .is_none());
    assert!(repo.job(&admin, &receipt.job_id).await.unwrap().is_some());
}
