//! Real SQLite transactions with a synthetic CODE_PASS qualification guard.
//! Prepared central identities are fixtures; no P041 resolver or worker is run.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_db::catalog_acquisition::{CatalogAcquisition, SqlCatalogAcquisitions};
use larm_domain::acquisition::{ModelAcquisitionArtifact, ModelAcquisitionManifest};
use larm_domain::model::{
    CanonicalModel, ModelArtifact, ModelDistribution, ModelRevision, ModelVariant,
};
use larm_domain::ports::{ModelRepository, VariantRepository};
use serde_json::json;
use sqlx::SqlitePool;

#[path = "support/acquisition_fixture.rs"]
mod fixture;
use fixture::{ctx, id};

#[derive(Clone)]
struct Prepared {
    catalog_revision: i64,
    model: CanonicalModel,
    distribution: ModelDistribution,
    revision: ModelRevision,
    variant: ModelVariant,
    artifacts: Vec<ModelArtifact>,
    manifest: ModelAcquisitionManifest,
}
impl Prepared {
    fn graph(&self) -> CatalogAcquisition<'_> {
        CatalogAcquisition {
            expected_catalog_revision: self.catalog_revision,
            expected_source_revision: 1,
            identity_updates: &[],
            model: &self.model,
            distribution: &self.distribution,
            revision: &self.revision,
            variant: &self.variant,
            artifacts: &self.artifacts,
            manifest: &self.manifest,
        }
    }
}

async fn prepared(pool: &SqlitePool) -> Prepared {
    let manifest = fixture::seed(pool).await;
    let models = larm_db::models::SqlModels { pool: pool.clone() };
    let variants = larm_db::variants::SqlVariants { pool: pool.clone() };
    let variant = variants
        .get_variant(&manifest.variant_id)
        .await
        .unwrap()
        .unwrap();
    let revision = models
        .get_revision(&variant.revision_id)
        .await
        .unwrap()
        .unwrap();
    let distribution = models
        .get_distribution(&revision.distribution_id)
        .await
        .unwrap()
        .unwrap();
    let model = models
        .get_model(&distribution.canonical_model_id)
        .await
        .unwrap()
        .unwrap();
    let artifacts = variants.artifacts_of(&variant.id).await.unwrap();
    // Remove only this fixture's unreferenced catalog graph. Keep its source
    // and explicit synthetic qualification. The production writer starts empty.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM model_artifact WHERE variant_id=?")
        .bind(&variant.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_variant WHERE id=?")
        .bind(&variant.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_revision WHERE id=?")
        .bind(&revision.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_distribution WHERE id=?")
        .bind(&distribution.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM canonical_model WHERE id=?")
        .bind(&model.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Prepared {
        catalog_revision: repo(pool).resolution_revision().await.unwrap(),
        model,
        distribution,
        revision,
        variant,
        artifacts,
        manifest,
    }
}
fn repo(pool: &SqlitePool) -> SqlCatalogAcquisitions {
    SqlCatalogAcquisitions {
        pool: pool.clone(),
        qualification: fixture::repo(pool).qualification,
    }
}
async fn counts(pool: &SqlitePool) -> Vec<i64> {
    let mut result = Vec::new();
    for table in [
        "canonical_model",
        "model_distribution",
        "model_revision",
        "model_variant",
        "model_artifact",
        "model_acquisition_manifest",
        "model_bundle",
        "artifact_location",
        "download_job",
        "volume_reservation",
        "artifact_provenance",
        "event",
        "event_outbox",
    ] {
        result.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    result
}
fn all_confidences(p: &mut Prepared) {
    let base = p.artifacts[0].clone();
    p.artifacts.clear();
    p.manifest.artifacts.clear();
    p.manifest.required_artifact_ids.clear();
    p.manifest.optional_artifact_ids.clear();
    for (confidence, point, lower, upper, source) in [
        ("EXACT", Some("10"), Some("10"), Some("10"), "MANIFEST"),
        ("HIGH", Some("11"), None, Some("11"), "HTTP_HEAD"),
        ("ESTIMATED", None, Some("12"), Some("24"), "SOURCE_METADATA"),
        ("UNKNOWN", None, None, None, "UNKNOWN"),
    ] {
        let mut a = base.clone();
        a.id = id();
        a.filename = format!("{}.bin", confidence.to_lowercase());
        a.required = confidence == "UNKNOWN";
        a.expected_size = if confidence == "EXACT" {
            Some(larm_domain::units::DecimalU64::new(10).unwrap())
        } else {
            None
        };
        a.sha256 = if confidence == "EXACT" {
            Some("a".repeat(64))
        } else {
            None
        };
        a.size_estimate=serde_json::from_value(json!({"confidence":confidence,"bytes":point,
            "lower_bound_bytes":lower,"upper_bound_bytes":upper,"source":source,
            "evidence_refs":["fixture:observed-source-metadata"],"reason":if confidence=="UNKNOWN" {Some("not_reported")}else{None}})).unwrap();
        p.manifest.artifacts.push(ModelAcquisitionArtifact {
            artifact_id: a.id.clone(),
            relative_path: a.filename.clone(),
            size_bytes: a.expected_size,
            sha256: a.sha256.clone(),
            required: a.required,
            role: "weights".into(),
            size_estimate: a.size_estimate.clone(),
        });
        if a.required {
            p.manifest.required_artifact_ids.push(a.id.clone());
        } else {
            p.manifest.optional_artifact_ids.push(a.id.clone());
        }
        p.artifacts.push(a);
    }
    p.manifest = p.manifest.clone().seal().unwrap();
}

#[tokio::test]
async fn entire_unverified_graph_four_confidences_replay_and_restart() {
    let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut p = prepared(&db.pool).await;
    all_confidences(&mut p);
    let before = counts(&db.pool).await;
    assert_eq!(&before[..11], &[0; 11]);
    let first = repo(&db.pool).record(&p.graph(), &ctx()).await.unwrap();
    assert!(
        first.model_created
            && first.distribution_created
            && first.revision_created
            && first.variant_created
            && first.snapshot_created
    );
    let after = counts(&db.pool).await;
    assert_eq!(&after[..11], &[1, 1, 1, 1, 4, 1, 0, 0, 0, 0, 0]);
    assert_eq!(&after[11..], &[before[11] + 5, before[12] + 5]);
    let replay = repo(&db.pool).record(&p.graph(), &ctx()).await.unwrap();
    assert!(
        !replay.model_created
            && !replay.distribution_created
            && !replay.revision_created
            && !replay.variant_created
            && !replay.snapshot_created
    );
    assert_eq!(counts(&db.pool).await, after);
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(&directory.path().join("larm.sqlite"))
        .await
        .unwrap();
    assert_eq!(
        fixture::repo(&reopened.pool)
            .get(&p.manifest.id)
            .await
            .unwrap(),
        Some(p.manifest.clone())
    );
    let variants = larm_db::variants::SqlVariants {
        pool: reopened.pool.clone(),
    };
    let actual = variants.artifacts_of(&p.variant.id).await.unwrap();
    for expected in p.artifacts {
        assert!(actual.contains(&expected));
    }
    assert_eq!(counts(&reopened.pool).await, after);
}

#[tokio::test]
async fn final_snapshot_outbox_fault_rolls_back_all_entities_and_events_then_retries() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let p = prepared(&db.pool).await;
    let before = counts(&db.pool).await;
    sqlx::raw_sql("CREATE TRIGGER fail_final_snapshot_outbox BEFORE INSERT ON event_outbox WHEN EXISTS(SELECT 1 FROM event WHERE id=NEW.event_record_id AND event_type='model.acquisition.recorded') BEGIN SELECT RAISE(ABORT,'final outbox fixture fault'); END;")
        .execute(&db.pool).await.unwrap();
    assert!(repo(&db.pool).record(&p.graph(), &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    sqlx::raw_sql("DROP TRIGGER fail_final_snapshot_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        repo(&db.pool)
            .record(&p.graph(), &ctx())
            .await
            .unwrap()
            .snapshot_created
    );
}

struct ExpiringGate;
struct ExpiringGuard;
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualification for ExpiringGate {
    async fn acquire(
        &self,
        _: &larm_domain::adapters::AdapterInstallation,
        _: &larm_domain::conformance::Run,
        _: &larm_domain::ports::MutationContext,
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
            "Expired fixture guard",
            "catalog_guard_expired_fixture",
        ))
    }
}
#[tokio::test]
async fn missing_disabled_or_expired_qualification_rolls_back_the_whole_graph() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let p = prepared(&db.pool).await;
    let before = counts(&db.pool).await;
    let missing = SqlCatalogAcquisitions {
        pool: db.pool.clone(),
        qualification: None,
    };
    assert!(missing.record(&p.graph(), &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    let expired = SqlCatalogAcquisitions {
        pool: db.pool.clone(),
        qualification: Some(std::sync::Arc::new(ExpiringGate)),
    };
    assert_eq!(
        expired
            .record(&p.graph(), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "catalog_guard_expired_fixture"
    );
    assert_eq!(counts(&db.pool).await, before);
    sqlx::query("UPDATE model_source SET enabled=0 WHERE id=?")
        .bind(&p.manifest.source_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo(&db.pool).record(&p.graph(), &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn simultaneous_replay_commits_once_and_competing_central_uuid_is_rejected() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let p = prepared(&db.pool).await;
    let r = repo(&db.pool);
    let one = ctx();
    let two = ctx();
    let a = p.graph();
    let b = p.graph();
    let (a, b) = tokio::join!(r.record(&a, &one), r.record(&b, &two));
    assert_ne!(a.unwrap().snapshot_created, b.unwrap().snapshot_created);
    let before = counts(&db.pool).await;
    let mut competing = p.clone();
    competing.variant.id = id();
    competing.manifest.id = id();
    competing.manifest.variant_id = competing.variant.id.clone();
    for artifact in &mut competing.artifacts {
        artifact.variant_id = competing.variant.id.clone();
    }
    competing.manifest = competing.manifest.seal().unwrap();
    assert_eq!(
        r.record(&competing.graph(), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "catalog_acquisition_identity_changed"
    );
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn foreign_descriptors_roles_sizes_and_physical_states_fail_without_partial_rows() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let p = prepared(&db.pool).await;
    let before = counts(&db.pool).await;
    for field in [
        "role",
        "size",
        "variant",
        "revision",
        "lifecycle",
        "actual_size",
        "duplicate",
    ] {
        let mut bad = p.clone();
        match field {
            "role" => {
                bad.artifacts[0]
                    .metadata
                    .insert("role".into(), json!("config"));
            }
            "size" => bad.artifacts[0].expected_size = larm_domain::units::DecimalU64::new(99),
            "variant" => bad.artifacts[0].variant_id = id(),
            "revision" => bad.variant.revision_id = id(),
            "lifecycle" => {
                bad.variant.lifecycle = "VERIFIED".into();
                bad.variant.status = "VERIFIED".into();
            }
            "actual_size" => bad.artifacts[0].actual_size = larm_domain::units::DecimalU64::new(99),
            _ => bad.artifacts.push(bad.artifacts[0].clone()),
        }
        assert!(
            repo(&db.pool).record(&bad.graph(), &ctx()).await.is_err(),
            "{field}"
        );
        assert_eq!(counts(&db.pool).await, before, "{field}");
    }
}

#[tokio::test]
async fn current_catalog_row_changed_after_resolution_is_never_overwritten() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let p = prepared(&db.pool).await;
    let r = repo(&db.pool);
    r.record(&p.graph(), &ctx()).await.unwrap();
    sqlx::query("UPDATE canonical_model SET name='Changed after identity resolution',row_version=row_version+1 WHERE id=?")
        .bind(&p.model.id).execute(&db.pool).await.unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        r.record(&p.graph(), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "catalog_acquisition_model_changed"
    );
    assert_eq!(counts(&db.pool).await, before);
    assert_eq!(
        larm_db::models::SqlModels {
            pool: db.pool.clone()
        }
        .get_model(&p.model.id)
        .await
        .unwrap()
        .unwrap()
        .name,
        "Changed after identity resolution"
    );
}

#[tokio::test]
async fn next_observation_keeps_the_first_snapshot_and_reuses_central_entities() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut p = prepared(&db.pool).await;
    let r = repo(&db.pool);
    r.record(&p.graph(), &ctx()).await.unwrap();
    let first = p.manifest.clone();
    let before = counts(&db.pool).await;
    p.manifest.id = id();
    p.manifest.observed_at = "2026-10-27T16:00:01.000Z".into();
    p.manifest = p.manifest.seal().unwrap();
    let result = r.record(&p.graph(), &ctx()).await.unwrap();
    assert!(
        !result.model_created
            && !result.distribution_created
            && !result.revision_created
            && !result.variant_created
            && result.snapshot_created
    );
    let after = counts(&db.pool).await;
    assert_eq!(&after[..5], &before[..5]);
    assert_eq!(after[5], 2);
    assert_eq!(&after[6..11], &[0; 5]);
    assert_eq!(&after[11..], &[before[11] + 1, before[12] + 1]);
    let snapshots = fixture::repo(&db.pool);
    assert_eq!(snapshots.get(&first.id).await.unwrap(), Some(first));
    assert_eq!(
        snapshots.latest(&p.variant.id).await.unwrap(),
        Some(p.manifest)
    );
}

#[tokio::test]
async fn catalog_change_during_identity_resolution_requires_retry_even_after_event_pruning() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut p = prepared(&db.pool).await;
    let mut other = p.model.clone();
    other.id = id();
    other.name = "Unrelated fixture model".into();
    larm_db::models::SqlModels {
        pool: db.pool.clone(),
    }
    .insert_model(&other, &ctx())
    .await
    .unwrap();
    // Explicit fixture pruning: the retained event table must not reset the
    // durable watermark used to protect an identity decision made earlier.
    sqlx::query("DELETE FROM event_outbox WHERE event_record_id IN (SELECT id FROM event WHERE resource_id=?)")
        .bind(&other.id).execute(&db.pool).await.unwrap();
    sqlx::query("DELETE FROM event WHERE resource_id=?")
        .bind(&other.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    let r = repo(&db.pool);
    assert_eq!(
        r.record(&p.graph(), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "catalog_acquisition_resolution_changed"
    );
    assert_eq!(counts(&db.pool).await, before);
    p.catalog_revision = r.resolution_revision().await.unwrap();
    assert!(r.record(&p.graph(), &ctx()).await.unwrap().model_created);
}
