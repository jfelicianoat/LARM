//! Source Watcher: cursors, deduplicación, replay y CAS sobre SQLite real.
#![allow(clippy::unwrap_used, clippy::panic)]

use larm_db::source_watches::{SqlSourceWatches, CREATE_SOURCE_WATCH_OPERATION};
use larm_domain::{
    error::ErrorCode,
    ports::{MutationContext, SourceCatalogRepository, SourceWatchRepository},
    watchers::{SourceWatchCreate, SourceWatchSyncSuccess, WatchDetection},
};
use serde_json::json;
use uuid::Uuid;

fn context() -> MutationContext {
    MutationContext {
        actor_id: "p050-test-actor".into(),
        correlation_id: Uuid::now_v7().to_string(),
    }
}

fn request(source_id: &str, enabled: bool) -> SourceWatchCreate {
    serde_json::from_value(json!({
        "source_id":source_id,
        "query":{"q":"fixture"},
        "interval_seconds":21600,
        "policy":{},
        "enabled":enabled
    }))
    .unwrap()
}

fn sync_success(
    cursor: &str,
    etag: &str,
    observed_at: &str,
    next_run_at: &str,
    detections: Vec<WatchDetection>,
) -> SourceWatchSyncSuccess {
    SourceWatchSyncSuccess {
        cursor: Some(cursor.to_owned()),
        etag: Some(etag.to_owned()),
        observed_at: observed_at.to_owned(),
        next_run_at: next_run_at.to_owned(),
        detections,
        rate_limit: None,
    }
}

async fn setup() -> (
    tempfile::TempDir,
    larm_db::pool::Db,
    SqlSourceWatches,
    String,
) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let now = "2026-10-25T12:00:00.000Z";
    let installation_id = Uuid::now_v7().to_string();
    let source_adapter_id = Uuid::now_v7().to_string();
    let source_id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES (?,?,?,1,'{}','fixture.source','1.0.0','source',?,'fixture','VERIFIED','{}','VALIDATED')")
        .bind(&installation_id)
        .bind(now)
        .bind(now)
        .bind("a".repeat(64))
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES (?,?,?,1,'{}',?,'HTTP','[\"source.sync\"]')")
        .bind(&source_adapter_id)
        .bind(now)
        .bind(now)
        .bind(&installation_id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES (?,?,?,1,'{}','fixture',?,'https://fixture.invalid/models',0,1,'NONE',NULL,'{}')")
        .bind(&source_id)
        .bind(now)
        .bind(now)
        .bind(&source_adapter_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let repository = SqlSourceWatches {
        pool: db.pool.clone(),
    };
    (dir, db, repository, source_id)
}

#[tokio::test]
async fn create_replays_atomically_and_conflicts_on_changed_body() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let request = request(&source_id, true);
    let key = "p050-create-watch";
    let body_hash = "b".repeat(64);
    let first = repo
        .create_source_watch(&request, key, &body_hash, &ctx)
        .await
        .unwrap();
    let replay = repo
        .create_source_watch(&request, key, &body_hash, &ctx)
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);
    assert!(first.next_run_at.is_some());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_watch")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let err = repo
        .create_source_watch(&request, key, &"c".repeat(64), &ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::IdempotencyConflict);
    let operation: String = sqlx::query_scalar("SELECT operation FROM idempotency WHERE key=?")
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(operation, CREATE_SOURCE_WATCH_OPERATION);
}

#[tokio::test]
async fn cursor_and_notifications_commit_together_and_duplicate_revision_is_suppressed() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let watch = repo
        .create_source_watch(
            &request(&source_id, true),
            "create-sync-watch",
            &"d".repeat(64),
            &ctx,
        )
        .await
        .unwrap();
    let detection = WatchDetection {
        upstream_identity: "fixture/public-model".into(),
        revision_fingerprint: "e".repeat(64),
        compatible: false,
    };
    let updated = repo
        .record_source_watch_success(
            &watch.id,
            watch.row_version,
            &sync_success(
                "cursor-2",
                "etag-2",
                "2026-10-25T12:10:00.000Z",
                "2026-10-25T18:10:00.000Z",
                vec![detection.clone()],
            ),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(updated.cursor.as_deref(), Some("cursor-2"));
    assert_eq!(updated.etag.as_deref(), Some("etag-2"));
    assert_eq!(updated.row_version, 2);
    assert_eq!(
        updated.metadata["watcher"]["last_success_at"],
        "2026-10-25T12:10:00.000Z"
    );
    let notifications: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(notifications, 1);
    let message_code: String = sqlx::query_scalar("SELECT message_code FROM notification")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(message_code, "NEW_MODEL");
    let replayed = repo
        .record_source_watch_success(
            &updated.id,
            updated.row_version,
            &sync_success(
                "cursor-2",
                "etag-2",
                "2026-10-25T12:11:00.000Z",
                "2026-10-25T18:11:00.000Z",
                vec![detection],
            ),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(replayed.row_version, 3);
    let notifications: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(notifications, 1);
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event WHERE event_type IN ('source.revision_detected','notification.created')")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, 2);
    let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(outbox, 5);
    let update_detection = WatchDetection {
        upstream_identity: "fixture/public-model".into(),
        revision_fingerprint: "f".repeat(64),
        compatible: false,
    };
    repo.record_source_watch_success(
        &replayed.id,
        replayed.row_version,
        &sync_success(
            "cursor-3",
            "etag-3",
            "2026-10-25T12:12:00.000Z",
            "2026-10-25T18:12:00.000Z",
            vec![update_detection],
        ),
        &ctx,
    )
    .await
    .unwrap();
    let messages: Vec<String> =
        sqlx::query_scalar("SELECT message_code FROM notification ORDER BY created_at,id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(messages, ["NEW_MODEL", "MODEL_UPDATE"]);
}

#[tokio::test]
async fn invalid_detection_rolls_back_cursor_notification_and_outbox_as_one_unit() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let watch = repo
        .create_source_watch(
            &request(&source_id, true),
            "create-rollback-watch",
            &"3".repeat(64),
            &ctx,
        )
        .await
        .unwrap();
    let invalid = WatchDetection {
        upstream_identity: "".into(),
        revision_fingerprint: "4".repeat(64),
        compatible: false,
    };
    let error = repo
        .record_source_watch_success(
            &watch.id,
            watch.row_version,
            &sync_success(
                "cursor-not-committed",
                "etag-not-committed",
                "2026-10-25T12:20:00.000Z",
                "2026-10-25T18:20:00.000Z",
                vec![invalid],
            ),
            &ctx,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    let persisted = repo.source_watch(&watch.id).await.unwrap().unwrap();
    assert_eq!(persisted.row_version, 1);
    assert_eq!(persisted.cursor, None);
    assert_eq!(persisted.etag, None);
    let notifications: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(notifications, 0);
    let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(outbox, 1);
}

#[tokio::test]
async fn failure_persists_safe_reason_and_exponential_retry_and_cas_rejects_stale_write() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let watch = repo
        .create_source_watch(
            &request(&source_id, true),
            "create-failed-watch",
            &"f".repeat(64),
            &ctx,
        )
        .await
        .unwrap();
    let failed = repo
        .record_source_watch_failure(
            &watch.id,
            watch.row_version,
            &larm_domain::watchers::SourceWatchSyncFailure {
                reason: "adapter_timeout".into(),
                retry_at: "2026-10-26T00:00:00.000Z".into(),
                observed_at: "2026-10-25T12:30:00.000Z".into(),
                operational_status: larm_domain::sources::SourceOperationalStatus::Error,
                rate_limit: None,
            },
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(failed.metadata["watcher"]["failure_count"], 1);
    assert_eq!(failed.metadata["watcher"]["last_error"], "adapter_timeout");
    assert_eq!(
        failed.next_run_at.as_deref(),
        Some("2026-10-26T00:00:00.000Z")
    );
    let error = repo
        .update_source_watch(
            &watch.id,
            watch.row_version,
            &request(&source_id, false),
            &ctx,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleRevision);
    let watch_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_watch")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(watch_rows, 1);
}

#[tokio::test]
async fn operational_snapshot_tracks_sync_rate_limit_and_keeps_configuration_version() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let watch = repo
        .create_source_watch(
            &request(&source_id, true),
            "create-operational-watch",
            &"9".repeat(64),
            &ctx,
        )
        .await
        .unwrap();
    let initial = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT status,last_sync_at,rate_limit_json FROM model_source_operational_snapshot WHERE source_id=?",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(initial, ("UNKNOWN".into(), None, None));

    repo.start_source_watch_sync(
        &watch.id,
        watch.row_version,
        "2026-10-25T12:01:00.000Z",
        &ctx,
    )
    .await
    .unwrap();
    let syncing: (String, Option<String>) = sqlx::query_as(
        "SELECT status,last_sync_at FROM model_source_operational_snapshot WHERE source_id=?",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(syncing, ("SYNCING".into(), None));

    let limited = larm_domain::watchers::SourceWatchSyncFailure {
        reason: "source_rate_limited".into(),
        retry_at: "2026-10-25T12:03:00.000Z".into(),
        observed_at: "2026-10-25T12:02:00.000Z".into(),
        operational_status: larm_domain::sources::SourceOperationalStatus::RateLimited,
        rate_limit: Some(larm_domain::sources::SourceRateLimit {
            limit: Some(larm_domain::units::DecimalU64::new(120).unwrap()),
            remaining: Some(larm_domain::units::DecimalU64::new(0).unwrap()),
            reset_at: Some("2026-10-25T12:05:00.000Z".into()),
            retry_after_seconds: Some(larm_domain::units::DecimalU64::new(60).unwrap()),
        }),
    };
    repo.record_source_watch_failure(&watch.id, watch.row_version, &limited, &ctx)
        .await
        .unwrap();
    let persisted: (String, String, String) = sqlx::query_as(
        "SELECT status,last_sync_at,rate_limit_json FROM model_source_operational_snapshot WHERE source_id=?",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(persisted.0, "RATE_LIMITED");
    assert_eq!(persisted.1, "2026-10-25T12:02:00.000Z");
    let rate_limit: serde_json::Value = serde_json::from_str(&persisted.2).unwrap();
    assert_eq!(rate_limit["limit"], "120");
    assert_eq!(rate_limit["remaining"], "0");
    assert_eq!(rate_limit["retry_after_seconds"], "60");

    let source = larm_db::sources::SqlSources {
        pool: db.pool.clone(),
        qualification: None,
    }
    .source(&source_id)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(source.row_version, 1);
    assert_eq!(
        source.operational.status,
        larm_domain::sources::SourceOperationalStatus::RateLimited
    );
    assert_eq!(
        source.operational.last_sync_at.as_deref(),
        Some("2026-10-25T12:02:00.000Z")
    );
    assert_eq!(
        source
            .operational
            .rate_limit
            .unwrap()
            .remaining
            .unwrap()
            .get(),
        0
    );

    let sync_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event WHERE event_type IN ('source.sync_started','source.sync_failed')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(sync_events, 2);
}

#[tokio::test]
async fn restart_recovers_syncing_snapshot_without_erasing_last_observation() {
    let (_dir, db, repo, source_id) = setup().await;
    let ctx = context();
    let watch = repo
        .create_source_watch(
            &request(&source_id, true),
            "create-recovery-watch",
            &"a".repeat(64),
            &ctx,
        )
        .await
        .unwrap();
    let mut success = sync_success(
        "cursor-1",
        "etag-1",
        "2026-10-25T12:00:00.000Z",
        "2026-10-25T18:00:00.000Z",
        Vec::new(),
    );
    success.rate_limit = Some(larm_domain::sources::SourceRateLimit {
        limit: Some(larm_domain::units::DecimalU64::new(80).unwrap()),
        remaining: Some(larm_domain::units::DecimalU64::new(70).unwrap()),
        reset_at: None,
        retry_after_seconds: None,
    });
    let updated = repo
        .record_source_watch_success(&watch.id, watch.row_version, &success, &ctx)
        .await
        .unwrap();
    repo.start_source_watch_sync(
        &updated.id,
        updated.row_version,
        "2026-10-25T12:01:00.000Z",
        &ctx,
    )
    .await
    .unwrap();

    let recovered = repo
        .recover_interrupted_source_syncs("2026-10-25T12:02:00.000Z", &ctx)
        .await
        .unwrap();
    assert_eq!(recovered, 1);
    let snapshot: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT status,last_sync_at,rate_limit_json FROM model_source_operational_snapshot WHERE source_id=?",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(snapshot.0, "ERROR");
    assert_eq!(snapshot.1.as_deref(), Some("2026-10-25T12:00:00.000Z"));
    let rate_limit: serde_json::Value =
        serde_json::from_str(snapshot.2.as_deref().unwrap()).unwrap();
    assert_eq!(rate_limit["remaining"], "70");

    let recovered_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event WHERE event_type='source.sync_interrupted' AND resource_id=?",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(recovered_events, 1);
    let recovered_outbox: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE event_record_id IN (SELECT id FROM event WHERE event_type='source.sync_interrupted' AND resource_id=?)",
    )
    .bind(&source_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(recovered_outbox, 1);
}

#[tokio::test]
async fn source_disabled_watches_cannot_be_enabled_and_policies_cannot_autodownload() {
    let (_dir, db, repo, source_id) = setup().await;
    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&source_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut disabled_request = request(&source_id, false);
    disabled_request.policy = serde_json::from_value(json!({"automatic_download":true})).unwrap();
    let auto = repo
        .create_source_watch(
            &disabled_request,
            "auto-download-watch",
            &"1".repeat(64),
            &context(),
        )
        .await
        .unwrap_err();
    assert_eq!(auto.code, ErrorCode::UnsupportedCapability);
    let enabled = repo
        .create_source_watch(
            &request(&source_id, true),
            "disabled-source-watch",
            &"2".repeat(64),
            &context(),
        )
        .await
        .unwrap_err();
    assert_eq!(enabled.code, ErrorCode::InvalidStateTransition);
}
