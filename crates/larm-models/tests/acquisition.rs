//! P041 resolver + atomic SQLite writer. RPC observations and current physical
//! qualification are explicit CODE_PASS fixtures, not native worker evidence.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_adapter_sdk::source::{ArtifactManifest, ModelInspection, SourceArtifact, SourceMetadata};
use larm_db::catalog_acquisition::SqlCatalogAcquisitions;
use larm_domain::model::{ArtifactSizeConfidence, ArtifactSizeEstimate, ModelVariant};
use larm_domain::ports::{ModelRepository, VariantRepository};
use larm_models::acquisition::{
    SourceCatalogIngestor, SourceCatalogObservation, SourceObservationQualification,
};
use larm_models::identity::IdentityGrade;
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;

#[path = "../../larm-db/tests/support/acquisition_fixture.rs"]
mod fixture;
use fixture::{ctx, id};

fn service(pool: &SqlitePool) -> SourceCatalogIngestor {
    SourceCatalogIngestor {
        catalog: SqlCatalogAcquisitions {
            pool: pool.clone(),
            qualification: fixture::repo(pool).qualification,
        },
    }
}
fn attach(observed: &mut SourceCatalogObservation) {
    SourceMetadata {
        original: Map::from_iter([("arbitrary_upstream_field".into(), json!([1, true, null]))]),
        normalized: Map::from_iter([("catalog_reference".into(), json!(observed.reference))]),
        limitations: vec!["controlled RPC observation fixture".into()],
        rate_limit: None,
    }
    .attach(&mut observed.hit.metadata)
    .unwrap();
}
async fn observation(pool: &SqlitePool) -> SourceCatalogObservation {
    let seed = fixture::seed(pool).await;
    // Source and qualification remain; remove only the fixture's unreferenced
    // model graph. Product resolution starts with no model/variant/artifact.
    sqlx::query("DELETE FROM model_artifact WHERE variant_id=?")
        .bind(&seed.variant_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_variant WHERE id=?")
        .bind(&seed.variant_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_revision WHERE id=?")
        .bind(&seed.revision_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM model_distribution WHERE source_id=?")
        .bind(&seed.source_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM canonical_model")
        .execute(pool)
        .await
        .unwrap();
    let protocol: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/source/protocol.json")).unwrap();
    let mut hit: ModelVariant =
        serde_json::from_value(protocol["responses"]["source.search"]["items"][0].clone()).unwrap();
    hit.id = id();
    hit.revision_id = id();
    hit.architecture = Some("llama".into());
    hit.metadata = Map::from_iter([("display_name".into(), json!("Model fixture"))]);
    let artifact = SourceArtifact {
        relative_path: "weights.bin".into(),
        size_bytes: None,
        sha256: None,
        required: true,
        role: "weights".into(),
        size_estimate: Some(ArtifactSizeEstimate::unknown(
            "not_reported",
            "fixture:source-rpc",
        )),
    };
    let mut inspection_artifact = artifact.clone();
    inspection_artifact.size_estimate = None;
    let mut result = SourceCatalogObservation {
        source_revision: 1,
        id: id(),
        observed_at: "2026-10-27T22:00:00.000Z".into(),
        reference: seed.source_reference,
        hit,
        inspection: ModelInspection {
            upstream_identity: None,
            architecture: Some("llama".into()),
            format: Some("GGUF".into()),
            quantization: None,
            capabilities: vec!["text.generate".into()],
            limits: Map::new(),
            artifacts: vec![inspection_artifact],
            license: None,
            evidence_refs: vec!["fixture:source-rpc-inspection".into()],
        },
        manifest: ArtifactManifest {
            revision_key: "r1".into(),
            manifest_hash: "b".repeat(64),
            artifacts: vec![artifact],
        },
        qualification: SourceObservationQualification {
            adapter_installation_id: seed.adapter_installation_id,
            adapter_installation_revision: seed.adapter_installation_revision,
            adapter_installation_sha256: seed.adapter_installation_sha256,
            conformance_run_id: seed.conformance_run_id,
            conformance_run_sha256: seed.conformance_run_sha256,
        },
    };
    attach(&mut result);
    result
}
fn another(observed: &SourceCatalogObservation, model: &str) -> SourceCatalogObservation {
    let mut next = observed.clone();
    next.id = id();
    next.hit.id = id();
    next.hit.revision_id = id();
    next.reference.upstream_model_id = model.into();
    attach(&mut next);
    next
}
fn exact(observed: &mut SourceCatalogObservation) {
    let artifact = &mut observed.manifest.artifacts[0];
    artifact.size_bytes = larm_domain::units::DecimalU64::new(10);
    artifact.sha256 = Some("a".repeat(64));
    artifact.size_estimate = Some(
        serde_json::from_value(json!({
        "confidence":"EXACT","bytes":"10","lower_bound_bytes":"10","upper_bound_bytes":"10",
        "source":"MANIFEST","evidence_refs":["fixture:pinned-manifest"],"reason":null}))
        .unwrap(),
    );
    let mut inspected = artifact.clone();
    inspected.size_estimate = None;
    observed.inspection.artifacts = vec![inspected];
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

#[tokio::test]
async fn ingestion_resolves_central_ids_replays_and_reopens_without_any_physical_state() {
    let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    let before = counts(&db.pool).await;
    assert_eq!(&before[..11], &[0; 11]);
    let registered = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    assert_ne!(registered.variant.id, o.hit.id);
    assert_ne!(registered.variant.revision_id, o.hit.revision_id);
    assert!(registered.report.model_created);
    assert_eq!(registered.report.variants_created, 1);
    assert_eq!(registered.variant.lifecycle, "DISCOVERED");
    assert_eq!(registered.variant.availability, "UNKNOWN");
    assert!(registered.variant.bundle_hash.is_none());
    let after = counts(&db.pool).await;
    assert_eq!(&after[..11], &[1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0]);
    assert_eq!(&after[11..], &[before[11] + 5, before[12] + 5]);
    // Private IDs can change without changing the Core's public reference.
    o.hit.id = id();
    o.hit.revision_id = id();
    let replay = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    assert_eq!(replay.manifest, registered.manifest);
    assert_eq!(replay.variant.id, registered.variant.id);
    assert!(!replay.report.model_created);
    assert_eq!(replay.report.variants_created, 0);
    assert_eq!(counts(&db.pool).await, after);
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(&directory.path().join("larm.sqlite"))
        .await
        .unwrap();
    assert_eq!(
        service(&reopened.pool)
            .ingest(&o, &ctx())
            .await
            .unwrap()
            .manifest,
        registered.manifest
    );
    assert_eq!(counts(&reopened.pool).await, after);
}

#[tokio::test]
async fn equal_names_remain_separate_with_reciprocal_duplicate_annotations_and_replay() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let s = service(&db.pool);
    let one = s.ingest(&o, &ctx()).await.unwrap();
    let second = another(&o, "different-publication");
    let two = s.ingest(&second, &ctx()).await.unwrap();
    assert_ne!(one.model.id, two.model.id);
    assert_eq!(two.model.possible_duplicates, vec![one.model.id.clone()]);
    assert!(two
        .report
        .resolutions
        .iter()
        .any(|r| r.grade == IdentityGrade::ProbableSameModel));
    let stored = larm_db::models::SqlModels {
        pool: db.pool.clone(),
    }
    .get_model(&one.model.id)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.possible_duplicates, vec![two.model.id.clone()]);
    assert_eq!(stored.row_version, 2);
    let after = counts(&db.pool).await;
    s.ingest(&second, &ctx()).await.unwrap();
    assert_eq!(counts(&db.pool).await, after);
}

#[tokio::test]
async fn late_outbox_failure_reverts_identity_annotations_and_all_new_entities() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let s = service(&db.pool);
    let one = s.ingest(&o, &ctx()).await.unwrap();
    let second = another(&o, "duplicate-candidate");
    let before = counts(&db.pool).await;
    sqlx::raw_sql("CREATE TRIGGER fail_p041_outbox BEFORE INSERT ON event_outbox WHEN EXISTS(SELECT 1 FROM event WHERE id=NEW.event_record_id AND event_type='model.acquisition.recorded') BEGIN SELECT RAISE(ABORT,'late p041 fixture fault'); END;")
        .execute(&db.pool).await.unwrap();
    assert!(s.ingest(&second, &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    let stored = larm_db::models::SqlModels {
        pool: db.pool.clone(),
    }
    .get_model(&one.model.id)
    .await
    .unwrap()
    .unwrap();
    assert!(stored.possible_duplicates.is_empty());
    assert_eq!(stored.row_version, 1);
    sqlx::raw_sql("DROP TRIGGER fail_p041_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let two = s.ingest(&second, &ctx()).await.unwrap();
    assert_eq!(two.model.possible_duplicates, vec![one.model.id]);
}

#[tokio::test]
async fn identical_complete_required_bytes_merge_even_with_a_different_name() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    exact(&mut o);
    let s = service(&db.pool);
    let one = s.ingest(&o, &ctx()).await.unwrap();
    let mut other = another(&o, "mirror-publication");
    other
        .hit
        .metadata
        .insert("display_name".into(), json!("Another name"));
    let two = s.ingest(&other, &ctx()).await.unwrap();
    assert_eq!(one.model.id, two.model.id);
    assert!(!two.report.model_created);
    assert!(two
        .report
        .resolutions
        .iter()
        .any(|r| r.grade == IdentityGrade::Identical));
    assert!(two.model.possible_duplicates.is_empty());
    assert_eq!(&counts(&db.pool).await[..6], &[1, 2, 2, 2, 2, 2]);
}

#[tokio::test]
async fn incomplete_required_hashes_and_historical_partial_evidence_never_merge() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    exact(&mut o);
    let mut unknown = o.manifest.artifacts[0].clone();
    unknown.relative_path = "required-unknown.bin".into();
    unknown.sha256 = None;
    unknown.size_bytes = None;
    unknown.size_estimate = Some(ArtifactSizeEstimate::unknown(
        "not_reported",
        "fixture:source-rpc",
    ));
    o.manifest.artifacts.push(unknown.clone());
    unknown.size_estimate = None;
    o.inspection.artifacts.push(unknown);
    let s = service(&db.pool);
    let one = s.ingest(&o, &ctx()).await.unwrap();
    assert_eq!(
        one.model.metadata["identity_evidence"]["required_hashes"],
        json!([])
    );
    // Simulate historical metadata produced before the completeness guard.
    let mut historical = one.model.metadata.clone();
    historical["identity_evidence"]["required_hashes"] = json!(["a".repeat(64)]);
    sqlx::query("UPDATE canonical_model SET metadata=? WHERE id=?")
        .bind(json!(historical).to_string())
        .bind(&one.model.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut other = another(&o, "complete-but-different-required-set");
    other
        .hit
        .metadata
        .insert("display_name".into(), json!("Different model"));
    other.manifest.artifacts.truncate(1);
    other.inspection.artifacts.truncate(1);
    let two = s.ingest(&other, &ctx()).await.unwrap();
    assert_ne!(one.model.id, two.model.id);
    assert!(!two
        .report
        .resolutions
        .iter()
        .any(|r| r.grade == IdentityGrade::Identical));
}

#[tokio::test]
async fn immutable_revision_changes_and_lost_qualification_leave_the_catalog_untouched() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let s = service(&db.pool);
    s.ingest(&o, &ctx()).await.unwrap();
    let before = counts(&db.pool).await;
    let mut changed = o.clone();
    changed.id = id();
    changed.manifest.manifest_hash = "c".repeat(64);
    assert_eq!(
        s.ingest(&changed, &ctx()).await.unwrap_err().details.reason,
        "source_catalog_revision_immutable"
    );
    assert_eq!(counts(&db.pool).await, before);
    let missing = SourceCatalogIngestor {
        catalog: SqlCatalogAcquisitions {
            pool: db.pool.clone(),
            qualification: None,
        },
    };
    assert!(missing
        .ingest(&another(&o, "unqualified-publication"), &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn malformed_source_references_roles_and_descriptors_never_write_a_model() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let before = counts(&db.pool).await;
    for field in [
        "source",
        "revision",
        "pending",
        "path",
        "role",
        "inspection",
        "empty",
        "required",
    ] {
        let mut bad = o.clone();
        match field {
            "source" => bad.reference.source_id = id(),
            "revision" => bad.manifest.revision_key = "other".into(),
            "pending" => {
                bad.reference.revision_key = format!("pending:{}", id());
                attach(&mut bad);
            }
            "path" => bad.manifest.artifacts[0].relative_path = "../hostile.bin".into(),
            "role" => bad.manifest.artifacts[0].role = "not-a-model-role".into(),
            "inspection" => bad.inspection.architecture = Some("different".into()),
            "empty" => bad.manifest.artifacts.clear(),
            _ => bad.manifest.artifacts[0].required = false,
        }
        assert!(
            service(&db.pool).ingest(&bad, &ctx()).await.is_err(),
            "{field}"
        );
        assert_eq!(counts(&db.pool).await, before, "{field}");
    }
}

#[tokio::test]
async fn two_simultaneous_observations_reuse_all_central_ids_and_publish_once() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let s = service(&db.pool);
    let a = ctx();
    let b = ctx();
    let before = counts(&db.pool).await;
    let (a, b) = tokio::join!(s.ingest(&o, &a), s.ingest(&o, &b));
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.model.id, b.model.id);
    assert_eq!(a.variant.id, b.variant.id);
    assert_eq!(a.manifest, b.manifest);
    let after = counts(&db.pool).await;
    assert_eq!(&after[..11], &[1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0]);
    assert_eq!(&after[11..], &[before[11] + 5, before[12] + 5]);
}

#[tokio::test]
async fn repository_union_inspection_can_supply_one_pinned_bundle_and_requiredness() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    o.inspection.format = None;
    o.inspection.capabilities.push("image.generate".into());
    o.inspection.artifacts[0].required = false;
    let mut other = o.inspection.artifacts[0].clone();
    other.relative_path = "other-variant.bin".into();
    o.inspection.artifacts.push(other);
    let result = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    assert_eq!(result.manifest.artifacts.len(), 1);
    assert_eq!(result.manifest.artifacts[0].relative_path, "weights.bin");
    assert!(result.manifest.artifacts[0].required);
    assert_eq!(result.variant.capabilities, vec!["text.generate"]);
    assert!(result
        .model
        .capabilities
        .contains(&"image.generate".to_owned()));
    assert_eq!(
        larm_db::variants::SqlVariants {
            pool: db.pool.clone()
        }
        .artifacts_of(&result.variant.id)
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn absent_size_certainty_keeps_a_declared_scalar_unknown() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    exact(&mut o);
    o.manifest.artifacts[0].size_estimate = None;
    let result = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    let a = &result.manifest.artifacts[0];
    assert_eq!(a.size_bytes.unwrap().get(), 10);
    assert_eq!(a.size_estimate.confidence, ArtifactSizeConfidence::Unknown);
    assert!(a.size_estimate.bytes.is_none());
    assert!(a.size_estimate.upper_bound_bytes.is_none());
}

#[tokio::test]
async fn explicit_four_confidences_survive_identity_resolution_and_atomic_storage() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    o.manifest.artifacts.clear();
    o.inspection.artifacts.clear();
    for (confidence, point, lower, upper, source) in [
        ("EXACT", Some("10"), Some("10"), Some("10"), "MANIFEST"),
        ("HIGH", Some("11"), None, Some("11"), "HTTP_HEAD"),
        ("ESTIMATED", None, Some("12"), Some("24"), "SOURCE_METADATA"),
        ("UNKNOWN", None, None, None, "UNKNOWN"),
    ] {
        let estimate:ArtifactSizeEstimate=serde_json::from_value(json!({"confidence":confidence,"bytes":point,
            "lower_bound_bytes":lower,"upper_bound_bytes":upper,"source":source,"evidence_refs":["fixture:source-rpc"],
            "reason":if confidence=="UNKNOWN"{Some("not_reported")}else{None}})).unwrap();
        let file = SourceArtifact {
            relative_path: format!("{}.bin", confidence.to_lowercase()),
            size_bytes: if confidence == "EXACT" {
                larm_domain::units::DecimalU64::new(10)
            } else {
                None
            },
            sha256: if confidence == "EXACT" {
                Some("a".repeat(64))
            } else {
                None
            },
            required: true,
            role: "weights".into(),
            size_estimate: Some(estimate),
        };
        let mut inspection = file.clone();
        inspection.size_estimate = None;
        o.inspection.artifacts.push(inspection);
        o.manifest.artifacts.push(file);
    }
    let result = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    for original in &o.manifest.artifacts {
        let stored = result
            .manifest
            .artifacts
            .iter()
            .find(|a| a.relative_path == original.relative_path)
            .unwrap();
        assert_eq!(
            stored.size_estimate,
            original.size_estimate.clone().unwrap()
        );
        assert_eq!(stored.size_bytes, original.size_bytes);
    }
    assert_eq!(
        &counts(&db.pool).await[..11],
        &[1, 1, 1, 1, 4, 1, 0, 0, 0, 0, 0]
    );
}

#[tokio::test]
async fn reobservation_reuses_a_matured_variant_and_preserves_local_facts() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut o = observation(&db.pool).await;
    let first = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    // CODE_PASS persisted-state fixture, not evidence of a physical download.
    sqlx::query("UPDATE model_variant SET bundle_hash=?,lifecycle='VERIFIED',status='VERIFIED',row_version=row_version+1 WHERE id=?")
        .bind("c".repeat(64)).bind(&first.variant.id).execute(&db.pool).await.unwrap();
    sqlx::query(
        "UPDATE model_artifact SET actual_size=4,row_version=row_version+1 WHERE variant_id=?",
    )
    .bind(&first.variant.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let variants = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let mature = variants
        .get_variant(&first.variant.id)
        .await
        .unwrap()
        .unwrap();
    let artifacts = variants.artifacts_of(&first.variant.id).await.unwrap();
    o.id = id();
    o.hit.id = id();
    let before = counts(&db.pool).await;
    let replay = service(&db.pool).ingest(&o, &ctx()).await.unwrap();
    assert_eq!(replay.variant, mature);
    assert_eq!(variants.artifacts_of(&mature.id).await.unwrap(), artifacts);
    assert_eq!(replay.manifest.variant_id, mature.id);
    assert_eq!(
        replay.manifest.artifacts[0].artifact_id,
        first.manifest.artifacts[0].artifact_id
    );
    assert_eq!(replay.manifest.artifacts[0].sha256, None);
    assert_eq!(replay.manifest.artifacts[0].size_bytes, None);
    assert_eq!(replay.report.variants_created, 0);
    assert_eq!(&counts(&db.pool).await[..5], &before[..5]);
}

#[tokio::test]
async fn source_configuration_changed_after_rpc_requires_new_observations() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let o = observation(&db.pool).await;
    let before = counts(&db.pool).await;
    sqlx::query("UPDATE model_source SET row_version=row_version+1,base_url='https://changed.invalid' WHERE id=?")
        .bind(&o.reference.source_id).execute(&db.pool).await.unwrap();
    assert_eq!(
        service(&db.pool)
            .ingest(&o, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "catalog_acquisition_source_changed"
    );
    assert_eq!(counts(&db.pool).await, before);
}
