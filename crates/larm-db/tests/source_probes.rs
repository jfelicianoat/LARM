//! Journal durable P045 para el sondeo source, idempotencia y redacción.
#![allow(clippy::unwrap_used, clippy::panic)]

use larm_db::source_probes::SqlSourceProbes;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::JobPhase;
use larm_domain::error::ErrorCode;
use larm_domain::hardware::Probe;
use larm_domain::ports::{
    AdapterInstallationRepository, MutationContext, SourceCatalogRepository, SourceProbeRepository,
};
use larm_domain::source_probes::SourceAuthProbeResult;
use larm_domain::sources::{AuthMode, ModelSourceCreate};
use serde_json::{json, Map};

fn ctx(actor: &str) -> MutationContext {
    MutationContext {
        actor_id: actor.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn request() -> Probe {
    Probe {
        resource_id: None,
        profile_id: None,
    }
}

async fn source(db: &larm_db::pool::Db) -> String {
    let installed = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-24T00:00:00.000Z".into(),
        updated_at: "2026-10-24T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        adapter_id: "fixture.source-probe".into(),
        version: "1.0.0".into(),
        family: "source".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.source-probe","version":"1.0.0","family":"source","protocol_version":"1.0","publisher":"fixture","executable":{"relative_path":"bin/source.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":["core.handshake","core.cancel","core.heartbeat","source.auth_probe"],"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":[],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]}),
    };
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&installed, &ctx("seed"))
    .await
    .unwrap();
    let sources = larm_db::sources::SqlSources {
        pool: db.pool.clone(),
        qualification: None,
    };
    let binding = sources
        .register_source_adapter(&installed.id, "HTTP", &Map::new(), &ctx("seed"))
        .await
        .unwrap();
    let config = ModelSourceCreate {
        name: "probe fixture".into(),
        adapter_id: binding.id,
        base_url: "https://source.example/models".into(),
        priority: 0,
        enabled: false,
        auth_mode: AuthMode::None,
        account_id: None,
        policy: Map::new(),
    };
    let record = sources
        .create_source(&config, "source-create", &"b".repeat(64), &ctx("seed"))
        .await
        .unwrap();
    sqlx::query("UPDATE model_source SET enabled=1 WHERE id=?")
        .bind(&record.id)
        .execute(&db.pool)
        .await
        .unwrap();
    record.id
}

async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='source.auth_probe'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM idempotency WHERE operation='P045_post__sources_id_probe')")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn enqueue_replay_claim_finish_and_owner_are_durable_and_atomic() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let source_id = source(&db).await;
    let repo = SqlSourceProbes {
        pool: db.pool.clone(),
    };
    let actor = ctx("technician");
    let first = repo
        .enqueue_source_probe(&source_id, &request(), &"c".repeat(64), "key", &actor)
        .await
        .unwrap();
    let before = counts(&db).await;
    assert_eq!(
        repo.enqueue_source_probe(&source_id, &request(), &"c".repeat(64), "key", &actor)
            .await
            .unwrap()
            .accepted(),
        first.accepted()
    );
    assert_eq!(counts(&db).await, before);
    assert_eq!(
        repo.enqueue_source_probe(&source_id, &request(), &"f".repeat(64), "key", &actor)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        repo.enqueue_source_probe(&source_id, &request(), &"c".repeat(64), "another", &actor)
            .await
            .unwrap_err()
            .details
            .reason,
        "source_probe_active"
    );
    assert_eq!(
        repo.enqueue_source_probe(
            &source_id,
            &request(),
            &"c".repeat(64),
            "key",
            &ctx("other")
        )
        .await
        .unwrap_err()
        .details
        .reason,
        "source_probe_active"
    );
    let claimed = repo.claim_source_probe().await.unwrap().unwrap();
    assert_eq!(claimed.phase, JobPhase::Running);
    let result = SourceAuthProbeResult {
        authenticated: true,
        auth_required: false,
        license_required: false,
        reason: "authenticated".into(),
    };
    repo.finish_source_probe(&first.id, &result).await.unwrap();
    let completed = repo.source_probe_job(&first.id).await.unwrap().unwrap();
    assert_eq!(completed.phase, JobPhase::Completed);
    assert_eq!(
        completed.public_state(false).result.unwrap()["authenticated"],
        true
    );
    assert!(repo.claim_source_probe().await.unwrap().is_none());
    assert_eq!(counts(&db).await.0, 1);
}

#[tokio::test]
async fn failure_is_redacted_disabled_source_is_rejected_and_outbox_tracks_transitions() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let source_id = source(&db).await;
    let repo = SqlSourceProbes {
        pool: db.pool.clone(),
    };
    let actor = ctx("technician");
    let job = repo
        .enqueue_source_probe(&source_id, &request(), &"d".repeat(64), "failure", &actor)
        .await
        .unwrap();
    repo.claim_source_probe().await.unwrap();
    let secret_error = larm_domain::error::LarmError::new(
        ErrorCode::Other("upstream-token-value".into()),
        "Authorization: Bearer upstream-token-value",
        "upstream-token-value",
    );
    repo.fail_source_probe(&job.id, &secret_error, false)
        .await
        .unwrap();
    let failed = repo.source_probe_job(&job.id).await.unwrap().unwrap();
    let persisted = serde_json::to_string(&failed).unwrap();
    assert_eq!(failed.phase, JobPhase::Failed);
    assert!(!persisted.contains("upstream-token-value"));
    assert_eq!(
        failed.error.unwrap().details.reason,
        "source_auth_probe_failed"
    );
    let outbox: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_id=? AND e.event_type='source.updated'",
    )
    .bind(&job.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(outbox, 3);

    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&source_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.enqueue_source_probe(
            &source_id,
            &request(),
            &"e".repeat(64),
            "disabled",
            &ctx("x")
        )
        .await
        .unwrap_err()
        .details
        .reason,
        "source_disabled"
    );
}
