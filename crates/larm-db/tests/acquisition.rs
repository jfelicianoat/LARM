//! SQLite real con cualificación sintética CODE_PASS; no ejecuta workers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_db::acquisition::SqlAcquisitionManifests;
use larm_domain::acquisition::ModelAcquisitionManifest;
use larm_domain::adapters::AdapterInstallation;
use larm_domain::ports::{AcquisitionManifestCatalog, MutationContext};
use serde_json::json;
use sqlx::SqlitePool;

#[path = "support/acquisition_fixture.rs"]
mod fixture;
use fixture::{ctx, id, repo, seed};

async fn counts(pool: &SqlitePool) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM model_acquisition_manifest),(SELECT COUNT(*) FROM event),(SELECT COUNT(*) FROM event_outbox)")
        .fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn current_snapshot_reads_recheck_links_without_publishing_any_state() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let snapshot = seed(&db.pool).await;
    let catalog = repo(&db.pool);
    catalog.record(&snapshot, &ctx()).await.unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        catalog
            .latest_current(&snapshot.variant_id, &ctx())
            .await
            .unwrap(),
        Some(snapshot.clone())
    );
    assert_eq!(catalog.latest_current(&id(), &ctx()).await.unwrap(), None);
    assert_eq!(
        catalog
            .bound_current(&snapshot.variant_id, &snapshot.binding(), &ctx())
            .await
            .unwrap(),
        snapshot
    );
    let mut changed = snapshot.binding();
    changed.snapshot_hash = "e".repeat(64);
    assert_eq!(
        catalog
            .bound_current(&snapshot.variant_id, &changed, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "acquisition_plan_binding_mismatch"
    );
    assert!(catalog
        .bound_current(&id(), &snapshot.binding(), &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
    let physical: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM model_bundle),(SELECT COUNT(*) FROM artifact_location),(SELECT COUNT(*) FROM download_job),(SELECT COUNT(*) FROM volume_reservation)").fetch_one(&db.pool).await.unwrap();
    assert_eq!(physical, (0, 0, 0, 0));
}

#[tokio::test]
async fn current_reads_never_fall_back_to_historical_or_unqualified_snapshots() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let snapshot = seed(&db.pool).await;
    let catalog = repo(&db.pool);
    catalog.record(&snapshot, &ctx()).await.unwrap();
    let before = counts(&db.pool).await;
    let no_gate = SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: None,
    };
    assert_eq!(
        no_gate
            .latest_current(&snapshot.variant_id, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_physical_gate_unavailable"
    );
    let expiring = SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: Some(std::sync::Arc::new(ExpiringGate)),
    };
    assert_eq!(
        expiring
            .bound_current(&snapshot.variant_id, &snapshot.binding(), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "qualification_expired_fixture"
    );
    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&snapshot.source_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        catalog
            .latest_current(&snapshot.variant_id, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "acquisition_catalog_mismatch"
    );
    // An archival read remains possible; it is not admission for a download.
    assert_eq!(catalog.get(&snapshot.id).await.unwrap(), Some(snapshot));
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn remote_snapshot_persists_replays_and_never_creates_verified_bytes() {
    let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let r = repo(&db.pool);
    let before = counts(&db.pool).await;
    assert!(r.record(&s, &ctx()).await.unwrap());
    assert!(!r.record(&s, &ctx()).await.unwrap());
    assert_eq!(
        counts(&db.pool).await,
        (before.0 + 1, before.1 + 1, before.2 + 1)
    );
    assert_eq!(r.get(&s.id).await.unwrap(), Some(s.clone()));
    assert_eq!(r.latest(&s.variant_id).await.unwrap(), Some(s.clone()));
    let physical:(i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT COUNT(*) FROM model_bundle),(SELECT COUNT(*) FROM artifact_location),(SELECT COUNT(*) FROM download_job),(SELECT COUNT(*) FROM volume_reservation),(SELECT COUNT(*) FROM model_variant WHERE lifecycle<>'DISCOVERED' OR availability<>'UNKNOWN')")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(physical, (0, 0, 0, 0, 0));
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(&directory.path().join("larm.sqlite"))
        .await
        .unwrap();
    assert_eq!(repo(&reopened.pool).get(&s.id).await.unwrap(), Some(s));
}

#[tokio::test]
async fn immutable_snapshot_rejects_updates_deletes_and_changed_replay() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let r = repo(&db.pool);
    r.record(&s, &ctx()).await.unwrap();
    let before = counts(&db.pool).await;
    assert!(
        sqlx::query("UPDATE model_acquisition_manifest SET observed_at='changed' WHERE id=?")
            .bind(&s.id)
            .execute(&db.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM model_acquisition_manifest WHERE id=?")
            .bind(&s.id)
            .execute(&db.pool)
            .await
            .is_err()
    );
    let mut changed = s.clone();
    changed.observed_at = "2026-10-27T16:00:01.000Z".into();
    assert!(r.record(&changed.seal().unwrap(), &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    assert_eq!(r.get(&s.id).await.unwrap(), Some(s));
}

#[tokio::test]
async fn artifact_role_mismatch_never_publishes_a_snapshot() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    sqlx::query("UPDATE model_artifact SET metadata=? WHERE id=?")
        .bind(json!({"role":"weights"}).to_string())
        .bind(&s.artifacts[0].artifact_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    let mut changed = s;
    changed.artifacts[0].role = "config".into();
    assert!(repo(&db.pool)
        .record(&changed.seal().unwrap(), &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn catalog_and_qualification_mismatches_never_publish_a_snapshot() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let r = repo(&db.pool);
    let before = counts(&db.pool).await;
    for field in [
        "variant_id",
        "revision_id",
        "upstream_manifest_hash",
        "conformance_run_id",
        "conformance_run_sha256",
        "adapter_installation_sha256",
    ] {
        let mut value = json!(s);
        value[field] = json!(if field.ends_with("_id") {
            id()
        } else {
            "c".repeat(64)
        });
        let changed: ModelAcquisitionManifest = serde_json::from_value(value).unwrap();
        assert!(
            r.record(&changed.seal().unwrap(), &ctx()).await.is_err(),
            "{field}"
        );
        assert_eq!(counts(&db.pool).await, before);
    }
    let mut changed = s.clone();
    changed.artifacts[0].artifact_id = id();
    changed.required_artifact_ids[0] = changed.artifacts[0].artifact_id.clone();
    assert!(r.record(&changed.seal().unwrap(), &ctx()).await.is_err());
    let missing_gate = SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: None,
    };
    assert!(missing_gate.record(&s, &ctx()).await.is_err());
    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&s.source_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(r.record(&s, &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn event_outbox_fault_rolls_back_the_snapshot() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let r = repo(&db.pool);
    let before = counts(&db.pool).await;
    sqlx::raw_sql("CREATE TRIGGER fail_acquisition_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fault'); END;")
        .execute(&db.pool).await.unwrap();
    assert!(r.record(&s, &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    assert!(r.get(&s.id).await.unwrap().is_none());
}

struct ExpiringGate;
struct ExpiringGuard;
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualification for ExpiringGate {
    async fn acquire(
        &self,
        _: &AdapterInstallation,
        _: &larm_domain::conformance::Run,
        _: &MutationContext,
    ) -> Result<Box<dyn larm_domain::ports::AdapterQualificationGuard>, larm_domain::error::LarmError>
    {
        Ok(Box::new(ExpiringGuard))
    }
}
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualificationGuard for ExpiringGuard {
    async fn recheck(&self) -> Result<(), larm_domain::error::LarmError> {
        Err(larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::Forbidden,
            "Autorización caducada",
            "qualification_expired_fixture",
        ))
    }
}

#[tokio::test]
async fn qualification_lost_before_commit_rolls_back_snapshot_and_outbox() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let before = counts(&db.pool).await;
    let r = SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: Some(std::sync::Arc::new(ExpiringGate)),
    };
    assert_eq!(
        r.record(&s, &ctx()).await.unwrap_err().details.reason,
        "qualification_expired_fixture"
    );
    assert_eq!(counts(&db.pool).await, before);
    assert!(r.get(&s.id).await.unwrap().is_none());
}

#[tokio::test]
async fn simultaneous_replay_commits_once_and_new_observation_keeps_history() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let s = seed(&db.pool).await;
    let r = repo(&db.pool);
    let before = counts(&db.pool).await;
    let one = ctx();
    let two = ctx();
    let (a, b) = tokio::join!(r.record(&s, &one), r.record(&s, &two));
    assert_ne!(a.unwrap(), b.unwrap());
    assert_eq!(
        counts(&db.pool).await,
        (before.0 + 1, before.1 + 1, before.2 + 1)
    );
    let mut next = s.clone();
    next.id = id();
    next.observed_at = "2026-10-27T16:00:01.000Z".into();
    let next = next.seal().unwrap();
    r.record(&next, &ctx()).await.unwrap();
    assert_eq!(r.latest(&s.variant_id).await.unwrap(), Some(next));
    assert_eq!(r.get(&s.id).await.unwrap(), Some(s));
}
