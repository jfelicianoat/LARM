//! Procesos/P022/retención física reales; cualificación sintética CODE_PASS.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use larm_domain::bridge::{Constraints, ResourceRequirements};
use larm_domain::error::ErrorCode;
use larm_domain::instances::{
    InstanceRef, InstanceState, LaunchRequest, RuntimeHealthState, RuntimeInstance,
    RuntimeLaunchSnapshot,
};
use larm_domain::launch::{EnvironmentBinding, EnvironmentConfig, LaunchParameters};
use larm_domain::runtime::{InstallationStrategy, RuntimeChannel, RuntimeOwnership};
use larm_domain::runtime_execution::{
    RuntimeDiscoveryHost, RuntimeExecutionContext, RuntimeOperationAccess,
};
use larm_domain::runtime_install::{
    ExecutePlan, InstallRequest, InstallStatusRequest, RuntimeVersionSource, VersionQuery,
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
#[path = "../../../tests/fixtures/adapters/runtime_session.rs"]
mod fixture;
use fixture::{Fixture, RID};

fn process_alive(pid: u32, expected_exe: &Path) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let ids = [Pid::from_u32(pid)];
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let Some(process) = system.process(ids[0]) else {
        return false;
    };
    let expected = std::fs::canonicalize(expected_exe).ok();
    process
        .exe()
        .and_then(|path| std::fs::canonicalize(path).ok())
        == expected
}

async fn wait_for_process_exit(pid: u32, expected_exe: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while process_alive(pid, expected_exe) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime fixture process survived"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_observation(path: &Path) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        if let Ok(bytes) = tokio::fs::read(path).await {
            return serde_json::from_slice(&bytes).unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime fixture observation missing"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct FixtureDiscoveryAccess(larm_adapter_sdk::permissions::Access);

#[async_trait::async_trait]
impl larm_adapter_host::runtime::discovery::DiscoveryAccessResolver for FixtureDiscoveryAccess {
    async fn resolve(
        &self,
        _context: &RuntimeExecutionContext,
        _ctx: &larm_domain::ports::MutationContext,
    ) -> Result<larm_adapter_host::runtime::discovery::DiscoveryAccess, larm_domain::error::LarmError>
    {
        Ok(larm_adapter_host::runtime::discovery::DiscoveryAccess {
            access: self.0.clone(),
            timeout: Duration::from_secs(10),
        })
    }
}

fn execution_context(
    fixture: &Fixture,
    installation: &larm_domain::adapters::AdapterInstallation,
    run: &larm_domain::conformance::Run,
) -> RuntimeExecutionContext {
    let at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let adapter_id = uuid::Uuid::now_v7().to_string();
    RuntimeExecutionContext {
        operation_id: uuid::Uuid::now_v7().to_string(),
        runtime: larm_domain::runtime::Runtime {
            id: RID.into(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: Default::default(),
            name: "fixture runtime".into(),
            adapter_id: adapter_id.clone(),
            capabilities: vec![],
            default_version_id: None,
            channel_policy: Default::default(),
            retention_policy: Default::default(),
        },
        adapter: larm_domain::runtime::RuntimeAdapter {
            id: adapter_id,
            created_at: at.clone(),
            updated_at: at,
            row_version: 1,
            metadata: Default::default(),
            adapter_installation_id: installation.id.clone(),
            runtime_family: "fixture".into(),
            capabilities: vec![],
        },
        installation: installation.clone(),
        run: run.clone(),
        actor_id: fixture.context.actor_id.clone(),
    }
}

fn query(cursor: Option<&str>) -> VersionQuery {
    VersionQuery {
        runtime_id: RID.into(),
        channel: RuntimeChannel::Commit,
        cursor: cursor.map(str::to_owned),
    }
}
fn install() -> InstallRequest {
    InstallRequest {
        runtime_id: RID.into(),
        upstream_version: "b7142-fixture".into(),
        strategy: InstallationStrategy::PrebuiltBinary,
        storage_location_id: uuid::Uuid::now_v7().to_string(),
        profile_lock_hash: "a".repeat(64),
        external_host_id: None,
        execution_host_id: None,
    }
}

#[tokio::test]
async fn runtime_control_tipado_valida_plan_inicio_salud_y_parada() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let source_error = runner
        .source_worker(
            &installed,
            &run,
            &uuid::Uuid::now_v7().to_string(),
            larm_adapter_sdk::source::SearchSupport {
                search_applicable: false,
                supported_filters: Default::default(),
                limitations: vec![],
            },
            &f.context,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(source_error.details.reason, "source_worker_family_mismatch");
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let request = LaunchRequest {
        installation_id: uuid::Uuid::now_v7().to_string(),
        variant_id: uuid::Uuid::now_v7().to_string(),
        launch_profile: LaunchParameters::builtin(RID, 4, None, 128),
        constraints: Constraints {
            runtime_version_id: None,
            backend_id: None,
            execution_fingerprint: None,
            allow_backend_fallback: false,
            max_memory_bytes: None,
        },
        resource_requirements: None::<ResourceRequirements>,
    };

    let mut invalid = request.clone();
    invalid.variant_id = "not-an-id".into();
    let calls_before = f.calls();
    assert_eq!(
        client
            .launch_plan(
                &uuid::Uuid::now_v7().to_string(),
                &invalid,
                &f.access(),
                None,
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_launch_request_invalid"
    );
    assert_eq!(
        f.calls(),
        calls_before,
        "una petición inválida no llega al worker"
    );

    let plan = client
        .launch_plan(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(plan.port, 24000);
    let handle = client
        .start(
            &uuid::Uuid::now_v7().to_string(),
            &ExecutePlan {
                plan_id: plan.plan_id,
                plan_hash: plan.plan_hash,
                approval_id: None,
            },
            &f.access(),
            None,
        )
        .await
        .unwrap();
    let reference = InstanceRef {
        instance_id: handle.instance_id.clone(),
        generation: handle.generation,
    };
    assert_eq!(
        client
            .health(
                &uuid::Uuid::now_v7().to_string(),
                &reference,
                &f.access(),
                None,
            )
            .await
            .unwrap()
            .state,
        RuntimeHealthState::Ready
    );
    assert!(
        client
            .stop(
                &uuid::Uuid::now_v7().to_string(),
                &reference,
                &f.access(),
                None,
            )
            .await
            .unwrap()
            .stopped
    );
    assert_eq!(
        client
            .health(
                &uuid::Uuid::now_v7().to_string(),
                &reference,
                &f.access(),
                None,
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "instance_not_found"
    );

    let loaded = client
        .load(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert!(
        client
            .stop(
                &uuid::Uuid::now_v7().to_string(),
                &InstanceRef {
                    instance_id: loaded.instance_id,
                    generation: loaded.generation,
                },
                &f.access(),
                None,
            )
            .await
            .unwrap()
            .stopped
    );

    client.close().await.unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sesion_cualificada_expone_ciclo_runtime_con_acceso_explicito_por_llamada() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let context = execution_context(&f, &installed, &run);
    let host = larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
        runner: runner.clone(),
        access: Arc::new(FixtureDiscoveryAccess(f.access())),
    };
    let session = host.open(&context, &f.context).await.unwrap();
    let access = RuntimeOperationAccess {
        writes: vec![f.target.spec.work_dir.clone()],
        ..RuntimeOperationAccess::default()
    };
    let request = LaunchRequest {
        installation_id: uuid::Uuid::now_v7().to_string(),
        variant_id: uuid::Uuid::now_v7().to_string(),
        launch_profile: LaunchParameters::builtin(RID, 4, None, 128),
        constraints: Constraints {
            runtime_version_id: None,
            backend_id: None,
            execution_fingerprint: None,
            allow_backend_fallback: false,
            max_memory_bytes: None,
        },
        resource_requirements: None::<ResourceRequirements>,
    };

    let calls_before_denied_access = f.calls();
    let denied_access = RuntimeOperationAccess {
        reads: vec![f.target.spec.package_dir.clone()],
        ..RuntimeOperationAccess::default()
    };
    assert_eq!(
        session
            .launch_plan(&request, &denied_access, Some(Duration::from_secs(3)))
            .await
            .unwrap_err()
            .details
            .reason,
        "read_denied"
    );
    assert_eq!(f.calls(), calls_before_denied_access);
    assert_eq!(
        session
            .launch_plan(&request, &access, Some(Duration::ZERO))
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_lifecycle_timeout_invalid"
    );
    assert_eq!(f.calls(), calls_before_denied_access);

    let plan = session
        .launch_plan(&request, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let execute = ExecutePlan {
        plan_id: plan.plan_id,
        plan_hash: plan.plan_hash,
        approval_id: None,
    };
    let handle = session
        .start(&execute, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let reference = InstanceRef {
        instance_id: handle.instance_id,
        generation: handle.generation,
    };
    assert_eq!(
        session
            .health(&reference, &access, Some(Duration::from_secs(3)))
            .await
            .unwrap()
            .state,
        RuntimeHealthState::Ready
    );
    assert!(
        session
            .stop(&reference, &access, Some(Duration::from_secs(3)))
            .await
            .unwrap()
            .stopped
    );

    let loaded = session
        .load(&request, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    assert!(
        session
            .stop(
                &InstanceRef {
                    instance_id: loaded.instance_id,
                    generation: loaded.generation,
                },
                &access,
                Some(Duration::from_secs(3)),
            )
            .await
            .unwrap()
            .stopped
    );
    session.close().await.unwrap();
    drop(session);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sesion_cualificada_inicia_proceso_propio_desde_snapshot_efectivo() {
    const SECRET: &str = "P033_fixture_secret_not_for_logs";
    let mut f = Fixture::new();
    f.target.spec.environment = vec![
        ("LARM_FIXTURE_START_PROCESS".into(), "1".into()),
        ("LARM_FIXTURE_SECRET_VALUE".into(), SECRET.into()),
    ];
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let context = execution_context(&f, &installed, &run);
    let host = larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
        runner: runner.clone(),
        access: Arc::new(FixtureDiscoveryAccess(f.access())),
    };
    let session = host.open(&context, &f.context).await.unwrap();
    let mut parameters = LaunchParameters::builtin(RID, 4, None, 128);
    parameters.runtime_arguments = vec!["--profile-arg".into(), "phase-three".into()];
    parameters.environment = EnvironmentConfig {
        allowlist: vec!["LARM_FIXTURE_VALUE".into(), "LARM_FIXTURE_SECRET".into()],
        variables: vec![
            EnvironmentBinding::Value {
                name: "LARM_FIXTURE_VALUE".into(),
                value: "visible-profile-value".into(),
            },
            EnvironmentBinding::Credential {
                name: "LARM_FIXTURE_SECRET".into(),
                credential_reference: "larm/test/p033-secret".into(),
            },
        ],
    };
    let request = LaunchRequest {
        installation_id: uuid::Uuid::now_v7().to_string(),
        variant_id: uuid::Uuid::now_v7().to_string(),
        launch_profile: parameters,
        constraints: Constraints {
            runtime_version_id: None,
            backend_id: None,
            execution_fingerprint: None,
            allow_backend_fallback: false,
            max_memory_bytes: None,
        },
        resource_requirements: None::<ResourceRequirements>,
    };
    let access = RuntimeOperationAccess {
        writes: vec![f.target.spec.work_dir.clone()],
        processes: vec!["bin/worker.exe".into()],
        ..RuntimeOperationAccess::default()
    };

    let mut unsupported_limit = request.clone();
    unsupported_limit.launch_profile.memory_limit_bytes =
        larm_domain::units::DecimalU64::new(1 << 30);
    assert_eq!(
        session
            .launch_plan(&unsupported_limit, &access, Some(Duration::from_secs(3)),)
            .await
            .unwrap_err()
            .details
            .reason,
        "fixture_memory_limit_unsupported",
        "la fixture debe rechazar un límite que no aplica al proceso"
    );

    let plan = session
        .launch_plan(&request, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    assert_eq!(plan.args, request.launch_profile.runtime_arguments);
    assert_eq!(plan.environment, request.launch_profile.environment);
    assert_eq!(plan.effective_memory_limit_bytes, None);
    let snapshot = RuntimeLaunchSnapshot::from_plan(plan.clone(), chrono::Utc::now()).unwrap();
    let execute = ExecutePlan {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        approval_id: None,
    };
    let handle = session
        .start(&execute, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let pid = handle.pid.expect("fixture returns the owned child PID");
    let started_at = handle
        .process_started_at
        .clone()
        .expect("fixture returns the observed OS start time");
    let executable = f.target.spec.package_dir.join("bin/worker.exe");
    assert!(process_alive(pid, &executable));

    let observation_path = f
        .target
        .spec
        .work_dir
        .join(format!("p033-runtime-{}.json", handle.instance_id));
    let observed = wait_for_observation(&observation_path).await;
    assert_eq!(
        observed["args"],
        serde_json::json!(["--profile-arg", "phase-three"])
    );
    assert_eq!(
        observed["environment"]["LARM_FIXTURE_VALUE"],
        "visible-profile-value"
    );
    assert_eq!(observed["environment"]["LARM_FIXTURE_SECRET"], SECRET);
    assert_eq!(observed["memory_limit_bytes"], serde_json::Value::Null);

    let at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let executable_hash = hex::encode(Sha256::digest(std::fs::read(&executable).unwrap()));
    let mut instance = RuntimeInstance {
        id: handle.instance_id.clone(),
        created_at: at.clone(),
        updated_at: at,
        row_version: 1,
        metadata: Default::default(),
        runtime_installation_id: request.installation_id.clone(),
        variant_id: request.variant_id.clone(),
        backend_id: request.launch_profile.backend_id.clone(),
        launch_profile_id: uuid::Uuid::now_v7().to_string(),
        state: InstanceState::Created,
        pid: None,
        process_started_at: None,
        executable_hash: None,
        generation: handle.generation,
        execution_fingerprint: None,
        last_used_at: None,
        ownership: RuntimeOwnership::LarmManaged,
    };
    instance.store_launch_snapshot(&snapshot).unwrap();
    instance = instance.transition_to(InstanceState::Starting).unwrap();
    instance.pid = Some(pid);
    instance.process_started_at = Some(started_at);
    instance.executable_hash = Some(executable_hash);
    instance = instance.transition_to(InstanceState::Loading).unwrap();
    instance = instance.transition_to(InstanceState::Ready).unwrap();
    instance.validate_record().unwrap();
    assert_eq!(instance.launch_snapshot().unwrap(), Some(snapshot));

    let reference = InstanceRef {
        instance_id: handle.instance_id.clone(),
        generation: handle.generation,
    };
    assert_eq!(
        session
            .health(&reference, &access, Some(Duration::from_secs(3)))
            .await
            .unwrap()
            .state,
        RuntimeHealthState::Ready
    );
    assert!(
        session
            .stop(&reference, &access, Some(Duration::from_secs(3)))
            .await
            .unwrap()
            .stopped
    );
    wait_for_process_exit(pid, &executable).await;
    assert!(
        !std::fs::read_to_string(f.target.spec.work_dir.join("requests.jsonl"))
            .unwrap()
            .contains(SECRET)
    );

    let second_plan = session
        .launch_plan(&request, &access, Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let second_handle = session
        .start(
            &ExecutePlan {
                plan_id: second_plan.plan_id,
                plan_hash: second_plan.plan_hash,
                approval_id: None,
            },
            &access,
            Some(Duration::from_secs(3)),
        )
        .await
        .unwrap();
    let second_pid = second_handle.pid.unwrap();
    assert!(process_alive(second_pid, &executable));
    session.close().await.unwrap();
    wait_for_process_exit(second_pid, &executable).await;
    drop(session);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sesion_fisica_tipificada_scope_estrategia_y_recibos_sin_instalar_engine() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let page = client
        .version_source(f.access(), None)
        .list_versions(&query(None))
        .await
        .unwrap();
    assert_eq!(page.items[0].channel, RuntimeChannel::Commit);
    let mut custom = query(None);
    custom.channel = RuntimeChannel::Custom;
    let custom_page = client
        .version_source(f.access(), None)
        .list_versions(&custom)
        .await
        .unwrap();
    assert_eq!(
        custom_page.items[0].metadata["upstream_channel"],
        serde_json::json!({"value":"fixture-preview/42", "reason":"UNKNOWN"})
    );
    assert_eq!(
        custom_page.items[0]
            .observed_channel()
            .unwrap()
            .upstream_value,
        "fixture-preview/42"
    );
    let before = f.calls();
    let mut wrong = query(None);
    wrong.runtime_id = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        client
            .versions(&uuid::Uuid::now_v7().to_string(), &wrong, &f.access(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let mut unannounced = install();
    unannounced.strategy = InstallationStrategy::Uv;
    assert_eq!(
        client
            .plan_install(
                &uuid::Uuid::now_v7().to_string(),
                &unannounced,
                &f.access(),
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    assert_eq!(f.calls(), before);
    let request = install();
    let plan = client
        .plan_install(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(plan.steps[0].request["strategy"], "PREBUILT_BINARY");
    let receipt = client
        .install(
            &uuid::Uuid::now_v7().to_string(),
            &ExecutePlan {
                plan_id: plan.plan_id.clone(),
                plan_hash: plan.plan_hash,
                approval_id: None,
            },
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt.job_id, plan.plan_id);
    client.close().await.unwrap();
    assert!(client.io_drained());
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn runtime_install_transporta_recibos_terminales_del_worker() {
    for (mode, expected) in [
        ("completed", larm_domain::bridge::JobPhase::Completed),
        ("failed", larm_domain::bridge::JobPhase::Failed),
    ] {
        let f = Fixture::new();
        let runner = f.runner();
        let (installed, run) = f.validated(&runner).await;
        let client = runner
            .runtime_worker(&installed, &run, RID, &f.context)
            .await
            .unwrap();
        let plan = client
            .plan_install(
                &uuid::Uuid::now_v7().to_string(),
                &install(),
                &f.access(),
                None,
            )
            .await
            .unwrap();
        std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), mode).unwrap();
        let receipt = client
            .install(
                &uuid::Uuid::now_v7().to_string(),
                &ExecutePlan {
                    plan_id: plan.plan_id.clone(),
                    plan_hash: plan.plan_hash,
                    approval_id: None,
                },
                &f.access(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(receipt.job_id, plan.plan_id, "{mode}");
        assert_eq!(receipt.status_url, format!("/api/v1/jobs/{}", plan.plan_id));
        assert_eq!(receipt.state, expected, "{mode}");
        client.close().await.unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn sesion_cualificada_cancela_install_usando_el_id_durable_del_plan() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let context = execution_context(&f, &installed, &run);
    let host = larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
        runner: runner.clone(),
        access: Arc::new(FixtureDiscoveryAccess(f.access())),
    };
    let session = host.open(&context, &f.context).await.unwrap();
    let plan = session.plan_install(&install()).await.unwrap();
    std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), "delay").unwrap();
    let install_call_count = f.calls();
    let execute = ExecutePlan {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash,
        approval_id: None,
    };
    let install_session: Arc<dyn larm_domain::runtime_execution::RuntimeDiscoverySession> =
        Arc::from(session);
    let pending_session = install_session.clone();
    let pending = tokio::spawn(async move { pending_session.install(&execute).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while f.calls() <= install_call_count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "runtime.install no llegó al worker"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(install_session.cancel_install(&plan.plan_id).await.unwrap());
    assert_eq!(
        pending.await.unwrap().unwrap_err().details.reason,
        "fixture_runtime_install_cancelled"
    );
    install_session.close().await.unwrap();
    drop(install_session);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn runtime_install_status_exige_binding_de_plan_y_recibo() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let plan = client
        .plan_install(
            &uuid::Uuid::now_v7().to_string(),
            &install(),
            &f.access(),
            None,
        )
        .await
        .unwrap();
    let execute = ExecutePlan {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        approval_id: None,
    };
    let receipt = client
        .install(
            &uuid::Uuid::now_v7().to_string(),
            &execute,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt.state, larm_domain::bridge::JobPhase::Queued);
    let status = client
        .install_status(
            &uuid::Uuid::now_v7().to_string(),
            &InstallStatusRequest {
                plan_id: plan.plan_id.clone(),
                plan_hash: plan.plan_hash.clone(),
            },
            &f.access(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(status, receipt);
    let error = client
        .install_status(
            &uuid::Uuid::now_v7().to_string(),
            &InstallStatusRequest {
                plan_id: plan.plan_id,
                plan_hash: "0".repeat(64),
            },
            &f.access(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    client.close().await.unwrap();
    drop(client);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn plan_install_rechaza_hash_y_binding_de_recursos_manipulados() {
    for (mode, reason) in [
        ("bad-hash", "runtime_install_plan_hash_mismatch"),
        ("wrong-runtime", "runtime_install_plan_reply_invalid"),
    ] {
        let f = Fixture::new();
        let runner = f.runner();
        let (installed, run) = f.validated(&runner).await;
        let client = runner
            .runtime_worker(&installed, &run, RID, &f.context)
            .await
            .unwrap();
        std::fs::write(
            f.target.spec.work_dir.join("fixture-install-plan-mode"),
            mode,
        )
        .unwrap();
        let before = f.calls();
        let error = client
            .plan_install(
                &uuid::Uuid::now_v7().to_string(),
                &install(),
                &f.access(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolError, "{mode}");
        assert_eq!(error.details.reason, reason, "{mode}");
        assert_eq!(f.calls(), before + 1, "{mode}");
        client.close().await.unwrap();
        assert!(client.io_drained());
        drop(client);
        tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn recibo_install_rechaza_id_o_url_ajenos() {
    for mode in [
        "invalid-job-id",
        "empty-status-url",
        "different-job-id",
        "foreign-status-url",
    ] {
        let f = Fixture::new();
        let runner = f.runner();
        let (installed, run) = f.validated(&runner).await;
        let client = runner
            .runtime_worker(&installed, &run, RID, &f.context)
            .await
            .unwrap();
        let request = install();
        let plan = client
            .plan_install(
                &uuid::Uuid::now_v7().to_string(),
                &request,
                &f.access(),
                None,
            )
            .await
            .unwrap();
        std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), mode).unwrap();

        let result = client
            .install(
                &uuid::Uuid::now_v7().to_string(),
                &ExecutePlan {
                    plan_id: plan.plan_id,
                    plan_hash: plan.plan_hash,
                    approval_id: None,
                },
                &f.access(),
                None,
            )
            .await;
        let error = match result {
            Err(error) => error,
            Ok(receipt) => panic!("{mode}: malformed receipt was accepted: {receipt:?}"),
        };
        assert_eq!(error.code, ErrorCode::ProtocolError, "{mode}");
        client.close().await.unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn contexto_cambiado_no_envia_rpc_y_siempre_permite_cerrar() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let before = f.calls();
    std::fs::write(
        f.target.spec.package_dir.join("new-input"),
        b"fault injection",
    )
    .unwrap();
    assert!(client
        .versions(
            &uuid::Uuid::now_v7().to_string(),
            &query(None),
            &f.access(),
            None
        )
        .await
        .is_err());
    assert_eq!(f.calls(), before);
    client.close().await.unwrap();
    drop(client);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelacion_timeout_y_rechazo_de_reply_ajeno_son_reales() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let mut progress = client.progress();
    let id = uuid::Uuid::now_v7().to_string();
    let slow = query(Some("delay"));
    let access = f.access();
    {
        let call = client.versions(&id, &slow, &access, None);
        tokio::pin!(call);
        tokio::select! { event=progress.recv()=>assert_eq!(event.unwrap().request_id,id), _=&mut call=>panic!("fixture terminó antes de cancelar") }
        assert!(client.cancel(&id).await.unwrap());
        assert_eq!(
            call.await.unwrap_err().details.reason,
            "fixture_runtime_cancelled"
        );
    }
    assert_eq!(
        client
            .versions(
                &uuid::Uuid::now_v7().to_string(),
                &slow,
                &access,
                Some(Duration::from_millis(50))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    assert_eq!(
        client
            .versions(
                &uuid::Uuid::now_v7().to_string(),
                &query(Some("wrong-runtime")),
                &access,
                None
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_version_binding_mismatch"
    );
    assert_eq!(
        client
            .versions(
                &uuid::Uuid::now_v7().to_string(),
                &query(None),
                &access,
                None
            )
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    client.close().await.unwrap();
    drop(client);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn install_timeout_y_cancelacion_cooperativa_drenan_worker() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let request = install();
    let plan = client
        .plan_install(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), "delay").unwrap();

    let mut progress = client.progress();
    let id = uuid::Uuid::now_v7().to_string();
    let execution = ExecutePlan {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash,
        approval_id: None,
    };
    let access = f.access();
    {
        let call = client.install(&id, &execution, &access, None);
        tokio::pin!(call);
        tokio::select! {
            event = progress.recv() => {
                let event = event.unwrap();
                assert_eq!(event.request_id, id);
                assert_eq!(event.phase, "INSTALLING");
            }
            _ = &mut call => panic!("la instalación de fixture terminó antes de cancelar"),
        }
        assert!(client.cancel(&id).await.unwrap());
        assert_eq!(
            call.await.unwrap_err().details.reason,
            "fixture_runtime_install_cancelled"
        );
    }
    client.close().await.unwrap();
    assert!(client.io_drained());
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();

    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let request = install();
    let plan = client
        .plan_install(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), "delay").unwrap();
    let error = client
        .install(
            &uuid::Uuid::now_v7().to_string(),
            &ExecutePlan {
                plan_id: plan.plan_id,
                plan_hash: plan.plan_hash,
                approval_id: None,
            },
            &f.access(),
            Some(Duration::from_millis(50)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    client.close().await.unwrap();
    assert!(client.io_drained());
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn crash_del_worker_durante_install_drena_io_y_conserva_el_workspace() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let request = install();
    let plan = client
        .plan_install(
            &uuid::Uuid::now_v7().to_string(),
            &request,
            &f.access(),
            None,
        )
        .await
        .unwrap();
    std::fs::write(f.target.spec.work_dir.join("fixture-install-mode"), "crash").unwrap();

    let error = client
        .install(
            &uuid::Uuid::now_v7().to_string(),
            &ExecutePlan {
                plan_id: plan.plan_id.clone(),
                plan_hash: plan.plan_hash,
                approval_id: None,
            },
            &f.access(),
            Some(Duration::from_secs(3)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AdapterCrashed);
    assert!(f
        .target
        .spec
        .work_dir
        .join("runtime-installs")
        .join(&plan.plan_id)
        .join("engine.bin")
        .is_file());

    client.close().await.unwrap();
    assert!(client.io_drained());
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn error_original_del_rpc_prevalece_si_tambien_cambia_el_contexto() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = runner
        .runtime_worker(&installed, &run, RID, &f.context)
        .await
        .unwrap();
    let mut progress = client.progress();
    let id = uuid::Uuid::now_v7().to_string();
    let q = query(Some("vendor-delay"));
    let access = f.access();
    {
        let call = client.versions(&id, &q, &access, None);
        tokio::pin!(call);
        tokio::select! { _=progress.recv()=>{}, _=&mut call=>panic!("fixture terminó antes de alterar contexto") }
        std::fs::write(
            f.target.spec.package_dir.join("new-input"),
            b"fault injection",
        )
        .unwrap();
        let error = call.await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Other("VENDOR_P031".into()));
        assert_eq!(error.details.reason, "fixture_vendor_failure");
    }
    client.close().await.unwrap();
    drop(client);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn estado_actor_run_o_familia_ajenos_no_abren_consumidor() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let before = f.calls();
    assert!(runner
        .runtime_worker(&f.installation, &run, RID, &f.context)
        .await
        .is_err());
    let mut wrong = f.context.clone();
    wrong.actor_id = "another actor".into();
    assert_eq!(
        runner
            .runtime_worker(&installed, &run, RID, &wrong)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Forbidden
    );
    let mut stale = run.clone();
    stale.package_sha256 = "f".repeat(64);
    assert!(runner
        .runtime_worker(&installed, &stale, RID, &f.context)
        .await
        .is_err());
    let mut foreign = installed;
    foreign.family = "source".into();
    assert_eq!(
        runner
            .runtime_worker(&foreign, &run, RID, &f.context)
            .await
            .err()
            .unwrap()
            .details
            .reason,
        "runtime_worker_family_mismatch"
    );
    assert_eq!(f.calls(), before);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn abandonar_observador_cierra_proceso_antes_de_liberar_handles_y_admision() {
    let f = Fixture::new();
    let runner = f.runner();
    let (installed, run) = f.validated(&runner).await;
    let client = Arc::new(
        runner
            .runtime_worker(&installed, &run, RID, &f.context)
            .await
            .unwrap(),
    );
    let mut progress = client.progress();
    let access = f.access();
    let observer = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .versions(
                    &uuid::Uuid::now_v7().to_string(),
                    &query(Some("delay")),
                    &access,
                    None,
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), progress.recv())
        .await
        .unwrap()
        .unwrap();
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(f.target.spec.package_dir.join("manifest.json"))
        .is_err());
    drop(client);
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), runner.shutdown())
        .await
        .unwrap()
        .unwrap();
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(f.target.spec.package_dir.join("bin/worker.exe"))
        .is_ok());
}
