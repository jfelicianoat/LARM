//! Proceso Windows/P022/SQLite reales; la suite de cualificación usa observaciones
//! CODE_PASS explícitas. No es un motor ni una instalación managed de producto.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use larm_adapter_host::{conformance::NativeConformanceRunner, runtime::discovery::*};
use larm_adapter_sdk::permissions::Access;
use larm_db::{
    adapters::SqlAdapterInstallations, conformance::SqlAdapterConformance, pool::Db,
    runtimes::SqlRuntimes,
};
use larm_domain::{
    conformance::{hash, Outcome},
    error::{ErrorCode, LarmError},
    ports::{
        AdapterConformanceRepository, AdapterConformanceRunner, AdapterInstallationRepository,
        AdapterQualificationGuard, MutationContext,
    },
    runtime::*,
    runtime_execution::*,
    runtime_install::*,
    runtime_ports::RuntimeRepository,
};
use larm_runtime::discovery_service::DiscoveryService;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    num::NonZeroU32,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
#[path = "../../../tests/fixtures/adapters/runtime_session.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::Fixture;

struct Resolver {
    access: Access,
    timeout: Duration,
}
#[async_trait]
impl DiscoveryAccessResolver for Resolver {
    async fn resolve(
        &self,
        _: &RuntimeExecutionContext,
        _: &MutationContext,
    ) -> Result<DiscoveryAccess, LarmError> {
        Ok(DiscoveryAccess {
            access: self.access.clone(),
            timeout: self.timeout,
        })
    }
}
struct Host {
    native: NativeDiscoveryHost,
    closed: Arc<AtomicBool>,
    close_error: bool,
    wrong_binding: bool,
}
struct Session {
    inner: Box<dyn RuntimeDiscoverySession>,
    binding: RuntimeSessionBinding,
    closed: Arc<AtomicBool>,
    close_error: bool,
}
#[async_trait]
impl RuntimeDiscoveryHost for Host {
    async fn open(
        &self,
        context: &RuntimeExecutionContext,
        ctx: &MutationContext,
    ) -> Result<Box<dyn RuntimeDiscoverySession>, LarmError> {
        let inner = self.native.open(context, ctx).await?;
        let mut binding = inner.binding().clone();
        if self.wrong_binding {
            binding.runtime_id = uuid::Uuid::now_v7().to_string();
        }
        Ok(Box::new(Session {
            inner,
            binding,
            closed: self.closed.clone(),
            close_error: self.close_error,
        }))
    }
}
#[async_trait]
impl RuntimeVersionSource for Session {
    async fn list_versions(&self, q: &VersionQuery) -> Result<RuntimeVersionPage, LarmError> {
        self.inner.list_versions(q).await
    }
}
#[async_trait]
impl AdapterQualificationGuard for Session {
    async fn recheck(&self) -> Result<(), LarmError> {
        self.inner.recheck().await
    }
}
impl RuntimeSessionGuard for Session {
    fn binding(&self) -> &RuntimeSessionBinding {
        &self.binding
    }
}
#[async_trait]
impl RuntimeDiscoverySession for Session {
    async fn close(&self) -> Result<(), LarmError> {
        self.inner.close().await?;
        self.closed.store(true, Ordering::SeqCst);
        if self.close_error {
            Err(LarmError::new(
                ErrorCode::Internal,
                "Fixture cierre",
                "fixture_close_failed",
            ))
        } else {
            Ok(())
        }
    }
}
struct Repo {
    sql: SqlRuntimes,
    closed: Arc<AtomicBool>,
    commits: AtomicUsize,
    package_exe: PathBuf,
}
#[async_trait]
impl RuntimeExecutionRepository for Repo {
    async fn prepare_execution(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeExecutionContext, LarmError> {
        self.sql.prepare_execution(id, ctx).await
    }
    async fn commit_discovery(
        &self,
        c: &RuntimeExecutionContext,
        v: &[RuntimeVersion],
        g: &dyn RuntimeSessionGuard,
        ctx: &MutationContext,
    ) -> Result<Vec<RuntimeVersion>, LarmError> {
        assert!(
            self.closed.load(Ordering::SeqCst),
            "publicación antes del cierre/IO"
        );
        #[cfg(windows)]
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&self.package_exe)
                .is_err(),
            "se liberó el paquete antes de COMMIT"
        );
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.sql.commit_discovery(c, v, g, ctx).await
    }
}
struct Test {
    f: Fixture,
    _dbdir: tempfile::TempDir,
    db: Db,
    repo: Arc<Repo>,
    runner: Arc<NativeConformanceRunner>,
    runtime: Runtime,
}
impl Test {
    async fn new() -> Self {
        let mut f = Fixture::new();
        let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let sql = SqlRuntimes {
            pool: db.pool.clone(),
        };
        let adapters = SqlAdapterInstallations {
            pool: db.pool.clone(),
        };
        adapters.insert(&f.installation, &f.context).await.unwrap();
        let adapter = sql
            .register_runtime_adapter(&f.installation.id, "fixture.runtime", &f.context)
            .await
            .unwrap();
        f.installation = adapters.get(&f.installation.id).await.unwrap().unwrap();
        let request = RuntimeCreate {
            name: "P031 process and SQLite fixture".into(),
            adapter_id: adapter.id,
            capabilities: vec![],
            default_version_id: None,
            channel_policy: RuntimeChannelPolicy {
                allowed_channels: vec![
                    RuntimeChannel::Stable,
                    RuntimeChannel::Prerelease,
                    RuntimeChannel::Nightly,
                    RuntimeChannel::Commit,
                    RuntimeChannel::Custom,
                ],
            },
            retention_policy: Default::default(),
        };
        let runtime = sql
            .create_runtime(
                &request,
                &uuid::Uuid::now_v7().to_string(),
                &hash(&request).unwrap(),
                &f.context,
            )
            .await
            .unwrap();
        let runner = f.runner();
        let qualification = runner
            .execute_conformance(&f.installation, &f.context)
            .await
            .unwrap();
        // La suite compartida produce observaciones sintéticas; el proceso y la
        // persistencia del recibo sí recorren la implementación real.
        SqlAdapterConformance {
            pool: db.pool.clone(),
        }
        .commit_conformance(
            &qualification.run,
            &Outcome::Passed {
                proof: qualification.proof.unwrap(),
            },
            &f.context,
        )
        .await
        .unwrap();
        let repo = Arc::new(Repo {
            sql,
            closed: Arc::new(AtomicBool::new(false)),
            commits: AtomicUsize::new(0),
            package_exe: f.target.spec.package_dir.join("bin/worker.exe"),
        });
        Self {
            f,
            _dbdir: dir,
            db,
            repo,
            runner,
            runtime,
        }
    }
    fn service(
        &self,
        pages: u32,
        close_error: bool,
        wrong_binding: bool,
        access: Access,
        timeout: Duration,
    ) -> DiscoveryService {
        self.repo.closed.store(false, Ordering::SeqCst);
        DiscoveryService::new(
            self.repo.clone(),
            Arc::new(Host {
                native: NativeDiscoveryHost {
                    runner: self.runner.clone(),
                    access: Arc::new(Resolver { access, timeout }),
                },
                closed: self.repo.closed.clone(),
                close_error,
                wrong_binding,
            }),
            NonZeroU32::new(pages).unwrap(),
        )
    }
    fn normal(&self, pages: u32) -> DiscoveryService {
        self.service(pages, false, false, self.f.access(), Duration::from_secs(3))
    }
    fn mode(&self, mode: &str) {
        std::fs::write(
            self.f.target.spec.work_dir.join("fixture-discovery-mode"),
            mode,
        )
        .unwrap();
    }
    async fn counts(&self) -> (i64, i64, i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM runtime_version),(SELECT count(*) FROM operation_journal),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM event_sequence_alloc)").fetch_one(&self.db.pool).await.unwrap()
    }
    async fn drained(&self) {
        tokio::time::timeout(Duration::from_secs(15), self.runner.shutdown())
            .await
            .unwrap()
            .unwrap();
        #[cfg(windows)]
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&self.repo.package_exe)
                .is_ok(),
            "el paquete sigue retenido después de terminar"
        );
    }
    async fn retain(&self) {
        let Some(root) = std::env::var_os("LARM_P031_NATIVE_EVIDENCE_DIR") else {
            return;
        };
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = root.join("inventory.sqlite");
        larm_db::backup::backup(&self.db.pool, &snapshot)
            .await
            .unwrap();
        let verification = larm_db::backup::verify_backup(&self.db.pool, &snapshot)
            .await
            .unwrap();
        assert!(verification.is_valid());
        std::fs::copy(
            self.f.target.spec.work_dir.join("requests.jsonl"),
            root.join("requests.jsonl"),
        )
        .unwrap();
        std::fs::copy(&self.repo.package_exe, root.join("fake_runtime_worker.exe")).unwrap();
        std::fs::write(
            root.join("fixture-installation.json"),
            serde_json::to_vec_pretty(&self.f.installation).unwrap(),
        )
        .unwrap();
        let mut files = serde_json::Map::new();
        for name in [
            "inventory.sqlite",
            "requests.jsonl",
            "fake_runtime_worker.exe",
            "fixture-installation.json",
        ] {
            files.insert(
                name.into(),
                json!(hex::encode(Sha256::digest(
                    std::fs::read(root.join(name)).unwrap()
                ))),
            );
        }
        let receipt = json!({"schema_version":"1.0","status":"PASS","scope":"CODE_PASS synthetic qualification; actual Windows worker/P022/SQLite discovery; no engine or managed installation",
            "runtime_id":self.runtime.id,"versions":5,"discovery_commits":2,"rpc_calls":self.f.calls(),"io_drained_and_package_released":true,
            "fixture_worker_sha256":hex::encode(Sha256::digest(std::fs::read(&self.repo.package_exe).unwrap())),"backup_verification":verification,"files_sha256":files});
        std::fs::write(
            root.join("receipt.json"),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn cinco_canales_via_worker_publican_despues_del_cierre_con_guard_hasta_commit() {
    let t = Test::new().await;
    let result = t
        .normal(10)
        .discover(&t.runtime.id, &t.f.context)
        .await
        .unwrap();
    assert_eq!(result.versions.len(), 5);
    assert_eq!(result.page_snapshots.len(), 5);
    assert_eq!(
        result.versions[4].metadata["upstream_channel"]["reason"],
        "UNKNOWN"
    );
    assert!(result
        .versions
        .iter()
        .all(|v| v.promotion == RuntimePromotion::None));
    for version in &result.versions {
        assert_eq!(
            t.repo.sql.version(&version.id).await.unwrap(),
            Some(version.clone())
        );
    }
    assert_eq!(t.repo.commits.load(Ordering::SeqCst), 1);
    let before = t.counts().await;
    let next = t
        .normal(10)
        .discover(&t.runtime.id, &t.f.context)
        .await
        .unwrap();
    assert_eq!(next.versions, result.versions);
    assert_ne!(next.operation_id, result.operation_id);
    let mut expected = before;
    expected.1 += 1;
    assert_eq!(t.counts().await, expected);
    t.drained().await;
    t.retain().await;
}

#[tokio::test]
async fn presupuesto_o_rpc_fallido_no_publican_paginas_previas_y_conservan_causa_al_fallar_cierre()
{
    for vendor in [false, true] {
        let t = Test::new().await;
        if vendor {
            t.mode("fail-nightly");
        }
        let before = t.counts().await;
        let service = t.service(
            if vendor { 10 } else { 2 },
            true,
            false,
            t.f.access(),
            Duration::from_secs(3),
        );
        let error = service
            .discover(&t.runtime.id, &t.f.context)
            .await
            .unwrap_err();
        assert_eq!(
            error.details.reason,
            if vendor {
                "fixture_vendor_failure"
            } else {
                "runtime_discovery_page_budget_exhausted"
            }
        );
        assert!(t.repo.closed.load(Ordering::SeqCst));
        assert_eq!(t.repo.commits.load(Ordering::SeqCst), 0);
        assert!(t.f.calls() >= 3, "no se recorrieron las páginas previas");
        assert_eq!(t.counts().await, before);
        t.drained().await;
    }
}

#[tokio::test]
async fn sesion_ajena_o_cierre_fallido_impiden_publicar_y_siempre_limpian_proceso() {
    for wrong in [false, true] {
        let t = Test::new().await;
        let before = t.counts().await;
        let calls = t.f.calls();
        let error = t
            .service(10, !wrong, wrong, t.f.access(), Duration::from_secs(3))
            .discover(&t.runtime.id, &t.f.context)
            .await
            .unwrap_err();
        assert_eq!(
            error.details.reason,
            if wrong {
                "runtime_discovery_session_mismatch"
            } else {
                "fixture_close_failed"
            }
        );
        if wrong {
            assert_eq!(t.f.calls(), calls);
        }
        assert_eq!(t.repo.commits.load(Ordering::SeqCst), 0);
        assert_eq!(t.counts().await, before);
        assert!(t.repo.closed.load(Ordering::SeqCst));
        t.drained().await;
    }
}

#[tokio::test]
async fn denegacion_access_o_timeout_no_publican_y_el_host_rechaza_timeout_invalido() {
    for case in 0..4 {
        let t = Test::new().await;
        let before = t.counts().await;
        let calls = t.f.calls();
        let mut access = t.f.access();
        let timeout = match case {
            0 => Duration::from_secs(3),
            1 => Duration::from_millis(50),
            2 => Duration::ZERO,
            _ => Duration::from_secs(601),
        };
        if case == 0 {
            let outside = t.f._dir.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            access.writes = vec![outside];
        }
        if case == 1 {
            t.mode("delay");
        }
        let error = t
            .service(10, false, false, access, timeout)
            .discover(&t.runtime.id, &t.f.context)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            match case {
                0 => ErrorCode::Forbidden,
                1 => ErrorCode::Timeout,
                _ => ErrorCode::ValidationFailed,
            }
        );
        if case != 1 {
            assert_eq!(t.f.calls(), calls);
        }
        assert_eq!(t.repo.commits.load(Ordering::SeqCst), 0);
        assert_eq!(t.counts().await, before);
        t.drained().await;
    }
}

#[tokio::test]
async fn fallo_sql_despues_del_cierre_revierte_batch_y_nuevo_descubrimiento_reintenta() {
    let t = Test::new().await;
    let before = t.counts().await;
    sqlx::raw_sql("CREATE TRIGGER fail_discovery BEFORE INSERT ON operation_journal WHEN NEW.kind='runtime.discovery' BEGIN SELECT RAISE(IGNORE); END;").execute(&t.db.pool).await.unwrap();
    let error = t
        .normal(10)
        .discover(&t.runtime.id, &t.f.context)
        .await
        .unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_discovery_journal_unconfirmed"
    );
    assert_eq!(t.repo.commits.load(Ordering::SeqCst), 1);
    assert_eq!(t.counts().await, before);
    sqlx::raw_sql("DROP TRIGGER fail_discovery")
        .execute(&t.db.pool)
        .await
        .unwrap();
    assert_eq!(
        t.normal(10)
            .discover(&t.runtime.id, &t.f.context)
            .await
            .unwrap()
            .versions
            .len(),
        5
    );
    t.drained().await;
}

#[tokio::test]
async fn abandonar_futuro_mata_worker_drena_io_y_no_publica() {
    let t = Test::new().await;
    t.mode("delay");
    let before = t.counts().await;
    let calls = t.f.calls();
    let service = t.normal(10);
    let id = t.runtime.id.clone();
    let ctx = t.f.context.clone();
    let observer = tokio::spawn(async move { service.discover(&id, &ctx).await });
    tokio::time::timeout(Duration::from_secs(15), async {
        while t.f.calls() == calls {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    t.drained().await;
    assert_eq!(t.counts().await, before);
    assert_eq!(t.repo.commits.load(Ordering::SeqCst), 0);
}
