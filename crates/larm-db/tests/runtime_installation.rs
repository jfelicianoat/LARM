#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::{
    pool::Db,
    runtime_profiles::SqlRuntimeProfileLocks,
    runtimes::{SqlRuntimeInstallJobs, SqlRuntimes},
};
use larm_domain::{
    auth::{ApiScope, Principal, PrincipalKind},
    bridge::{timestamp, JobPhase, JobReceipt},
    error::ErrorCode,
    guard::{LockMode, Resource, ResourceKind},
    ports::{AdapterQualificationGuard, JobSource, MutationContext, ResourceGuard},
    runtime::{InstallationStrategy, RuntimeCreate, RuntimeVersion},
    runtime_execution::{RuntimeDiscoverySession, RuntimeSessionBinding, RuntimeSessionGuard},
    runtime_install::{
        plan_hash, InstallRequest, RuntimeInstallAdmission, RuntimeInstallJobRepository,
        INSTALL_SPEC,
    },
    runtime_ports::RuntimeRepository,
    runtime_profiles::{
        RuntimeProfileKey, RuntimeProfileLock, RuntimeProfileLockRepository, LOCK_SCHEMA,
        REQUIRED_FIELDS,
    },
};
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;

struct FixtureSession {
    binding: RuntimeSessionBinding,
    recheck_fails: bool,
}

#[async_trait::async_trait]
impl AdapterQualificationGuard for FixtureSession {
    async fn recheck(&self) -> Result<(), larm_domain::error::LarmError> {
        if self.recheck_fails {
            Err(larm_domain::error::LarmError::new(
                ErrorCode::Forbidden,
                "La cualificación de fixture cambió",
                "fixture_qualification_changed",
            ))
        } else {
            Ok(())
        }
    }
}

impl RuntimeSessionGuard for FixtureSession {
    fn binding(&self) -> &RuntimeSessionBinding {
        &self.binding
    }
}

#[async_trait::async_trait]
impl larm_domain::runtime_install::RuntimeVersionSource for FixtureSession {
    async fn list_versions(
        &self,
        _query: &larm_domain::runtime_install::VersionQuery,
    ) -> Result<larm_domain::runtime_install::RuntimeVersionPage, larm_domain::error::LarmError>
    {
        unreachable!("fixture de admisión no consulta versiones")
    }
}

#[async_trait::async_trait]
impl RuntimeDiscoverySession for FixtureSession {
    async fn close(&self) -> Result<(), larm_domain::error::LarmError> {
        Ok(())
    }
}

fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: uid(),
        correlation_id: uid(),
    }
}

fn digest(value: &impl serde::Serialize) -> String {
    larm_domain::canonical::sha256_hex(&serde_json::to_value(value).unwrap()).unwrap()
}

fn profile_lock(runtime: &str, upstream_version: &str) -> RuntimeProfileLock {
    let key = RuntimeProfileKey {
        runtime: runtime.into(),
        source_url: "https://example.org/llama-server/fixture".into(),
        source_revision: "0123456789abcdef0123456789abcdef01234567".into(),
        source_sha256: "a".repeat(64),
        upstream_version: upstream_version.into(),
        os: "Windows".into(),
        os_build: "10.0.26100".into(),
        backend: "CPU".into(),
        protocol: "stdio-v1".into(),
    };
    let mut fields = Map::new();
    for name in REQUIRED_FIELDS {
        let value = match *name {
            "upstream_version" => json!(upstream_version),
            "install_strategy" => json!("PREBUILT_BINARY"),
            "installation_strategies" => json!(["PREBUILT_BINARY"]),
            "fixture_hashes" => json!({"positive": "b".repeat(64)}),
            "evidence_refs" => json!(["fixture://p031/runtime-profile"]),
            _ => Value::Null,
        };
        fields.insert((*name).to_owned(), value);
    }
    let mut result = RuntimeProfileLock {
        schema_version: LOCK_SCHEMA.into(),
        key,
        fields: Value::Object(fields),
        qualification: "UNQUALIFIED".into(),
        lock_sha256: String::new(),
    };
    result.lock_sha256 = result.computed_hash().unwrap();
    result
}

fn rebind_install_plan(
    fixture: &mut Fixture,
    profile_lock_hash: &str,
    strategy: Option<InstallationStrategy>,
) {
    let path_prefix = fixture
        .installation
        .relative_path
        .as_deref()
        .unwrap()
        .rsplit_once('/')
        .unwrap()
        .0
        .to_owned();
    let relative_path = format!("{path_prefix}/{profile_lock_hash}");
    fixture.installation.lock_hash = Some(profile_lock_hash.to_owned());
    fixture.installation.relative_path = Some(relative_path.clone());
    if let Some(strategy) = strategy {
        fixture.installation.strategy = strategy;
    }
    let request_value = fixture.plan.steps[0].request[INSTALL_SPEC]["request"].clone();
    let mut request: InstallRequest = serde_json::from_value(request_value).unwrap();
    request.profile_lock_hash = profile_lock_hash.to_owned();
    if let Some(strategy) = strategy {
        request.strategy = strategy;
    }
    fixture.plan.steps[0].request[INSTALL_SPEC]["request"] = json!(request);
    fixture.plan.steps[0].request[INSTALL_SPEC]["relative_destination"] = json!(relative_path);
    fixture.plan.plan_hash = plan_hash(&fixture.plan).unwrap();
    fixture.request_hash = digest(&request);
}

async fn counts(pool: &SqlitePool) -> Vec<i64> {
    let mut values = Vec::new();
    for table in [
        "runtime_installation",
        "operation_journal",
        "event",
        "event_outbox",
        "idempotency",
        "resource_lock",
        "volume_reservation",
    ] {
        values.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    values
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    jobs: SqlRuntimeInstallJobs,
    installation: larm_domain::runtime::RuntimeInstallation,
    plan: larm_domain::artifacts::Plan,
    storage_id: String,
    volume_key: String,
    request_hash: String,
    ctx: MutationContext,
}

async fn fixture(free_bytes: i64, last_seen_at: String) -> Fixture {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let runtimes = SqlRuntimes {
        pool: db.pool.clone(),
    };
    let installed_adapter = uid();
    let at = timestamp(chrono::Utc::now());
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{}','fixture.runtime','1.0.0','runtime',?,'fixture','UNTRUSTED',?,'INSTALLED')")
        .bind(&installed_adapter).bind(&at).bind(&at).bind("a".repeat(64))
        .bind(json!({"id":"fixture.runtime","version":"1.0.0","family":"runtime","capabilities":["runtime.install_plan"]}).to_string())
        .execute(&db.pool).await.unwrap();
    let binding = runtimes
        .register_runtime_adapter(&installed_adapter, "llama.cpp", &ctx())
        .await
        .unwrap();
    let runtime = runtimes
        .create_runtime(
            &RuntimeCreate {
                name: "llama.cpp".into(),
                adapter_id: binding.id.clone(),
                capabilities: vec!["runtime.install_plan".into()],
                default_version_id: None,
                channel_policy: Default::default(),
                retention_policy: Default::default(),
            },
            &uid(),
            "b".repeat(64).as_str(),
            &ctx(),
        )
        .await
        .unwrap();
    let version: RuntimeVersion = serde_json::from_value(json!({
        "id":uid(),"created_at":at,"updated_at":at,"row_version":1,"metadata":{},
        "runtime_id":runtime.id,"version_string":"fixture-1","commit":null,"build":null,
        "channel":"stable","released_at":null,"artifact_hash":null,"promotion":"NONE"
    }))
    .unwrap();
    runtimes.record_version(&version, &ctx()).await.unwrap();

    let profile = profile_lock("llama.cpp", &version.version_string);
    SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    }
    .acquire(&profile, &ctx())
    .await
    .unwrap();

    let storage_id = uid();
    let volume_key = "fixture-volume-p031".to_owned();
    sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,volume_guid,drive_letter,unc_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,last_seen_at,volume_key,performance_class) VALUES(?,?,?,1,'{}','runtime-test','C:/larm-runtime','C:/larm-runtime','11111111-1111-4111-8111-111111111111','C:',NULL,'NTFS','LOCAL',1000000,?,100,1,1,'ONLINE',?,?, 'SSD')")
        .bind(&storage_id).bind(&at).bind(&at).bind(free_bytes).bind(&last_seen_at).bind(&volume_key)
        .execute(&db.pool).await.unwrap();

    let request = InstallRequest {
        runtime_id: runtime.id.clone(),
        upstream_version: version.version_string.clone(),
        strategy: InstallationStrategy::PrebuiltBinary,
        storage_location_id: storage_id.clone(),
        profile_lock_hash: profile.lock_sha256,
        external_host_id: None,
    };
    let install_id = uid();
    let relative_path = format!(
        "runtimes/{}/{}/{}",
        runtime.id, version.id, request.profile_lock_hash
    );
    let installation: larm_domain::runtime::RuntimeInstallation = serde_json::from_value(json!({
        "id":install_id,"created_at":at,"updated_at":at,"row_version":1,"metadata":{"fixture_only":true},
        "runtime_version_id":version.id,"storage_location_id":storage_id,"relative_path":relative_path,
        "strategy":"PREBUILT_BINARY","external_host_id":null,"state":"INSTALL_PLANNED",
        "ownership":"LARM_MANAGED","lock_hash":request.profile_lock_hash,"stage":"INSTALL_PLANNED","registration_mode":"MANAGED"
    })).unwrap();
    let mut plan: larm_domain::artifacts::Plan = serde_json::from_value(json!({
        "plan_id":uid(),"plan_hash":"0".repeat(64),"operation":"runtime.install",
        "resource_ids":[runtime.id,version.id,storage_id],
        "storage_estimate":{
            "download_bytes":"80","final_artifact_bytes":"80","installed_bytes":"80","derived_cache_bytes":"0",
            "temporary_peak_bytes":"0","existing_bytes":"0","peak_extra_bytes":"80","required_free_bytes":"100",
            "upper_bound_bytes":"80","confidence":"EXACT","reason":"fixture estimate","evidence_refs":[],
            "per_volume":[{"volume_key":volume_key,"peak_extra_bytes":"80","margin_bytes":"20","other_reservations_bytes":"0","usable_free_bytes":"899900","eligible":true,"reason":"fits"}]
        },
        "steps":[{"phase":"BIND_INSTALLATION","adapter_id":"fixture.runtime","request":{}}],
        "expires_at":timestamp(chrono::Utc::now()+chrono::Duration::minutes(10)),"approval_required":false
    })).unwrap();
    plan.steps[0].request.insert(INSTALL_SPEC.into(), json!({
        "request":request,"runtime_version_id":version.id,"adapter_installation_id":binding.adapter_installation_id,
        "adapter_package_hash":"a".repeat(64),"registration_mode":"MANAGED","ownership":"LARM_MANAGED",
        "relative_destination":relative_path,"environment_prefix":null,"dependencies_lock_hash":null,
        "global_effects":false,"reboot_required":false
    }));
    plan.plan_hash = plan_hash(&plan).unwrap();
    let request_hash = digest(&request);
    Fixture {
        _dir: dir,
        jobs: SqlRuntimeInstallJobs {
            pool: db.pool.clone(),
        },
        db,
        installation,
        plan,
        storage_id,
        volume_key,
        request_hash,
        ctx: ctx(),
    }
}

fn idempotency_expiry() -> String {
    timestamp(chrono::Utc::now() + chrono::Duration::hours(24))
}

#[tokio::test]
async fn resuelve_version_upstream_exacta_y_rechaza_duplicados_en_catalogo() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let request = larm_domain::runtime_install::request_from_plan(&f.plan).unwrap();
    let repo = SqlRuntimes {
        pool: f.db.pool.clone(),
    };
    let version = repo
        .version_by_runtime_and_string(&request.runtime_id, &request.upstream_version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.id, f.installation.runtime_version_id);

    let mut duplicate = version;
    duplicate.id = uid();
    let error = repo.record_version(&duplicate, &f.ctx).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.details.reason, "runtime_identity_exists");
    let error = repo
        .version_by_runtime_and_string(&request.runtime_id, &request.upstream_version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(error.id, f.installation.runtime_version_id);
}

async fn enqueue(
    fixture: &Fixture,
    key: &str,
    request_hash: &str,
    expires_at: &str,
) -> Result<(larm_domain::bridge::JobReceipt, bool), larm_domain::error::LarmError> {
    let request = larm_domain::runtime_install::request_from_plan(&fixture.plan)?;
    let session = FixtureSession {
        binding: RuntimeSessionBinding {
            runtime_id: request.runtime_id,
            installation_sha256: "a".repeat(64),
            run_sha256: "b".repeat(64),
            actor_id: fixture.ctx.actor_id.clone(),
        },
        recheck_fails: false,
    };
    fixture
        .jobs
        .enqueue_plan(RuntimeInstallAdmission {
            installation: &fixture.installation,
            plan: &fixture.plan,
            principal_id: &fixture.ctx.actor_id,
            idempotency_key: key,
            request_hash,
            idempotency_expires_at: expires_at,
            ctx: &fixture.ctx,
            session: &session,
        })
        .await
}

#[tokio::test]
async fn cualificacion_o_actor_distinto_impiden_admision_sin_escrituras() {
    for fail_recheck in [false, true] {
        let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
        let request = larm_domain::runtime_install::request_from_plan(&f.plan).unwrap();
        let session = FixtureSession {
            binding: RuntimeSessionBinding {
                runtime_id: request.runtime_id,
                installation_sha256: "a".repeat(64),
                run_sha256: "b".repeat(64),
                actor_id: if fail_recheck {
                    f.ctx.actor_id.clone()
                } else {
                    uid()
                },
            },
            recheck_fails: fail_recheck,
        };
        let before = counts(&f.db.pool).await;
        let error = f
            .jobs
            .enqueue_plan(RuntimeInstallAdmission {
                installation: &f.installation,
                plan: &f.plan,
                principal_id: &f.ctx.actor_id,
                idempotency_key: "session-fence",
                request_hash: &f.request_hash,
                idempotency_expires_at: &idempotency_expiry(),
                ctx: &f.ctx,
                session: &session,
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.details.reason,
            if fail_recheck {
                "fixture_qualification_changed"
            } else {
                "runtime_install_session_mismatch"
            }
        );
        assert_eq!(counts(&f.db.pool).await, before);
    }
}

#[tokio::test]
async fn encola_plan_con_instalacion_lock_reserva_journal_evento_y_replay_atomicos() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let expiry = idempotency_expiry();
    assert!(f
        .jobs
        .replay(&f.ctx.actor_id, "install-once", &f.request_hash)
        .await
        .unwrap()
        .is_none());
    let (receipt, created) = enqueue(&f, "install-once", &f.request_hash, &expiry)
        .await
        .unwrap();
    assert!(created);
    assert_eq!(receipt.state, JobPhase::Queued);
    assert_eq!(
        receipt.status_url,
        format!("/api/v1/jobs/{}", receipt.job_id)
    );
    let job = f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap();
    assert_eq!(job.plan, f.plan);
    assert_eq!(job.installation, f.installation);
    assert_eq!(job.request_hash, f.request_hash);
    let installed: String = sqlx::query_scalar("SELECT stage FROM runtime_installation WHERE id=?")
        .bind(&f.installation.id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(installed, "INSTALL_PLANNED");
    let guard = larm_db::guard::SqlResourceGuard {
        pool: f.db.pool.clone(),
    };
    assert!(guard
        .holds(
            &receipt.job_id,
            1,
            &Resource::new(ResourceKind::Volume, f.volume_key.clone()),
            LockMode::Exclusive
        )
        .await
        .unwrap());
    let reserved: i64 =
        sqlx::query_scalar("SELECT remaining_bytes FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
    assert_eq!(reserved, 100);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND json_extract(e.payload,'$.action')='installation_queued'").bind(&f.installation.id).fetch_one(&f.db.pool).await.unwrap(), 1);
    let before = counts(&f.db.pool).await;
    assert_eq!(
        f.jobs
            .replay(&f.ctx.actor_id, "install-once", &f.request_hash)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    let (replayed, created) = enqueue(&f, "install-once", &f.request_hash, &expiry)
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(replayed, receipt);
    assert_eq!(counts(&f.db.pool).await, before);
    let error = enqueue(&f, "install-once", &"f".repeat(64), &expiry)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdempotencyConflict);

    let before = counts(&f.db.pool).await;
    let error = enqueue(&f, "invalid-request-hash", "not-a-sha256", &expiry)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    assert_eq!(counts(&f.db.pool).await, before);
}

#[tokio::test]
async fn cancela_solo_jobs_queued_y_libera_guards_atomicos_con_replay() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (queued, _) = enqueue(&f, "cancel-queued", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();

    let cancelled = f
        .jobs
        .cancel_queued_job(&queued.job_id, &f.ctx.actor_id, &f.ctx)
        .await
        .unwrap();
    assert_eq!(cancelled.state, JobPhase::Cancelled);
    assert_eq!(cancelled.result, Some(json!({"state":"CANCELLED"})));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT phase FROM operation_journal WHERE id=?")
            .bind(&queued.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        "CANCELLED"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT stage FROM runtime_installation WHERE id=?")
            .bind(&f.installation.id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        "FAILED"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&queued.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource_lock WHERE owner_id=?")
            .bind(&queued.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND json_extract(e.payload,'$.action')='installation_cancelled'")
            .bind(&f.installation.id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        1
    );

    let after_cancel = counts(&f.db.pool).await;
    let replay = f
        .jobs
        .cancel_queued_job(&queued.job_id, &f.ctx.actor_id, &f.ctx)
        .await
        .unwrap();
    assert_eq!(replay.state, JobPhase::Cancelled);
    assert_eq!(counts(&f.db.pool).await, after_cancel);

    let other = ctx();
    let error = f
        .jobs
        .cancel_queued_job(&queued.job_id, &other.actor_id, &other)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);

    let running = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, _) = enqueue(
        &running,
        "cancel-running",
        &running.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    running.jobs.start_job(&receipt.job_id).await.unwrap();
    let error = running
        .jobs
        .cancel_queued_job(&receipt.job_id, &running.ctx.actor_id, &running.ctx)
        .await
        .unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_install_cancel_state_conflict"
    );
    let still_running = running
        .jobs
        .get_job(&receipt.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_running.state, JobPhase::Running);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&running.db.pool)
            .await
            .unwrap(),
        1
    );

    let cancelled = running
        .jobs
        .cancel_running_job(&receipt.job_id, &running.ctx.actor_id, &running.ctx)
        .await
        .unwrap();
    assert_eq!(cancelled.state, JobPhase::Cancelled);
    assert_eq!(cancelled.result, Some(json!({"state":"CANCELLED"})));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&running.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource_lock WHERE owner_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&running.db.pool)
            .await
            .unwrap(),
        0
    );
    let running_cancel_counts = counts(&running.db.pool).await;
    assert_eq!(
        running
            .jobs
            .cancel_running_job(&receipt.job_id, &running.ctx.actor_id, &running.ctx)
            .await
            .unwrap()
            .state,
        JobPhase::Cancelled
    );
    assert_eq!(counts(&running.db.pool).await, running_cancel_counts);
    let other = ctx();
    assert_eq!(
        running
            .jobs
            .cancel_running_job(&receipt.job_id, &other.actor_id, &other)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn admisiones_invalidas_por_recursos_o_profile_lock_no_escriben() {
    let mut approval_required = fixture(900_000, timestamp(chrono::Utc::now())).await;
    approval_required.plan.approval_required = true;
    approval_required.plan.plan_hash = plan_hash(&approval_required.plan).unwrap();
    let before = counts(&approval_required.db.pool).await;
    let error = enqueue(
        &approval_required,
        "approval-required-plan",
        &approval_required.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ApprovalRequired);
    assert_eq!(error.details.reason, "runtime_install_approval_required");
    assert_eq!(counts(&approval_required.db.pool).await, before);

    let f = fixture(50, timestamp(chrono::Utc::now())).await;
    let before = counts(&f.db.pool).await;
    let error = enqueue(&f, "insufficient", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageInsufficient);
    assert_eq!(counts(&f.db.pool).await, before);

    let mut unknown = fixture(900_000, timestamp(chrono::Utc::now())).await;
    unknown.plan.storage_estimate.peak_extra_bytes = None;
    unknown.plan.storage_estimate.required_free_bytes = None;
    unknown.plan.plan_hash = plan_hash(&unknown.plan).unwrap();
    let before = counts(&unknown.db.pool).await;
    let error = enqueue(
        &unknown,
        "unknown-size",
        &unknown.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnknownSize);
    assert_eq!(counts(&unknown.db.pool).await, before);

    let old = fixture(900_000, "2020-01-01T00:00:00.000Z".into()).await;
    let before = counts(&old.db.pool).await;
    let error = enqueue(&old, "stale", &old.request_hash, &idempotency_expiry())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceUnavailable);
    assert_eq!(counts(&old.db.pool).await, before);

    let missing = fixture(900_000, timestamp(chrono::Utc::now())).await;
    sqlx::query("UPDATE storage_location SET last_seen_at = NULL WHERE id = ?")
        .bind(&missing.storage_id)
        .execute(&missing.db.pool)
        .await
        .unwrap();
    let before = counts(&missing.db.pool).await;
    let error = enqueue(
        &missing,
        "missing-snapshot-time",
        &missing.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceUnavailable);
    assert_eq!(counts(&missing.db.pool).await, before);

    let mut unregistered = fixture(900_000, timestamp(chrono::Utc::now())).await;
    rebind_install_plan(&mut unregistered, &"d".repeat(64), None);
    let before = counts(&unregistered.db.pool).await;
    let error = enqueue(
        &unregistered,
        "missing-profile-lock",
        &unregistered.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.details.reason, "runtime_install_profile_lock_missing");
    assert_eq!(counts(&unregistered.db.pool).await, before);

    let mut unsupported = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let lock_hash = unsupported.installation.lock_hash.clone().unwrap();
    rebind_install_plan(
        &mut unsupported,
        &lock_hash,
        Some(InstallationStrategy::SourceBuild),
    );
    let before = counts(&unsupported.db.pool).await;
    let error = enqueue(
        &unsupported,
        "profile-disallows-strategy",
        &unsupported.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_install_strategy_not_in_profile"
    );
    assert_eq!(counts(&unsupported.db.pool).await, before);

    let mut wrong_version = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let mismatched_profile = profile_lock("llama.cpp", "another-version");
    SqlRuntimeProfileLocks {
        pool: wrong_version.db.pool.clone(),
    }
    .acquire(&mismatched_profile, &ctx())
    .await
    .unwrap();
    rebind_install_plan(&mut wrong_version, &mismatched_profile.lock_sha256, None);
    let before = counts(&wrong_version.db.pool).await;
    let error = enqueue(
        &wrong_version,
        "profile-version-mismatch",
        &wrong_version.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_install_profile_lock_binding_changed"
    );
    assert_eq!(counts(&wrong_version.db.pool).await, before);

    let mut wrong_family = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let mismatched_profile = profile_lock("Ollama", "fixture-1");
    SqlRuntimeProfileLocks {
        pool: wrong_family.db.pool.clone(),
    }
    .acquire(&mismatched_profile, &ctx())
    .await
    .unwrap();
    rebind_install_plan(&mut wrong_family, &mismatched_profile.lock_sha256, None);
    let before = counts(&wrong_family.db.pool).await;
    let error = enqueue(
        &wrong_family,
        "profile-family-mismatch",
        &wrong_family.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_install_profile_lock_binding_changed"
    );
    assert_eq!(counts(&wrong_family.db.pool).await, before);
}

#[tokio::test]
async fn cambio_del_paquete_o_corrupcion_del_journal_se_detectan_como_integridad() {
    let changed = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let runtime_id: String =
        sqlx::query_scalar("SELECT runtime_id FROM runtime_version WHERE id = ?")
            .bind(&changed.installation.runtime_version_id)
            .fetch_one(&changed.db.pool)
            .await
            .unwrap();
    let adapter_installation_id: String = sqlx::query_scalar(
        "SELECT ra.adapter_installation_id FROM runtime r \
         JOIN runtime_adapter ra ON ra.id = r.adapter_id WHERE r.id = ?",
    )
    .bind(runtime_id)
    .fetch_one(&changed.db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE adapter_installation SET package_hash = ? WHERE id = ?")
        .bind("d".repeat(64))
        .bind(adapter_installation_id)
        .execute(&changed.db.pool)
        .await
        .unwrap();
    let before = counts(&changed.db.pool).await;
    let error = enqueue(
        &changed,
        "adapter-package-changed",
        &changed.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(counts(&changed.db.pool).await, before);

    let corrupt = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(
        &corrupt,
        "corrupt-payload",
        &corrupt.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    assert!(created);
    sqlx::query("UPDATE operation_journal SET payload='{}' WHERE id=?")
        .bind(&receipt.job_id)
        .execute(&corrupt.db.pool)
        .await
        .unwrap();
    let error = corrupt.jobs.get_job(&receipt.job_id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::IntegrityFailed);
    let error = enqueue(
        &corrupt,
        "corrupt-payload",
        &corrupt.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::IntegrityFailed);
}

#[tokio::test]
async fn get_job_solo_expone_al_actor_o_a_administracion() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(
        &f,
        "visible-to-owner",
        &f.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    assert!(created);
    let owner = Principal::new(
        f.ctx.actor_id.clone(),
        "owner",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap();
    let state = f.jobs.job(&owner, &receipt.job_id).await.unwrap().unwrap();
    assert_eq!(state.job_id, receipt.job_id);
    assert_eq!(state.state, JobPhase::Queued);
    assert_eq!(state.resource_type, "runtime_installation");
    assert_eq!(
        state.resource_id.as_deref(),
        Some(f.installation.id.as_str())
    );

    let unrelated = Principal::new(
        uid(),
        "unrelated-reader",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap();
    assert_eq!(f.jobs.job(&unrelated, &receipt.job_id).await.unwrap(), None);
    let admin = Principal::new(
        uid(),
        "administrator",
        PrincipalKind::Operator,
        [ApiScope::Admin],
        [],
    )
    .unwrap();
    assert_eq!(
        f.jobs.job(&admin, &receipt.job_id).await.unwrap(),
        Some(state)
    );
    assert_eq!(f.jobs.job(&owner, &uid()).await.unwrap(), None);
}

#[tokio::test]
async fn inicio_transiciona_job_y_instalacion_una_vez_y_conserva_guards() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(&f, "start-once", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();
    assert!(created);
    assert_eq!(
        f.jobs.queued_job_ids(4).await.unwrap(),
        vec![receipt.job_id.clone()]
    );
    assert_eq!(
        f.jobs.queued_job_ids(0).await.unwrap_err().details.reason,
        "runtime_install_queue_limit_invalid"
    );
    assert!(f.jobs.running_job_ids(4).await.unwrap().is_empty());

    let started = f.jobs.start_job(&receipt.job_id).await.unwrap();
    assert_eq!(
        f.jobs.running_job_ids(4).await.unwrap(),
        vec![receipt.job_id.clone()]
    );
    assert_eq!(started.state, JobPhase::Running);
    assert_eq!(
        started.installation.stage,
        larm_domain::runtime::InstallationStage::Installing
    );
    assert_eq!(
        started.installation.state,
        larm_domain::runtime::InstallationState::Installing
    );
    assert_eq!(started.installation.row_version, 2);
    assert!(f.jobs.queued_job_ids(4).await.unwrap().is_empty());
    assert_eq!(
        f.jobs.get_job(&receipt.job_id).await.unwrap(),
        Some(started)
    );
    let (replayed, created) = enqueue(&f, "start-once", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(replayed, receipt);
    assert_eq!(
        f.jobs
            .get_job(&receipt.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobPhase::Running
    );

    let stage: String = sqlx::query_scalar("SELECT stage FROM runtime_installation WHERE id=?")
        .bind(&f.installation.id)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(stage, "INSTALLING");
    let guard = larm_db::guard::SqlResourceGuard {
        pool: f.db.pool.clone(),
    };
    assert!(guard
        .holds(
            &receipt.job_id,
            1,
            &Resource::new(ResourceKind::Volume, f.volume_key.clone()),
            LockMode::Exclusive
        )
        .await
        .unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT remaining_bytes FROM volume_reservation WHERE job_id=? AND volume_key=?",
        )
        .bind(&receipt.job_id)
        .bind(&f.volume_key)
        .fetch_one(&f.db.pool)
        .await
        .unwrap(),
        100
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND json_extract(e.payload,'$.action')='installation_started'").bind(&f.installation.id).fetch_one(&f.db.pool).await.unwrap(), 1);

    let before = counts(&f.db.pool).await;
    let error = f.jobs.start_job(&receipt.job_id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidStateTransition);
    assert_eq!(counts(&f.db.pool).await, before);
    let current = f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap();
    assert_eq!(current.state, JobPhase::Running);
    assert_eq!(current.installation.row_version, 2);
}

#[tokio::test]
async fn completion_cierra_job_instalacion_y_guards_tras_recibo_terminal() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(
        &f,
        "complete-running",
        &f.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    assert!(created);

    let worker_job_id = uid();
    let not_terminal = JobReceipt {
        job_id: worker_job_id.clone(),
        status_url: format!("/worker/jobs/{worker_job_id}"),
        state: JobPhase::Running,
    };
    let before = counts(&f.db.pool).await;
    let error = f
        .jobs
        .complete_job(&receipt.job_id, &not_terminal)
        .await
        .unwrap_err();
    assert_eq!(error.details.reason, "runtime_install_worker_not_completed");
    assert_eq!(counts(&f.db.pool).await, before);

    f.jobs.start_job(&receipt.job_id).await.unwrap();
    let terminal = JobReceipt {
        state: JobPhase::Completed,
        ..not_terminal
    };
    let completed = f
        .jobs
        .complete_job(&receipt.job_id, &terminal)
        .await
        .unwrap();
    assert_eq!(completed.state, JobPhase::Completed);
    assert_eq!(
        completed.installation.stage,
        larm_domain::runtime::InstallationStage::Installed
    );
    assert_eq!(
        completed.installation.state,
        larm_domain::runtime::InstallationState::Installed
    );
    assert_eq!(completed.installation.row_version, 3);
    assert_eq!(
        completed.result,
        Some(json!({"worker_job_id":worker_job_id,"state":"COMPLETED"}))
    );
    assert!(completed.error.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource_lock WHERE owner_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND json_extract(e.payload,'$.action')='installation_completed'").bind(&f.installation.id).fetch_one(&f.db.pool).await.unwrap(), 1);

    let owner = Principal::new(
        f.ctx.actor_id.clone(),
        "owner",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap();
    let visible = f.jobs.job(&owner, &receipt.job_id).await.unwrap().unwrap();
    assert_eq!(visible.state, JobPhase::Completed);
    assert_eq!(visible.result, completed.result);
    assert_eq!(
        f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap(),
        completed
    );

    let before = counts(&f.db.pool).await;
    let error = f
        .jobs
        .complete_job(&receipt.job_id, &terminal)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidStateTransition);
    assert_eq!(counts(&f.db.pool).await, before);
}

#[tokio::test]
async fn fallo_al_publicar_completion_revierte_job_instalacion_y_liberacion_de_guards() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(
        &f,
        "complete-outbox-rollback",
        &f.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    assert!(created);
    f.jobs.start_job(&receipt.job_id).await.unwrap();
    let running = f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap();
    let worker_job_id = uid();
    let terminal = JobReceipt {
        job_id: worker_job_id.clone(),
        status_url: format!("/worker/jobs/{worker_job_id}"),
        state: JobPhase::Completed,
    };
    let before = counts(&f.db.pool).await;
    sqlx::query("CREATE TRIGGER test_complete_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected'); END")
        .execute(&f.db.pool)
        .await
        .unwrap();

    assert!(f
        .jobs
        .complete_job(&receipt.job_id, &terminal)
        .await
        .is_err());
    assert_eq!(counts(&f.db.pool).await, before);
    assert_eq!(
        f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap(),
        running
    );
    sqlx::query("DROP TRIGGER test_complete_outbox")
        .execute(&f.db.pool)
        .await
        .unwrap();

    let completed = f
        .jobs
        .complete_job(&receipt.job_id, &terminal)
        .await
        .unwrap();
    assert_eq!(completed.state, JobPhase::Completed);
    assert_eq!(completed.installation.row_version, 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn fallo_al_publicar_inicio_revierte_job_instalacion_y_permite_reintento() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(
        &f,
        "start-outbox-rollback",
        &f.request_hash,
        &idempotency_expiry(),
    )
    .await
    .unwrap();
    assert!(created);
    let before = counts(&f.db.pool).await;
    sqlx::query("CREATE TRIGGER test_start_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected'); END")
        .execute(&f.db.pool)
        .await
        .unwrap();

    assert!(f.jobs.start_job(&receipt.job_id).await.is_err());
    assert_eq!(counts(&f.db.pool).await, before);
    let queued = f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap();
    assert_eq!(queued.state, JobPhase::Queued);
    assert_eq!(
        queued.installation.stage,
        larm_domain::runtime::InstallationStage::InstallPlanned
    );
    assert_eq!(queued.installation.row_version, 1);
    sqlx::query("DROP TRIGGER test_start_outbox")
        .execute(&f.db.pool)
        .await
        .unwrap();

    let retried = f.jobs.start_job(&receipt.job_id).await.unwrap();
    assert_eq!(retried.state, JobPhase::Running);
    assert_eq!(
        retried.installation.stage,
        larm_domain::runtime::InstallationStage::Installing
    );
}

#[tokio::test]
async fn fallo_desde_queued_cierra_instalacion_job_y_libera_guards_atomicos() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(&f, "fail-queued", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();
    assert!(created);
    let failure = larm_domain::error::LarmError::new(
        ErrorCode::DownloadFailed,
        "No se pudo instalar el runtime",
        "runtime_download_failed",
    );

    let failed = f.jobs.fail_job(&receipt.job_id, &failure).await.unwrap();
    assert_eq!(failed.state, JobPhase::Failed);
    assert_eq!(
        failed.installation.stage,
        larm_domain::runtime::InstallationStage::Failed
    );
    assert_eq!(
        failed.installation.state,
        larm_domain::runtime::InstallationState::Failed
    );
    assert_eq!(failed.installation.row_version, 2);
    assert_eq!(
        failed.error.as_ref().map(|error| error.code.as_str()),
        Some("DOWNLOAD_FAILED")
    );
    assert_eq!(
        failed
            .error
            .as_ref()
            .map(|error| error.details.reason.as_str()),
        Some("runtime_download_failed")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource_lock WHERE owner_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id=e.id WHERE e.resource_id=? AND json_extract(e.payload,'$.action')='installation_failed'").bind(&f.installation.id).fetch_one(&f.db.pool).await.unwrap(), 1);

    let owner = Principal::new(
        f.ctx.actor_id.clone(),
        "owner",
        PrincipalKind::Operator,
        [ApiScope::Read],
        [],
    )
    .unwrap();
    let state = f.jobs.job(&owner, &receipt.job_id).await.unwrap().unwrap();
    assert_eq!(state.state, JobPhase::Failed);
    assert_eq!(state.error, Some(failure));
    assert!(state.result.is_none());
    assert_eq!(
        f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap(),
        failed
    );
    let (replayed, created) = enqueue(&f, "fail-queued", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(replayed, receipt);
    assert_eq!(
        f.jobs
            .get_job(&receipt.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobPhase::Failed
    );

    let before = counts(&f.db.pool).await;
    let error = f
        .jobs
        .fail_job(&receipt.job_id, &state.error.clone().unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidStateTransition);
    assert_eq!(counts(&f.db.pool).await, before);
}

#[tokio::test]
async fn fallo_durante_running_y_error_de_outbox_revierten_la_transicion_completa() {
    let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
    let (receipt, created) = enqueue(&f, "fail-running", &f.request_hash, &idempotency_expiry())
        .await
        .unwrap();
    assert!(created);
    f.jobs.start_job(&receipt.job_id).await.unwrap();
    let running = f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap();
    let failure = larm_domain::error::LarmError::new(
        ErrorCode::Timeout,
        "La instalación superó el tiempo permitido",
        "runtime_install_timeout",
    );
    let before = counts(&f.db.pool).await;
    sqlx::query("CREATE TRIGGER test_fail_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected'); END")
        .execute(&f.db.pool)
        .await
        .unwrap();

    assert!(f.jobs.fail_job(&receipt.job_id, &failure).await.is_err());
    assert_eq!(counts(&f.db.pool).await, before);
    assert_eq!(
        f.jobs.get_job(&receipt.job_id).await.unwrap().unwrap(),
        running
    );
    sqlx::query("DROP TRIGGER test_fail_outbox")
        .execute(&f.db.pool)
        .await
        .unwrap();

    let failed = f.jobs.fail_job(&receipt.job_id, &failure).await.unwrap();
    assert_eq!(failed.state, JobPhase::Failed);
    assert_eq!(failed.installation.row_version, 3);
    assert_eq!(failed.error, Some(failure));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM volume_reservation WHERE job_id=?")
            .bind(&receipt.job_id)
            .fetch_one(&f.db.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn fallo_del_outbox_y_de_idempotencia_revierte_instalacion_job_lock_y_reserva() {
    for (name, trigger) in [
        ("outbox_abort", "CREATE TRIGGER test_install_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected'); END"),
        ("outbox_ignore", "CREATE TRIGGER test_install_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(IGNORE); END"),
        ("idempotency_abort", "CREATE TRIGGER test_install_idempotency BEFORE INSERT ON idempotency BEGIN SELECT RAISE(ABORT,'injected'); END"),
        ("reservation_abort", "CREATE TRIGGER test_install_reservation BEFORE INSERT ON volume_reservation BEGIN SELECT RAISE(ABORT,'injected'); END"),
    ] {
        let f = fixture(900_000, timestamp(chrono::Utc::now())).await;
        let before = counts(&f.db.pool).await;
        sqlx::query(trigger).execute(&f.db.pool).await.unwrap();
        assert!(enqueue(&f, name, &f.request_hash, &idempotency_expiry())
            .await
            .is_err());
        assert_eq!(counts(&f.db.pool).await, before, "{name}");
        assert_eq!(f.jobs.get_job(&f.plan.plan_id).await.unwrap(), None, "{name}");
    }
}
