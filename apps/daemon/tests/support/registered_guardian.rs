//! Real Core/native-owner registration and recovery. Catalogue and preview
//! mapping are fixtures; qualification, files, volumes, SQL and daemon are real.
use super::*;
use crate::daemon_process::RunningDaemon;
use larm_adapter_host::guardian::NativeTransformationAttempt;
use larm_db::{
    locations::SqlStorageLocations, transformation_inputs::SqlTransformationInputs,
    transformation_plans::SqlTransformationPlans, transformations::SqlTransformationJobs,
};
use larm_domain::{
    artifacts::{below_mount, VOLUME_RELATIVE_KEY},
    conformance::{hash, Run},
    estimate::{
        estimate, Confidence, ItemKind, MarginPolicy, Phase, PhaseItem, Size, VolumeSnapshot,
    },
    model::{bundle_hash, BundleEntry, ModelLimits},
    ports::{AdapterQualification, AdapterQualificationGuard, StorageProber},
    transformation_execution::{TransformationAttemptGuard, TransformationReadyOwner},
    transformations::{
        seal_publication_plan, PlanDraft, TransformationActivationIdempotency,
        TransformationAdmission, TransformationInputGuard, TransformationOutputPolicy,
        TransformationState,
    },
};
use larm_hardware::{devices::NativeCommand, storage::NativeStorageProber};
use larm_storage::estimate::SpacePolicy;
use larm_transform::environment::{
    TransformationEnvironment, TransformationSpacePolicy, WorkspaceRoot,
};
use sqlx::Connection;
#[path = "registered_publication.rs"]
mod publication_case;

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn bytes(n: u64) -> DecimalU64 {
    DecimalU64::new(n).unwrap()
}
struct Space(SpacePolicy);
#[async_trait::async_trait]
impl TransformationSpacePolicy for Space {
    async fn current(&self) -> Result<SpacePolicy, larm_domain::error::LarmError> {
        Ok(self.0)
    }
}
struct Setup {
    env: TransformationEnvironment,
    prober: Arc<NativeStorageProber>,
    key: String,
    policy: SpacePolicy,
    workspace: PathBuf,
    original: Vec<u8>,
}
async fn setup(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    request: &TransformationRequest,
) -> Setup {
    setup_named(db, fixture, request, "fixture.bin").await
}

async fn setup_named(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    request: &TransformationRequest,
    input_file: &str,
) -> Setup {
    let source = fixture.work.join("input");
    let destination = fixture.work.join("output");
    let workspace = fixture.work.join("state");
    let original = std::fs::read(source.join(input_file)).unwrap();
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
        relative_path: input_file.into(),
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
    sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,sha256,required) VALUES(?,?,?,1,'{}',?,?,?,?,1)")
        .bind(&artifact).bind(&at).bind(&at).bind(&request.input_variant_id).bind(input_file).bind(original.len() as i64).bind(&digest).execute(&mut *tx).await.unwrap();
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
    sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership,verified_at) VALUES(?,?,?,1,'{}',?,?,?,?,?,'ONLINE','LARM_MANAGED',?)")
        .bind(id()).bind(&at).bind(&at).bind(&artifact).bind(&source_id).bind(input_file).bind(original.len() as i64).bind(&digest).bind(&at).execute(&mut *tx).await.unwrap();
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
        policy: Arc::new(Space(policy)),
    };
    Setup {
        env,
        prober,
        key,
        policy,
        workspace,
        original,
    }
}

/// Seeds only catalogue/location fixture data. Public tests deliberately do
/// not reuse this module's preview mapping or direct admission controller.
pub(super) async fn seed_public_input(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    request: &TransformationRequest,
) -> Vec<u8> {
    setup(db, fixture, request).await.original
}

/// Igual que `seed_public_input` pero con el nombre de fichero de entrada del
/// conversor (p. ej. `fixture.safetensors` para el paquete GGUF).
pub(super) async fn seed_public_input_named(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    request: &TransformationRequest,
    input_file: &str,
) -> Vec<u8> {
    setup_named(db, fixture, request, input_file).await.original
}

// The test driver maps a real, daemon-qualified preview to global volumes.
// Rechecks still cross the signed pipe and execute physical qualification in
// that daemon binary, including while the Core writer holds its transaction.
struct PreviewQualification {
    client: Arc<GuardianTransformationClient>,
    access: Access,
    actor: String,
}
#[async_trait::async_trait]
impl AdapterQualificationGuard for PreviewQualification {
    async fn recheck(&self) -> Result<(), larm_domain::error::LarmError> {
        self.client
            .describe(&id(), &self.access, None)
            .await
            .map(|_| ())
    }
}
#[async_trait::async_trait]
impl AdapterQualification for PreviewQualification {
    async fn acquire(
        &self,
        installation: &AdapterInstallation,
        run: &Run,
        ctx: &MutationContext,
    ) -> Result<Box<dyn AdapterQualificationGuard>, larm_domain::error::LarmError> {
        let identity = self.client.identity();
        assert_eq!(identity.installation_id, installation.id);
        assert_eq!(identity.adapter_id, installation.adapter_id);
        assert_eq!(identity.run_id, run.id);
        assert_eq!(identity.run_sha256, run.digest()?);
        assert_eq!(ctx.actor_id, self.actor);
        self.recheck().await?;
        Ok(Box::new(Self {
            client: self.client.clone(),
            access: self.access.clone(),
            actor: self.actor.clone(),
        }))
    }
}
pub(super) struct Inputs<'a> {
    pub db: &'a larm_db::pool::Db,
    pub fixture: &'a Fixture,
    pub request: &'a TransformationRequest,
    pub identity: &'a SessionIdentity,
    pub context: &'a MutationContext,
    pub repo: &'a Path,
    pub output: &'a Path,
    pub data: &'a Path,
    pub settings: &'a Path,
    pub port: u16,
    pub principal: &'a download_restart::TemporaryPrincipal,
    pub daemon: &'a mut RunningDaemon,
}
pub(super) async fn exercise(inputs: Inputs<'_>) -> Value {
    let Inputs {
        db,
        fixture,
        request,
        identity,
        context: ctx,
        repo,
        output,
        data,
        settings,
        port,
        principal,
        daemon,
    } = inputs;
    let backend_actor = ctx.actor_id.clone();
    let mut core_principal = download_restart::TemporaryPrincipal::enroll(data, settings);
    let context = MutationContext {
        actor_id: core_principal.actor_id.clone(),
        correlation_id: id(),
    };
    let ctx = &context;
    assert_ne!(ctx.actor_id, backend_actor);
    let output = output.join("registered-owner");
    std::fs::create_dir(&output).unwrap();
    let prepared = Arc::new(setup(db, fixture, request).await);
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
    let preview_output = output.join("preview");
    std::fs::create_dir(&preview_output).unwrap();
    let preview_bootstrap = GuardianBootstrap::transformation(
        data.join("larm.sqlite"),
        identity.clone(),
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
    let mut preview_parent = Parent::start(&preview_bootstrap, repo, &preview_output).await;
    let preview = Arc::new(
        preview_parent
            .client(&preview_bootstrap, &preview_output)
            .await,
    );
    let worker_plan = preview.plan(&id(), request, &access, None).await.unwrap();
    let binding = preview.execution_binding(&worker_plan).unwrap();
    let input = prepared
        .env
        .input(&request.input_variant_id, None)
        .await
        .unwrap();
    let plan = mapped_plan(&prepared, request, input.artifacts(), &worker_plan, binding).await;
    let qualification = Arc::new(PreviewQualification {
        client: preview.clone(),
        access: access.clone(),
        actor: backend_actor.clone(),
    });
    let plans = SqlTransformationPlans {
        pool: db.pool.clone(),
        qualification: Some(qualification),
    };
    plans.store_plan(&plan, ctx).await.unwrap();
    let space = prepared
        .env
        .space(
            &plan,
            vec![WorkspaceRoot {
                path: prepared.workspace.clone(),
                volume_key: prepared.key.clone(),
            }],
        )
        .await
        .unwrap();
    let execute_core = ExecuteTransformationPlan {
        plan_id: plan.response.plan_id.clone(),
        plan_hash: plan.response.plan_hash.clone(),
        approval_id: None,
    };
    let receipt = plans
        .activate_plan(
            &execute_core,
            ctx,
            &TransformationActivationIdempotency {
                key: format!("registered-native-{}", id()),
                body_hash: hash(&execute_core).unwrap(),
            },
            TransformationAdmission {
                input: &input,
                space: &space,
            },
        )
        .await
        .unwrap();
    let jobs = SqlTransformationJobs {
        pool: db.pool.clone(),
    };
    assert_eq!(counts(db).await, (1, 1, 1, 4));
    checkpoint(
        &output,
        "admitted",
        json!({"receipt":receipt,"counts":counts(db).await}),
    );
    preview.owner().stop().await.unwrap();
    wait_closed(preview.owner()).await;
    NativeGuardian::attach(preview_bootstrap.descriptor().clone())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    drop(plans);
    drop(preview);
    drop(preview_parent);
    let proposal = jobs
        .propose_native_preparation(&receipt.job_id)
        .await
        .unwrap();
    let bootstrap = GuardianBootstrap::registered_transformation(
        data.join("larm.sqlite"),
        identity.clone(),
        &proposal,
    )
    .unwrap();
    let owner = bootstrap.descriptor().native_owner.clone().unwrap();
    jobs.prepare_native_owner(&proposal, &owner).await.unwrap();
    std::fs::write(
        output.join("descriptor.json"),
        serde_json::to_vec_pretty(bootstrap.descriptor()).unwrap(),
    )
    .unwrap();
    let mut parent = Parent::start(&bootstrap, repo, &output).await;
    let client = Arc::new(parent.client(&bootstrap, &output).await);
    let guard = NativeTransformationAttempt::attach(owner.clone())
        .await
        .unwrap();
    guard.require_ready().await.unwrap();
    let execute = ExecuteTransformationPlan {
        plan_id: owner.binding.worker_plan_id.clone(),
        plan_hash: owner.binding.worker_plan_hash.clone(),
        approval_id: None,
    };
    assert!(client
        .execute(&id(), &execute, &access, None)
        .await
        .is_err());
    assert!(jobs.owned_claim(&receipt.job_id).await.is_err());
    let registered = jobs
        .claim_with_native_owner(&proposal, &guard)
        .await
        .unwrap();
    assert_eq!(registered.owner, owner);
    let registered_journal = journal(db, &receipt.job_id).await;
    checkpoint(
        &output,
        "registered",
        json!({"owner":owner,"journal":registered_journal}),
    );
    guard.recheck().await.unwrap();
    assert!(client.verify(&id(), &id(), &access, None).await.is_err());
    let running_counts = counts(db).await;
    assert_eq!(running_counts, (1, 1, 1, 4));
    // The private SQLite writer makes an accepted Execute RPC stay in flight.
    // This proves active IO/closure gating, not a partial converted-byte crash.
    let mut lock = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(fixture.work.join("state/transform.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut lock)
        .await
        .unwrap();
    let running = client.clone();
    let a = access.clone();
    let e = execute.clone();
    let mut first = tokio::spawn(async move {
        running
            .execute(&id(), &e, &a, Some(Duration::from_secs(60)))
            .await
    });
    let running = client.clone();
    let a = access.clone();
    let e = execute.clone();
    let mut second = tokio::spawn(async move {
        running
            .execute(&id(), &e, &a, Some(Duration::from_secs(60)))
            .await
    });
    // Either caller may win the atomic execution admission. The other fresh
    // signed nonce must be rejected while the winning RPC is still in flight.
    let pending = tokio::select! {
        denied=&mut first=>{assert_eq!(denied.unwrap().unwrap_err().details.reason,"transformation_native_execute_already_started");second},
        denied=&mut second=>{assert_eq!(denied.unwrap().unwrap_err().details.reason,"transformation_native_execute_already_started");first}
    };
    assert!(!pending.is_finished());
    assert!(guard.observer().active_rpcs().await.unwrap() > 0);
    checkpoint(
        &output,
        "concurrent-execute",
        json!({"duplicate_denied":true,"active_rpcs":guard.observer().active_rpcs().await.unwrap()}),
    );
    let failure = larm_domain::error::LarmError::new(
        larm_domain::error::ErrorCode::AdapterCrashed,
        "Owned native recovery fixture",
        "registered_native_recovery",
    );
    assert!(jobs
        .finish_failed_attempt(&registered.claim, &guard, &failure, true)
        .await
        .is_err());
    assert_eq!(counts(db).await, running_counts);
    daemon.stop();
    *daemon = RunningDaemon::start(data, settings, port, output.join("restarted-daemon.log"));
    daemon.wait_live(port, Duration::from_secs(30)).unwrap();
    let recovery_observations = wait_reconciled(db, &receipt.job_id).await;
    let recovered = jobs.owned_claim(&receipt.job_id).await.unwrap();
    let recovered_journal = journal(db, &receipt.job_id).await;
    assert_eq!(recovered.owner, owner);
    assert_eq!(
        recovered.claim.job.state,
        TransformationState::FailedRecoverable
    );
    assert_eq!(
        recovered.claim.job.error.as_ref().unwrap()["code"],
        "WORKER_INTERRUPTED"
    );
    let status: larm_domain::bridge::JobState = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{}", receipt.status_url))
        .bearer_auth(&core_principal.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status.progress["transformation_state"],
        "FAILED_RECOVERABLE"
    );
    assert_eq!(status.job_id, receipt.job_id);
    assert_eq!(status.resource_type, "transformation_job");
    assert_eq!(
        status.progress["error"],
        json!(recovered.claim.job.error.as_ref().unwrap())
    );
    checkpoint(
        &output,
        "recovered",
        json!({"job":recovered.claim.job,"journal":recovered_journal,"status":status,"counts":counts(db).await}),
    );
    // The daemon performed signed reattachment, stop, native closure, SQL
    // release and witness retirement. The test driver does not close this owner.
    wait_retired(&owner).await;
    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(client
        .execute(&id(), &execute, &access, None)
        .await
        .is_err());
    let closed = recovered.claim.job.clone();
    assert!(closed
        .metadata
        .get("attempt_closed")
        .is_some_and(|v| !v.is_null()));
    assert_eq!(counts(db).await, (1, 1, 0, 0));
    checkpoint(
        &output,
        "closed",
        json!({"job":closed,"counts":counts(db).await}),
    );
    let event_count = events(db, &receipt.job_id).await;
    let stranger = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{}", receipt.status_url))
        .bearer_auth(&principal.token)
        .send()
        .await
        .unwrap();
    assert_eq!(stranger.status(), reqwest::StatusCode::NOT_FOUND);
    daemon.stop();
    *daemon = RunningDaemon::start(data, settings, port, output.join("replay-daemon.log"));
    daemon.wait_live(port, Duration::from_secs(30)).unwrap();
    let replay = jobs.owned_claim(&receipt.job_id).await.unwrap();
    assert_eq!(json!(replay.claim.job), json!(closed));
    assert_eq!(events(db, &receipt.job_id).await, event_count);
    assert!(jobs
        .native_preparations(None, 128)
        .await
        .unwrap()
        .is_empty());
    let final_journal = journal(db, &receipt.job_id).await;
    let event_rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&receipt.job_id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let outbox:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id IN (SELECT event_id FROM event WHERE resource_id=?)")
        .bind(&receipt.job_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(outbox, event_count);
    let preparation_recovery = early_preparation(
        Early {
            db,
            prepared: &prepared,
            request,
            identity,
            context: ctx,
            backend_actor: &backend_actor,
            access: &access,
            data,
            settings,
            port,
            daemon,
            output: &output,
            previous: &closed,
            previous_owner: &owner,
        },
        false,
    )
    .await;
    let prior = jobs.owned_claim(&receipt.job_id).await.unwrap();
    let retirement_recovery = early_preparation(
        Early {
            db,
            prepared: &prepared,
            request,
            identity,
            context: ctx,
            backend_actor: &backend_actor,
            access: &access,
            data,
            settings,
            port,
            daemon,
            output: &output,
            previous: &prior.claim.job,
            previous_owner: &prior.owner,
        },
        true,
    )
    .await;
    let recovery_counts = counts(db).await;
    assert_eq!(recovery_counts, (1, 1, 0, 0));
    let previous_publication = jobs.owned_claim(&receipt.job_id).await.unwrap();
    let observed_publication = publication_case::start(publication_case::Inputs {
        db: larm_db::pool::Db {
            pool: db.pool.clone(),
        },
        prepared: prepared.clone(),
        request: request.clone(),
        identity: identity.clone(),
        context: ctx.clone(),
        backend_actor: backend_actor.clone(),
        access: access.clone(),
        data: data.into(),
        output: output.clone(),
        previous: previous_publication.claim.job,
        repo: repo.into(),
    })
    .await
    .unwrap();
    input.recheck().await.unwrap();
    assert_eq!(
        std::fs::read(fixture.work.join("input/fixture.bin")).unwrap(),
        prepared.original
    );
    std::fs::write(output.join("input.bin"), &prepared.original).unwrap();
    assert!(core_principal.revoke());
    let report = json!({"status":"PASS","scope":"real Core admission and durable intent, separate backend/job actors, actual API crash and automatic daemon-owned native reconciliation/release; test-driver preview mapping/controller; no public producer/complete restart acceptance",
        "plan":plan,"receipt":receipt,"registered_owner":owner,"claim":registered.claim.fence,"recovered":recovered.claim.job,"closed":closed,
        "job_status":status,"counts_running":running_counts,"counts_after_closure":counts(db).await,"events_after_idempotent_closure":event_count,
        "observed_publication":observed_publication,"recovery_counts_before_publication":recovery_counts,"retirement_recovery":retirement_recovery,"registered_intent":intent(db,&owner.session_id).await,"preparation_recovery":preparation_recovery,"automatic_daemon_recovery":true,"recovery_observations":recovery_observations,"backend_actor":backend_actor,"job_actor":ctx.actor_id,"other_actor_job_hidden":true,"core_principal_revoked":true,"registered_journal":registered_journal,"recovered_journal":recovered_journal,"final_journal":final_journal,"events":event_rows,"outbox_count":outbox,
        "execution_before_registration_denied":true,"foreign_verification_denied":true,"second_execute_denied":true,
        "daemon_crash_with_live_helper":true,"inflight_rpc_observed":true,"release_before_native_closure_denied":true,
        "public_metadata_only_reattachment":true,"native_process_io_closed":true,"original_error_preserved":true,
        "closure_replay_no_extra_events":true,"core_resources_released":true,"input_sha256":hex::encode(Sha256::digest(&prepared.original)),"derived_variant_published":true});
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    drop(client);
    drop(guard);
    drop(parent);
    report
}
async fn wait_closed(owner: &NativeGuardian) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while owner.require_closed().await.is_err() {
        assert!(Instant::now() < deadline, "native owner did not close");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn events(db: &larm_db::pool::Db, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM event WHERE resource_id=?")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}
async fn journal(db: &larm_db::pool::Db, id: &str) -> Value {
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM operation_journal WHERE id=? AND kind='transformation.job'",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    serde_json::from_str(&payload).unwrap()
}
fn checkpoint(output: &Path, name: &str, value: Value) {
    std::fs::write(
        output.join(format!("checkpoint-{name}.json")),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
}

async fn wait_reconciled(db: &larm_db::pool::Db, id: &str) -> Vec<Value> {
    wait_reconciled_state(db, id, false).await
}
async fn wait_reconciled_state(db: &larm_db::pool::Db, id: &str, allow_queued: bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut observations = Vec::new();
    loop {
        let row:(String,i64,String,i64,i64)=sqlx::query_as("SELECT state,row_version,metadata,(SELECT count(*) FROM volume_reservation WHERE job_id=t.id),(SELECT count(*) FROM resource_lock WHERE owner_id=t.id) FROM transformation_job t WHERE id=?")
            .bind(id).fetch_one(&db.pool).await.unwrap();
        let metadata: Value = serde_json::from_str(&row.2).unwrap();
        let closed = metadata.get("attempt_closed").is_some_and(|v| !v.is_null());
        observations.push(json!({"state":row.0,"version":row.1,"closed":closed,"reservations":row.3,"locks":row.4}));
        assert!(
            row.0 == "FAILED_RECOVERABLE"
                || (allow_queued && matches!(row.0.as_str(), "QUEUED" | "RUNNING"))
        );
        if closed {
            assert_eq!((row.3, row.4), (0, 0));
            return observations;
        }
        assert_eq!((row.3, row.4), (1, 4));
        assert!(
            Instant::now() < deadline,
            "daemon did not reconcile native owner"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn mapped_plan(
    prepared: &Setup,
    request: &TransformationRequest,
    artifacts: &[larm_domain::transformations::InputArtifact],
    worker_plan: &larm_domain::transformations::TransformationPlanResponse,
    binding: larm_domain::transformations::TransformationExecutionBinding,
) -> larm_domain::transformations::SealedTransformationPlan {
    let observed = prepared.prober.probe(&[]).await.unwrap();
    let volume = observed
        .volumes
        .iter()
        .find(|v| v.identity.volume_key.as_ref() == Some(&prepared.key))
        .unwrap();
    let private = &worker_plan.storage_estimate;
    let final_bytes = private.final_artifact_bytes.unwrap().get();
    let policy = TransformationOutputPolicy {
        worker_volume_key: prepared.key.clone(),
        cas_volume_key: prepared.key.clone(),
        max_total_bytes: bytes(final_bytes + (16 << 10)),
        max_file_bytes: bytes(final_bytes.max(16 << 10)),
        max_artifacts: 2,
        worker_control_bytes: private.temporary_peak_bytes.unwrap(),
        control_bytes: bytes(1 << 20),
    };
    let estimate = estimate(
        &[
            Phase {
                name: "qualified_reference_conversion".into(),
                items: vec![PhaseItem {
                    name: "worker_private_peak".into(),
                    kind: ItemKind::Temporary,
                    volume_key: prepared.key.clone(),
                    size: Size::estimated(
                        private.peak_extra_bytes.unwrap().get(),
                        private.upper_bound_bytes.unwrap().get(),
                        Confidence::Estimated,
                    ),
                    existing: false,
                }],
            },
            policy.registration_phase().unwrap(),
        ],
        &[VolumeSnapshot {
            volume_key: prepared.key.clone(),
            os_free: volume.free_bytes.unwrap().get(),
            policy_minimum: 0,
            other_reservations: 0,
        }],
        prepared.policy.margin,
        None,
    )
    .unwrap();
    assert!(estimate.per_volume.iter().all(|v| v.eligible));
    seal_publication_plan(
        id(),
        request.clone(),
        artifacts.to_vec(),
        PlanDraft {
            operation: worker_plan.operation.clone(),
            resource_ids: worker_plan.resource_ids.clone(),
            storage_estimate: estimate,
            steps: worker_plan.steps.clone(),
            expires_at: worker_plan.expires_at.clone(),
            approval_required: worker_plan.approval_required,
        },
        binding,
        policy,
    )
    .unwrap()
}

async fn wait_retired(owner: &larm_domain::transformation_execution::TransformationNativeOwner) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match NativeTransformationAttempt::attach(owner.clone()).await {
            Err(_) => return,
            Ok(observer) => {
                observer.require_closed().await.unwrap();
                assert!(Instant::now() < deadline, "closed witness not retired");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}
struct RetainedHelper {
    system: System,
    pid: Pid,
}
impl RetainedHelper {
    fn capture(process: &larm_adapter_host::guardian::GuardianProcess) -> Self {
        let pid = Pid::from_u32(process.process_id().unwrap());
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        assert!(system.process(pid).is_some());
        Self { system, pid }
    }
}
impl Drop for RetainedHelper {
    fn drop(&mut self) {
        if let Some(process) = self.system.process(self.pid) {
            let _ = process.kill();
        }
    }
}
struct Early<'a> {
    db: &'a larm_db::pool::Db,
    prepared: &'a Setup,
    request: &'a TransformationRequest,
    identity: &'a SessionIdentity,
    context: &'a MutationContext,
    backend_actor: &'a str,
    access: &'a Access,
    data: &'a Path,
    settings: &'a Path,
    port: u16,
    daemon: &'a mut RunningDaemon,
    output: &'a Path,
    previous: &'a larm_domain::transformations::TransformationJob,
    previous_owner: &'a larm_domain::transformation_execution::TransformationNativeOwner,
}
async fn early_preparation(e: Early<'_>, retire_only: bool) -> Value {
    use larm_adapter_host::guardian::GuardianProcess;
    let directory = e.output.join(if retire_only {
        "closed-witness-retirement"
    } else {
        "early-preparation"
    });
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
    let binding = preview.execution_binding(&worker_plan).unwrap();
    let plan = mapped_plan(
        e.prepared,
        e.request,
        input.artifacts(),
        &worker_plan,
        binding,
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
                key: format!("early-preparation-{}", id()),
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
    drop(preview);
    drop(plans);
    let jobs = SqlTransformationJobs {
        pool: e.db.pool.clone(),
    };
    let proposal = jobs
        .propose_native_preparation(&receipt.job_id)
        .await
        .unwrap();
    assert_eq!(proposal.claim.fence.attempt, e.previous.attempt + 1);
    assert_eq!(
        proposal.claim.fence.generation,
        e.previous_owner.fence.generation + 1
    );
    let bootstrap = GuardianBootstrap::registered_transformation(
        e.data.join("larm.sqlite"),
        e.identity.clone(),
        &proposal,
    )
    .unwrap();
    let owner = bootstrap.descriptor().native_owner.clone().unwrap();
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
    let execute = ExecuteTransformationPlan {
        plan_id: owner.binding.worker_plan_id.clone(),
        plan_hash: owner.binding.worker_plan_hash.clone(),
        approval_id: None,
    };
    assert!(client
        .execute(&id(), &execute, e.access, None)
        .await
        .is_err());
    assert!(jobs.owned_claim(&receipt.job_id).await.is_err());
    let before_intent = intent(e.db, &owner.session_id).await;
    assert_eq!(before_intent["phase"], "PREPARING");
    checkpoint(
        &directory,
        "prepared",
        json!({"owner":owner,"intent":before_intent,"proposal_fence":proposal.claim.fence}),
    );
    let before_restart_job = if retire_only {
        let claimed = jobs
            .claim_with_native_owner(&proposal, &guard)
            .await
            .unwrap();
        guard.observer().stop().await.unwrap();
        wait_closed(guard.observer()).await;
        let failure = larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::AdapterCrashed,
            "Owned fault window after durable Core closure",
            "closed_witness_pending_retirement",
        )
        .retryable(true);
        let closed = jobs
            .finish_failed_attempt(&claimed.claim, &guard, &failure, true)
            .await
            .unwrap();
        // Deliberately omit Release. The witness still answers a fresh CLOSED
        // challenge while Core resources are already zero; startup must retire it.
        NativeTransformationAttempt::attach(owner.clone())
            .await
            .unwrap()
            .require_closed()
            .await
            .unwrap();
        assert_eq!(counts(e.db).await, (1, 1, 0, 0));
        assert!(jobs
            .native_retirements(None, 128)
            .await
            .unwrap()
            .iter()
            .any(|o| o == &owner));
        Some(closed)
    } else {
        None
    };
    let before_intent = if retire_only {
        intent(e.db, &owner.session_id).await
    } else {
        before_intent
    };
    let counts_before_restart = counts(e.db).await;
    let events_before_restart = events(e.db, &receipt.job_id).await;
    e.daemon.stop();
    *e.daemon = RunningDaemon::start(
        e.data,
        e.settings,
        e.port,
        directory.join("restarted-daemon.log"),
    );
    e.daemon.wait_live(e.port, Duration::from_secs(30)).unwrap();
    let observations = wait_reconciled_state(e.db, &receipt.job_id, true).await;
    wait_retired(&owner).await;
    process.wait().await.unwrap();
    drop(cleanup);
    let recovered = jobs.owned_claim(&receipt.job_id).await.unwrap();
    assert_eq!(recovered.owner, owner);
    assert_eq!(recovered.claim.job.attempt, e.previous.attempt + 1);
    assert_eq!(recovered.claim.job.row_version, e.previous.row_version + 4);
    if let Some(before) = &before_restart_job {
        assert_eq!(json!(before), json!(recovered.claim.job));
        assert_eq!(events(e.db, &receipt.job_id).await, events_before_restart);
    }
    assert_eq!(
        recovered.claim.job.state,
        TransformationState::FailedRecoverable
    );
    let previous_index = e.previous.attempt as usize - 1;
    assert_eq!(
        recovered.claim.job.metadata["attempt_history"][previous_index]["native_owner"],
        json!(e.previous_owner)
    );
    assert_eq!(
        recovered.claim.job.metadata["attempt_history"][previous_index]["error"],
        json!(e.previous.error)
    );
    assert_eq!(counts(e.db).await, (1, 1, 0, 0));
    let after_intent = intent(e.db, &owner.session_id).await;
    assert_eq!(after_intent["phase"], "CLOSED");
    input.recheck().await.unwrap();
    let event_rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT resource_version,payload FROM event WHERE resource_id=? ORDER BY resource_version",
    )
    .bind(&receipt.job_id)
    .fetch_all(&e.db.pool)
    .await
    .unwrap();
    let result = json!({"status":"PASS","scope":if retire_only {"actual CLOSED witness deliberately left after durable Core closure; daemon retires it without job/resource changes"} else {"actual interrupted PREPARING helper before RUNNING; daemon independently adopts only after native closure and retains same job/attempt history; production stdin launcher exercised"},
        "new_plan":plan,"receipt":receipt,"owner":owner,"intent_before_crash":before_intent,"intent_after_recovery":after_intent,"recovered":recovered.claim.job,"journal":journal(e.db,&receipt.job_id).await,"events":event_rows,"observations":observations,"final_counts":counts(e.db).await,
        "before_restart_job":before_restart_job,"counts_before_restart":counts_before_restart,"events_before_restart":events_before_restart,"execute_before_registration_denied":true,"unregistered_helper_closed_by_daemon":!retire_only,"closed_witness_retired_by_daemon":retire_only,"same_job_attempt_two":!retire_only,"same_job_attempt_three":retire_only,"prior_error_and_owner_preserved":true,"stdin_kept_open_until_explicit_retirement":true});
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}
async fn intent(db: &larm_db::pool::Db, id: &str) -> Value {
    let row: (String, String) = sqlx::query_as(
        "SELECT phase,payload FROM operation_journal WHERE id=? AND kind='transformation.owner'",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    json!({"phase":row.0,"payload":serde_json::from_str::<Value>(&row.1).unwrap()})
}
