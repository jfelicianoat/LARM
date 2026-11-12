//! Real qualified reference worker, measured outputs, managed CAS and Core
//! SQLite. Fault triggers and free-space pressure are explicit fixtures.
use super::*;
use larm_adapter_host::{
    conformance::transformation::TransformationConfig, input_files::PinnedReadFile,
    transformation::TransformationWorker,
};
use larm_db::{pool::Db, transformations::SqlTransformationJobs};
use larm_domain::{
    error::ErrorCode,
    ports::StorageProber,
    transformation_execution::{TransformationAttemptGuard, TransformationClaim},
    transformations::{
        ExecuteTransformationPlan, TransformationInputGuard, TransformationJob,
        TransformationSpaceObserver, TransformationState,
    },
};
use larm_transform::{
    output::{TransformationOutputScope, VerifiedTransformationOutput},
    output_validation::TransformationOutputValidators,
    publication::PublishedTransformationOutput,
};

pub(super) struct Inputs<'a> {
    pub db: &'a Db,
    pub jobs: &'a SqlTransformationJobs,
    pub claim: &'a TransformationClaim,
    pub previous: &'a TransformationClaim,
    pub guard: &'a dyn TransformationAttemptGuard,
    pub worker: &'a TransformationWorker,
    pub input: &'a dyn TransformationInputGuard,
    pub space: &'a dyn TransformationSpaceObserver,
    pub prober: Arc<dyn StorageProber>,
    pub destination: &'a Path,
    pub config: &'a TransformationConfig,
    pub access: &'a Access,
}
fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
async fn snapshot(db: &Db, job: &str) -> Value {
    let row:(String,i64,String,Option<String>,Option<String>)=sqlx::query_as("SELECT state,row_version,metadata,error,output_variant_id FROM transformation_job WHERE id=?").bind(job).fetch_one(&db.pool).await.unwrap();
    let counts:(i64,i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM model_variant),(SELECT count(*) FROM model_artifact),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM artifact_provenance),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM volume_reservation)").fetch_one(&db.pool).await.unwrap();
    json!({"job":row,"counts":counts})
}
pub(super) async fn exercise(inputs: Inputs<'_>) -> (TransformationJob, Value) {
    let Inputs {
        db,
        jobs,
        claim,
        previous,
        guard,
        worker,
        input,
        space,
        prober,
        destination,
        config,
        access,
    } = inputs;
    let binding = claim.plan.execution_binding.as_ref().unwrap();
    let receipt = worker
        .execute(
            &id(),
            &ExecuteTransformationPlan {
                plan_id: binding.worker_plan_id.clone(),
                plan_hash: binding.worker_plan_hash.clone(),
                approval_id: None,
            },
            access,
            Some(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    jobs.mark_verifying(claim, guard).await.unwrap();
    let reply = worker
        .verify_claim_output(claim, &id(), &receipt.job_id, access, None)
        .await
        .unwrap();
    let prefix = reply.manifest().artifacts[0]
        .relative_path
        .rsplit_once('/')
        .unwrap()
        .0
        .to_owned();
    // Reference-specific registered namespace rule; an arbitrary manifest
    // prefix alone does not identify the expected worker attempt.
    let private_attempt: u32 = prefix
        .strip_prefix(&format!("{}-", receipt.job_id))
        .unwrap()
        .parse()
        .unwrap();
    assert!(private_attempt > 0);
    worker.close().await.unwrap();
    let measured = VerifiedTransformationOutput::capture(
        claim,
        &reply,
        guard,
        TransformationOutputScope {
            root: destination.into(),
            bundle_prefix: prefix,
        },
        prober.clone(),
    )
    .await
    .unwrap();
    let checked = Arc::new(
        TransformationOutputValidators::default()
            .validate(measured)
            .await
            .unwrap(),
    );
    let baseline = snapshot(db, &claim.job.id).await;
    assert!(!destination.join("blobs").exists());
    let mut negatives = Vec::new();
    for trigger in [
        format!("CREATE TEMP TRIGGER publication_fault BEFORE UPDATE ON operation_journal WHEN NEW.id='{}' AND json_type(NEW.payload,'$.publication')='object' BEGIN SELECT RAISE(IGNORE); END",claim.job.id),
        "CREATE TEMP TRIGGER publication_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(IGNORE); END".into(),
    ] {
        // TEMP triggers are connection-local: use persistent fixture triggers
        // on the test database so every pool connection observes the fault.
        let trigger=trigger.replace("CREATE TEMP TRIGGER","CREATE TRIGGER");
        // Fixed fixture SQL plus the internally minted job UUID only.
        sqlx::query(sqlx::AssertSqlSafe(trigger.clone())).execute(&db.pool).await.unwrap();
        assert!(jobs.prepare_publication(claim,guard,input,checked.as_ref()).await.is_err());
        sqlx::query("DROP TRIGGER publication_fault").execute(&db.pool).await.unwrap();
        assert_eq!(snapshot(db,&claim.job.id).await,baseline);
        assert!(!destination.join("blobs").exists());
        negatives.push(json!({"phase":"prepare","trigger":trigger,"status":"PASS"}));
    }
    let model_id:String=sqlx::query_scalar("SELECT d.canonical_model_id FROM model_distribution d JOIN model_revision r ON r.distribution_id=d.id WHERE r.id=?").bind(&claim.input_identity.revision_id).fetch_one(&db.pool).await.unwrap();
    let metadata: String = sqlx::query_scalar("SELECT metadata FROM canonical_model WHERE id=?")
        .bind(&model_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE canonical_model SET metadata=? WHERE id=?")
        .bind(json!({"fixture_padding":"x".repeat(1<<20)}).to_string())
        .bind(&model_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(jobs
        .prepare_publication(claim, guard, input, checked.as_ref())
        .await
        .is_err());
    sqlx::query("UPDATE canonical_model SET metadata=? WHERE id=?")
        .bind(metadata)
        .bind(&model_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(snapshot(db, &claim.job.id).await, baseline);
    negatives.push(json!({"phase":"prepare","case":"actual_serialized_graph_exceeds_control_budget","status":"PASS","metadata_pressure":"fixture"}));

    let prepared = Arc::new(
        jobs.prepare_publication(claim, guard, input, checked.as_ref())
            .await
            .unwrap(),
    );
    let durable = snapshot(db, &claim.job.id).await;
    let replay = jobs
        .prepare_publication(claim, guard, input, checked.as_ref())
        .await
        .unwrap();
    assert_eq!(prepared.record(), replay.record());
    assert_eq!(snapshot(db, &claim.job.id).await, durable);
    assert!(jobs.confirm_publication(&prepared).await.is_err());
    let mut pressure = space.observe().await.unwrap();
    pressure.demands[0].os_free = 0;
    assert_eq!(
        jobs.check_publication_space(&prepared, &pressure)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageInsufficient
    );
    assert!(!destination.join("blobs").exists());
    negatives.push(json!({"phase":"copy","case":"synthetic_free_space_pressure","status":"PASS","observation":"fixture"}));
    let record = prepared.record().clone();
    let sha = prepared.record_sha256().to_owned();
    drop(replay);
    drop(prepared);
    let recovered = Arc::new(
        jobs.prepared_publication(claim, guard, input)
            .await
            .unwrap(),
    );
    assert_eq!(recovered.record_sha256(), sha);
    let first = PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober.clone(),
        space,
        jobs,
    )
    .await
    .unwrap();
    assert_eq!(snapshot(db, &claim.job.id).await, durable);
    assert!(jobs.confirm_publication(&recovered).await.is_err());
    // The real OS lease rejects a second concurrent publisher of this claim.
    assert!(PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober.clone(),
        space,
        jobs
    )
    .await
    .is_err());
    let blob = destination.join(&record.locations[0].relative_path);
    let pinned = PinnedReadFile::open(&blob).unwrap();
    drop(first);
    let reentered = PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober.clone(),
        space,
        jobs,
    )
    .await
    .unwrap();
    pinned.recheck().unwrap();
    drop(pinned);
    drop(reentered);

    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let original = std::fs::read(&blob).unwrap();
    let mut permissions = std::fs::metadata(&blob).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    {
        permissions.set_readonly(false);
    }
    std::fs::set_permissions(&blob, permissions).unwrap();
    let mut damaged = original.clone();
    damaged[0] ^= 255;
    std::fs::write(&blob, &damaged).unwrap();
    let corrupt_evidence = config.evidence_root.join("core-cas-corrupt.bin");
    std::fs::write(&corrupt_evidence, &damaged).unwrap();
    assert!(PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober.clone(),
        space,
        jobs
    )
    .await
    .is_err());
    assert_eq!(snapshot(db, &claim.job.id).await, durable);
    std::fs::write(&blob, &original).unwrap();
    let mut permissions = std::fs::metadata(&blob).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&blob, permissions).unwrap();
    negatives.push(json!({"phase":"recover","case":"same_length_cas_corruption","status":"PASS","file":corrupt_evidence.strip_prefix(project).unwrap(),"sha256":hex::encode(Sha256::digest(&damaged))}));
    let stage = destination.join("staging").join(&claim.fence.claim_id);
    let receipt_path = stage.join(format!("{}.promote.json", record.artifacts[0].id));
    let receipt_bytes = std::fs::read(&receipt_path).unwrap();
    let mut bad_receipt: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    bad_receipt["plan_hash"] = json!("f".repeat(64));
    std::fs::write(&receipt_path, serde_json::to_vec(&bad_receipt).unwrap()).unwrap();
    assert!(PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober.clone(),
        space,
        jobs
    )
    .await
    .is_err());
    std::fs::write(&receipt_path, receipt_bytes).unwrap();
    negatives.push(json!({"phase":"recover","case":"receipt_of_another_plan","status":"PASS"}));
    let published = PublishedTransformationOutput::publish(
        recovered.clone(),
        checked.clone(),
        guard,
        prober,
        space,
        jobs,
    )
    .await
    .unwrap();
    assert_eq!(
        jobs.complete_publication(previous, guard, input, &recovered, &published)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    for (table, operation, condition) in [
        ("model_variant", "INSERT", "1"),
        ("model_artifact", "INSERT", "1"),
        ("model_bundle", "INSERT", "1"),
        ("artifact_location", "INSERT", "1"),
        ("artifact_provenance", "INSERT", "1"),
        ("event", "INSERT", "1"),
        ("event_outbox", "INSERT", "1"),
        ("transformation_job", "UPDATE", "NEW.state='COMPLETED'"),
        ("operation_journal", "UPDATE", "NEW.phase='COMPLETED'"),
        ("resource_lock", "DELETE", "1"),
        ("volume_reservation", "DELETE", "1"),
    ] {
        let trigger=format!("CREATE TRIGGER publication_fault BEFORE {operation} ON {table} WHEN {condition} BEGIN SELECT RAISE(IGNORE); END");
        // Table, operation and condition all come from the literal list above.
        sqlx::query(sqlx::AssertSqlSafe(trigger))
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            jobs.complete_publication(claim, guard, input, &recovered, &published)
                .await
                .is_err(),
            "{table}"
        );
        sqlx::query("DROP TRIGGER publication_fault")
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(snapshot(db, &claim.job.id).await, durable, "{table}");
        negatives.push(json!({"phase":"commit","table":table,"status":"PASS"}));
    }
    let completed = jobs
        .complete_publication(claim, guard, input, &recovered, &published)
        .await
        .unwrap();
    assert_eq!(completed.state, TransformationState::Completed);
    assert_eq!(completed.attempt, 3);
    assert_eq!(
        completed.output_variant_id.as_ref(),
        Some(&record.variant.id)
    );
    let committed = snapshot(db, &claim.job.id).await;
    assert_eq!(
        jobs.complete_publication(claim, guard, input, &recovered, &published)
            .await
            .unwrap(),
        completed
    );
    assert_eq!(snapshot(db, &claim.job.id).await, committed);
    let confirmation = jobs.confirm_publication(&recovered).await.unwrap();
    published.finish(&confirmation).unwrap();
    assert!(!stage.join("publication.json").exists());
    for artifact in &record.artifacts {
        assert!(!stage.join(format!("{}.promote.json", artifact.id)).exists());
        assert!(!stage.join(format!("{}.partial", artifact.id)).exists());
    }
    let mut artifacts = Vec::new();
    for (index, location) in record.locations.iter().enumerate() {
        let bytes = std::fs::read(destination.join(&location.relative_path)).unwrap();
        let evidence = config
            .evidence_root
            .join(format!("core-published-{index}.bin"));
        std::fs::write(&evidence, &bytes).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            location.sha256.as_ref().unwrap().as_str()
        );
        artifacts.push(json!({"file":evidence.strip_prefix(project).unwrap(),"location":location,"artifact":record.artifacts[index],"provenance":record.provenance[index]}));
    }
    let manifest = config.evidence_root.join("core-published-manifest.json");
    std::fs::copy(
        destination
            .join("manifests")
            .join(format!("{}.json", record.variant.id)),
        &manifest,
    )
    .unwrap();
    let graph = config.evidence_root.join("core-publication-record.json");
    std::fs::write(&graph, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let result = json!({"status":"PASS","scope":"Direct native qualified process/Core/SQLite/CAS, seeded catalogue/preview/reference namespace; handle/repository reentry, no actual daemon crash", "record_file":graph.strip_prefix(project).unwrap(),"record_sha256":sha,
        "manifest_file":manifest.strip_prefix(project).unwrap(),"artifacts":artifacts,"negatives":negatives,
        "private_worker_attempt":private_attempt,"worker_job_id":receipt.job_id,"completed_job":completed,
        "prepared_snapshot":durable,"committed_snapshot":committed,"copy_reentry":true,"pinned_cas_dedup":true,
        "exclusive_writer_rejected":true,"receipts_removed_after_commit":true,"idempotent_completion":true,"catalogue_published":true});
    (completed, result)
}
