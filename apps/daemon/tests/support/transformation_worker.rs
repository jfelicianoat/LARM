//! Original native P026 followed by the typed consumer of that qualification.
//! Output stays in the reference worker's private ledger, outside Core models.
use super::*;
use base64::Engine;
use larm_adapter_host::conformance::{
    transformation::TransformationConfig, NativeConformanceRunner,
};
use larm_domain::{
    conformance::{hash, Run},
    error::ErrorCode,
    model::{bundle_hash, BundleEntry},
    ports::ConformanceProfileRepository,
    transformations::{ExecuteTransformationPlan, TransformationRequest},
    units::DecimalU64,
};

pub(super) async fn exercise(
    db: &larm_db::pool::Db,
    runner: &Arc<NativeConformanceRunner>,
    installation: &AdapterInstallation,
    run: &Run,
    ctx: &MutationContext,
    fixture: &Fixture,
    config: &TransformationConfig,
) -> Value {
    let source = fixture.work.join("input/fixture.bin");
    let original = std::fs::read(&source).unwrap();
    let profile = larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .conformance_profile(&installation.id)
    .await
    .unwrap()
    .unwrap();
    let worker = runner
        .transformation_worker(installation, run, ctx)
        .await
        .unwrap();
    assert!(std::fs::write(&source, b"must remain pinned").is_err());
    let duplicate = runner.transformation_worker(installation, run, ctx).await;
    assert_eq!(
        duplicate.err().unwrap().details.reason,
        "conformance_already_running"
    );
    let access = Access {
        reads: ["input", "profile", "state", "output"]
            .into_iter()
            .map(|p| fixture.work.join(p))
            .collect(),
        writes: ["state", "output"]
            .into_iter()
            .map(|p| fixture.work.join(p))
            .collect(),
        ..Access::default()
    };
    let id = || uuid::Uuid::now_v7().to_string();
    let description = worker.describe(&id(), &access, None).await.unwrap();
    assert!(description.formats_output.iter().any(|f| f == "BASE64"));
    assert!(!description.pause_supported);
    let request = TransformationRequest {
        input_variant_id: fixture.data["profile"]["input_variant_id"]
            .as_str()
            .unwrap()
            .into(),
        adapter_id: installation.adapter_id.clone(),
        tool_version: description.tool_version,
        target_format: "BASE64".into(),
        parameters: json!({}),
        storage_location_id: fixture.data["profile"]["storage_location_id"]
            .as_str()
            .unwrap()
            .into(),
        hard_budget_bytes: DecimalU64::new(1 << 30),
    };
    assert_eq!(
        worker
            .plan("invalid", &request, &access, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ValidationFailed
    );
    let mut foreign = request.clone();
    foreign.adapter_id = "foreign.transformation".into();
    assert_eq!(
        worker
            .plan(&id(), &foreign, &access, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let denied = Access {
        reads: vec![config.forbidden.clone()],
        ..Access::default()
    };
    assert_eq!(
        worker
            .plan(&id(), &request, &denied, None)
            .await
            .unwrap_err()
            .details
            .reason,
        "read_denied"
    );
    let plan = worker.plan(&id(), &request, &access, None).await.unwrap();
    assert!(
        plan.storage_estimate.per_volume.iter().all(|v| !v.eligible),
        "private preflight is not global admission"
    );
    let binding = worker.execution_binding(&plan).unwrap();
    assert_eq!(binding.adapter_installation_id, installation.id);
    assert_eq!(binding.conformance_run_id, run.id);
    assert_eq!(binding.conformance_run_sha256, run.digest().unwrap());
    assert_eq!(binding.profile_sha256, hash(&profile).unwrap());
    assert_eq!(binding.worker_plan_id, plan.plan_id);
    let execute = ExecuteTransformationPlan {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        approval_id: None,
    };
    let receipt = worker
        .execute(&id(), &execute, &access, Some(Duration::from_secs(60)))
        .await
        .unwrap();
    let replay = worker
        .execute(&id(), &execute, &access, None)
        .await
        .unwrap();
    assert_eq!(receipt, replay);
    let manifest = worker
        .verify(&id(), &receipt.job_id, &access, None)
        .await
        .unwrap();
    assert_eq!(manifest.artifacts.len(), 2);
    let expected = base64::engine::general_purpose::STANDARD.encode(&original);
    let evidence = config.evidence_root.join("typed-consumer");
    assert!(!evidence.exists());
    std::fs::create_dir_all(&evidence).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let input_evidence = evidence.join("input.bin");
    std::fs::write(&input_evidence, &original).unwrap();
    let mut retained = Vec::new();
    let mut entries = Vec::new();
    let mut encoded = false;
    for artifact in &manifest.artifacts {
        let data =
            std::fs::read(fixture.work.join("output").join(&artifact.relative_path)).unwrap();
        assert_eq!(
            artifact.sha256.as_deref(),
            Some(hex::encode(Sha256::digest(&data)).as_str())
        );
        assert_eq!(artifact.size_bytes.unwrap().get(), data.len() as u64);
        encoded |= data == expected.as_bytes();
        let file = evidence.join(format!("artifact-{}.bin", retained.len()));
        std::fs::write(&file, &data).unwrap();
        retained.push(
            json!({"relative_path":artifact.relative_path,"file":file.strip_prefix(repo).unwrap(),
            "sha256":artifact.sha256,"size_bytes":artifact.size_bytes}),
        );
        entries.push(BundleEntry {
            relative_path: artifact.relative_path.clone(),
            sha256: artifact.sha256.clone().unwrap(),
            size_bytes: data.len() as u64,
            required: artifact.required,
        });
    }
    assert!(encoded);
    assert_eq!(bundle_hash(&entries).unwrap(), manifest.manifest_hash);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    worker.recheck().await.unwrap();
    // Adding a previously unowned package entry changes the measured context.
    // Cleanup must still work after qualification is lost.
    let unexpected = fixture.package.join("typed-consumer-owned-drift.txt");
    assert!(!unexpected.exists());
    std::fs::write(&unexpected, b"owned qualification drift probe").unwrap();
    let drift = worker.describe(&id(), &access, None).await;
    worker.close().await.unwrap();
    assert!(worker.io_drained());
    drop(worker);
    std::fs::remove_file(&unexpected).unwrap();
    std::fs::write(&source, &original).unwrap();
    assert_eq!(drift.err().unwrap().details.reason, "package_hash_mismatch");
    let core:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(core, (0, 0, 0));
    let lifecycle = super::transformation_lifecycle::exercise(
        db,
        runner,
        installation,
        run,
        ctx,
        super::transformation_lifecycle::Inputs {
            fixture,
            config,
            request: &request,
        },
    )
    .await;
    json!({"status":"PASS","scope":"typed qualified RPC consumer with initial private-ledger effects measured before Core, followed by the separately recorded Core lifecycle/CAS/atomic publication; no public daemon pipeline, GUI or GGUF acceptance",
        "binding":binding,"worker_job_id":receipt.job_id,"manifest":manifest,"source_preserved":true,
        "independent_output_sha256_and_size":true,"io_drained":true,"input_pins_released":true,
        "qualification_drift_rejected":true,"cleanup_after_qualification_loss":true,"core_effects":core,
        "retained_artifacts":retained,"input_evidence":input_evidence.strip_prefix(repo).unwrap(),"core_lifecycle":lifecycle})
}
