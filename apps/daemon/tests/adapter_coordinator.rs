//! P024: coordinador real de FS + SQLite, sin ejecutar los workers fixture.
//! Los recibos sintéticos del origen sólo prueban persistencia, no workers.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use larm_adapter_host::package::{ArchivePlan, PackageControl};
use larm_adapter_host::updates::{UpdateCoordinator, UpdatePorts};
use larm_adapter_sdk::manifest::{AdapterManifest, Family, MethodTable};
use larm_adapter_sdk::permissions::{
    adapter_resource, elevation_digest, package_hash, PermissionDelta, TrustStore,
};
use larm_db::adapter_selection::SqlAdapterSelections;
use larm_db::adapter_updates::SqlAdapterUpdates;
use larm_db::adapters::SqlAdapterInstallations;
use larm_domain::adapter_updates::{AdapterImport, AdapterUpdate, UpdatePlan, UpdateStep as Step};
use larm_domain::adapters::{
    AdapterInstallation, AdapterSelectionChange, AdapterState, AdapterTrust,
};
use larm_domain::bridge::{timestamp, JobReceipt};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterInstallationRepository, AdapterPackageSpace, AdapterSelectionRepository,
    AdapterUpdateRepository, AdapterUpdater, ApprovalRepository, MutationContext, ResourceGuard,
    VolumeDemand, VolumeReservationRepository,
};
use larm_domain::units::DecimalU64;
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
mod persistence;

const WORKER: &[u8] = b"MZ fixture, never executed";
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "fixture-actor".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn manifest(version: &str) -> AdapterManifest {
    let table = MethodTable::contract();
    let mut methods = table.mandatory_common.clone();
    methods.extend(
        table
            .required_for(Family::Runtime)
            .iter()
            .map(|s| s.to_string()),
    );
    let hash = hex::encode(Sha256::digest(
        format!("bin/worker.exe\0{}\n", hex::encode(Sha256::digest(WORKER))).as_bytes(),
    ));
    serde_json::from_value(json!({"id":"acme.coordinator-fixture","version":version,"family":"runtime","protocol_version":"1.0",
        "publisher":"Fixture","executable":{"relative_path":"bin/worker.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],
        "methods":methods,"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
        "capabilities":["runtime.gguf"],"package_sha256":hash,"signature":null,"installation_strategies":[]})).unwrap()
}
fn zip(path: &Path, m: &AdapterManifest) -> AdapterImport {
    let mut z = zip::ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("manifest.json", options).unwrap();
    z.write_all(serde_json::to_string(m).unwrap().as_bytes())
        .unwrap();
    z.start_file("bin/worker.exe", options).unwrap();
    z.write_all(WORKER).unwrap();
    z.finish().unwrap();
    AdapterImport {
        package_path: path.to_string_lossy().into_owned(),
        expected_hash: hex::encode(Sha256::digest(std::fs::read(path).unwrap())),
        approval_id: None,
    }
}

#[derive(Default)]
struct Space {
    change_source: Mutex<Option<std::path::PathBuf>>,
    barrier: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    short: bool,
}
#[async_trait]
impl AdapterPackageSpace for Space {
    async fn measure_package_space(
        &self,
        _root: &str,
        required: u64,
    ) -> Result<(VolumeDemand, chrono::DateTime<chrono::Utc>), LarmError> {
        if let Some((entered, resume)) = &self.barrier {
            entered.notify_one();
            resume.notified().await;
        }
        if let Some(path) = self.change_source.lock().unwrap().take() {
            std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(b"changed")
                .unwrap();
        }
        Ok((
            VolumeDemand {
                volume_key: "fixture-volume".into(),
                required_bytes: required,
                os_free: if self.short { 0 } else { 100 << 30 },
                policy_minimum: 0,
            },
            chrono::Utc::now(),
        ))
    }
}
struct Reservations {
    inner: larm_db::reservations::SqlVolumeReservations,
    writes: Mutex<Vec<u64>>,
    cancel: Mutex<Option<PackageControl>>,
    lose_fence: AtomicU8,
}
#[async_trait]
impl VolumeReservationRepository for Reservations {
    async fn others(&self, v: &str, j: Option<&str>) -> Result<u64, LarmError> {
        self.inner.others(v, j).await
    }
    async fn admit(
        &self,
        j: &str,
        d: &[VolumeDemand],
        t: chrono::DateTime<chrono::Utc>,
        e: Option<String>,
    ) -> Result<(), LarmError> {
        self.inner.admit(j, d, t, e).await
    }
    async fn consume(&self, j: &str, v: &str, n: u64) -> Result<(), LarmError> {
        self.inner.consume(j, v, n).await?;
        self.writes.lock().unwrap().push(n);
        if let Some(c) = self.cancel.lock().unwrap().take() {
            c.cancel();
        }
        if self.lose_fence.swap(0, Ordering::AcqRel) != 0 {
            sqlx::query("DELETE FROM resource_lock WHERE owner_id=?")
                .bind(j)
                .execute(&self.inner.pool)
                .await
                .unwrap();
        }
        Ok(())
    }
    async fn release(&self, j: &str) -> Result<u64, LarmError> {
        self.inner.release(j).await
    }
}
struct Jobs {
    inner: SqlAdapterUpdates,
    commit_fault: AtomicU8,
    acceptance_barrier: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}
#[async_trait]
impl AdapterUpdateRepository for Jobs {
    async fn enqueue_update(
        &self,
        t: &str,
        r: &AdapterImport,
        h: &str,
        k: &str,
        c: &MutationContext,
    ) -> Result<(JobReceipt, bool), LarmError> {
        let pause = self.acceptance_barrier.lock().unwrap().take();
        if let Some((entered, resume)) = pause {
            entered.notify_one();
            resume.notified().await;
        }
        self.inner.enqueue_update(t, r, h, k, c).await
    }
    async fn update_job(&self, id: &str) -> Result<Option<AdapterUpdate>, LarmError> {
        self.inner.update_job(id).await
    }
    async fn pending_updates(&self) -> Result<Vec<AdapterUpdate>, LarmError> {
        self.inner.pending_updates().await
    }
    async fn advance_update(&self, id: &str, f: Step, t: Step) -> Result<(), LarmError> {
        self.inner.advance_update(id, f, t).await
    }
    async fn prepare_update(
        &self,
        id: &str,
        p: &UpdatePlan,
        d: &VolumeDemand,
        t: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LarmError> {
        self.inner.prepare_update(id, p, d, t).await
    }
    async fn complete_update(&self, id: &str) -> Result<(), LarmError> {
        let fault = self.commit_fault.swap(0, Ordering::AcqRel);
        if fault == 1 {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "Injected before COMMIT",
                "fixture_before_commit",
            ));
        }
        self.inner.complete_update(id).await?;
        if fault == 2 {
            return Err(LarmError::new(
                ErrorCode::Internal,
                "Injected lost COMMIT ack",
                "fixture_commit_ack_lost",
            ));
        }
        Ok(())
    }
    async fn fail_update(&self, id: &str, e: &LarmError) -> Result<(), LarmError> {
        self.inner.fail_update(id, e).await
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    db: larm_db::pool::Db,
    root: std::path::PathBuf,
    jobs: Arc<Jobs>,
    repo: Arc<SqlAdapterInstallations>,
    reservations: Arc<Reservations>,
    space: Arc<Space>,
    target: AdapterInstallation,
    request: AdapterImport,
}
impl Fixture {
    async fn new(space: Space) -> Self {
        let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let root = dir.path().join("adapters");
        let old = manifest("1.0");
        std::fs::create_dir_all(root.join(&old.id).join(&old.version).join("bin")).unwrap();
        std::fs::write(
            root.join(&old.id).join(&old.version).join("bin/worker.exe"),
            WORKER,
        )
        .unwrap();
        std::fs::write(
            root.join(&old.id).join(&old.version).join("manifest.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let now = timestamp(chrono::Utc::now());
        let target = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: json!({"fixture_prevalidated":true})
                .as_object()
                .cloned()
                .unwrap(),
            adapter_id: old.id.clone(),
            version: old.version.clone(),
            family: "runtime".into(),
            package_hash: old.package_sha256.clone(),
            publisher: old.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            manifest: serde_json::to_value(old).unwrap(),
            state: AdapterState::Installed,
        };
        let repo = Arc::new(SqlAdapterInstallations {
            pool: db.pool.clone(),
        });
        repo.insert(&target, &ctx()).await.unwrap();
        let target = persistence::qualify_fixture(&db.pool, &target, &ctx()).await;
        let selections = SqlAdapterSelections {
            pool: db.pool.clone(),
            qualification: persistence::physical_gate_fixture(),
        };
        selections
            .change_selection(
                &target.adapter_id,
                0,
                &AdapterSelectionChange::Promote {
                    installation_id: target.id.clone(),
                },
                &ctx(),
            )
            .await
            .unwrap();
        selections
            .change_selection(
                &target.adapter_id,
                1,
                &AdapterSelectionChange::SetEnabled { enabled: true },
                &ctx(),
            )
            .await
            .unwrap();
        let request = zip(&dir.path().join("package.zip"), &manifest("2.0"));
        let jobs = Arc::new(Jobs {
            inner: SqlAdapterUpdates {
                pool: db.pool.clone(),
            },
            commit_fault: AtomicU8::new(0),
            acceptance_barrier: Mutex::new(None),
        });
        let reservations = Arc::new(Reservations {
            inner: larm_db::reservations::SqlVolumeReservations {
                pool: db.pool.clone(),
            },
            writes: Mutex::new(vec![]),
            cancel: Mutex::new(None),
            lose_fence: AtomicU8::new(0),
        });
        Self {
            _dir: dir,
            db,
            root,
            jobs,
            repo,
            reservations,
            space: Arc::new(space),
            target,
            request,
        }
    }
    fn coordinator(&self) -> Arc<UpdateCoordinator> {
        UpdateCoordinator::open(self.root.clone(), self.ports(), TrustStore::default()).unwrap()
    }
    fn ports(&self) -> UpdatePorts {
        UpdatePorts {
            jobs: self.jobs.clone(),
            installations: self.repo.clone(),
            approvals: Arc::new(larm_db::security::SqlApprovals {
                pool: self.db.pool.clone(),
            }),
            reservations: self.reservations.clone(),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: self.db.pool.clone(),
            }),
            space: self.space.clone(),
        }
    }
    async fn enqueue(&self, key: &str) -> String {
        self.jobs
            .enqueue_update(
                &self.target.id,
                &self.request,
                &self.request.expected_hash,
                key,
                &ctx(),
            )
            .await
            .unwrap()
            .0
            .job_id
    }
    async fn state(&self, id: &str) -> AdapterUpdate {
        self.jobs.update_job(id).await.unwrap().unwrap()
    }
    async fn resources(&self) -> (i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)",
        )
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }
    async fn old_unchanged(&self) {
        assert_eq!(
            self.repo.get(&self.target.id).await.unwrap(),
            Some(self.target.clone())
        );
        let s = SqlAdapterSelections {
            pool: self.db.pool.clone(),
            qualification: persistence::physical_gate_fixture(),
        }
        .selection(&self.target.adapter_id)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(s.active_installation_id, Some(self.target.id.clone()));
        assert_eq!(s.previous_installation_id, None);
        assert!(s.enabled);
        assert_eq!(s.row_version, 2);
        assert_eq!(
            std::fs::read(
                self.root
                    .join(&self.target.adapter_id)
                    .join("1.0/bin/worker.exe")
            )
            .unwrap(),
            WORKER
        );
    }
    fn no_workspace(&self, id: &str) {
        assert!(!self.root.join(".staging").join(id).exists());
    }
}

async fn interrupted_plan(f: &Fixture, c: &UpdateCoordinator, id: &str, step: Step) -> UpdatePlan {
    f.jobs
        .advance_update(id, Step::Queued, Step::Inspecting)
        .await
        .unwrap();
    let archive = ArchivePlan::inspect(
        Path::new(&f.request.package_path),
        &f.request.expected_hash,
        &PackageControl::default(),
    )
    .unwrap();
    let mut candidate = f.target.clone();
    candidate.id = uuid::Uuid::now_v7().to_string();
    candidate.row_version = 1;
    candidate.version = "2.0".into();
    candidate.state = AdapterState::Installed;
    candidate.metadata = serde_json::Map::new();
    candidate.manifest = serde_json::to_value(archive.manifest).unwrap();
    let p = UpdatePlan {
        candidate,
        workspace_path: c
            .root()
            .join(".staging")
            .join(id)
            .to_string_lossy()
            .into_owned(),
        destination_path: c
            .root()
            .join(&f.target.adapter_id)
            .join("2.0")
            .to_string_lossy()
            .into_owned(),
        volume_key: "fixture-volume".into(),
        archive_bytes: DecimalU64::new(archive.archive_bytes).unwrap(),
        unpacked_bytes: DecimalU64::new(archive.unpacked_bytes).unwrap(),
        required_bytes: DecimalU64::new(archive.archive_bytes + archive.unpacked_bytes + (1 << 30))
            .unwrap(),
        approval: None,
    };
    let d = VolumeDemand {
        volume_key: p.volume_key.clone(),
        required_bytes: p.required_bytes.get(),
        os_free: 100 << 30,
        policy_minimum: 0,
    };
    f.jobs
        .prepare_update(id, &p, &d, chrono::Utc::now())
        .await
        .unwrap();
    for (from, to) in [
        (Step::Reserved, Step::Extracting),
        (Step::Extracting, Step::Staged),
        (Step::Staged, Step::Publishing),
    ] {
        if from == step {
            break;
        }
        f.jobs.advance_update(id, from, to).await.unwrap();
    }
    std::fs::create_dir_all(&p.workspace_path).unwrap();
    std::fs::write(
        Path::new(&p.workspace_path).join("partial.fixture"),
        b"partial",
    )
    .unwrap();
    p
}

#[tokio::test]
async fn recuperacion_de_cada_paso_pre_publicacion_borra_solo_el_workspace_propio() {
    for step in [
        Step::Reserved,
        Step::Extracting,
        Step::Staged,
        Step::Publishing,
    ] {
        let f = Fixture::new(Space::default()).await;
        let c = f.coordinator();
        let id = f.enqueue("interrupted").await;
        let p = interrupted_plan(&f, &c, &id, step).await;
        let foreign = c.root().join(".staging/another-workflow.partial");
        std::fs::write(&foreign, b"keep").unwrap();
        drop(c);
        assert_eq!(f.coordinator().recover().await.unwrap(), 1);
        assert_eq!(f.state(&id).await.step, Step::Failed);
        assert_eq!(
            f.state(&id).await.error.unwrap().details.reason,
            "adapter_update_interrupted"
        );
        assert_eq!(std::fs::read(foreign).unwrap(), b"keep");
        assert!(!Path::new(&p.destination_path).exists());
        assert_eq!(f.resources().await, (0, 0));
        f.no_workspace(&id);
        f.old_unchanged().await;
    }
}

#[tokio::test]
async fn recuperacion_rechaza_bytes_corruptos_y_preserva_copia_para_inspeccion() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("corrupt").await;
    f.jobs.commit_fault.store(1, Ordering::Release);
    assert!(c
        .clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .is_err());
    let p = f.state(&id).await.plan.unwrap();
    let worker = Path::new(&p.destination_path).join("bin/worker.exe");
    std::fs::write(&worker, b"corrupt").unwrap();
    drop(c);
    assert_eq!(f.coordinator().recover().await.unwrap(), 1);
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Failed);
    assert_eq!(r.error.unwrap().code, ErrorCode::IntegrityFailed);
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_none());
    assert_eq!(std::fs::read(worker).unwrap(), b"corrupt");
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    f.old_unchanged().await;
}

#[tokio::test]
async fn firma_desconocida_se_rechaza_y_clave_fijada_instala_verified_sin_promover() {
    let mut f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let mut m = manifest("2.0");
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    m.signature = Some(larm_adapter_sdk::permissions::sign(&m, "fixture-key", &key).unwrap());
    f.request = zip(Path::new(&f.request.package_path), &m);
    let id = f.enqueue("unknown-key").await;
    assert_eq!(
        c.clone()
            .execute(id.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .details
            .reason,
        "signature_invalid"
    );
    assert!(f.reservations.writes.lock().unwrap().is_empty());
    assert_eq!(f.resources().await, (0, 0));
    drop(c);
    let trust = TrustStore {
        keys: vec![larm_adapter_sdk::permissions::PinnedKey {
            key_id: "fixture-key".into(),
            publisher: m.publisher.clone(),
            public_key: hex::encode(key.verifying_key().to_bytes()),
            official: false,
            revoked: false,
        }],
        revoked_packages: vec![],
    };
    let c = UpdateCoordinator::open(f.root.clone(), f.ports(), trust).unwrap();
    let id = f.enqueue("pinned-key").await;
    c.execute(id.clone(), PackageControl::default())
        .await
        .unwrap();
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Completed);
    assert_eq!(r.plan.unwrap().candidate.trust, AdapterTrust::Verified);
    f.old_unchanged().await;
}

#[tokio::test]
async fn revocacion_en_recuperacion_no_registra_y_conserva_bytes_para_inspeccion() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("revoked-on-recovery").await;
    f.jobs.commit_fault.store(1, Ordering::Release);
    assert!(c
        .clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .is_err());
    let p = f.state(&id).await.plan.unwrap();
    drop(c);
    let trust = TrustStore {
        keys: vec![],
        revoked_packages: vec![p.candidate.package_hash.clone()],
    };
    let c = UpdateCoordinator::open(f.root.clone(), f.ports(), trust).unwrap();
    assert_eq!(c.recover().await.unwrap(), 1);
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Failed);
    assert_eq!(r.error.unwrap().details.reason, "revoked");
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_none());
    assert!(Path::new(&p.destination_path).exists());
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    f.old_unchanged().await;
}

#[tokio::test]
async fn recuperacion_respeta_el_lock_fisico_de_version_y_reintenta_sin_perder_reserva() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("version-lock").await;
    f.jobs.commit_fault.store(1, Ordering::Release);
    assert!(c
        .clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .is_err());
    let p = f.state(&id).await.plan.unwrap();
    let manifest: AdapterManifest = serde_json::from_value(p.candidate.manifest.clone()).unwrap();
    let (lock, _) =
        larm_adapter_host::package::lock_version(c.root(), &manifest, &PackageControl::default())
            .unwrap();
    drop(c);
    let c = f.coordinator();
    assert_eq!(
        c.clone().recover().await.unwrap_err().details.reason,
        "adapter_install_busy"
    );
    assert_eq!(f.state(&id).await.step, Step::Publishing);
    assert_eq!(f.resources().await, (1, 2));
    drop(lock);
    assert_eq!(c.recover().await.unwrap(), 1);
    assert_eq!(f.state(&id).await.step, Step::Completed);
    assert_eq!(f.resources().await, (0, 0));
}

#[tokio::test]
async fn instalacion_real_side_by_side_reserva_bytes_y_conserva_default() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("success").await;
    let plan = ArchivePlan::inspect(
        Path::new(&f.request.package_path),
        &f.request.expected_hash,
        &PackageControl::default(),
    )
    .unwrap();
    c.clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .unwrap();
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Completed);
    let p = r.plan.unwrap();
    assert_eq!(p.candidate.state, AdapterState::Installed);
    assert_eq!(p.candidate.trust, AdapterTrust::Untrusted);
    let registered = f.repo.get(&p.candidate.id).await.unwrap().unwrap();
    assert_eq!(registered, p.candidate);
    assert_eq!(
        package_hash(Path::new(&p.destination_path), &["manifest.json"]).unwrap(),
        p.candidate.package_hash
    );
    assert_eq!(
        f.reservations.writes.lock().unwrap().last().copied(),
        Some(plan.archive_bytes + plan.unpacked_bytes)
    );
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    f.old_unchanged().await;
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM event e JOIN event_outbox o ON e.event_id=o.event_id WHERE e.resource_id=?")
        .bind(&p.candidate.id).fetch_one(&f.db.pool).await.unwrap();
    assert_eq!(count, 1);
    let same = f.enqueue("same-version").await;
    assert_eq!(
        c.execute(same.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_version_exists"
    );
    assert_eq!(f.state(&same).await.step, Step::Failed);
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
}

#[tokio::test]
async fn cancelacion_y_falta_de_espacio_no_publican_ni_liberan_antes_de_parar_io() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("cancel").await;
    let control = PackageControl::default();
    *f.reservations.cancel.lock().unwrap() = Some(control.clone());
    assert_eq!(
        c.clone()
            .execute(id.clone(), control)
            .await
            .unwrap_err()
            .details
            .reason,
        "operation_cancelled"
    );
    assert_eq!(f.state(&id).await.step, Step::Failed);
    f.no_workspace(&id);
    assert_eq!(f.resources().await, (0, 0));
    assert!(!f.root.join(&f.target.adapter_id).join("2.0").exists());
    f.old_unchanged().await;
    let f = Fixture::new(Space {
        short: true,
        ..Space::default()
    })
    .await;
    let c = f.coordinator();
    let id = f.enqueue("space").await;
    assert_eq!(
        c.execute(id.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageInsufficient
    );
    assert_eq!(f.state(&id).await.step, Step::Failed);
    assert_eq!(f.resources().await, (0, 0));
    assert!(f.reservations.writes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn archivo_cambiado_y_timeout_se_rechazan_con_limpieza() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("changed").await;
    *f.space.change_source.lock().unwrap() = Some(f.request.package_path.clone().into());
    assert_eq!(
        c.clone()
            .execute(id.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .details
            .reason,
        "archive_changed"
    );
    assert_eq!(f.state(&id).await.step, Step::Failed);
    f.no_workspace(&id);
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
    let id = f.enqueue("timeout").await;
    assert_eq!(
        c.execute(
            id.clone(),
            PackageControl::with_timeout(std::time::Duration::ZERO)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Timeout
    );
    assert_eq!(f.state(&id).await.step, Step::Failed);
    assert_eq!(f.resources().await, (0, 0));
}

#[tokio::test]
async fn commit_ambiguo_conserva_bytes_y_recuperacion_termina_el_mismo_job() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("ambiguous").await;
    f.jobs.commit_fault.store(1, Ordering::Release);
    assert!(c
        .clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .is_err());
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Publishing);
    let p = r.plan.unwrap();
    assert!(Path::new(&p.destination_path)
        .join("manifest.json")
        .exists());
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_none());
    assert_eq!(f.resources().await, (1, 2));
    f.old_unchanged().await;
    drop(c);
    let c = f.coordinator();
    assert_eq!(c.clone().recover().await.unwrap(), 1);
    assert_eq!(f.state(&id).await.step, Step::Completed);
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_some());
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    f.old_unchanged().await;
}

#[tokio::test]
async fn ack_perdido_despues_de_commit_nunca_borra_paquete_registrado() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("lost-ack").await;
    f.jobs.commit_fault.store(2, Ordering::Release);
    c.clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .unwrap();
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Completed);
    let p = r.plan.unwrap();
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_some());
    assert!(Path::new(&p.destination_path)
        .join("bin/worker.exe")
        .exists());
    assert_eq!(f.resources().await, (0, 0));
    drop(c);
    assert_eq!(f.coordinator().recover().await.unwrap(), 0);
    f.no_workspace(&id);
    f.old_unchanged().await;
}

#[tokio::test]
async fn perdida_de_fence_impide_escribir_mas_y_conserva_reserva_hasta_reconciliar() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("fence").await;
    f.reservations.lose_fence.store(1, Ordering::Release);
    assert_eq!(
        c.clone()
            .execute(id.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .details
            .reason,
        "fence_lost"
    );
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Extracting);
    let p = r.plan.unwrap();
    assert_eq!(f.resources().await, (1, 0));
    assert!(!Path::new(&p.destination_path).exists());
    larm_db::guard::SqlResourceGuard {
        pool: f.db.pool.clone(),
    }
    .acquire(&id, 1, &p.locks(), None)
    .await
    .unwrap();
    assert_eq!(c.recover().await.unwrap(), 1);
    assert_eq!(f.state(&id).await.step, Step::Failed);
    f.no_workspace(&id);
    assert_eq!(f.resources().await, (0, 0));
}

#[tokio::test]
async fn permisos_elevados_exigen_aprobacion_exacta_y_se_consumen_antes_de_copiar() {
    let mut f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let mut m = manifest("2.0");
    m.permissions.network_origins = vec!["https://fixture.example".into()];
    f.request = zip(Path::new(&f.request.package_path), &m);
    let id = f.enqueue("no-approval").await;
    assert_eq!(
        c.clone()
            .execute(id.clone(), PackageControl::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ApprovalRequired
    );
    assert_eq!(f.resources().await, (0, 0));
    assert!(f.reservations.writes.lock().unwrap().is_empty());
    let old: AdapterManifest = serde_json::from_value(f.target.manifest.clone()).unwrap();
    let request = larm_domain::approval::ApprovalRequest {
        operation_digest: elevation_digest(
            &m,
            &PermissionDelta::between(&old.permissions, &m.permissions),
        )
        .unwrap(),
        resource_ids: vec![adapter_resource(&m)],
        code_hash: Some(m.package_sha256.clone()),
        permissions: serde_json::Map::new(),
        approved: true,
    };
    let approval = request
        .grant(
            uuid::Uuid::now_v7().to_string(),
            "fixture-actor",
            chrono::Utc::now(),
            chrono::Duration::seconds(900),
        )
        .unwrap();
    let approvals = larm_db::security::SqlApprovals {
        pool: f.db.pool.clone(),
    };
    approvals.insert(&approval, &ctx()).await.unwrap();
    f.request.approval_id = Some(approval.id.clone());
    let id = f.enqueue("approved").await;
    c.execute(id.clone(), PackageControl::default())
        .await
        .unwrap();
    assert_eq!(f.state(&id).await.step, Step::Completed);
    assert!(approvals
        .get(&approval.id)
        .await
        .unwrap()
        .unwrap()
        .consumed_at
        .is_some());
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
}

#[tokio::test]
async fn cancelar_observador_no_suelta_lock_mientras_siga_el_hilo_de_io() {
    let (entered, resume) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let f = Fixture::new(Space {
        barrier: Some((entered.clone(), resume.clone())),
        ..Space::default()
    })
    .await;
    let c = f.coordinator();
    let id = f.enqueue("observer").await;
    let task = tokio::spawn(c.clone().execute(id.clone(), PackageControl::default()));
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(c);
    let second = UpdateCoordinator::open(f.root.clone(), f.ports(), TrustStore::default());
    assert_eq!(
        second.err().unwrap().details.reason,
        "adapter_coordinator_busy"
    );
    assert_eq!(f.state(&id).await.step, Step::Inspecting);
    resume.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if f.state(&id).await.step == Step::Failed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.resources().await, (0, 0));
    assert!(!f.root.join(&f.target.adapter_id).join("2.0").exists());
}

#[tokio::test]
async fn recuperacion_de_target_modificado_falla_sin_dejar_job_eternamente_running() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let id = f.enqueue("stale-target").await;
    f.jobs.commit_fault.store(1, Ordering::Release);
    assert!(c
        .clone()
        .execute(id.clone(), PackageControl::default())
        .await
        .is_err());
    let p = f.state(&id).await.plan.unwrap();
    sqlx::query("UPDATE adapter_installation SET row_version=row_version+1 WHERE id=?")
        .bind(&f.target.id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    drop(c);
    assert_eq!(f.coordinator().recover().await.unwrap(), 1);
    let r = f.state(&id).await;
    assert_eq!(r.step, Step::Failed);
    assert_eq!(r.error.unwrap().code, ErrorCode::StaleRevision);
    assert!(f.repo.get(&p.candidate.id).await.unwrap().is_none());
    assert!(Path::new(&p.destination_path).exists());
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    let version: i64 = sqlx::query_scalar(
        "SELECT resource_version FROM event WHERE resource_id=? ORDER BY event_id DESC LIMIT 1",
    )
    .bind(&f.target.id)
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(version, f.target.row_version + 1);
}

async fn wait_terminal(f: &Fixture, id: &str) -> Step {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let step = f.state(id).await.step;
            if matches!(step, Step::Completed | Step::Failed) {
                return step;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn aceptacion_sobrevive_a_cancelar_el_request_y_worker_queda_supervisado() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let service = larm_adapter_host::update_service::UpdateService::new(c, f.jobs.clone());
    service.start().await;
    let (entered, resume) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    *f.jobs.acceptance_barrier.lock().unwrap() = Some((entered.clone(), resume.clone()));
    let (target, request, svc) = (f.target.id.clone(), f.request.clone(), service.clone());
    let request_task = tokio::spawn(async move {
        svc.request_update(
            &target,
            &request,
            &request.expected_hash,
            "disconnected",
            &ctx(),
        )
        .await
    });
    entered.notified().await;
    request_task.abort();
    assert!(request_task.await.unwrap_err().is_cancelled());
    resume.notify_one();
    let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some((id,)) = sqlx::query_as::<_, (String,)>(
                "SELECT id FROM operation_journal WHERE kind='adapter.update'",
            )
            .fetch_optional(&f.db.pool)
            .await
            .unwrap()
            {
                return id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(wait_terminal(&f, &id).await, Step::Completed);
    service.finish_shutdown().await;
    assert_eq!(f.resources().await, (0, 0));
    f.no_workspace(&id);
    f.old_unchanged().await;
}

#[tokio::test]
async fn shutdown_no_reabre_admision_y_espera_al_io_antes_de_terminar() {
    let (entered, resume) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let f = Fixture::new(Space {
        barrier: Some((entered.clone(), resume.clone())),
        ..Space::default()
    })
    .await;
    let service =
        larm_adapter_host::update_service::UpdateService::new(f.coordinator(), f.jobs.clone());
    service.start().await;
    let receipt = service
        .clone()
        .request_update(
            &f.target.id,
            &f.request,
            &f.request.expected_hash,
            "active",
            &ctx(),
        )
        .await
        .unwrap();
    entered.notified().await;
    service.begin_shutdown().await;
    let closing = service.clone();
    let done = tokio::spawn(async move {
        closing.finish_shutdown().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!done.is_finished());
    assert_eq!(f.state(&receipt.job_id).await.step, Step::Inspecting);
    assert_eq!(
        service
            .clone()
            .request_update(
                &f.target.id,
                &f.request,
                &f.request.expected_hash,
                "closed",
                &ctx()
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_service_closing"
    );
    resume.notify_one();
    done.await.unwrap();
    assert_eq!(f.state(&receipt.job_id).await.step, Step::Failed);
    service.start().await;
    assert!(service
        .clone()
        .request_update(
            &f.target.id,
            &f.request,
            &f.request.expected_hash,
            "still-closed",
            &ctx()
        )
        .await
        .is_err());
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
}

#[tokio::test]
async fn cola_durable_no_crea_hilos_por_request_y_reanuda_queued_tras_shutdown() {
    let (entered, resume) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let mut f = Fixture::new(Space {
        barrier: Some((entered.clone(), resume.clone())),
        ..Space::default()
    })
    .await;
    let service =
        larm_adapter_host::update_service::UpdateService::new(f.coordinator(), f.jobs.clone());
    service.start().await;
    let first = service
        .clone()
        .request_update(
            &f.target.id,
            &f.request,
            &f.request.expected_hash,
            "first",
            &ctx(),
        )
        .await
        .unwrap();
    entered.notified().await;
    let req3 = zip(&f._dir.path().join("version3.zip"), &manifest("3.0"));
    let second = service
        .clone()
        .request_update(&f.target.id, &req3, &req3.expected_hash, "second", &ctx())
        .await
        .unwrap();
    assert_eq!(f.state(&second.job_id).await.step, Step::Queued);
    service.begin_shutdown().await;
    resume.notify_one();
    service.finish_shutdown().await;
    assert_eq!(f.state(&first.job_id).await.step, Step::Failed);
    assert_eq!(f.state(&second.job_id).await.step, Step::Queued);
    drop(service);
    f.space = Arc::new(Space::default());
    let c = f.coordinator();
    assert_eq!(c.clone().recover_started().await.unwrap(), 0);
    assert_eq!(f.state(&second.job_id).await.step, Step::Queued);
    let service = larm_adapter_host::update_service::UpdateService::new(c, f.jobs.clone());
    service.start().await;
    assert_eq!(wait_terminal(&f, &second.job_id).await, Step::Completed);
    service.finish_shutdown().await;
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
}

#[tokio::test]
async fn recuperar_queued_invalido_no_bloquea_los_otros_trabajos_ni_el_arranque() {
    let f = Fixture::new(Space::default()).await;
    let c = f.coordinator();
    let mut bad = f.request.clone();
    bad.package_path = f
        ._dir
        .path()
        .join("missing.zip")
        .to_string_lossy()
        .into_owned();
    let first = f
        .jobs
        .enqueue_update(&f.target.id, &bad, &bad.expected_hash, "bad-queued", &ctx())
        .await
        .unwrap()
        .0;
    let good = f.enqueue("good-queued").await;
    assert_eq!(c.recover().await.unwrap(), 2);
    assert_eq!(f.state(&first.job_id).await.step, Step::Failed);
    assert_eq!(f.state(&good).await.step, Step::Completed);
    assert_eq!(f.resources().await, (0, 0));
    f.old_unchanged().await;
}
