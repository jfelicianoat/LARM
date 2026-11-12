//! Movimiento de modelos (P016) sobre SQLite real y ficheros reales en
//! carpetas temporales: dos «volúmenes» simulados (`vol-a`, `vol-b`) y una
//! segunda raíz en `vol-a`. Los fallos se inyectan en el disco con un
//! `MoveFs` que envuelve al real; las caídas se simulan abortando la tarea
//! del job bloqueada en una operación y arrancando la recuperación.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use larm_db::artifacts::{SqlArtifactLocations, SqlMoveJournal};
use larm_db::guard::SqlResourceGuard;
use larm_db::locations::SqlStorageLocations;
use larm_db::reservations::SqlVolumeReservations;
use larm_domain::artifacts::MoveRequest;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::MarginPolicy;
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{
    ArtifactLocationRepository, MutationContext, StorageLocationRepository, StorageProber,
};
use larm_domain::storage::{
    DriveKind, PerformanceClass, StorageObservation, StorageVolume, VolumeIdentity,
};
use larm_domain::units::DecimalU64;
use larm_storage::estimate::SpacePolicy;
use larm_storage::move_job::{MoveFs, MovePorts, MoveService, StdMoveFs};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

const VARIANT: &str = "0192f000-0000-7000-8000-000000000001";
const ART: &str = "0192f000-0000-7000-8000-0000000000a1";
const LOC: &str = "0192f000-0000-7000-8000-0000000000b1";
const EXT_ART: &str = "0192f000-0000-7000-8000-0000000000a2";
const EXT_LOC: &str = "0192f000-0000-7000-8000-0000000000b2";
const ROOT_A: &str = "0192f000-0000-7000-8000-0000000000c1";
const ROOT_B: &str = "0192f000-0000-7000-8000-0000000000c2";
const ROOT_A2: &str = "0192f000-0000-7000-8000-0000000000c3";
const SIZE: usize = 3 * 1024 * 1024 + 17;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "operador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

/// Prober con dos volúmenes en línea y mucho espacio.
struct TwoVolumes;

#[async_trait]
impl StorageProber for TwoVolumes {
    async fn probe(&self, _unc: &[String]) -> Result<StorageObservation, LarmError> {
        let volume = |key: &str| StorageVolume {
            identity: VolumeIdentity {
                volume_key: Some(key.into()),
                volume_guid: None,
                unc_path: None,
                mount_points: vec![],
                drive_letter: None,
            },
            label: None,
            filesystem: Some("NTFS".into()),
            drive_kind: DriveKind::Fixed,
            bus: None,
            seek_penalty: None,
            performance_class: PerformanceClass::Ssd,
            capacity_bytes: DecimalU64::new(1 << 40),
            free_bytes: DecimalU64::new(1 << 39),
            read_only: false,
            online: true,
            evidence_refs: vec![],
        };
        Ok(StorageObservation {
            volumes: vec![volume("vol-a"), volume("vol-b")],
            unknown: vec![],
            observed_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        })
    }
}

type Hook = Box<dyn FnMut(&str, &str) -> Option<std::io::Error> + Send>;

/// Disco real con un gancho antes de cada operación y corrupción opcional
/// de la copia.
struct FaultFs {
    hook: Mutex<Option<Hook>>,
    corrupt_copy: Mutex<bool>,
}

impl FaultFs {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            hook: Mutex::new(None),
            corrupt_copy: Mutex::new(false),
        })
    }

    fn set(&self, hook: Hook) {
        *self.hook.lock().unwrap() = Some(hook);
    }

    fn clear(&self) {
        *self.hook.lock().unwrap() = None;
    }

    fn before(&self, op: &str, path: &str) -> std::io::Result<()> {
        let mut guard = self.hook.lock().unwrap();
        if let Some(hook) = guard.as_mut() {
            if let Some(e) = hook(op, path) {
                return Err(e);
            }
        }
        Ok(())
    }
}

impl MoveFs for FaultFs {
    fn size(&self, path: &str) -> std::io::Result<Option<u64>> {
        self.before("size", path)?;
        StdMoveFs.size(path)
    }
    fn copy_flush(
        &self,
        from: &str,
        to: &str,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> std::io::Result<(u64, String)> {
        self.before("copy_flush", to)?;
        let out = StdMoveFs.copy_flush(from, to, progress)?;
        if *self.corrupt_copy.lock().unwrap() {
            let mut bytes = std::fs::read(to)?;
            bytes[1000] ^= 0xff;
            std::fs::write(to, bytes)?;
        }
        Ok(out)
    }
    fn sha256(&self, path: &str) -> std::io::Result<String> {
        self.before("sha256", path)?;
        StdMoveFs.sha256(path)
    }
    fn rename_new(&self, from: &str, to: &str) -> std::io::Result<()> {
        self.before("rename_new", to)?;
        StdMoveFs.rename_new(from, to)
    }
    fn remove_file(&self, path: &str) -> std::io::Result<()> {
        self.before("remove_file", path)?;
        StdMoveFs.remove_file(path)
    }
    fn remove_dir_all(&self, path: &str) -> std::io::Result<()> {
        self.before("remove_dir_all", path)?;
        StdMoveFs.remove_dir_all(path)
    }
    fn create_dir_all(&self, path: &str) -> std::io::Result<()> {
        self.before("create_dir_all", path)?;
        StdMoveFs.create_dir_all(path)
    }
    fn open_test(&self, path: &str) -> std::io::Result<u64> {
        self.before("open_test", path)?;
        StdMoveFs.open_test(path)
    }
}

struct Env {
    _dir: tempfile::TempDir,
    _db_dir: tempfile::TempDir,
    pool: SqlitePool,
    fs: Arc<FaultFs>,
    service: Arc<MoveService>,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    a2: std::path::PathBuf,
    content: Vec<u8>,
    sha: String,
}

fn location(id: &str, root: &std::path::Path, volume: &str) -> StorageLocation {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let path = root.display().to_string();
    StorageLocation {
        id: id.into(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        name: id.into(),
        root_path: path.clone(),
        canonical_path: path,
        volume_guid: None,
        drive_letter: None,
        unc_path: None,
        filesystem: "NTFS".into(),
        storage_type: "LOCAL".into(),
        capacity_bytes: DecimalU64::new(1 << 40).unwrap(),
        free_bytes: DecimalU64::new(1 << 39).unwrap(),
        reserved_bytes: DecimalU64::default(),
        priority: 0,
        auto_placement: true,
        state: LocationState::Online,
        last_seen_at: None,
        volume_key: volume.into(),
        performance_class: PerformanceClass::Ssd,
    }
}

async fn seed_sql(pool: &SqlitePool, statements: &[String]) {
    // La cadena completa del registro de modelos es de P040/P041: aquí solo
    // las filas que el movimiento lee, sin comprobar claves ajenas.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    for s in statements {
        sqlx::query(sqlx::AssertSqlSafe(s.clone()))
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
}

fn now() -> String {
    larm_domain::bridge::timestamp(chrono::Utc::now())
}

fn variant_sql(id: &str) -> String {
    let n = now();
    format!(
        "INSERT INTO model_variant (id,created_at,updated_at,row_version,metadata,update_available,status,revision_id,\
         format,quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,\
         capabilities,model_limits) VALUES ('{id}','{n}','{n}',1,'{{}}',0,'AVAILABLE','rev','GGUF','Q4_K_M','llama',NULL,\
         'AVAILABLE','AVAILABLE','LARM_MANAGED','NONE',NULL,'GENERAL','[]','{{}}')"
    )
}

fn artifact_sql(id: &str, variant: &str, name: &str) -> String {
    format!(
        "INSERT INTO model_artifact (id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,\
         expected_size,actual_size,sha256,source_url,required) VALUES ('{id}','{n}','{n}',1,'{{}}','{variant}','{name}',\
         'GGUF',NULL,NULL,NULL,NULL,NULL,1)",
        n = now()
    )
}

fn location_sql(
    id: &str,
    artifact: &str,
    root: &str,
    rel: &str,
    size: usize,
    sha: Option<&str>,
    ownership: &str,
) -> String {
    let sha = sha.map_or_else(|| "NULL".to_string(), |s| format!("'{s}'"));
    format!(
        "INSERT INTO artifact_location (id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,\
         relative_path,file_size,sha256,state,verified_at,ownership) VALUES ('{id}','{n}','{n}',1,'{{}}','{artifact}',\
         '{root}','{rel}',{size},{sha},'ONLINE',NULL,'{ownership}')",
        n = now()
    )
}

async fn env_with(ttl: chrono::Duration) -> Env {
    let (db_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b, a2) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("a2"),
    );
    for d in [&a, &b, &a2] {
        std::fs::create_dir_all(d).unwrap();
    }
    let locations = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    for (id, root, vol) in [
        (ROOT_A, &a, "vol-a"),
        (ROOT_B, &b, "vol-b"),
        (ROOT_A2, &a2, "vol-a"),
    ] {
        locations
            .insert(&location(id, root, vol), &ctx())
            .await
            .unwrap();
    }
    let content: Vec<u8> = (0..SIZE).map(|i| (i * 31 % 251) as u8).collect();
    std::fs::create_dir_all(a.join("m")).unwrap();
    std::fs::write(a.join("m").join("w.gguf"), &content).unwrap();
    let sha = hex::encode(Sha256::digest(&content));
    seed_sql(
        &db.pool,
        &[
            variant_sql(VARIANT),
            artifact_sql(ART, VARIANT, "w.gguf"),
            location_sql(
                LOC,
                ART,
                ROOT_A,
                "m/w.gguf",
                SIZE,
                Some(&sha),
                "LARM_MANAGED",
            ),
        ],
    )
    .await;
    let fs = FaultFs::new();
    let service = Arc::new(MoveService::new(
        MovePorts {
            artifacts: Arc::new(SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            locations: Arc::new(locations),
            journal: Arc::new(SqlMoveJournal {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            reservations: Arc::new(SqlVolumeReservations {
                pool: db.pool.clone(),
            }),
            prober: Arc::new(TwoVolumes),
            fs: fs.clone(),
        },
        SpacePolicy {
            margin: MarginPolicy::default(),
            minimum_free_bytes: 1 << 30,
        },
        ttl,
    ));
    Env {
        _dir: dir,
        _db_dir: db_dir,
        pool: db.pool.clone(),
        fs,
        service,
        a,
        b,
        a2,
        content,
        sha,
    }
}

async fn env() -> Env {
    env_with(chrono::Duration::seconds(900)).await
}

fn request(target: &str, hash: Option<&str>) -> MoveRequest {
    MoveRequest {
        target_storage_location_id: target.into(),
        plan_hash: hash.map(String::from),
        dry_run: hash.is_none(),
        approval_id: None,
    }
}

impl Env {
    fn repo(&self) -> SqlArtifactLocations {
        SqlArtifactLocations {
            pool: self.pool.clone(),
        }
    }

    async fn row(&self) -> larm_domain::artifacts::ArtifactLocation {
        self.repo().get(LOC).await.unwrap().unwrap()
    }

    async fn phase(&self, job: &str) -> String {
        sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = ?")
            .bind(job)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Sin locks ni reservas vivos.
    async fn clean(&self) {
        assert_eq!(
            self.count("SELECT count(*) FROM resource_lock").await,
            0,
            "locks"
        );
        assert_eq!(
            self.count("SELECT count(*) FROM volume_reservation").await,
            0,
            "reservas"
        );
    }

    fn src(&self) -> std::path::PathBuf {
        self.a.join("m").join("w.gguf")
    }

    /// El origen sigue intacto y es el registrado.
    async fn source_authoritative(&self) {
        assert_eq!(
            std::fs::read(self.src()).unwrap(),
            self.content,
            "origen íntegro"
        );
        let row = self.row().await;
        assert_eq!(row.storage_location_id, ROOT_A);
        assert_eq!(row.row_version, 1, "el registro no cambió");
        assert!(!self.b.join("staging").exists(), "sin provisionales");
    }

    async fn moved_to(&self, root: &std::path::Path, location: &str) {
        let dst = root.join("m").join("w.gguf");
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            self.content,
            "destino íntegro"
        );
        assert!(!self.src().exists(), "origen borrado");
        let row = self.row().await;
        assert_eq!(row.storage_location_id, location);
        assert_eq!(row.relative_path, "m/w.gguf");
        assert_eq!(row.sha256.as_deref(), Some(self.sha.as_str()));
        assert!(row.row_version >= 2);
    }

    async fn start(&self, target: &str) -> (String, larm_storage::move_job::MoveTask) {
        let plan = self.service.plan(VARIANT, target, &ctx()).await.unwrap();
        let (plan, task) = self
            .service
            .execute(VARIANT, &request(target, Some(&plan.plan_hash)), &ctx())
            .await
            .unwrap();
        (plan.plan_id, task)
    }
}

#[tokio::test]
async fn mismo_volumen_renombra() {
    let e = env().await;
    let plan = e.service.plan(VARIANT, ROOT_A2, &ctx()).await.unwrap();
    assert_eq!(plan.steps[0].phase, "RENAME");
    assert!(!plan.approval_required);
    assert_eq!(plan.operation, "model.move");
    assert_eq!(
        plan.storage_estimate.peak_extra_bytes.map(DecimalU64::get),
        Some(0),
        "un renombrado no ocupa espacio nuevo"
    );
    let (plan, task) = e
        .service
        .execute(VARIANT, &request(ROOT_A2, Some(&plan.plan_hash)), &ctx())
        .await
        .unwrap();
    task.await;
    assert_eq!(e.phase(&plan.plan_id).await, "COMPLETED");
    e.moved_to(&e.a2, ROOT_A2).await;
    e.clean().await;
    assert_eq!(
        e.count("SELECT count(*) FROM event WHERE event_type = 'artifact.moved'")
            .await,
        1
    );
}

#[tokio::test]
async fn otro_volumen_copia_verifica_y_borra() {
    let e = env().await;
    let plan = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    assert_eq!(plan.steps[0].phase, "COPY");
    assert_eq!(
        plan.storage_estimate.peak_extra_bytes.map(DecimalU64::get),
        Some(SIZE as u64)
    );
    assert!(
        !serde_json::to_string(&plan)
            .unwrap()
            .contains(&e.a.display().to_string()),
        "el plan no expone rutas absolutas"
    );
    let (plan, task) = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .unwrap();
    // Durante el job hay reserva de espacio y locks.
    assert_eq!(e.count("SELECT count(*) FROM volume_reservation").await, 1);
    assert!(e.count("SELECT count(*) FROM resource_lock").await >= 3);
    task.await;
    assert_eq!(e.phase(&plan.plan_id).await, "COMPLETED");
    e.moved_to(&e.b, ROOT_B).await;
    assert!(e.row().await.verified_at.is_some());
    assert!(!e.b.join("staging").exists(), "staging retirado");
    e.clean().await;
    // El plan usado no se puede repetir.
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::StaleRevision, "plan_unknown")
    );
}

#[tokio::test]
async fn hash_distinto_conserva_el_origen() {
    let e = env().await;
    *e.fs.corrupt_copy.lock().unwrap() = true;
    let (job, task) = e.start(ROOT_B).await;
    task.await;
    assert_eq!(e.phase(&job).await, "FAILED");
    let error: serde_json::Value = sqlx::query_scalar::<_, String>(
        "SELECT json_extract(payload, '$.error') FROM operation_journal WHERE id = ?",
    )
    .bind(&job)
    .fetch_one(&e.pool)
    .await
    .map(|s| serde_json::from_str(&s).unwrap())
    .unwrap();
    assert_eq!(error["code"], "INTEGRITY_FAILED");
    assert_eq!(error["details"]["reason"], "hash_mismatch");
    e.source_authoritative().await;
    assert!(!e.b.join("m").join("w.gguf").exists());
    e.clean().await;
}

#[tokio::test]
async fn destino_desaparece_antes_del_commit() {
    let e = env().await;
    let dst = e.b.join("m").join("w.gguf").display().to_string();
    let target = e.b.join("m");
    let mut seen = 0;
    e.fs.set(Box::new(move |op, path| {
        if op == "size" && path == dst {
            seen += 1;
            // 1.ª: plan; 2.ª: ejecución; 3.ª: comprobación previa al cambio.
            if seen == 3 {
                std::fs::remove_dir_all(&target).unwrap();
            }
        }
        None
    }));
    let (job, task) = e.start(ROOT_B).await;
    task.await;
    assert_eq!(e.phase(&job).await, "FAILED");
    e.source_authoritative().await;
    e.clean().await;
}

#[tokio::test]
async fn lease_activa_bloquea_y_al_soltarla_se_mueve() {
    let e = env().await;
    let n = now();
    seed_sql(
        &e.pool,
        &[
            format!(
                "INSERT INTO runtime_instance (id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,\
                 backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,\
                 last_used_at,ownership) VALUES ('inst','{n}','{n}',1,'{{}}','ri','{VARIANT}','be','lp','READY',NULL,NULL,NULL,1,NULL,NULL,'LARM_MANAGED')"
            ),
            format!(
                "INSERT INTO runtime_lease (id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,\
                 generation,state,expires_at,endpoint_id,constraints) VALUES ('lease-1','{n}','{n}',1,'{{}}','broker','r1','inst',1,\
                 'ACTIVE',NULL,NULL,'{{}}')"
            ),
        ],
    )
    .await;
    let plan = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::ActiveLease);
    assert_eq!(err.http_status(), 409);
    assert!(err.details.resource_ids.contains(&"lease-1".to_string()));
    assert_eq!(e.phase(&plan.plan_id).await, "PLANNED", "nada a medias");
    e.source_authoritative().await;
    e.clean().await;
    sqlx::query("UPDATE runtime_lease SET state = 'RELEASED'")
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, task) = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .unwrap();
    task.await;
    e.moved_to(&e.b, ROOT_B).await;
}

#[tokio::test]
async fn bloqueado_por_otra_operacion() {
    use larm_domain::guard::{LockMode, Resource, ResourceKind};
    use larm_domain::ports::ResourceGuard;
    let e = env().await;
    let guard = SqlResourceGuard {
        pool: e.pool.clone(),
    };
    guard
        .acquire(
            "otro-job",
            1,
            &[(Resource::new(ResourceKind::Artifact, ART), LockMode::Shared)],
            None,
        )
        .await
        .unwrap();
    let plan = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::Conflict, "resource_locked")
    );
    assert!(err.retryable);
    e.source_authoritative().await;
}

#[tokio::test]
async fn plan_obsoleto_caducado_o_ausente() {
    let e = env().await;
    // Sin plan_hash.
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, None), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(err.details.reason, "plan_hash_required");
    // Hash inventado.
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&"0".repeat(64))), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(err.details.reason, "plan_unknown");
    // Otro destino con el hash de un plan.
    let plan = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_A2, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(err.details.reason, "plan_mismatch");
    // La fila cambió tras el plan.
    sqlx::query("UPDATE artifact_location SET row_version = 2")
        .execute(&e.pool)
        .await
        .unwrap();
    let err = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::StaleRevision, "plan_changed")
    );
    assert_eq!(e.phase(&plan.plan_id).await, "CANCELLED");
    // Plan caducado.
    let short = env_with(chrono::Duration::milliseconds(1)).await;
    let plan = short.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = short
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(err.details.reason, "plan_expired");
    // Variante y destino desconocidos.
    let err = e
        .service
        .plan("0192f000-0000-7000-8000-00000000ffff", ROOT_B, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = e
        .service
        .plan(VARIANT, "0192f000-0000-7000-8000-00000000fffe", &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    // Origen desaparecido: no se planifica.
    std::fs::remove_file(e.src()).unwrap();
    let err = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap_err();
    assert_eq!(err.details.reason, "artifact_missing");
}

#[tokio::test]
async fn external_y_compartidos_se_conservan() {
    let e = env().await;
    std::fs::write(e.a.join("ext.bin"), b"externo").unwrap();
    seed_sql(
        &e.pool,
        &[
            artifact_sql(EXT_ART, VARIANT, "ext.bin"),
            location_sql(EXT_LOC, EXT_ART, ROOT_A, "ext.bin", 7, None, "EXTERNAL"),
        ],
    )
    .await;
    let plan = e.service.plan(VARIANT, ROOT_B, &ctx()).await.unwrap();
    let keep: Vec<_> = plan.steps.iter().filter(|s| s.phase == "KEEP").collect();
    assert_eq!(keep.len(), 1);
    assert_eq!(keep[0].request["reason"], "external_preserved");
    let (_, task) = e
        .service
        .execute(VARIANT, &request(ROOT_B, Some(&plan.plan_hash)), &ctx())
        .await
        .unwrap();
    task.await;
    e.moved_to(&e.b, ROOT_B).await;
    assert_eq!(std::fs::read(e.a.join("ext.bin")).unwrap(), b"externo");
    let ext = e.repo().get(EXT_LOC).await.unwrap().unwrap();
    assert_eq!(
        (ext.storage_location_id.as_str(), ext.row_version),
        (ROOT_A, 1)
    );
}

#[tokio::test]
async fn acceso_fallido_tras_el_cambio_devuelve_el_registro() {
    let e = env().await;
    e.fs.set(Box::new(|op, _| {
        (op == "open_test").then(|| std::io::Error::other("acceso denegado simulado"))
    }));
    let (job, task) = e.start(ROOT_B).await;
    task.await;
    assert_eq!(e.phase(&job).await, "FAILED");
    // Ambas copias se conservan y el registro vuelve al origen.
    assert_eq!(std::fs::read(e.src()).unwrap(), e.content);
    assert!(e.b.join("m").join("w.gguf").exists());
    let row = e.row().await;
    assert_eq!(row.storage_location_id, ROOT_A);
    assert_eq!(row.row_version, 3, "cambio y vuelta, ambos con CAS");
    e.clean().await;
    assert_eq!(
        e.count("SELECT count(*) FROM event WHERE event_type = 'artifact.moved'")
            .await,
        2
    );
}

/// Caída simulada: el job se bloquea en `op` (la `nth` vez que la ve sobre
/// `path_part`), se aborta su tarea y se arranca la recuperación.
async fn crash_at(e: &Env, op: &'static str, path_part: String, nth: usize) -> String {
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut seen = 0;
    e.fs.set(Box::new(move |o, path| {
        if o == op && path.contains(&path_part) {
            seen += 1;
            if seen == nth {
                reached_tx.send(()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
                return Some(std::io::Error::other("proceso muerto"));
            }
        }
        None
    }));
    let (job, task) = e.start(ROOT_B).await;
    let handle = tokio::spawn(task);
    tokio::task::spawn_blocking(move || reached_rx.recv_timeout(Duration::from_secs(30)).unwrap())
        .await
        .unwrap();
    handle.abort();
    let _ = handle.await;
    // Primero se suelta el hilo bloqueado (tiene el mutex del gancho).
    drop(release_tx);
    e.fs.clear();
    job
}

#[tokio::test]
async fn caida_copiando_recupera_al_origen() {
    let e = env().await;
    let job = crash_at(&e, "copy_flush", "staging".into(), 1).await;
    assert_eq!(e.phase(&job).await, "COPYING");
    assert!(
        e.count("SELECT count(*) FROM resource_lock").await > 0,
        "el lock sobrevive a la caída"
    );
    let report = e.service.recover().await.unwrap();
    assert_eq!(
        report,
        vec![(job.clone(), "rolled_back:FAILED".to_string())]
    );
    e.source_authoritative().await;
    e.clean().await;
}

#[tokio::test]
async fn caida_verificado_retira_la_copia_colocada() {
    let e = env().await;
    let dst = e.b.join("m").join("w.gguf").display().to_string();
    // size(dst): 1.ª plan, 2.ª ejecución, 3.ª comprobación tras VERIFIED.
    let job = crash_at(&e, "size", dst.clone(), 3).await;
    assert_eq!(e.phase(&job).await, "VERIFIED");
    assert!(
        std::path::Path::new(&dst).exists(),
        "la copia ya estaba colocada"
    );
    let report = e.service.recover().await.unwrap();
    assert_eq!(report[0].1, "rolled_back:FAILED");
    e.source_authoritative().await;
    assert!(!std::path::Path::new(&dst).exists(), "copia retirada");
    e.clean().await;
}

#[tokio::test]
async fn caida_tras_cambio_de_registro_termina() {
    let e = env().await;
    let job = crash_at(&e, "open_test", "w.gguf".into(), 1).await;
    assert_eq!(e.phase(&job).await, "DB_SWITCHED");
    assert!(
        e.src().exists(),
        "el origen no se borra antes de probar el acceso"
    );
    let report = e.service.recover().await.unwrap();
    assert_eq!(report[0].1, "completed:COMPLETED");
    e.moved_to(&e.b, ROOT_B).await;
    e.clean().await;
}

#[tokio::test]
async fn caida_tras_cambio_con_destino_perdido_vuelve_al_origen() {
    let e = env().await;
    let job = crash_at(&e, "open_test", "w.gguf".into(), 1).await;
    assert_eq!(e.phase(&job).await, "DB_SWITCHED");
    std::fs::remove_dir_all(e.b.join("m")).unwrap();
    let report = e.service.recover().await.unwrap();
    assert_eq!(report[0].1, "reverted:FAILED");
    assert_eq!(std::fs::read(e.src()).unwrap(), e.content);
    let row = e.row().await;
    assert_eq!(row.storage_location_id, ROOT_A);
    e.clean().await;
}

#[tokio::test]
async fn caida_borrando_el_origen_termina() {
    let e = env().await;
    let src = e.src().display().to_string();
    let job = crash_at(&e, "remove_file", src, 1).await;
    assert_eq!(e.phase(&job).await, "ACCESS_TESTED");
    let report = e.service.recover().await.unwrap();
    assert_eq!(report[0].1, "completed:COMPLETED");
    e.moved_to(&e.b, ROOT_B).await;
    e.clean().await;
}

#[tokio::test]
async fn caida_renombrando_se_deshace() {
    let e = env().await;
    // Caída con el renombrado hecho y antes del cambio de registro: el hook
    // deja pasar el rename y bloquea la comprobación posterior del destino.
    let dst = e.a2.join("m").join("w.gguf").display().to_string();
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut seen = 0;
    let d = dst.clone();
    e.fs.set(Box::new(move |o, path| {
        if o == "size" && path == d {
            seen += 1;
            if seen == 3 {
                reached_tx.send(()).unwrap();
                let _ = release_rx.lock().unwrap().recv();
                return Some(std::io::Error::other("proceso muerto"));
            }
        }
        None
    }));
    let plan = e.service.plan(VARIANT, ROOT_A2, &ctx()).await.unwrap();
    let (plan, task) = e
        .service
        .execute(VARIANT, &request(ROOT_A2, Some(&plan.plan_hash)), &ctx())
        .await
        .unwrap();
    let handle = tokio::spawn(task);
    tokio::task::spawn_blocking(move || reached_rx.recv_timeout(Duration::from_secs(30)).unwrap())
        .await
        .unwrap();
    handle.abort();
    let _ = handle.await;
    // Primero se suelta el hilo bloqueado (tiene el mutex del gancho).
    drop(release_tx);
    e.fs.clear();
    assert!(
        !e.src().exists() && std::path::Path::new(&dst).exists(),
        "renombrado hecho"
    );
    let report = e.service.recover().await.unwrap();
    assert_eq!(
        report,
        vec![(plan.plan_id, "rolled_back:FAILED".to_string())]
    );
    e.source_authoritative().await;
    assert!(!std::path::Path::new(&dst).exists());
    e.clean().await;
}

#[tokio::test]
async fn cambio_de_registro_exige_lock_y_version() {
    use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
    use larm_domain::ports::{Fence, LocationSwitch, ResourceGuard};
    let e = env().await;
    let fence = Fence {
        owner_id: "job-x".into(),
        generation: 1,
        resource: Resource::new(ResourceKind::Variant, VARIANT),
    };
    let switch = |version: i64| LocationSwitch {
        artifact_location_id: LOC.into(),
        expected_row_version: version,
        storage_location_id: ROOT_B.into(),
        relative_path: "m/w.gguf".into(),
        sha256: None,
        file_size: None,
        verified_at: None,
    };
    let step = JournalStep {
        id: "job-x".into(),
        kind: "artifact.move".into(),
        phase: "DB_SWITCHED".into(),
        plan_hash: "0".repeat(64),
        resource_ids: vec![],
        payload: serde_json::json!({}),
        from_phases: vec![],
    };
    // Sin el lock exclusivo: rechazo y nada cambia.
    let err = e
        .repo()
        .switch(&[switch(1)], &fence, &step, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.details.reason, "fencing_lost");
    // Con lock compartido tampoco.
    let guard = SqlResourceGuard {
        pool: e.pool.clone(),
    };
    guard
        .acquire(
            "job-x",
            1,
            &[(fence.resource.clone(), LockMode::Shared)],
            None,
        )
        .await
        .unwrap();
    assert!(e
        .repo()
        .switch(&[switch(1)], &fence, &step, &ctx())
        .await
        .is_err());
    guard.release("job-x", 1, None).await.unwrap();
    guard
        .acquire(
            "job-x",
            1,
            &[(fence.resource.clone(), LockMode::Exclusive)],
            None,
        )
        .await
        .unwrap();
    // Versión vieja: STALE_REVISION, ni fila ni journal ni evento.
    let err = e
        .repo()
        .switch(&[switch(7)], &fence, &step, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleRevision);
    assert_eq!(
        e.count("SELECT count(*) FROM operation_journal WHERE id = 'job-x'")
            .await,
        0
    );
    assert_eq!(
        e.count("SELECT count(*) FROM event WHERE event_type = 'artifact.moved'")
            .await,
        0
    );
    e.source_authoritative().await;
    // Con lock y versión: cambia, con journal y evento en la misma transacción.
    assert_eq!(
        e.repo()
            .switch(&[switch(1)], &fence, &step, &ctx())
            .await
            .unwrap(),
        vec![2]
    );
    assert_eq!(e.phase("job-x").await, "DB_SWITCHED");
    assert_eq!(
        e.count("SELECT count(*) FROM event WHERE event_type = 'artifact.moved'")
            .await,
        1
    );
}

#[tokio::test]
async fn sin_lock_no_se_borra_el_origen() {
    let e = env().await;
    let job = crash_at(&e, "open_test", "w.gguf".into(), 1).await;
    assert_eq!(e.phase(&job).await, "DB_SWITCHED");
    // Otro proceso retiró el lock (p. ej. una reconciliación indebida).
    sqlx::query("DELETE FROM resource_lock")
        .execute(&e.pool)
        .await
        .unwrap();
    e.service.recover().await.unwrap();
    assert_eq!(
        e.phase(&job).await,
        "ACCESS_TESTED",
        "se detiene antes de la escritura irreversible"
    );
    assert_eq!(
        std::fs::read(e.src()).unwrap(),
        e.content,
        "el origen sigue"
    );
    assert!(e.b.join("m").join("w.gguf").exists());
}
