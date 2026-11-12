//! Original qualified source -> product services/consumer -> actual bytes/CAS.
//! Windows storage is observed by the built daemon, not a capacity fixture.
use super::*;
use larm_domain::ports::{
    DownloadActivationIdempotency, DownloadPlanActivationService, DownloadPlanPreviewService,
    StorageObservationRefresher, StorageProber, VariantRepository,
};

struct Refresh(Arc<dyn StorageProber>);
#[async_trait::async_trait]
impl StorageObservationRefresher for Refresh {
    async fn refresh(
        &self,
        _: &MutationContext,
    ) -> Result<larm_domain::storage::StorageObservation, larm_domain::bridge::ApiError> {
        self.0.probe(&[]).await
    }
}

pub(super) async fn exercise(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    runner: Arc<larm_adapter_host::conformance::NativeConformanceRunner>,
    ctx: &MutationContext,
    variant_id: &str,
) -> Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = repo
        .join("docs/reports/P044_runs/native-first-core-download")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&output).unwrap();
    let requests = PathBuf::from(fixture.data["requests"].as_str().unwrap());
    let trace = || {
        std::fs::read_to_string(&requests)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .collect::<Vec<_>>()
    };
    let before = trace();
    let root = fixture._root.path().join("core-first-download");
    let location = download_restart::create_location(db, &root, ctx).await;
    let variants = Arc::new(larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    });
    let original = variants.get_variant(variant_id).await.unwrap().unwrap();
    assert_eq!(original.lifecycle, "DISCOVERED");
    assert!(original.bundle_hash.is_none());
    assert!(variants.bundle_of(variant_id).await.unwrap().is_none());
    let acquisitions = Arc::new(larm_db::acquisition::SqlAcquisitionManifests {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    });
    let snapshot = acquisitions.latest(variant_id).await.unwrap().unwrap();
    let models = Arc::new(larm_db::models::SqlModels {
        pool: db.pool.clone(),
    });
    let locations = Arc::new(larm_db::locations::SqlStorageLocations {
        pool: db.pool.clone(),
    });
    let reservations = Arc::new(larm_db::reservations::SqlVolumeReservations {
        pool: db.pool.clone(),
    });
    let plans = Arc::new(larm_db::download_plans::SqlDownloadPlans {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    });
    let sources = Arc::new(
        qualified_download_execution::QualifiedDownloadSourceResolver {
            sources: larm_db::sources::SqlSources {
                pool: db.pool.clone(),
                qualification: Some(runner.clone()),
            },
            conformance: larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            },
            models: (*models).clone(),
            variants: variants.clone(),
            runner: runner.clone(),
        },
    );
    let gate = Arc::new(
        qualified_download_execution::QualifiedDownloadExecutionGate {
            sources: sources.clone(),
        },
    );
    let mirrors = Arc::new(
        qualified_download_execution::QualifiedDownloadMirrorCatalog {
            variants: variants.clone(),
            models: models.clone(),
            sources: larm_db::sources::SqlSources {
                pool: db.pool.clone(),
                qualification: Some(runner.clone()),
            },
            conformance: larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            },
        },
    );
    let prober: Arc<dyn StorageProber> = Arc::new(larm_hardware::storage::NativeStorageProber {
        command: larm_hardware::devices::NativeCommand {
            program: env!("CARGO_BIN_EXE_larm-daemon").into(),
            args: vec!["probe-storage".into()],
            timeout: Duration::from_secs(60),
        },
    });
    let storage = Arc::new(Refresh(prober.clone()));
    let policy = larm_storage::estimate::SpacePolicy {
        margin: larm_domain::estimate::MarginPolicy {
            minimum_margin_bytes: 1 << 20,
            margin_basis_points: 500,
        },
        minimum_free_bytes: 0,
    };
    let preview = larm_download::preview_service::DownloadPlanPreview {
        acquisitions: acquisitions.clone(),
        gate: gate.clone(),
        variants: variants.clone(),
        models: models.clone(),
        locations: locations.clone(),
        storage: storage.clone(),
        reservations: reservations.clone(),
        plans: plans.clone(),
        mirrors: mirrors.clone(),
        policy,
    };
    let activation = larm_download::activation_service::DownloadPlanActivator {
        acquisitions: acquisitions.clone(),
        plans: plans.clone(),
        activation: plans.clone(),
        variants: variants.clone(),
        models,
        locations: locations.clone(),
        storage,
        reservations,
        mirrors,
        gate,
        policy,
    };
    let request = larm_domain::download::DownloadPlanRequest {
        variant_id: variant_id.into(),
        storage_location_id: Some(location.id.clone()),
        // A scalar without explicit source certainty remains UNKNOWN. This is
        // the operator's bound, not a fabricated upstream size declaration.
        hard_budget_bytes: larm_domain::units::DecimalU64::new(8 << 20),
        source_id: None,
        priority: larm_domain::download::DownloadPriority::Normal,
        optional_artifact_ids: vec![],
    };
    let planned = preview.preview(&request, ctx).await.unwrap();
    std::fs::write(
        output.join("preview.json"),
        serde_json::to_vec_pretty(&planned).unwrap(),
    )
    .unwrap();
    assert!(
        planned.plan_id.is_some(),
        "native preparation was not eligible; see owned preview receipt"
    );
    assert_eq!(planned.acquisition, Some(snapshot.binding()));
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM model_bundle),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation)")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    assert_eq!(
        variants.get_variant(variant_id).await.unwrap().unwrap(),
        original
    );
    assert_eq!(
        trace().len(),
        before.len(),
        "preparation never reads upstream bytes"
    );
    let mut execute = larm_domain::download::ExecutePlan {
        plan_id: planned.plan_id.clone().unwrap(),
        plan_hash: planned.plan_hash.clone().unwrap(),
        operator_confirmed: false,
    };
    let idempotency = DownloadActivationIdempotency {
        key: uuid::Uuid::now_v7().to_string(),
        body_hash: hex::encode(Sha256::digest(b"owned native first acquisition")),
    };
    assert_eq!(
        activation
            .activate(&execute, ctx, &idempotency)
            .await
            .unwrap_err()
            .details
            .reason,
        "operator_confirmation_required"
    );
    execute.operator_confirmed = true;
    let accepted = activation
        .activate(&execute, ctx, &idempotency)
        .await
        .unwrap();
    assert_eq!(
        activation
            .activate(&execute, ctx, &idempotency)
            .await
            .unwrap(),
        accepted
    );
    let consumer = larm_download::consumer::DownloadConsumer {
        jobs: larm_db::download_jobs::SqlDownloadJobs {
            pool: db.pool.clone(),
        },
        plans: (*plans).clone(),
        progress: larm_download::transfer::SqliteTransferProgress::new(
            larm_db::download_progress::SqlDownloadProgress {
                pool: db.pool.clone(),
            },
        ),
        sources,
        storage: Arc::new(
            qualified_download_execution::OnlineDownloadStorageRootResolver {
                locations: (*locations).clone(),
            },
        ),
        bandwidth_limiter: Arc::new(larm_download::policy::BandwidthLimiter::new(None)),
    };
    let outcome = consumer.run_once().await.unwrap();
    if !matches!(
        outcome,
        larm_download::consumer::DownloadConsumerOutcome::Completed { .. }
    ) {
        let error: Option<String> = sqlx::query_scalar("SELECT error FROM download_job WHERE id=?")
            .bind(&accepted.job_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        std::fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"outcome":format!("{outcome:?}"),"error":error}))
                .unwrap(),
        )
        .unwrap();
        panic!("native first acquisition failed; diagnostic persisted in owned evidence directory");
    }
    let physical = variants.bundle_of(variant_id).await.unwrap().unwrap();
    assert_eq!(physical.state, larm_domain::model::BundleState::Verified);
    let final_variant = variants.get_variant(variant_id).await.unwrap().unwrap();
    assert_eq!(final_variant.lifecycle, "VERIFIED");
    assert_eq!(final_variant.availability, "UNKNOWN");
    let (blob,digest,trust):(String,String,String)=sqlx::query_as("SELECT l.relative_path,l.sha256,p.trust FROM artifact_location l JOIN artifact_provenance p ON p.artifact_id=l.artifact_id WHERE l.artifact_id=?")
        .bind(&snapshot.artifacts[0].artifact_id).fetch_one(&db.pool).await.unwrap();
    let bytes = std::fs::read(root.join(&blob)).unwrap();
    let expected = std::fs::read(fixture.data["expected_files"][0].as_str().unwrap()).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(digest, hex::encode(Sha256::digest(&bytes)));
    assert_eq!(trust, "VERIFIED");
    let pending: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(pending, (0, 0));
    let jobs = Arc::new(larm_db::model_verification::SqlModelVerificationJobs {
        pool: db.pool.clone(),
    });
    use larm_domain::model_verification::ModelVerificationSubmitter;
    jobs.enqueue_verification(variant_id, ctx).await.unwrap();
    let verification = larm_models::verification::ModelVerificationWorker::new(
        larm_models::verification::VerificationPorts {
            jobs,
            variants: variants.clone(),
            artifact_locations: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            storage_locations: locations,
            prober,
        },
    );
    assert!(verification.run_once().await.unwrap());
    assert_eq!(
        variants.bundle_of(variant_id).await.unwrap().unwrap().state,
        larm_domain::model::BundleState::Verified
    );
    let after = trace();
    assert!(after[before.len()..]
        .iter()
        .any(|row| row["path"].as_str().unwrap().starts_with("/artifact/")));
    let result = json!({"status":"PASS","scope":"product preview/explicit confirmation/activation/consumer/SQLite with original P026-qualified source worker and observed Windows volume; own loopback bytes; no TCP/GUI/load/private-auth acceptance",
        "variant_id":variant_id,"acquisition":snapshot.binding(),"plan":planned,"job_id":accepted.job_id,
        "bundle":physical,"final_variant":final_variant,"size_bytes":bytes.len().to_string(),"sha256":digest,"trust":trust,
        "no_bytes_before_confirmation":true,"confirmation_replay_same_job":true,"independent_cas_bytes_equal":true,"reverification":"VERIFIED","pending_reservations_and_locks":pending});
    std::fs::copy(&requests, output.join("requests.ndjson")).unwrap();
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}
