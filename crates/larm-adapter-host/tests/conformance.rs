//! Motor nativo con procesos Windows reales y P022. Las observaciones completas
//! son fixtures CODE_PASS: NO son los oráculos funcionales de las cinco familias
//! ni conceden conformidad de producto. No se compone este fixture en el daemon.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use larm_adapter_host::conformance::{
    NativeConformanceRunner, NativeIo, NativeSession, NativeSuite, NativeTarget, NativeTargets,
};
use larm_adapter_host::policy::{ElevationProbe, Policy, TrustedOnlyGrant};
use larm_adapter_host::worker::{HostConfig, WorkerSpec};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{
    adapter_resource, execution_digest, package_hash, Access, RootMap, TrustStore,
};
use larm_adapter_sdk::protocol::Request;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterConformanceRunner, MutationContext};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[path = "../../../tests/fixtures/adapters/benchmark_support.rs"]
mod benchmark_fixture;

struct NotElevated;
impl ElevationProbe for NotElevated {
    fn elevated(&self) -> Result<bool, LarmError> {
        Ok(false)
    }
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    Pass,
    Missing,
    Fail,
    ChangedInput,
    ChangedPackage,
    Hang,
    Wait,
    Panic,
    Restart,
    DeniedAccess,
    WrongFamily,
    RawEofRestart,
    RawExtra,
    RawExtraFailure,
    OracleIoTimeout,
    OracleIoSlowClose,
    OracleIoRestart,
    OracleIoPanic,
}
struct FixtureSuite {
    mode: Mode,
    caps: Vec<String>,
    input: PathBuf,
    package: PathBuf,
    entered: AtomicBool,
    done: AtomicBool,
    pids: Mutex<Vec<u32>>,
    io_closed: Arc<AtomicBool>,
}
struct FixtureIo {
    file: tokio::sync::Mutex<Option<std::fs::File>>,
    closed: Arc<AtomicBool>,
    delay: Duration,
}
#[async_trait]
impl NativeIo for FixtureIo {
    async fn close_io(&self) -> Result<(), LarmError> {
        tokio::time::sleep(self.delay).await;
        self.file.lock().await.take();
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl NativeSuite for FixtureSuite {
    fn descriptor(&self) -> Value {
        json!({"family":if matches!(self.mode, Mode::WrongFamily) {"source"} else {"benchmark"},
            "capabilities":self.caps,"fixture_only":"CODE_PASS synthetic observations",
            "case":format!("{:?}",self.mode),"version":"1.0"})
    }
    fn protocol_probes_first(&self) -> bool {
        matches!(
            self.mode,
            Mode::RawEofRestart | Mode::RawExtra | Mode::RawExtraFailure
        )
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        if matches!(self.mode, Mode::RawExtra | Mode::RawExtraFailure) {
            session.probe_start().await?;
            self.pids.lock().unwrap().push(session.pid()?);
            let hello = Request::new(
                "core.handshake",
                json!({"protocol_version":"1.0","adapter_id":session.manifest().id,"nonce":"late-frame-check"}),
            );
            session
                .probe_send(&serde_json::to_vec(&hello).unwrap(), &Access::default())
                .await?;
            let reply: Value =
                serde_json::from_slice(&session.probe_next(Duration::from_secs(3)).await?).unwrap();
            assert_eq!(reply["id"], hello.id);
            if matches!(self.mode, Mode::RawExtraFailure) {
                return Err(LarmError::new(
                    ErrorCode::Other("VENDOR_TEST_FAILURE".into()),
                    "fixture failure",
                    "fixture_oracle_failed",
                ));
            }
            // El motor debe rechazar el frame que aparecerá durante su cierre,
            // aunque esta fixture entregue cobertura sintética completa.
            return Ok(run
                .required_checks()
                .into_iter()
                .map(|id| Observation {
                    evidence_sha256: hash(&json!({"CODE_PASS_fixture":id,"nonce":run.id})).unwrap(),
                    check_id: id,
                    passed: true,
                })
                .collect());
        }
        if matches!(self.mode, Mode::RawEofRestart) {
            for first in [true, false] {
                session.probe_start().await?;
                self.pids.lock().unwrap().push(session.pid()?);
                let nonce = uuid::Uuid::now_v7().to_string();
                let hello = Request::new(
                    "core.handshake",
                    json!({"protocol_version":"1.0","adapter_id":session.manifest().id,"nonce":nonce}),
                );
                session
                    .probe_send(&serde_json::to_vec(&hello).unwrap(), &Access::default())
                    .await?;
                let reply: Value =
                    serde_json::from_slice(&session.probe_next(Duration::from_secs(3)).await?)
                        .unwrap();
                assert_eq!(reply["result"]["nonce"], nonce);
                if !first {
                    let request = Request::new("benchmark.describe", json!({}));
                    session
                        .probe_send(&serde_json::to_vec(&request).unwrap(), &Access::default())
                        .await?;
                    let reply: Value =
                        serde_json::from_slice(&session.probe_next(Duration::from_secs(3)).await?)
                            .unwrap();
                    assert_eq!(reply["id"], request.id);
                    larm_adapter_sdk::contracts::Contracts::shared()?
                        .response("benchmark.describe", &reply["result"])?;
                }
                let closed = session.probe_close().await?;
                assert_eq!(closed.success,!first,"primero fuerza kill tras EOF ignorado; luego vuelve a operar y cierra cooperativamente");
                assert_eq!(closed.extra_frames, 0);
            }
            self.done.store(true, Ordering::SeqCst);
            return Ok(run
                .required_checks()
                .into_iter()
                .map(|id| Observation {
                    evidence_sha256: hash(&json!({"CODE_PASS_fixture":id,"nonce":run.id})).unwrap(),
                    check_id: id,
                    passed: true,
                })
                .collect());
        }
        self.pids.lock().unwrap().push(session.pid()?);
        let mut access = Access::default();
        if matches!(self.mode, Mode::DeniedAccess) {
            access.reads.push(self.package.join("not-granted"));
        }
        let value = session
            .call(
                Request::new("benchmark.run", benchmark_fixture::request(20)),
                &access,
                None,
                None,
            )
            .await?;
        assert_eq!(
            value["metrics"]["ms"], 20,
            "se observa un RPC real, no un proceso echo del test"
        );
        self.entered.store(true, Ordering::SeqCst);
        if matches!(
            self.mode,
            Mode::OracleIoTimeout
                | Mode::OracleIoSlowClose
                | Mode::OracleIoRestart
                | Mode::OracleIoPanic
        ) {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(0);
            }
            let file = options
                .open(session.work_dir().join("oracle-io.bin"))
                .unwrap();
            session.register_io(Arc::new(FixtureIo {
                file: tokio::sync::Mutex::new(Some(file)),
                closed: self.io_closed.clone(),
                delay: Duration::from_millis(if matches!(self.mode, Mode::OracleIoSlowClose) {
                    1200
                } else {
                    150
                }),
            }));
        }
        match self.mode {
            Mode::ChangedInput => std::fs::write(&self.input, b"changed").unwrap(),
            Mode::ChangedPackage => {
                std::fs::write(self.package.join("added.bin"), b"changed").unwrap()
            }
            Mode::Hang => std::future::pending::<()>().await,
            Mode::Wait => tokio::time::sleep(Duration::from_millis(600)).await,
            Mode::Panic => panic!("fixture_native_suite_panic"),
            Mode::OracleIoPanic => panic!("fixture_oracle_io_panic"),
            Mode::OracleIoTimeout => std::future::pending::<()>().await,
            Mode::OracleIoRestart => {
                session.restart().await?;
                assert!(
                    !self.io_closed.load(Ordering::SeqCst),
                    "restart conserva el recurso del oraculo"
                );
            }
            Mode::Fail => {
                return Err(LarmError::new(
                    ErrorCode::Other("VENDOR_TEST_FAILURE".into()),
                    "fixture failure",
                    "fixture_oracle_failed",
                ))
            }
            Mode::Restart => {
                session.crash()?;
                session.restart().await?;
                self.pids.lock().unwrap().push(session.pid()?);
                let value = session
                    .call(
                        Request::new("benchmark.run", benchmark_fixture::request(10)),
                        &Access::default(),
                        None,
                        None,
                    )
                    .await?;
                assert_eq!(value["metrics"]["ms"], 10);
            }
            _ => {}
        }
        self.done.store(true, Ordering::SeqCst);
        // Sólo fixture de cobertura del motor: no acredita ejecutar estos
        // quince oráculos con benchmark.fake ni con una familia de producto.
        let mut observations = run
            .required_checks()
            .into_iter()
            .map(|id| Observation {
                evidence_sha256: hash(&json!({"CODE_PASS_fixture":id,"nonce":run.id})).unwrap(),
                check_id: id,
                passed: true,
            })
            .collect::<Vec<_>>();
        if matches!(self.mode, Mode::Missing) {
            observations.pop();
        }
        Ok(observations)
    }
}

#[derive(Clone)]
struct Target {
    spec: WorkerSpec,
    grant: Option<TrustedOnlyGrant>,
    suite: Arc<FixtureSuite>,
    profile: Option<PathBuf>,
    timeout: Duration,
    input: PathBuf,
}
#[async_trait]
impl NativeTargets for Target {
    async fn target(
        &self,
        _: &AdapterInstallation,
        _: &MutationContext,
    ) -> Result<NativeTarget, LarmError> {
        Ok(NativeTarget {
            spec: self.spec.clone(),
            roots: None,
            custom: false,
            grant: self.grant.clone(),
            capability_profile: self.profile.clone(),
            immutable_inputs: vec![self.input.clone()],
            suite: self.suite.clone(),
            timeout: self.timeout,
        })
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    spec: WorkerSpec,
    input: PathBuf,
    installation: AdapterInstallation,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("package");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/examples/fake_adapter.exe"),
            root.join("bin/fake_adapter.exe"),
        )
        .unwrap();
        let mut manifest: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/fake_adapter/manifest.json"
        ))
        .unwrap();
        manifest["executable"]["args"] =
            json!(["--manifest", root.join("manifest.json"), "--mode", "ok"]);
        manifest["permissions"] = json!({"read_roots":[],"write_roots":["$work"],"network_origins":[],"process_allowlist":[],"secret_refs":[]});
        let caps: Vec<String> = serde_json::from_value(manifest["capabilities"].clone()).unwrap();
        let registry = benchmark_fixture::registry(&caps).unwrap().unwrap();
        let descriptor = registry.descriptor(&caps[0]).unwrap();
        std::fs::write(
            root.join("profile.json"),
            json!({"schema_version":"1.0","entries":[descriptor]}).to_string(),
        )
        .unwrap();
        manifest["package_sha256"] = json!(package_hash(&root, &["manifest.json"]).unwrap());
        let manifest = AdapterManifest::parse(&manifest.to_string()).unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let input = dir.path().join("immutable.bin");
        std::fs::write(&input, b"immutable fixture input").unwrap();
        let installation = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: "2026-10-18T00:00:00.000Z".into(),
            updated_at: "2026-10-18T00:00:00.000Z".into(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            adapter_id: manifest.id.clone(),
            version: manifest.version.clone(),
            family: manifest.family.as_str().into(),
            package_hash: manifest.package_sha256.clone(),
            publisher: manifest.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            state: AdapterState::Installed,
            manifest: serde_json::to_value(&manifest).unwrap(),
        };
        Self {
            spec: WorkerSpec {
                package_dir: root,
                manifest,
                environment: vec![],
                work_dir: dir.path().join("work"),
            },
            input,
            installation,
            _dir: dir,
        }
    }
    fn target(&self, mode: Mode) -> (Target, Arc<FixtureSuite>) {
        let suite = Arc::new(FixtureSuite {
            mode,
            caps: self.spec.manifest.capabilities.clone(),
            input: self.input.clone(),
            package: self.spec.package_dir.clone(),
            entered: AtomicBool::new(false),
            done: AtomicBool::new(false),
            pids: Mutex::new(vec![]),
            io_closed: Arc::new(AtomicBool::new(false)),
        });
        let grant = TrustedOnlyGrant {
            approval_id: uuid::Uuid::now_v7().to_string(),
            actor_id: ctx().actor_id,
            resource: adapter_resource(&self.spec.manifest),
            execution_digest: execution_digest(&self.spec.manifest).unwrap(),
            package_sha256: self.spec.manifest.package_sha256.clone(),
            publisher: self.spec.manifest.publisher.clone(),
            secret_refs: vec![],
            expires_at: larm_domain::bridge::timestamp(
                chrono::Utc::now() + chrono::Duration::minutes(5),
            ),
        };
        (
            Target {
                spec: self.spec.clone(),
                grant: Some(grant),
                suite: suite.clone(),
                profile: Some("profile.json".into()),
                // Varias RPC de hasta 3 s, reinicios y mediciones físicas bajo
                // concurrencia requieren margen de suite. Las pruebas de
                // timeout conservan sus cotas explícitas; RPC/cancel/kill no cambian.
                timeout: Duration::from_secs(20),
                input: self.input.clone(),
            },
            suite,
        )
    }
    fn runner(&self, target: Target) -> Arc<NativeConformanceRunner> {
        Arc::new(NativeConformanceRunner::new(
            Arc::new(Policy {
                trust: TrustStore::default(),
                roots: RootMap(vec![("$work".into(), self.spec.work_dir.clone())]),
                elevation: Arc::new(NotElevated),
            }),
            Arc::new(target),
            HostConfig {
                handshake_timeout: Duration::from_secs(3),
                heartbeat: Duration::from_secs(1),
                cancel_grace: Duration::from_millis(400),
                ..HostConfig::default()
            },
        ))
    }
}
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "native-engine-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn pid_alive(pid: u32, binary: &Path) -> bool {
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
    // Un PID reutilizado por otra imagen no es nuestro worker. Una identidad
    // ilegible se trata conservadoramente como viva, nunca como cierre probado.
    let Some(image) = process.exe() else {
        return true;
    };
    let expected = std::fs::canonicalize(binary).unwrap();
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_uppercase()
    };
    normalize(image) == normalize(&expected)
        && std::fs::canonicalize(image).ok().as_ref() == Some(&expected)
}

fn assert_closed(suite: &FixtureSuite) {
    for pid in suite.pids.lock().unwrap().iter() {
        assert!(
            !pid_alive(*pid, &suite.package.join("bin/fake_adapter.exe")),
            "worker propio {pid} sigue vivo"
        );
    }
}

#[tokio::test]
async fn mide_bytes_reales_y_devuelve_evidencia_solo_despues_del_cierre() {
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Pass);
    let runner = f.runner(target);
    let first = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    first.proof.unwrap().check_run(&first.run).unwrap();
    assert_closed(&suite);
    let second = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    second.proof.unwrap().check_run(&second.run).unwrap();
    assert_ne!(first.run.id, second.run.id);
    assert_eq!(
        first.run.bindings, second.run.bindings,
        "mismo contexto medido, nonce nuevo"
    );
    assert_closed(&suite);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn oraculo_fallido_cobertura_incompleta_y_access_denegado_nunca_producen_proof() {
    for mode in [Mode::Missing, Mode::Fail, Mode::DeniedAccess] {
        let f = Fixture::new();
        let (target, suite) = f.target(mode);
        let runner = f.runner(target);
        let execution = runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap();
        let failure = execution.proof.err().unwrap();
        assert_eq!(
            failure.details.reason,
            match mode {
                Mode::Missing => "conformance_coverage_incomplete",
                Mode::Fail => "fixture_oracle_failed",
                _ => "read_denied",
            }
        );
        assert_closed(&suite);
        runner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn autorizacion_insuficiente_actor_ajeno_o_paquete_alterado_no_lanzan_worker() {
    for case in 0..4 {
        let f = Fixture::new();
        let (mut target, suite) = f.target(Mode::Pass);
        match case {
            0 => target.grant = None,
            1 => target.grant.as_mut().unwrap().actor_id = "other".into(),
            2 => target.grant.as_mut().unwrap().expires_at = "2020-01-01T00:00:00.000Z".into(),
            _ => std::fs::write(f.spec.package_dir.join("added.bin"), b"changed").unwrap(),
        }
        let runner = f.runner(target);
        assert!(runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .is_err());
        assert!(suite.pids.lock().unwrap().is_empty());
        assert!(
            !f.spec.work_dir.exists(),
            "P022 se deniega antes de crear work"
        );
        runner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn suite_ajena_o_perfil_ausente_o_fuera_del_paquete_se_rechazan() {
    for case in 0..3 {
        let f = Fixture::new();
        let (mut target, suite) = f.target(if case == 0 {
            Mode::WrongFamily
        } else {
            Mode::Pass
        });
        if case == 1 {
            target.profile = None;
        }
        if case == 2 {
            target.profile = Some("../immutable.bin".into());
        }
        let runner = f.runner(target);
        assert!(runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .is_err());
        assert!(suite.pids.lock().unwrap().is_empty());
        runner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cambio_de_fixtures_o_bytes_durante_la_suite_invalida_resultado_tras_cerrar_io() {
    for mode in [Mode::ChangedInput, Mode::ChangedPackage] {
        let f = Fixture::new();
        let (target, suite) = f.target(mode);
        let runner = f.runner(target);
        let result = runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap();
        assert_eq!(
            result.proof.err().unwrap().details.reason,
            if matches!(mode, Mode::ChangedInput) {
                "conformance_context_changed"
            } else {
                "package_hash_mismatch"
            }
        );
        assert_closed(&suite);
        runner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn timeout_de_suite_espera_el_worker_y_no_confirma_evidencia() {
    let f = Fixture::new();
    let (mut target, suite) = f.target(Mode::Hang);
    target.timeout = Duration::from_millis(200);
    let runner = f.runner(target);
    let result = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    assert_eq!(
        result.proof.err().unwrap().details.reason,
        "conformance_suite_timeout"
    );
    assert_closed(&suite);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn io_del_oraculo_se_cierra_en_timeout_panic_y_reinicio_sin_liberar_admision_prematura() {
    for mode in [
        Mode::OracleIoTimeout,
        Mode::OracleIoSlowClose,
        Mode::OracleIoRestart,
        Mode::OracleIoPanic,
    ] {
        let f = Fixture::new();
        let (mut target, suite) = f.target(mode);
        if matches!(mode, Mode::OracleIoTimeout) {
            target.timeout = Duration::from_millis(200);
        }
        let runner = f.runner(target);
        let result = runner.execute_conformance(&f.installation, &ctx()).await;
        match mode {
            Mode::OracleIoTimeout => assert_eq!(
                result.unwrap().proof.unwrap_err().details.reason,
                "conformance_suite_timeout"
            ),
            Mode::OracleIoSlowClose => {
                assert_eq!(
                    result.err().unwrap().details.reason,
                    "worker_io_cleanup_timeout"
                );
                assert!(!suite.io_closed.load(Ordering::SeqCst));
                assert_eq!(
                    runner
                        .execute_conformance(&f.installation, &ctx())
                        .await
                        .err()
                        .unwrap()
                        .details
                        .reason,
                    "conformance_already_running"
                );
            }
            Mode::OracleIoPanic => assert!(result.is_err()),
            Mode::OracleIoRestart => assert!(result.unwrap().proof.is_ok()),
            _ => unreachable!(),
        }
        runner.shutdown().await.unwrap();
        assert!(suite.io_closed.load(Ordering::SeqCst));
        assert!(
            std::fs::File::open(f.spec.work_dir.join("oracle-io.bin")).is_ok(),
            "handle exclusivo real cerrado"
        );
        assert_closed(&suite);
    }
}

#[tokio::test]
async fn observador_cancelado_no_libera_admision_ni_abandona_proceso_y_shutdown_espera() {
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Wait);
    let runner = f.runner(target);
    let child_runner = runner.clone();
    let installation = f.installation.clone();
    let observer = tokio::spawn(async move {
        child_runner
            .execute_conformance(&installation, &ctx())
            .await
    });
    tokio::time::timeout(Duration::from_secs(15), async {
        while !suite.entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let concurrent = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(concurrent.details.reason, "conformance_already_running");
    observer.abort();
    assert!(observer.await.err().unwrap().is_cancelled());
    runner.shutdown().await.unwrap();
    assert!(suite.done.load(Ordering::SeqCst));
    assert_closed(&suite);
    assert_eq!(
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_runner_closed"
    );
}

#[tokio::test]
async fn panic_del_oraculo_no_abandona_worker_y_no_produce_execution() {
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Panic);
    let runner = f.runner(target);
    assert_eq!(
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_execution_failed"
    );
    runner.shutdown().await.unwrap();
    assert_closed(&suite);
}

#[tokio::test]
async fn crash_y_restart_conservan_medicion_y_cierran_todos_los_workers_propios() {
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Restart);
    let runner = f.runner(target);
    let result = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    result.proof.unwrap().check_run(&result.run).unwrap();
    assert_eq!(suite.pids.lock().unwrap().len(), 2);
    assert_closed(&suite);
    runner.shutdown().await.unwrap();
}

#[tokio::test]
async fn handshake_fallido_conserva_owner_hasta_el_cierre_sin_ejecutar_oraculos() {
    let mut f = Fixture::new();
    *f.spec.manifest.executable.args.last_mut().unwrap() = "bad-nonce".into();
    f.installation.manifest = serde_json::to_value(&f.spec.manifest).unwrap();
    std::fs::write(
        f.spec.package_dir.join("manifest.json"),
        f.installation.manifest.to_string(),
    )
    .unwrap();
    let pid_file = f.spec.work_dir.join("started.pid");
    f.spec.environment.push((
        "LARM_TEST_PID_FILE".into(),
        pid_file.to_string_lossy().into_owned(),
    ));
    let (target, suite) = f.target(Mode::Pass);
    let runner = f.runner(target);
    let result = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    assert_eq!(result.proof.err().unwrap().details.reason, "nonce_mismatch");
    assert!(suite.pids.lock().unwrap().is_empty());
    runner.shutdown().await.unwrap();
    let pid = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert!(!pid_alive(
        pid,
        &f.spec.package_dir.join("bin/fake_adapter.exe")
    ));
}

#[tokio::test]
async fn probe_raw_que_ignora_eof_se_termina_drena_y_reinicia_en_el_mismo_contexto() {
    let mut f = Fixture::new();
    *f.spec.manifest.executable.args.last_mut().unwrap() = "deaf-input-once".into();
    f.installation.manifest = serde_json::to_value(&f.spec.manifest).unwrap();
    std::fs::write(
        f.spec.package_dir.join("manifest.json"),
        f.installation.manifest.to_string(),
    )
    .unwrap();
    let (target, suite) = f.target(Mode::RawEofRestart);
    let runner = f.runner(target);
    let result = runner
        .execute_conformance(&f.installation, &ctx())
        .await
        .unwrap();
    result.proof.unwrap().check_run(&result.run).unwrap();
    runner.shutdown().await.unwrap();
    assert_eq!(suite.pids.lock().unwrap().len(), 2);
    assert_closed(&suite);
}

#[tokio::test]
async fn reply_tardio_invalida_cobertura_y_cierre_no_sustituye_el_primer_error() {
    for mode in [Mode::RawExtra, Mode::RawExtraFailure] {
        let mut f = Fixture::new();
        *f.spec.manifest.executable.args.last_mut().unwrap() = "late-extra".into();
        f.installation.manifest = serde_json::to_value(&f.spec.manifest).unwrap();
        std::fs::write(
            f.spec.package_dir.join("manifest.json"),
            f.installation.manifest.to_string(),
        )
        .unwrap();
        let (target, suite) = f.target(mode);
        let runner = f.runner(target);
        let result = runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap();
        let failure = result.proof.err().unwrap();
        assert_eq!(
            failure.details.reason,
            if matches!(mode, Mode::RawExtra) {
                "probe_unconsumed_frames"
            } else {
                "fixture_oracle_failed"
            }
        );
        runner.shutdown().await.unwrap();
        assert_closed(&suite);
    }
}

// Fixture tipada de commit: los oráculos de este fichero son CODE_PASS.
fn committed_fixture(
    f: &Fixture,
    execution: larm_domain::conformance::Execution,
) -> (AdapterInstallation, Run) {
    use larm_domain::conformance::{committed_installation_hash, Outcome, Receipt, METADATA_KEY};
    let run = execution.run;
    let proof = execution.proof.unwrap();
    let mut a = f.installation.clone();
    a.state = AdapterState::Validated;
    a.row_version += 1;
    a.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let receipt = Receipt {
        run_id: run.id.clone(),
        installation_id: a.id.clone(),
        installation_revision: a.row_version.to_string(),
        committed_installation_sha256: committed_installation_hash(&a).unwrap(),
        package_sha256: a.package_hash.clone(),
        run_sha256: run.digest().unwrap(),
        evidence_sha256: Some(proof.digest().unwrap()),
        outcome_sha256: hash(&Outcome::Passed { proof }).unwrap(),
        passed: true,
        completed_at: a.updated_at.clone(),
    };
    a.metadata
        .insert(METADATA_KEY.into(), serde_json::to_value(receipt).unwrap());
    (a, run)
}

#[tokio::test]
async fn gate_fisico_retiene_archivos_admision_y_shutdown_sin_lanzar_worker() {
    use larm_domain::ports::AdapterQualification;
    let f = Fixture::new();
    assert!(pid_alive(
        std::process::id(),
        &std::env::current_exe().unwrap()
    ));
    assert!(!pid_alive(
        std::process::id(),
        &f.spec.package_dir.join("bin/fake_adapter.exe")
    ));
    let (target, suite) = f.target(Mode::Pass);
    let runner = f.runner(target);
    let (a, run) = committed_fixture(
        &f,
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap(),
    );
    let before = suite.pids.lock().unwrap().len();
    let guard = runner.acquire(&a, &run, &ctx()).await.unwrap();
    guard.recheck().await.unwrap();
    assert_eq!(suite.pids.lock().unwrap().len(), before);
    #[cfg(windows)]
    for path in [&f.input, &f.spec.package_dir.join("manifest.json")] {
        assert!(std::fs::OpenOptions::new().write(true).open(path).is_err());
        assert!(std::fs::remove_file(path).is_err());
    }
    assert_eq!(
        runner
            .execute_conformance(&a, &ctx())
            .await
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_already_running"
    );
    let shutdown = tokio::spawn({
        let r = runner.clone();
        async move { r.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!shutdown.is_finished());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&f.input)
        .is_ok());
    assert_eq!(
        runner
            .acquire(&a, &run, &ctx())
            .await
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_runner_closed"
    );
    assert_closed(&suite);
}

#[tokio::test]
async fn gate_fisico_rechaza_bytes_perfil_suite_entorno_grant_y_contexto_cambiados() {
    use larm_domain::ports::AdapterQualification;
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Pass);
    let runner = f.runner(target.clone());
    let (a, run) = committed_fixture(
        &f,
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap(),
    );
    runner.shutdown().await.unwrap();
    let before = suite.pids.lock().unwrap().len();
    for case in 0..11 {
        let mut changed = target.clone();
        let input = std::fs::read(&f.input).unwrap();
        let profile_path = f.spec.package_dir.join("profile.json");
        let profile = std::fs::read(&profile_path).unwrap();
        let extra = f.spec.package_dir.join("new-owned-file.bin");
        let mut context = ctx();
        match case {
            0 => std::fs::write(&f.input, b"changed after validation").unwrap(),
            1 => std::fs::write(&extra, b"unexpected package entry").unwrap(),
            2 => {
                let mut bytes = profile.clone();
                bytes.push(b' ');
                std::fs::write(&profile_path, bytes).unwrap();
            }
            3 => changed.profile = None,
            4 => changed
                .spec
                .environment
                .push(("LARM_GATE_TEST".into(), "changed".into())),
            5 => changed.timeout += Duration::from_millis(1),
            6 => changed.grant = None,
            7 => changed.grant.as_mut().unwrap().expires_at = "2020-01-01T00:00:00.000Z".into(),
            8 => context.actor_id = "other-authorized-context".into(),
            9 => changed.suite = f.target(Mode::WrongFamily).1,
            _ => changed.spec.work_dir = f._dir.path().join("missing-work"),
        }
        let checker = f.runner(changed);
        assert!(
            checker.acquire(&a, &run, &context).await.is_err(),
            "case {case}"
        );
        checker.shutdown().await.unwrap();
        std::fs::write(&f.input, &input).unwrap();
        std::fs::write(&profile_path, &profile).unwrap();
        if extra.exists() {
            std::fs::remove_file(&extra).unwrap();
        }
        assert!(!f._dir.path().join("missing-work").exists());
        assert_eq!(
            suite.pids.lock().unwrap().len(),
            before,
            "gate must not execute plugin"
        );
    }
    let checker = f.runner(target);
    checker
        .acquire(&a, &run, &ctx())
        .await
        .unwrap()
        .recheck()
        .await
        .unwrap();
    checker.shutdown().await.unwrap();
    assert_closed(&suite);
}

#[tokio::test]
async fn gate_fisico_remide_inventario_antes_del_commit_y_permite_reintento_tras_restaurar() {
    use larm_domain::ports::AdapterQualification;
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Pass);
    let runner = f.runner(target);
    let (a, run) = committed_fixture(
        &f,
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap(),
    );
    let guard = runner.acquire(&a, &run, &ctx()).await.unwrap();
    let extra = f.spec.package_dir.join("new-owned-file.bin");
    std::fs::write(&extra, b"added while guard holds existing files").unwrap();
    assert_eq!(
        guard.recheck().await.unwrap_err().code,
        ErrorCode::IntegrityFailed
    );
    drop(guard);
    std::fs::remove_file(&extra).unwrap();
    runner
        .acquire(&a, &run, &ctx())
        .await
        .unwrap()
        .recheck()
        .await
        .unwrap();
    runner.shutdown().await.unwrap();
    assert_closed(&suite);
}

struct SlowGateProbe {
    slow: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
}
impl ElevationProbe for SlowGateProbe {
    fn elevated(&self) -> Result<bool, LarmError> {
        if self.slow.load(Ordering::SeqCst) {
            self.entered.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(false)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_fisico_cancelar_observador_de_recheck_no_suelta_archivos_ni_admision() {
    use larm_domain::ports::AdapterQualification;
    let f = Fixture::new();
    let (target, suite) = f.target(Mode::Pass);
    let slow = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let runner = Arc::new(NativeConformanceRunner::new(
        Arc::new(Policy {
            trust: TrustStore::default(),
            roots: RootMap(vec![("$work".into(), f.spec.work_dir.clone())]),
            elevation: Arc::new(SlowGateProbe {
                slow: slow.clone(),
                entered: entered.clone(),
            }),
        }),
        Arc::new(target),
        HostConfig {
            handshake_timeout: Duration::from_secs(3),
            heartbeat: Duration::from_secs(1),
            cancel_grace: Duration::from_millis(400),
            ..HostConfig::default()
        },
    ));
    let (a, run) = committed_fixture(
        &f,
        runner
            .execute_conformance(&f.installation, &ctx())
            .await
            .unwrap(),
    );
    let guard = runner.acquire(&a, &run, &ctx()).await.unwrap();
    slow.store(true, Ordering::SeqCst);
    let observer = tokio::spawn(async move { guard.recheck().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&f.input)
        .is_err());
    let shutdown = tokio::spawn({
        let r = runner.clone();
        async move { r.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!shutdown.is_finished());
    tokio::time::timeout(Duration::from_secs(4), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&f.input)
        .is_ok());
    assert_closed(&suite);
}

#[tokio::test]
async fn catalogo_nativo_no_acepta_id_invalido_ni_reemplazo_ni_suite_desde_cliente() {
    let f = Fixture::new();
    let (target, _) = f.target(Mode::Pass);
    let catalog = larm_adapter_host::conformance::catalog::NativeCatalog::default();
    assert_eq!(
        catalog
            .target(&f.installation, &ctx())
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::UnsupportedCapability
    );
    let resolved = target.target(&f.installation, &ctx()).await.unwrap();
    assert!(catalog.register("bad", resolved.clone()).is_err());
    catalog
        .register(&f.installation.id, resolved.clone())
        .unwrap();
    assert_eq!(
        catalog
            .register(&f.installation.id, resolved)
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let found = catalog.target(&f.installation, &ctx()).await.unwrap();
    assert_eq!(found.spec.manifest, f.spec.manifest);
    assert!(!f.spec.work_dir.exists());
}
