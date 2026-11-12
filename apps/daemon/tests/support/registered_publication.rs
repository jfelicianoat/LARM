//! Registered native guardian -> observed Verify -> physical output -> CAS.
//! The catalogue/global preview/admission controller remains an explicit fixture.
use super::*;
use larm_adapter_host::guardian::GuardianProcess;
use larm_domain::transformation_publication::TransformationMeasuredOutputGuard;
use larm_transform::{
    output::{TransformationOutputScope, VerifiedTransformationOutput},
    output_validation::TransformationOutputValidators,
    publication::PublishedTransformationOutput,
};
use std::io::{Read, Seek, SeekFrom};

pub(super) struct Inputs {
    pub db: larm_db::pool::Db,
    pub prepared: Arc<Setup>,
    pub request: TransformationRequest,
    pub identity: SessionIdentity,
    pub context: MutationContext,
    pub backend_actor: String,
    pub access: Access,
    pub data: PathBuf,
    pub output: PathBuf,
    pub previous: larm_domain::transformations::TransformationJob,
    pub repo: PathBuf,
}

/// Keep the large native scenario out of its caller's coroutine layout and
/// polling stack. The runtime uses its ordinary worker stack; no stack override.
pub(super) fn start(input: Inputs) -> tokio::task::JoinHandle<Value> {
    let output = input.output.clone();
    let future = exercise(input);
    checkpoint(
        &output,
        "publication-future",
        json!({
            "future_bytes":std::mem::size_of_val(&future),
            "heap_pinned_by_driver":true,"independent_runtime_task":true,
            "test_thread_stack_override":false,
        }),
    );
    tokio::spawn(Box::pin(future))
}
struct View<'a> {
    db: &'a larm_db::pool::Db,
    prepared: &'a Setup,
    request: &'a TransformationRequest,
    identity: &'a SessionIdentity,
    context: &'a MutationContext,
    backend_actor: &'a str,
    access: &'a Access,
    data: &'a Path,
    output: &'a Path,
    previous: &'a larm_domain::transformations::TransformationJob,
}
async fn exercise(input: Inputs) -> Value {
    let repo = input.repo.as_path();
    let e = View {
        db: &input.db,
        prepared: &input.prepared,
        request: &input.request,
        identity: &input.identity,
        context: &input.context,
        backend_actor: &input.backend_actor,
        access: &input.access,
        data: &input.data,
        output: &input.output,
        previous: &input.previous,
    };
    let directory = e.output.join("observed-output-publication");
    std::fs::create_dir(&directory).unwrap();
    let ctx = MutationContext {
        actor_id: e.context.actor_id.clone(),
        correlation_id: id(),
    };
    let preview_bootstrap = GuardianBootstrap::transformation(
        e.data.join("larm.sqlite"),
        e.identity.clone(),
        ctx.clone(),
        TransformationAttemptFence {
            job_id: id(),
            claim_id: id(),
            generation: 0,
            attempt: 1,
            plan_hash: "a".repeat(64),
        },
    )
    .unwrap();
    let mut preview_process = GuardianProcess::start(
        Path::new(env!("CARGO_BIN_EXE_larm-daemon")),
        &preview_bootstrap,
    )
    .await
    .unwrap();
    let preview_cleanup = RetainedHelper::capture(&preview_process);
    let preview = Arc::new(preview_process.client(&preview_bootstrap).await.unwrap());
    let worker_plan = preview
        .plan(&id(), e.request, e.access, None)
        .await
        .unwrap();
    let input = e
        .prepared
        .env
        .input(&e.request.input_variant_id, None)
        .await
        .unwrap();
    let plan = mapped_plan(
        e.prepared,
        e.request,
        input.artifacts(),
        &worker_plan,
        preview.execution_binding(&worker_plan).unwrap(),
    )
    .await;
    let plans = SqlTransformationPlans {
        pool: e.db.pool.clone(),
        qualification: Some(Arc::new(PreviewQualification {
            client: preview.clone(),
            access: e.access.clone(),
            actor: e.backend_actor.into(),
        })),
    };
    plans.store_plan(&plan, &ctx).await.unwrap();
    let space = e
        .prepared
        .env
        .space(
            &plan,
            vec![WorkspaceRoot {
                path: e.prepared.workspace.clone(),
                volume_key: e.prepared.key.clone(),
            }],
        )
        .await
        .unwrap();
    let execute = ExecuteTransformationPlan {
        plan_id: plan.response.plan_id.clone(),
        plan_hash: plan.response.plan_hash.clone(),
        approval_id: None,
    };
    let receipt = plans
        .restart_plan(
            &larm_domain::transformations::TransformationRestart {
                job_id: e.previous.id.clone(),
                if_match: e.previous.row_version as u64,
            },
            &execute,
            &ctx,
            &TransformationActivationIdempotency {
                key: format!("observed-publication-{}", id()),
                body_hash: hash(&execute).unwrap(),
            },
            TransformationAdmission {
                input: &input,
                space: &space,
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.job_id, e.previous.id);
    preview.owner().stop().await.unwrap();
    wait_closed(preview.owner()).await;
    NativeGuardian::attach(preview_bootstrap.descriptor().clone())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    preview_process.wait().await.unwrap();
    drop(preview_cleanup);
    drop(plans);

    let jobs = SqlTransformationJobs {
        pool: e.db.pool.clone(),
    };
    let proposal = jobs
        .propose_native_preparation(&receipt.job_id)
        .await
        .unwrap();
    let bootstrap = GuardianBootstrap::registered_transformation(
        e.data.join("larm.sqlite"),
        e.identity.clone(),
        &proposal,
    )
    .unwrap();
    let owner = bootstrap.descriptor().native_owner.clone().unwrap();
    assert_eq!(owner.fence.attempt, 4);
    assert_eq!(owner.fence.generation, 3);
    jobs.prepare_native_owner(&proposal, &owner).await.unwrap();
    std::fs::write(
        directory.join("descriptor.json"),
        serde_json::to_vec_pretty(bootstrap.descriptor()).unwrap(),
    )
    .unwrap();
    let mut process =
        GuardianProcess::start(Path::new(env!("CARGO_BIN_EXE_larm-daemon")), &bootstrap)
            .await
            .unwrap();
    let cleanup = RetainedHelper::capture(&process);
    let client = process.client(&bootstrap).await.unwrap();
    let guard = NativeTransformationAttempt::attach(owner.clone())
        .await
        .unwrap();
    guard.require_ready().await.unwrap();
    let before = events(e.db, &receipt.job_id).await;
    assert!(client
        .verify_claim_output(&proposal.claim, &id(), &id(), e.access, None)
        .await
        .is_err());
    assert_eq!(events(e.db, &receipt.job_id).await, before);
    let claimed = Box::pin(jobs.claim_with_native_owner(&proposal, &guard))
        .await
        .unwrap();
    let claim = &claimed.claim;
    let baseline = events(e.db, &receipt.job_id).await;
    let mut negatives = Vec::new();
    // These malformed labels must be rejected locally, before a helper call.
    for fault in 0..10 {
        let mut bad = claim.clone();
        let name = match fault {
            0 => {
                bad.fence.claim_id = id();
                "foreign_claim_nonce"
            }
            1 => {
                bad.fence.generation += 1;
                "old_generation"
            }
            2 => {
                bad.job.id = id();
                "foreign_core_job"
            }
            3 => {
                bad.job.adapter_id = id();
                "foreign_installation"
            }
            4 => {
                bad.job.attempt += 1;
                "foreign_attempt"
            }
            5 => {
                bad.job.metadata.insert("native_owner".into(), Value::Null);
                "owner_missing"
            }
            6 => {
                bad.job
                    .metadata
                    .insert("attempt_execution".into(), Value::Null);
                "execution_missing"
            }
            7 => {
                bad.context.actor_id = id();
                "foreign_actor"
            }
            8 => {
                bad.context.correlation_id = id();
                "foreign_correlation"
            }
            _ => {
                bad.plan
                    .execution_binding
                    .as_mut()
                    .unwrap()
                    .worker_plan_hash = "0".repeat(64);
                "changed_sealed_binding"
            }
        };
        let error = client
            .verify_claim_output(&bad, &id(), &id(), e.access, None)
            .await
            .err()
            .unwrap();
        assert_eq!(events(e.db, &receipt.job_id).await, baseline);
        assert_eq!(counts(e.db).await, (1, 1, 1, 4));
        negatives.push(json!({"case":name,"reason":error.details.reason,"core_unchanged":true}));
    }
    assert!(preview
        .verify_claim_output(claim, &id(), &id(), e.access, None)
        .await
        .is_err());
    negatives.push(
        json!({"case":"unregistered_preview_cannot_mint_observed_output","core_unchanged":true}),
    );
    drop(preview);
    let worker_receipt = client
        .execute(
            &id(),
            &ExecuteTransformationPlan {
                plan_id: owner.binding.worker_plan_id.clone(),
                plan_hash: owner.binding.worker_plan_hash.clone(),
                approval_id: None,
            },
            e.access,
            Some(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    jobs.mark_verifying(claim, &guard).await.unwrap();
    let verifying_events = events(e.db, &receipt.job_id).await;
    let foreign = client
        .verify_claim_output(claim, &id(), &id(), e.access, None)
        .await
        .err()
        .unwrap();
    assert_eq!(
        foreign.details.reason,
        "guardian_registered_rpc_scope_invalid"
    );
    assert_eq!(events(e.db, &receipt.job_id).await, verifying_events);
    assert_eq!(counts(e.db).await, (1, 1, 1, 4));
    negatives.push(
        json!({"case":"foreign_worker_job","reason":foreign.details.reason,"core_unchanged":true}),
    );
    let reply = client
        .verify_claim_output(claim, &id(), &worker_receipt.job_id, e.access, None)
        .await
        .unwrap();
    assert_eq!(reply.fence(), &owner.fence);
    assert_eq!(reply.binding(), &owner.binding);
    assert_eq!(reply.worker_job_id(), worker_receipt.job_id);
    let prefix = reply.manifest().artifacts[0]
        .relative_path
        .rsplit_once('/')
        .unwrap()
        .0
        .to_owned();
    let attempt: u32 = prefix
        .strip_prefix(&format!("{}-", worker_receipt.job_id))
        .unwrap()
        .parse()
        .unwrap();
    assert!(attempt > 0);
    let output_root = e.prepared.workspace.parent().unwrap().join("output");
    assert!(VerifiedTransformationOutput::capture(
        claim,
        &reply,
        &guard,
        TransformationOutputScope {
            root: output_root.clone(),
            bundle_prefix: prefix.clone()
        },
        e.prepared.prober.clone()
    )
    .await
    .is_err());
    assert_eq!(counts(e.db).await, (1, 1, 1, 4));
    assert_eq!(events(e.db, &receipt.job_id).await, verifying_events);
    negatives.push(json!({"case":"physical_capture_before_native_closure","core_unchanged":true}));
    client.owner().stop().await.unwrap();
    wait_closed(client.owner()).await;
    guard.require_closed().await.unwrap();
    // A signed adapter manifest still cannot authorize corrupt physical bytes.
    let data = reply
        .manifest()
        .artifacts
        .iter()
        .find(|a| a.role == "DATA")
        .unwrap();
    let data_path = output_root.join(&data.relative_path);
    let offset = 10_u64;
    let original_byte = {
        let mut f = std::fs::File::open(&data_path).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        let mut b = [0];
        f.read_exact(&mut b).unwrap();
        b[0]
    };
    let write_byte = |b: u8| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&data_path)
            .unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.write_all(&[b]).unwrap();
        f.sync_all().unwrap();
    };
    write_byte(original_byte ^ 1);
    let damaged = directory.join("damaged-output.bin");
    std::fs::copy(&data_path, &damaged).unwrap();
    let corrupt = VerifiedTransformationOutput::capture(
        claim,
        &reply,
        &guard,
        TransformationOutputScope {
            root: output_root.clone(),
            bundle_prefix: prefix.clone(),
        },
        e.prepared.prober.clone(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(corrupt.details.reason, "transformation_output_hash_changed");
    assert_eq!(counts(e.db).await, (1, 1, 1, 4));
    assert_eq!(events(e.db, &receipt.job_id).await, verifying_events);
    assert!(!output_root.join("blobs").exists());
    negatives.push(json!({"case":"signed_manifest_with_corrupt_physical_output","reason":corrupt.details.reason,"file":damaged.strip_prefix(repo).unwrap(),"offset":offset,"core_unchanged":true}));
    write_byte(original_byte);
    let measured = VerifiedTransformationOutput::capture(
        claim,
        &reply,
        &guard,
        TransformationOutputScope {
            root: output_root.clone(),
            bundle_prefix: prefix,
        },
        e.prepared.prober.clone(),
    )
    .await
    .unwrap();
    let checked = Arc::new(
        TransformationOutputValidators::default()
            .validate(measured)
            .await
            .unwrap(),
    );
    let measured_snapshot = checked.measured();
    let prepared = Arc::new(
        Box::pin(jobs.prepare_publication(claim, &guard, &input, checked.as_ref()))
            .await
            .unwrap(),
    );
    assert_eq!(counts(e.db).await, (1, 1, 1, 4));
    let record = prepared.record().clone();
    let record_sha = prepared.record_sha256().to_owned();
    let published = Box::pin(PublishedTransformationOutput::publish(
        prepared.clone(),
        checked.clone(),
        &guard,
        e.prepared.prober.clone(),
        &space,
        &jobs,
    ))
    .await
    .unwrap();
    assert_eq!(counts(e.db).await, (1, 1, 1, 4));
    let completed =
        Box::pin(jobs.complete_publication(claim, &guard, &input, &prepared, &published))
            .await
            .unwrap();
    assert_eq!(completed.state, TransformationState::Completed);
    assert_eq!(
        completed.output_variant_id.as_ref(),
        Some(&record.variant.id)
    );
    assert_eq!(completed.attempt, 4);
    assert_eq!(completed.row_version, 19);
    assert_eq!(counts(e.db).await, (1, 2, 0, 0));
    assert_eq!(intent(e.db, &owner.session_id).await["phase"], "CLOSED");
    let event_count = events(e.db, &receipt.job_id).await;
    assert_eq!(event_count, 19);
    assert_eq!(
        Box::pin(jobs.complete_publication(claim, &guard, &input, &prepared, &published))
            .await
            .unwrap(),
        completed
    );
    assert_eq!(events(e.db, &receipt.job_id).await, event_count);
    let confirmation = jobs.confirm_publication(&prepared).await.unwrap();
    published.finish(&confirmation).unwrap();
    let mut artifacts = Vec::new();
    for (i, location) in record.locations.iter().enumerate() {
        let bytes = std::fs::read(output_root.join(&location.relative_path)).unwrap();
        let file = directory.join(format!("published-{i}.bin"));
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            location.sha256.as_ref().unwrap().as_str()
        );
        artifacts.push(json!({"file":file.strip_prefix(repo).unwrap(),"location":location,"artifact":record.artifacts[i],"provenance":record.provenance[i]}));
    }
    std::fs::copy(
        output_root
            .join("manifests")
            .join(format!("{}.json", record.variant.id)),
        directory.join("published-manifest.json"),
    )
    .unwrap();
    std::fs::write(
        directory.join("publication-record.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    let event_rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&receipt.job_id)
    .fetch_all(&e.db.pool)
    .await
    .unwrap();
    let outbox:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)").bind(&receipt.job_id).fetch_one(&e.db.pool).await.unwrap();
    assert_eq!(outbox, event_count);
    input.recheck().await.unwrap();
    assert_eq!(
        std::fs::read(
            e.prepared
                .workspace
                .parent()
                .unwrap()
                .join("input/fixture.bin")
        )
        .unwrap(),
        e.prepared.original
    );
    guard.release().await.unwrap();
    process.wait().await.unwrap();
    drop(cleanup);
    let result = json!({"status":"PASS","scope":"registered native helper observed Verify, native closure, physical measurement, validation and managed CAS/Core completion; test-driver preview/admission/restart, no public producer/H07 acceptance",
        "new_plan":plan,"receipt":receipt,"worker_receipt":worker_receipt,"private_worker_attempt":attempt,"owner":owner,"claim":claim.fence,"manifest":reply.manifest(),"measured":measured_snapshot,"record":record,"record_sha256":record_sha,"completed":completed,"intent":intent(e.db,&owner.session_id).await,"journal":journal(e.db,&receipt.job_id).await,"events":event_rows,"outbox_count":outbox,"final_counts":counts(e.db).await,"negatives":negatives,"artifacts":artifacts,"registered_observed_output":true,"native_closed_before_measurement":true,"publication_committed_before_release":true,"closure_replay_no_extra_events":true,"stdin_kept_open_until_explicit_retirement":true,"input_intact":true,"actual_runtime_acceptance":false,"public_pipeline_accepted":false});
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}
