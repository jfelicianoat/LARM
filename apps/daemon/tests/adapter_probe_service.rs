//! Ownership real de aceptación/archivo/SQLite. El ejecutor es CODE_PASS de
//! lifecycle: nunca produce Proof ni valida adaptadores funcionales.
#![allow(clippy::unwrap_used, clippy::panic)]
use async_trait::async_trait;
use larm_adapter_host::conformance::service::ProbeService;
use larm_db::adapter_probes::SqlAdapterProbes;
use larm_domain::{
    adapter_probes::ProbeJob,
    adapters::{AdapterInstallation, AdapterState, AdapterTrust},
    bridge::JobPhase,
    conformance::Receipt,
    error::{ErrorCode, LarmError},
    hardware::Probe,
    ports::{
        AdapterInstallationRepository, AdapterProbeExecutor, AdapterProbeRepository, AdapterProber,
        JobSource, MutationContext,
    },
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    Arc, Mutex,
};
async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn principal(id: &str) -> larm_domain::auth::Principal {
    larm_domain::auth::Principal {
        id: id.into(),
        name: "probe-test".into(),
        kind: larm_domain::auth::PrincipalKind::Operator,
        api_scopes: [larm_domain::auth::ApiScope::Read].into_iter().collect(),
        mcp_scopes: Default::default(),
    }
}
async fn seed(db: &larm_db::pool::Db) -> AdapterInstallation {
    let id = uuid::Uuid::now_v7().to_string();
    let a = AdapterInstallation {
        id: id.clone(),
        created_at: "2026-10-19T00:00:00.000Z".into(),
        updated_at: "2026-10-19T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "fixture.probe".into(),
        version: id.clone(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.probe","version":id,"family":"runtime","package_sha256":"a".repeat(64),"publisher":"fixture","capabilities":[],"permissions":{}}),
    };
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&a, &ctx())
    .await
    .unwrap();
    a
}
struct Hold {
    path: std::path::PathBuf,
    file: Mutex<Option<std::fs::File>>,
    entered: AtomicBool,
    release: AtomicBool,
    release_io: AtomicBool,
    closed: AtomicBool,
    calls: AtomicUsize,
}
impl Hold {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            file: Mutex::new(None),
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
            release_io: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
}
#[async_trait]
impl AdapterProbeExecutor for Hold {
    async fn execute_probe(
        &self,
        _: &ProbeJob,
    ) -> Result<Receipt, larm_domain::adapter_probes::ProbeFailure> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        *self.file.lock().unwrap() = Some(options.open(&self.path).unwrap());
        self.calls.fetch_add(1, SeqCst);
        self.entered.store(true, SeqCst);
        until(|| self.release.load(SeqCst)).await;
        Err(larm_domain::adapter_probes::ProbeFailure::before(
            LarmError::new(
                ErrorCode::Other("private-original".into()),
                "private message",
                "private-reason",
            ),
        ))
    }
    async fn wait_probe_io(&self, _: &str) -> Result<(), LarmError> {
        until(|| self.release_io.load(SeqCst)).await;
        self.file.lock().unwrap().take();
        self.closed.store(true, SeqCst);
        Ok(())
    }
}
struct SlowAccept {
    inner: SqlAdapterProbes,
    entered: AtomicBool,
    release: AtomicBool,
    id: Mutex<Option<String>>,
}
#[async_trait]
impl AdapterProbeRepository for SlowAccept {
    async fn enqueue_probe(
        &self,
        t: &str,
        r: &Probe,
        h: &str,
        k: &str,
        c: &MutationContext,
    ) -> Result<ProbeJob, LarmError> {
        self.entered.store(true, SeqCst);
        until(|| self.release.load(SeqCst)).await;
        let job = self.inner.enqueue_probe(t, r, h, k, c).await?;
        *self.id.lock().unwrap() = Some(job.id.clone());
        Ok(job)
    }
    async fn claim_probe(&self) -> Result<Option<ProbeJob>, LarmError> {
        self.inner.claim_probe().await
    }
    async fn probe_job(&self, id: &str) -> Result<Option<ProbeJob>, LarmError> {
        self.inner.probe_job(id).await
    }
    async fn fail_probe(
        &self,
        id: &str,
        e: &LarmError,
        authority_pending: bool,
    ) -> Result<(), LarmError> {
        self.inner.fail_probe(id, e, authority_pending).await
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desconectar_durante_aceptacion_no_pierde_job_y_shutdown_espera_io_antes_de_fallar() {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let c = ctx();
    let repo = Arc::new(SlowAccept {
        inner: SqlAdapterProbes {
            pool: db.pool.clone(),
        },
        entered: AtomicBool::new(false),
        release: AtomicBool::new(false),
        id: Mutex::new(None),
    });
    let executor = Arc::new(Hold::new(dir.path().join("owned-io.bin")));
    let service = ProbeService::new(repo.clone(), executor.clone());
    service.start().await;
    let request = tokio::spawn({
        let service = service.clone();
        let target = a.id.clone();
        let c = c.clone();
        async move {
            service
                .request_probe(
                    &target,
                    &Probe {
                        resource_id: None,
                        profile_id: None,
                    },
                    &"a".repeat(64),
                    "owned",
                    &c,
                )
                .await
        }
    });
    until(|| repo.entered.load(SeqCst)).await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    repo.release.store(true, SeqCst);
    until(|| executor.entered.load(SeqCst)).await;
    let id = repo.id.lock().unwrap().clone().unwrap();
    let replay = service
        .clone()
        .request_probe(
            &a.id,
            &Probe {
                resource_id: None,
                profile_id: None,
            },
            &"a".repeat(64),
            "owned",
            &c,
        )
        .await
        .unwrap();
    assert_eq!(replay.job_id, id);
    assert_eq!(executor.calls.load(SeqCst), 1);
    assert_eq!(
        service
            .job(&principal(&c.actor_id), &id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobPhase::Running
    );
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&executor.path)
        .is_err());
    service.begin_shutdown().await;
    assert!(service
        .clone()
        .request_probe(
            &a.id,
            &Probe {
                resource_id: None,
                profile_id: None
            },
            &"b".repeat(64),
            "closed",
            &c
        )
        .await
        .is_err());
    let shutdown = tokio::spawn({
        let service = service.clone();
        async move { service.finish_shutdown().await }
    });
    executor.release.store(true, SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!shutdown.is_finished());
    assert_eq!(
        repo.inner.probe_job(&id).await.unwrap().unwrap().phase,
        JobPhase::Running
    );
    assert!(!executor.closed.load(SeqCst));
    executor.release_io.store(true, SeqCst);
    tokio::time::timeout(std::time::Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert!(executor.closed.load(SeqCst));
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&executor.path)
        .is_ok());
    let done = repo.inner.probe_job(&id).await.unwrap().unwrap();
    assert_eq!(done.phase, JobPhase::Failed);
    assert!(!serde_json::to_string(&done).unwrap().contains("private"));
    assert_eq!(
        larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone()
        }
        .get(&a.id)
        .await
        .unwrap()
        .unwrap(),
        a
    );
}
#[tokio::test]
async fn running_de_otro_proceso_permanece_pendiente_sin_repetir_ni_inventar_cierre() {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let c = ctx();
    let repo = Arc::new(SqlAdapterProbes {
        pool: db.pool.clone(),
    });
    let job = repo
        .enqueue_probe(
            &a.id,
            &Probe {
                resource_id: None,
                profile_id: None,
            },
            &"a".repeat(64),
            "previous",
            &c,
        )
        .await
        .unwrap();
    repo.claim_probe().await.unwrap();
    let executor = Arc::new(Hold::new(dir.path().join("must-not-exist")));
    let service = ProbeService::new(repo.clone(), executor.clone());
    service.start().await;
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    let state = service
        .job(&principal(&c.actor_id), &job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.state, JobPhase::Running);
    assert_eq!(state.progress["recovery_required"], true);
    assert!(state.result.is_none() && state.error.is_none());
    assert_eq!(executor.calls.load(SeqCst), 0);
    assert!(!executor.path.exists());
    assert!(service
        .job(&principal("another"), &job.id)
        .await
        .unwrap()
        .is_none());
    service.finish_shutdown().await;
}
