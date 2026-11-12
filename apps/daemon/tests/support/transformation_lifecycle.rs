//! Native qualified process + actual storage + SQL attempt ownership. Input
//! catalogue is seeded and the preview's global volume mapping is fixture code;
//! this does not claim a public producer/consumer, output publication or H07.
use super::*;
use larm_adapter_host::conformance::{
    transformation::TransformationConfig, NativeConformanceRunner,
};
use larm_db::{
    locations::SqlStorageLocations, transformation_inputs::SqlTransformationInputs,
    transformation_plans::SqlTransformationPlans, transformations::SqlTransformationJobs,
};
use larm_domain::{
    artifacts::{below_mount, VOLUME_RELATIVE_KEY},
    conformance::{hash, Run},
    error::{ErrorCode, LarmError},
    estimate::{
        estimate, Confidence, ItemKind, MarginPolicy, Phase, PhaseItem, Size, VolumeSnapshot,
    },
    model::{bundle_hash, BundleEntry, ModelLimits},
    ports::StorageProber,
    transformations::{
        seal_publication_plan, ExecuteTransformationPlan, PlanDraft,
        TransformationActivationIdempotency, TransformationInputGuard, TransformationOutputPolicy,
        TransformationRequest, TransformationRestart, TransformationState,
    },
    units::DecimalU64,
};
use larm_hardware::{devices::NativeCommand, storage::NativeStorageProber};
use larm_storage::estimate::SpacePolicy;
use larm_transform::environment::{
    TransformationEnvironment, TransformationSpacePolicy, WorkspaceRoot,
};
use larm_transform::output::{TransformationOutputScope, VerifiedTransformationOutput};
use larm_transform::output_validation::{
    TransformationOutputValidators, TransformationRuntimeValidation,
};
use std::io::Write;

pub(super) struct Inputs<'a> {
    pub fixture: &'a Fixture,
    pub config: &'a TransformationConfig,
    pub request: &'a TransformationRequest,
}
struct Policy(SpacePolicy);
#[async_trait::async_trait]
impl TransformationSpacePolicy for Policy {
    async fn current(&self) -> Result<SpacePolicy, LarmError> {
        Ok(self.0)
    }
}
fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn bytes(n: u64) -> DecimalU64 {
    DecimalU64::new(n).unwrap()
}
async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock),(SELECT count(*) FROM model_variant)")
        .fetch_one(&db.pool).await.unwrap()
}
pub(super) async fn exercise(
    db: &larm_db::pool::Db,
    runner: &Arc<NativeConformanceRunner>,
    installation: &AdapterInstallation,
    run: &Run,
    ctx: &MutationContext,
    inputs: Inputs<'_>,
) -> Value {
    let Inputs {
        fixture,
        config,
        request,
    } = inputs;
    let source = fixture.work.join("input");
    let destination = fixture.work.join("output");
    let workspace = fixture.work.join("state");
    let original = std::fs::read(source.join("fixture.bin")).unwrap();
    let digest = hex::encode(Sha256::digest(&original));
    let prober = Arc::new(NativeStorageProber {
        command: NativeCommand {
            program: env!("CARGO_BIN_EXE_larm-daemon").into(),
            args: vec!["probe-storage".into()],
            timeout: Duration::from_secs(30),
        },
    });
    let observed = prober.probe(&[]).await.unwrap();
    let kind = larm_storage::locations::check_syntax(source.to_str().unwrap()).unwrap();
    let volume =
        larm_storage::locations::volume_for(&observed, source.to_str().unwrap(), &kind).unwrap();
    let key = volume.identity.volume_key.clone().unwrap();
    assert!(volume.online && !volume.read_only);
    let at = timestamp(chrono::Utc::now());
    let source_id = id();
    let revision = id();
    let artifact = id();
    let entry = BundleEntry {
        relative_path: "fixture.bin".into(),
        sha256: digest.clone(),
        size_bytes: original.len() as u64,
        required: true,
    };
    let bundle = bundle_hash(&[entry]).unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    let distribution_id = id();
    let names = [
        ("core-fixture-model", id()),
        ("core-source-install", id()),
        ("core-source-adapter", id()),
        ("core-source", id()),
        ("core-distribution", distribution_id.clone()),
    ];
    for sql in [
        "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('core-fixture-model','t','t',1,'{}','Native input fixture','[]','[]')",
        "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('core-source-install','t','t',1,'{}','fixture.local','1','source','fixture','fixture','UNTRUSTED','{}','INSTALLED')",
        "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('core-source-adapter','t','t',1,'{}','core-source-install','LOCAL','[]')",
        "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('core-source','t','t',1,'{}','Native fixture','core-source-adapter','file:///fixture',0,1,'NONE','{}')",
        "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('core-distribution','t','t',1,'{}','core-fixture-model','core-source','fixture','native')",
    ] {
        let mut sql=sql.replace("'t'",&format!("'{at}'"));
        for (name,value) in &names { sql=sql.replace(&format!("'{name}'"),&format!("'{value}'")); }
        // Only generated UUIDv7 IDs and the server-generated timestamp are
        // interpolated into these fixed fixture statements.
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,?,'CURRENT')")
        .bind(&revision).bind(&at).bind(&at).bind(&distribution_id).bind(format!("content:{}","c".repeat(64))).bind("c".repeat(64)).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available,bundle_hash) VALUES(?,?,?,1,'{}',?,'VERIFIED','VERIFIED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]',?,0,?)")
        .bind(&request.input_variant_id).bind(&at).bind(&at).bind(&revision).bind(json!(ModelLimits::default()).to_string()).bind(&bundle).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,sha256,required) VALUES(?,?,?,1,'{}',?,'fixture.bin',?,?,1)")
        .bind(&artifact).bind(&at).bind(&at).bind(&request.input_variant_id).bind(original.len() as i64).bind(&digest).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
        .bind(id()).bind(&at).bind(&at).bind(&request.input_variant_id).bind(json!([artifact]).to_string()).bind(&bundle).execute(&mut *tx).await.unwrap();
    for (location, path, priority) in [
        (&source_id, &source, 0),
        (&request.storage_location_id, &destination, 1),
    ] {
        let inside = volume
            .identity
            .mount_points
            .iter()
            .filter_map(|m| below_mount(m, path.to_str().unwrap()))
            .next()
            .unwrap();
        let metadata = json!({(VOLUME_RELATIVE_KEY):inside});
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,volume_guid,performance_class) VALUES(?,?,?,1,?,'Native fixture',?,?,?,'LOCAL',?,?,0,?,1,'ONLINE',?,?,'UNKNOWN')")
            .bind(location).bind(&at).bind(&at).bind(metadata.to_string()).bind(path.to_str().unwrap()).bind(path.to_str().unwrap()).bind(volume.filesystem.as_ref().unwrap())
            .bind(volume.capacity_bytes.unwrap().get() as i64).bind(volume.free_bytes.unwrap().get() as i64).bind(priority).bind(&key).bind(volume.identity.volume_guid.as_ref().unwrap()).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership,verified_at) VALUES(?,?,?,1,'{}',?,?,'fixture.bin',?,?,'ONLINE','LARM_MANAGED',?)")
        .bind(id()).bind(&at).bind(&at).bind(&artifact).bind(&source_id).bind(original.len() as i64).bind(&digest).bind(&at).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let reader = db.qualification_reader();
    let policy = SpacePolicy {
        margin: MarginPolicy::default(),
        minimum_free_bytes: 0,
    };
    let env = TransformationEnvironment {
        catalogue: SqlTransformationInputs {
            pool: reader.clone(),
        },
        locations: SqlStorageLocations {
            pool: reader.clone(),
        },
        prober: prober.clone(),
        policy: Arc::new(Policy(policy)),
    };
    let input = env.input(&request.input_variant_id, None).await.unwrap();
    let plans = SqlTransformationPlans {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    let jobs = SqlTransformationJobs {
        pool: db.pool.clone(),
    };
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
    let mut previous: Option<(String, u64)> = None;
    let mut previous_claim = None;
    let mut states = Vec::new();
    let mut retained_mismatch = Value::Null;
    let mut verified_output = Value::Null;
    let mut publication = Value::Null;
    for turn in 0..3 {
        let preview = runner
            .transformation_worker(installation, run, ctx)
            .await
            .unwrap();
        let worker_plan = preview.plan(&id(), request, &access, None).await.unwrap();
        let binding = preview.execution_binding(&worker_plan).unwrap();
        preview.close().await.unwrap();
        drop(preview);
        let observed = prober.probe(&[]).await.unwrap();
        let v = observed
            .volumes
            .iter()
            .find(|v| v.identity.volume_key.as_ref() == Some(&key))
            .unwrap();
        let private = &worker_plan.storage_estimate;
        let final_bytes = private.final_artifact_bytes.unwrap().get();
        // Reference package bounds: at most one data file and one provenance
        // sidecar capped at 16 KiB. This fixture is not the public producer.
        let output_policy = TransformationOutputPolicy {
            worker_volume_key: key.clone(),
            cas_volume_key: key.clone(),
            max_total_bytes: bytes(final_bytes + (16 << 10)),
            max_file_bytes: bytes(final_bytes.max(16 << 10)),
            max_artifacts: 2,
            worker_control_bytes: private.temporary_peak_bytes.unwrap(),
            control_bytes: bytes(1 << 20),
        };
        let usable = v.free_bytes.unwrap().get();
        let phases = [
            Phase {
                name: "qualified_reference_conversion".into(),
                items: vec![PhaseItem {
                    name: "worker_private_peak".into(),
                    kind: ItemKind::Temporary,
                    volume_key: key.clone(),
                    size: Size::estimated(
                        private.peak_extra_bytes.unwrap().get(),
                        private.upper_bound_bytes.unwrap().get(),
                        Confidence::Estimated,
                    ),
                    existing: false,
                }],
            },
            output_policy.registration_phase().unwrap(),
        ];
        let estimate = estimate(
            &phases,
            &[VolumeSnapshot {
                volume_key: key.clone(),
                os_free: usable,
                policy_minimum: 0,
                other_reservations: 0,
            }],
            policy.margin,
            None,
        )
        .unwrap();
        assert!(estimate.per_volume.iter().all(|v| v.eligible));
        let plan = seal_publication_plan(
            id(),
            request.clone(),
            input.artifacts().to_vec(),
            PlanDraft {
                operation: worker_plan.operation.clone(),
                resource_ids: worker_plan.resource_ids.clone(),
                storage_estimate: estimate,
                steps: worker_plan.steps.clone(),
                expires_at: worker_plan.expires_at.clone(),
                approval_required: worker_plan.approval_required,
            },
            binding,
            output_policy,
        )
        .unwrap();
        let context = MutationContext {
            actor_id: ctx.actor_id.clone(),
            correlation_id: id(),
        };
        plans.store_plan(&plan, &context).await.unwrap();
        let space = env
            .space(
                &plan,
                vec![WorkspaceRoot {
                    path: workspace.clone(),
                    volume_key: key.clone(),
                }],
            )
            .await
            .unwrap();
        let request = ExecuteTransformationPlan {
            plan_id: plan.response.plan_id.clone(),
            plan_hash: plan.response.plan_hash.clone(),
            approval_id: None,
        };
        let idem = TransformationActivationIdempotency {
            key: format!("native-core-attempt-{turn}"),
            body_hash: hash(&request).unwrap(),
        };
        let observed = larm_domain::transformations::TransformationAdmission {
            input: &input,
            space: &space,
        };
        let receipt = if let Some((ref job_id, version)) = previous {
            plans
                .restart_plan(
                    &TransformationRestart {
                        job_id: job_id.clone(),
                        if_match: version,
                    },
                    &request,
                    &context,
                    &idem,
                    observed,
                )
                .await
                .unwrap()
        } else {
            plans
                .activate_plan(&request, &context, &idem, observed)
                .await
                .unwrap()
        };
        let claim = jobs.claim_job(&receipt.job_id).await.unwrap();
        assert_eq!(claim.job.attempt, turn + 1);
        assert_eq!(counts(db).await, (1, 1, 4, 1));
        let worker = runner
            .transformation_worker(installation, run, ctx)
            .await
            .unwrap();
        let guard = worker.claim_guard(&claim).unwrap();
        jobs.check_active_claim(&claim, &guard).await.unwrap();
        if turn == 2 {
            let (completed, evidence) = super::transformation_publication::exercise(
                super::transformation_publication::Inputs {
                    db,
                    jobs: &jobs,
                    claim: &claim,
                    previous: previous_claim.as_ref().unwrap(),
                    guard: &guard,
                    worker: &worker,
                    input: &input,
                    space: &space,
                    prober: prober.clone(),
                    destination: &destination,
                    config,
                    access: &access,
                },
            )
            .await;
            assert_eq!(counts(db).await, (1, 0, 0, 2));
            publication = evidence;
            states.push(json!({"claim":claim.fence,"closed_job":completed}));
            previous = Some((completed.id.clone(), completed.row_version as u64));
            drop(worker);
            drop(space);
            input.recheck().await.unwrap();
            break;
        }
        let error = if turn == 0 {
            assert_eq!(jobs.recover_interrupted().await.unwrap(), 1);
            assert!(jobs.check_active_claim(&claim, &guard).await.is_err());
            LarmError::new(
                ErrorCode::AdapterCrashed,
                "Owned reconciliation fixture",
                "native_core_interrupted",
            )
        } else {
            assert!(worker
                .claim_guard(previous_claim.as_ref().unwrap())
                .is_err());
            let b = claim.plan.execution_binding.as_ref().unwrap();
            let execute = ExecuteTransformationPlan {
                plan_id: b.worker_plan_id.clone(),
                plan_hash: b.worker_plan_hash.clone(),
                approval_id: None,
            };
            let result = worker
                .execute(&id(), &execute, &access, Some(Duration::from_secs(60)))
                .await
                .unwrap();
            jobs.mark_verifying(&claim, &guard).await.unwrap();
            jobs.check_active_claim(&claim, &guard).await.unwrap();
            let reply = worker
                .verify_claim_output(&claim, &id(), &result.job_id, &access, None)
                .await
                .unwrap();
            let manifest = reply.manifest();
            let prefix = manifest.artifacts[0]
                .relative_path
                .rsplit_once('/')
                .unwrap()
                .0
                .to_owned();
            let scope = || TransformationOutputScope {
                root: destination.clone(),
                bundle_prefix: prefix.clone(),
            };
            // The actual process/IO owner, not its COMPLETED receipt, controls
            // whether Core may inspect output or release reservations.
            assert!(VerifiedTransformationOutput::capture(
                &claim,
                &reply,
                &guard,
                scope(),
                prober.clone()
            )
            .await
            .is_err());
            assert_eq!(counts(db).await, (1, 1, 4, 1));
            worker.close().await.unwrap();
            let captured = VerifiedTransformationOutput::capture(
                &claim,
                &reply,
                &guard,
                scope(),
                prober.clone(),
            )
            .await
            .unwrap();
            let checked = TransformationOutputValidators::default()
                .validate(captured)
                .await
                .unwrap();
            assert_eq!(
                checked.runtime(),
                &TransformationRuntimeValidation::NotRegistered
            );
            let output_guard = checked.output();
            assert_eq!(output_guard.fence(), &claim.fence);
            assert_eq!(output_guard.worker_job_id(), result.job_id);
            let entries = output_guard.entries();
            let mut retained = Vec::new();
            let project = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            for (index, entry) in entries.iter().enumerate() {
                let mut data = vec![0; entry.size_bytes as usize];
                let mut offset = 0;
                while offset < data.len() {
                    let read = output_guard
                        .read_at(index, offset as u64, &mut data[offset..])
                        .unwrap();
                    assert!(read > 0);
                    offset += read;
                }
                assert_eq!(hex::encode(Sha256::digest(&data)), entry.sha256);
                let evidence = config
                    .evidence_root
                    .join(format!("core-verified-{index}.bin"));
                std::fs::write(&evidence, &data).unwrap();
                retained.push(json!({"file":evidence.strip_prefix(project).unwrap(),"entry":entry,
                    "role":output_guard.role(index),"source_relative_path":output_guard.source_relative_path(index)}));
                assert!(std::fs::write(
                    destination.join(output_guard.source_relative_path(index).unwrap()),
                    b"blocked write"
                )
                .is_err());
            }
            output_guard.recheck().unwrap();
            verified_output = json!({"status":"PASS","claim":claim.fence,"worker_job_id":result.job_id,
                "policy":output_guard.policy(),"manifest":manifest,"artifacts":retained,
                "live_writer_rejected":true,"pins_blocked_overwrite":true,"physical_hashes_verified":true,
                "runtime_validation":"NOT_REGISTERED","catalogue_published":false});
            drop(checked);
            let data = manifest
                .artifacts
                .iter()
                .find(|a| a.role == "DATA")
                .unwrap();
            let output = destination.join(&data.relative_path);
            let mut corrupt = std::fs::OpenOptions::new()
                .write(true)
                .open(&output)
                .unwrap();
            corrupt.write_all(&[255]).unwrap();
            corrupt.flush().unwrap();
            drop(corrupt);
            let actual = std::fs::read(&output).unwrap();
            let actual_sha = hex::encode(Sha256::digest(&actual));
            assert_ne!(data.sha256.as_ref().unwrap(), &actual_sha);
            let evidence = config.evidence_root.join("core-corrupt-output.bin");
            std::fs::write(&evidence, &actual).unwrap();
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            retained_mismatch = json!({"file":evidence.strip_prefix(root).unwrap(),"observed_sha256":actual_sha,
                "expected_sha256":data.sha256,"size_bytes":actual.len().to_string(),"manifest":manifest});
            let error = VerifiedTransformationOutput::capture(
                &claim,
                &reply,
                &guard,
                scope(),
                prober.clone(),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.code, ErrorCode::IntegrityFailed);
            error
        };
        if !worker.io_drained() {
            assert!(jobs
                .finish_failed_attempt(&claim, &guard, &error, turn == 0)
                .await
                .is_err());
        }
        assert_eq!(counts(db).await, (1, 1, 4, 1));
        worker.close().await.unwrap();
        assert!(worker.io_drained());
        if let Some(old) = &previous_claim {
            assert_eq!(
                jobs.finish_failed_attempt(old, &guard, &error, true)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::StaleRevision
            );
            assert_eq!(counts(db).await, (1, 1, 4, 1));
        }
        let failed = jobs
            .finish_failed_attempt(&claim, &guard, &error, turn == 0)
            .await
            .unwrap();
        assert_eq!(failed.attempt, turn + 1);
        assert!(failed.output_variant_id.is_none());
        assert_eq!(
            failed.state,
            if turn == 0 {
                TransformationState::FailedRecoverable
            } else {
                TransformationState::Failed
            }
        );
        assert_eq!(counts(db).await, (1, 0, 0, 1));
        states.push(json!({"claim":claim.fence,"closed_job":failed}));
        previous = Some((failed.id.clone(), failed.row_version as u64));
        previous_claim = Some(claim);
        drop(worker);
        drop(space);
        input.recheck().await.unwrap();
    }
    assert_eq!(std::fs::read(source.join("fixture.bin")).unwrap(), original);
    let events:Vec<(i64,String)>=sqlx::query_as("SELECT resource_version,payload FROM event WHERE resource_type='transformation_job' ORDER BY event_id")
        .fetch_all(&db.pool).await.unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM operation_journal WHERE id=? AND kind='transformation.job'",
    )
    .bind(&previous.as_ref().unwrap().0)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let scope="Direct SQL/Core admission, fenced failure/restart, recoverable CAS and atomic output registration with live physical owners and observed Windows storage; seeded catalogue/global preview/scope, no public producer, actual daemon crash/restart or GGUF runtime acceptance";
    let result = json!({"status":"PASS","scope":scope,"states":states,"events":events,"final_journal":serde_json::from_str::<Value>(&payload).unwrap(),
        "counts":counts(db).await,"source_sha256":digest,"source_preserved":true,"live_process_never_releases_resources":true,
        "restart_attempt_is_two":true,"old_fence_rejected":true,"partial_output_not_published":true,
        "verified_output":verified_output,"output_mismatch":retained_mismatch,"publication":publication});
    std::fs::write(
        config.evidence_root.join("core-lifecycle.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    drop(input);
    drop(env);
    reader.close().await;
    result
}
