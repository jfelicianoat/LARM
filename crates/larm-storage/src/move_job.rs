//! Movimiento seguro de una variante entre ubicaciones (P016; spec §37–§38,
//! contrato 02).
//!
//! 1. **Plan** (`dry_run`): decide cada artefacto, el espacio y el hash; se
//!    guarda `PLANNED` en el journal con caducidad.
//! 2. **Ejecución** con ese `plan_hash`: se recalcula el plan (debe dar el
//!    mismo hash), se toman los locks del guard con el paso `LOCKED` en la
//!    misma transacción (una lease activa sobre la variante devuelve
//!    `ACTIVE_LEASE` 409 desde ese mismo lock) y se reserva el espacio.
//! 3. **Job**: `COPYING` (renombrado en el mismo volumen; copia a
//!    `staging/<job>/<artifact>.partial` con flush en otro), `VERIFIED` (hash
//!    releído de la copia), `DB_SWITCHED` (CAS por `row_version` con fencing),
//!    `ACCESS_TESTED` (se abre el destino resuelto desde el registro nuevo),
//!    `SOURCE_DELETED` (solo tras comprobar el fencing) y `COMPLETED`.
//!
//! Un fallo antes del cambio de registro deshace: el origen sigue siendo el
//! válido y se limpia lo provisional. Después, se conservan ambas copias y,
//! si el destino no se abre, el registro vuelve al origen. Tras una caída,
//! [`MoveService::recover`] aplica la misma regla desde el journal.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use larm_domain::artifacts::{
    join, resolve_root, MoveAction, MoveItem, MovePhase, MoveRequest, Plan, MOVE_JOB_KIND,
    MOVE_OPERATION, STAGING_DIR,
};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{timestamp, JobPhase, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{estimate, StorageEstimate, VolumeSnapshot};
use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
use larm_domain::locations::StorageLocation;
use larm_domain::ports::{
    ArtifactLocationRepository, Fence, JobSource, LocationSwitch, MoveJournal, MoveRecord,
    MutationContext, ResourceGuard, StorageLocationRepository, StorageProber,
    VolumeReservationRepository,
};
use larm_domain::storage::StorageObservation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifacts::{lock_set, plan_hash, plan_items, plan_steps, space_phases};
use crate::estimate::{admit, snapshots, SpacePolicy};

/// Operaciones de disco del movimiento (síncronas; se ejecutan en
/// `spawn_blocking`). Rutas absolutas ya resueltas.
pub trait MoveFs: Send + Sync + 'static {
    /// Tamaño del fichero, `None` si no existe.
    ///
    /// # Errors
    /// Error de E/S distinto de «no existe».
    fn size(&self, path: &str) -> std::io::Result<Option<u64>>;
    /// Copia a un fichero nuevo (falla si existe), con flush a disco.
    /// Devuelve los bytes y el SHA-256 de lo leído del origen.
    ///
    /// # Errors
    /// Error de E/S.
    fn copy_flush(
        &self,
        from: &str,
        to: &str,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> std::io::Result<(u64, String)>;
    /// SHA-256 releído del fichero.
    ///
    /// # Errors
    /// Error de E/S.
    fn sha256(&self, path: &str) -> std::io::Result<String>;
    /// Renombra sin sobrescribir (falla si el destino existe).
    ///
    /// # Errors
    /// Error de E/S o destino existente.
    fn rename_new(&self, from: &str, to: &str) -> std::io::Result<()>;
    /// # Errors
    /// Error de E/S.
    fn remove_file(&self, path: &str) -> std::io::Result<()>;
    /// # Errors
    /// Error de E/S.
    fn remove_dir_all(&self, path: &str) -> std::io::Result<()>;
    /// # Errors
    /// Error de E/S.
    fn create_dir_all(&self, path: &str) -> std::io::Result<()>;
    /// Abre el fichero y lee su principio y su final; devuelve el tamaño.
    ///
    /// # Errors
    /// Error de E/S.
    fn open_test(&self, path: &str) -> std::io::Result<u64>;
}

/// Disco real.
pub struct StdMoveFs;

const BUFFER: usize = 8 << 20;

fn hash_reader(mut r: impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl MoveFs for StdMoveFs {
    fn size(&self, path: &str) -> std::io::Result<Option<u64>> {
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(_) => Err(std::io::Error::other("no es un fichero")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn copy_flush(
        &self,
        from: &str,
        to: &str,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> std::io::Result<(u64, String)> {
        let mut src = std::fs::File::open(from)?;
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; BUFFER];
        let mut total = 0u64;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            dst.write_all(&buf[..n])?;
            total += n as u64;
            progress(total);
        }
        dst.flush()?;
        dst.sync_all()?;
        Ok((total, hex::encode(hasher.finalize())))
    }

    fn sha256(&self, path: &str) -> std::io::Result<String> {
        hash_reader(std::fs::File::open(path)?)
    }

    fn rename_new(&self, from: &str, to: &str) -> std::io::Result<()> {
        // `std::fs::rename` sobrescribe en Windows: se comprueba antes, con
        // los locks exclusivos del movimiento tomados.
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "el destino ya existe",
            ));
        }
        std::fs::rename(from, to)
    }

    fn remove_file(&self, path: &str) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir_all(&self, path: &str) -> std::io::Result<()> {
        std::fs::remove_dir_all(path)
    }

    fn create_dir_all(&self, path: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn open_test(&self, path: &str) -> std::io::Result<u64> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        let mut buf = vec![0u8; 64 << 10];
        let _ = f.read(&mut buf)?;
        if len > buf.len() as u64 {
            f.seek(SeekFrom::End(-(buf.len() as i64)))?;
            f.read_exact(&mut buf)?;
        }
        Ok(len)
    }
}

/// Puertos del servicio.
#[derive(Clone)]
pub struct MovePorts {
    pub artifacts: Arc<dyn ArtifactLocationRepository>,
    pub locations: Arc<dyn StorageLocationRepository>,
    pub journal: Arc<dyn MoveJournal>,
    pub guard: Arc<dyn ResourceGuard>,
    pub reservations: Arc<dyn VolumeReservationRepository>,
    pub prober: Arc<dyn StorageProber>,
    pub fs: Arc<dyn MoveFs>,
}

/// Servicio de movimiento: plan, ejecución, job, recuperación y estado.
pub struct MoveService {
    pub ports: MovePorts,
    pub policy: SpacePolicy,
    /// Validez de un plan (`security.approval_ttl_seconds`).
    pub plan_ttl: chrono::Duration,
    progress: Arc<Mutex<BTreeMap<String, u64>>>,
}

/// Tarea del job, para lanzarla con `tokio::spawn`.
pub type MoveTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Payload del journal: ids, nunca rutas absolutas.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Payload {
    actor_id: String,
    correlation_id: String,
    resource_id: String,
    target_storage_location_id: String,
    expires_at: String,
    plan: Option<Plan>,
    items: Vec<MoveItem>,
    generation: i64,
    #[serde(default)]
    renaming: Vec<String>,
    #[serde(default)]
    renamed: Vec<String>,
    #[serde(default)]
    placed: Vec<String>,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
    #[serde(default)]
    switched: BTreeMap<String, i64>,
    #[serde(default)]
    bytes_total: u64,
    #[serde(default)]
    bytes_copied: u64,
    #[serde(default)]
    leftovers: Vec<String>,
    /// El destino ya se abrió desde el registro nuevo: a partir de aquí
    /// nunca se vuelve atrás (puede haber orígenes ya borrados).
    #[serde(default)]
    access_tested: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<LarmError>,
}

impl Payload {
    fn ctx(&self) -> MutationContext {
        MutationContext {
            actor_id: self.actor_id.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }

    fn movable(&self) -> impl Iterator<Item = &MoveItem> {
        self.items.iter().filter(|i| i.action != MoveAction::Keep)
    }
}

/// Identidad del job en el journal.
#[derive(Debug, Clone)]
struct Job {
    id: String,
    plan_hash: String,
    resource_ids: Vec<String>,
}

const LIVE: [MovePhase; 6] = [
    MovePhase::Locked,
    MovePhase::Copying,
    MovePhase::Verified,
    MovePhase::DbSwitched,
    MovePhase::AccessTested,
    MovePhase::SourceDeleted,
];

fn io_err(what: &str, e: &std::io::Error) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo acceder a un fichero del movimiento",
        if e.kind() == std::io::ErrorKind::NotFound {
            "io_not_found"
        } else {
            "io_failed"
        },
    )
    .with_technical(format!("{what}: {e}"))
    .retryable(true)
}

fn internal(reason: &str, detail: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::Internal, "Error interno del movimiento", reason)
        .with_technical(detail.into())
}

fn parent(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map_or_else(String::new, |p| p.display().to_string())
}

/// Lo que el planificador necesita del mundo en un momento dado.
struct World {
    observation: StorageObservation,
    locations: Vec<StorageLocation>,
}

impl World {
    fn location(&self, id: &str) -> Result<&StorageLocation, LarmError> {
        self.locations.iter().find(|l| l.id == id).ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Ubicación desconocida", "not_found")
                .with_resource(id.to_string())
        })
    }

    fn root(&self, id: &str) -> Result<String, LarmError> {
        let l = self.location(id)?;
        resolve_root(l, &self.observation.volumes).map_err(|_| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "El volumen de la ubicación no está conectado",
                "volume_offline",
            )
            .with_resource(id.to_string())
            .retryable(true)
        })
    }

    fn path(&self, location_id: &str, relative: &str) -> Result<String, LarmError> {
        Ok(join(&self.root(location_id)?, relative))
    }
}

/// Plan calculado en un momento dado.
struct Built {
    items: Vec<MoveItem>,
    hash: String,
    estimate: StorageEstimate,
    volumes: Vec<VolumeSnapshot>,
    snapshot_at: chrono::DateTime<chrono::Utc>,
    locks: Vec<(Resource, LockMode)>,
}

impl MoveService {
    #[must_use]
    pub fn new(ports: MovePorts, policy: SpacePolicy, plan_ttl: chrono::Duration) -> Self {
        Self {
            ports,
            policy,
            plan_ttl,
            progress: Arc::default(),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn MoveFs) -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let fs = self.ports.fs.clone();
        tokio::task::spawn_blocking(move || f(fs.as_ref()))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    async fn world(&self) -> Result<World, LarmError> {
        let locations = self.ports.locations.all().await?;
        let mut unc: Vec<String> = locations
            .iter()
            .filter_map(|l| l.unc_path.clone())
            .collect();
        unc.sort();
        unc.dedup();
        let observation = self.ports.prober.probe(&unc).await?;
        Ok(World {
            observation,
            locations,
        })
    }

    async fn build(
        &self,
        variant_id: &str,
        target_id: &str,
        world: &World,
        exclude_job: Option<&str>,
    ) -> Result<Built, LarmError> {
        let artifacts = self
            .ports
            .artifacts
            .for_variant(variant_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
                    .with_resource(variant_id.to_string())
            })?;
        let target = world.location(target_id)?;
        let mut items = plan_items(target, &artifacts, &world.locations)?;
        // El disco manda: el origen debe existir con el tamaño registrado y
        // el destino no puede estar ocupado.
        for item in items.iter_mut().filter(|i| i.action != MoveAction::Keep) {
            let src = world.path(&item.from_storage_location_id, &item.relative_path)?;
            let dst = world.path(&item.to_storage_location_id, &item.relative_path)?;
            let (s, d) = (src.clone(), dst.clone());
            let (on_disk, occupied) = self
                .blocking(move |fs| Ok((fs.size(&s)?, fs.size(&d)?.is_some())))
                .await
                .map_err(|e| io_err("comprobando origen y destino", &e))?;
            let Some(on_disk) = on_disk else {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "Un artefacto del modelo no está en disco",
                    "artifact_missing",
                )
                .with_resource(item.artifact_location_id.clone()));
            };
            match item.file_size {
                Some(size) if size.get() != on_disk => {
                    return Err(LarmError::new(
                        ErrorCode::IntegrityFailed,
                        "El tamaño en disco no coincide con el registrado",
                        "size_mismatch",
                    )
                    .with_resource(item.artifact_location_id.clone()));
                }
                Some(_) => {}
                None => item.file_size = larm_domain::units::DecimalU64::new(on_disk),
            }
            if occupied {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya hay un fichero en el destino",
                    "destination_exists",
                )
                .with_resource(item.artifact_location_id.clone()));
            }
        }
        let hash = plan_hash(variant_id, target, &items)?;
        let snapshot_at = chrono::DateTime::parse_from_rfc3339(&world.observation.observed_at)
            .map_or_else(|_| chrono::Utc::now(), |d| d.with_timezone(&chrono::Utc));
        let volumes = snapshots(
            &world.observation,
            &world.locations,
            self.ports.reservations.as_ref(),
            self.policy,
            exclude_job,
        )
        .await?;
        let estimate = estimate(
            &space_phases(target, &items),
            &volumes,
            self.policy.margin,
            None,
        )
        .map_err(|e| internal("estimate", format!("{e:?}")))?;
        let locks = lock_set(variant_id, target, &items, &world.locations);
        Ok(Built {
            items,
            hash,
            estimate,
            volumes,
            snapshot_at,
            locks,
        })
    }

    fn step(job: &Job, phase: MovePhase, from: &[MovePhase], p: &Payload) -> JournalStep {
        JournalStep {
            id: job.id.clone(),
            kind: MOVE_JOB_KIND.into(),
            phase: phase.as_str().into(),
            plan_hash: job.plan_hash.clone(),
            resource_ids: job.resource_ids.clone(),
            payload: serde_json::to_value(p).unwrap_or_default(),
            from_phases: from.iter().map(|f| f.as_str().to_string()).collect(),
        }
    }

    async fn write(
        &self,
        job: &Job,
        phase: MovePhase,
        from: &[MovePhase],
        p: &Payload,
    ) -> Result<(), LarmError> {
        self.ports
            .journal
            .write(&Self::step(job, phase, from, p))
            .await
    }

    /// Plan sin efectos (`dry_run`): se guarda `PLANNED` con caducidad.
    ///
    /// # Errors
    /// Variante o ubicación desconocidas, plan imposible o fallo de E/S.
    pub async fn plan(
        &self,
        variant_id: &str,
        target_id: &str,
        ctx: &MutationContext,
    ) -> Result<Plan, LarmError> {
        let world = self.world().await?;
        let built = self.build(variant_id, target_id, &world, None).await?;
        let plan_id = uuid::Uuid::now_v7().to_string();
        let expires_at = timestamp(chrono::Utc::now() + self.plan_ttl);
        let plan = Plan {
            plan_id: plan_id.clone(),
            plan_hash: built.hash.clone(),
            operation: MOVE_OPERATION.into(),
            resource_ids: built.locks.iter().map(|(r, _)| r.key()).collect(),
            storage_estimate: built.estimate,
            steps: plan_steps(&built.items),
            expires_at: expires_at.clone(),
            approval_required: false,
        };
        let payload = Payload {
            actor_id: ctx.actor_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            resource_id: variant_id.into(),
            target_storage_location_id: target_id.into(),
            expires_at,
            plan: Some(plan.clone()),
            items: built.items,
            ..Payload::default()
        };
        let job = Job {
            id: plan_id,
            plan_hash: plan.plan_hash.clone(),
            resource_ids: plan.resource_ids.clone(),
        };
        self.write(&job, MovePhase::Planned, &[], &payload).await?;
        Ok(plan)
    }

    /// Ejecuta un plan `PLANNED`: lo recalcula, toma los locks (con la
    /// comprobación de lease en la misma transacción) y reserva el espacio.
    /// Devuelve el plan y la tarea del job.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` sin `plan_hash`; `STALE_REVISION` si el plan no
    /// existe, caducó o cambió; `ACTIVE_LEASE`/`CONFLICT` del guard;
    /// `STORAGE_INSUFFICIENT` de la reserva.
    pub async fn execute(
        self: &Arc<Self>,
        variant_id: &str,
        request: &MoveRequest,
        ctx: &MutationContext,
    ) -> Result<(Plan, MoveTask), LarmError> {
        let hash = request.plan_hash.as_deref().ok_or_else(|| {
            LarmError::new(
                ErrorCode::ValidationFailed,
                "Para mover hace falta el plan_hash de un dry_run",
                "plan_hash_required",
            )
        })?;
        let stale = |reason: &str, message: &str| {
            LarmError::new(ErrorCode::StaleRevision, message, reason).retryable(false)
        };
        let record = self
            .ports
            .journal
            .find_planned(hash)
            .await?
            .ok_or_else(|| {
                stale(
                    "plan_unknown",
                    "Plan desconocido, ya usado o cancelado: vuelve a planificar",
                )
            })?;
        let mut payload: Payload = serde_json::from_value(record.payload.clone())
            .map_err(|e| internal("payload", e.to_string()))?;
        let job = Job {
            id: record.id.clone(),
            plan_hash: record.plan_hash.clone(),
            resource_ids: record.resource_ids.clone(),
        };
        if payload.resource_id != variant_id
            || payload.target_storage_location_id != request.target_storage_location_id
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El plan es de otra variante u otro destino",
                "plan_mismatch",
            ));
        }
        let expired = chrono::DateTime::parse_from_rfc3339(&payload.expires_at).map_or(true, |e| {
            e.with_timezone(&chrono::Utc) <= chrono::Utc::now()
        });
        if expired {
            self.write(&job, MovePhase::Cancelled, &[MovePhase::Planned], &payload)
                .await?;
            return Err(stale("plan_expired", "El plan caducó: vuelve a planificar"));
        }
        let world = self.world().await?;
        let built = self
            .build(
                variant_id,
                &request.target_storage_location_id,
                &world,
                Some(&job.id),
            )
            .await?;
        if built.hash != hash {
            self.write(&job, MovePhase::Cancelled, &[MovePhase::Planned], &payload)
                .await?;
            return Err(stale(
                "plan_changed",
                "El modelo o el destino cambiaron desde el plan: vuelve a planificar",
            ));
        }
        let copies = built.items.iter().any(|i| i.action == MoveAction::Copy);
        if copies && !built.estimate.per_volume.iter().all(|v| v.eligible) {
            return Err(LarmError::new(
                ErrorCode::StorageInsufficient,
                "No hay espacio suficiente en el destino",
                "insufficient_space",
            ));
        }
        payload.items = built.items.clone();
        payload.generation = 1;
        payload.correlation_id = ctx.correlation_id.clone();
        payload.bytes_total = built
            .items
            .iter()
            .filter(|i| i.action == MoveAction::Copy)
            .filter_map(|i| i.file_size.map(larm_domain::units::DecimalU64::get))
            .sum();
        // Locks y LOCKED en una transacción; la lease se mira bajo ese lock.
        self.ports
            .guard
            .acquire(
                &job.id,
                payload.generation,
                &built.locks,
                Some(&Self::step(
                    &job,
                    MovePhase::Locked,
                    &[MovePhase::Planned],
                    &payload,
                )),
            )
            .await?;
        // Bajo el lock exclusivo, las filas siguen como en el plan.
        let again = self
            .ports
            .artifacts
            .for_variant(variant_id)
            .await?
            .unwrap_or_default();
        let unchanged = built.items.iter().all(|i| {
            again.iter().any(|a| {
                a.location.id == i.artifact_location_id && a.location.row_version == i.row_version
            })
        });
        if !unchanged {
            let e = stale("plan_changed", "El modelo cambió mientras se bloqueaba");
            self.abort_locked(&job, &mut payload, e.clone()).await;
            return Err(e);
        }
        if copies {
            if let Err(e) = admit(
                &job.id,
                &built.estimate,
                &built.volumes,
                built.snapshot_at,
                self.ports.reservations.as_ref(),
            )
            .await
            {
                self.abort_locked(&job, &mut payload, e.clone()).await;
                return Err(e);
            }
        }
        let mut plan = payload
            .plan
            .clone()
            .ok_or_else(|| internal("payload", "plan ausente"))?;
        plan.storage_estimate = built.estimate;
        let service = Arc::clone(self);
        let id = job.id.clone();
        Ok((plan, Box::pin(async move { service.run(&id).await })))
    }

    async fn abort_locked(&self, job: &Job, p: &mut Payload, error: LarmError) {
        p.error = Some(error);
        let step = Self::step(job, MovePhase::Failed, &LIVE, p);
        if let Err(e) = self
            .ports
            .guard
            .release(&job.id, p.generation, Some(&step))
            .await
        {
            tracing_fallback("no se pudieron soltar los locks del movimiento", &e);
        }
        let _ = self.ports.reservations.release(&job.id).await;
    }

    async fn load(&self, id: &str) -> Result<(Job, MovePhase, Payload), LarmError> {
        let record: MoveRecord = self
            .ports
            .journal
            .get(id)
            .await?
            .ok_or_else(|| internal("journal", format!("job {id} desaparecido")))?;
        let phase = MovePhase::parse(&record.phase).map_err(|e| internal("journal", e))?;
        let payload: Payload = serde_json::from_value(record.payload)
            .map_err(|e| internal("payload", e.to_string()))?;
        Ok((
            Job {
                id: record.id,
                plan_hash: record.plan_hash,
                resource_ids: record.resource_ids,
            },
            phase,
            payload,
        ))
    }

    /// Ejecuta el job hasta el final o hasta un fallo tratado.
    pub async fn run(&self, id: &str) {
        let (job, _, mut p) = match self.load(id).await {
            Ok(x) => x,
            Err(e) => {
                tracing_fallback("no se pudo leer el job de movimiento", &e);
                return;
            }
        };
        if let Err(e) = self.forward(&job, &mut p).await {
            self.fail(&job, &mut p, e).await;
        }
        if let Ok(mut map) = self.progress.lock() {
            map.remove(id);
        }
    }

    async fn forward(&self, job: &Job, p: &mut Payload) -> Result<(), LarmError> {
        let world = self.world().await?;
        self.write(job, MovePhase::Copying, &[MovePhase::Locked], p)
            .await?;
        let movable: Vec<MoveItem> = p.movable().cloned().collect();
        for item in &movable {
            let src = world.path(&item.from_storage_location_id, &item.relative_path)?;
            let dst = world.path(&item.to_storage_location_id, &item.relative_path)?;
            match item.action {
                MoveAction::Rename => {
                    p.renaming.push(item.artifact_location_id.clone());
                    self.write(job, MovePhase::Copying, &[MovePhase::Copying], p)
                        .await?;
                    let (s, d) = (src.clone(), dst.clone());
                    self.blocking(move |fs| {
                        fs.create_dir_all(&parent(&d))?;
                        fs.rename_new(&s, &d)
                    })
                    .await
                    .map_err(|e| io_err("renombrando", &e))?;
                    p.renamed.push(item.artifact_location_id.clone());
                }
                MoveAction::Copy => {
                    let staging = self.staging_file(&world, job, item)?;
                    let (s, t) = (src.clone(), staging.clone());
                    let progress = self.progress.clone();
                    let (key, base) = (job.id.clone(), p.bytes_copied);
                    let report: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
                        if let Ok(mut map) = progress.lock() {
                            map.insert(key.clone(), base + n);
                        }
                    });
                    let (bytes, source_hash) = self
                        .blocking(move |fs| {
                            fs.create_dir_all(&parent(&t))?;
                            fs.copy_flush(&s, &t, report.as_ref())
                        })
                        .await
                        .map_err(|e| io_err("copiando", &e))?;
                    if item.sha256.as_deref().is_some_and(|h| h != source_hash) {
                        return Err(LarmError::new(
                            ErrorCode::IntegrityFailed,
                            "El origen no coincide con el hash registrado",
                            "source_hash_mismatch",
                        )
                        .with_resource(item.artifact_location_id.clone()));
                    }
                    p.hashes
                        .insert(item.artifact_location_id.clone(), source_hash);
                    p.bytes_copied += bytes;
                }
                MoveAction::Keep => {}
            }
            self.write(job, MovePhase::Copying, &[MovePhase::Copying], p)
                .await?;
        }
        // VERIFIED: hash releído de cada copia y colocación sin sobrescribir.
        for item in &movable {
            let dst = world.path(&item.to_storage_location_id, &item.relative_path)?;
            let expected = item.file_size.map(larm_domain::units::DecimalU64::get);
            if item.action == MoveAction::Rename {
                let d = dst.clone();
                let size = self
                    .blocking(move |fs| fs.size(&d))
                    .await
                    .map_err(|e| io_err("verificando el renombrado", &e))?;
                if size != expected {
                    return Err(LarmError::new(
                        ErrorCode::IntegrityFailed,
                        "El fichero renombrado no tiene el tamaño esperado",
                        "size_mismatch",
                    )
                    .with_resource(item.artifact_location_id.clone()));
                }
            }
            if item.action == MoveAction::Copy {
                let staging = self.staging_file(&world, job, item)?;
                let t = staging.clone();
                let (copied_hash, size) = self
                    .blocking(move |fs| Ok((fs.sha256(&t)?, fs.size(&t)?)))
                    .await
                    .map_err(|e| io_err("verificando la copia", &e))?;
                if Some(&copied_hash) != p.hashes.get(&item.artifact_location_id)
                    || size != expected
                {
                    return Err(LarmError::new(
                        ErrorCode::IntegrityFailed,
                        "La copia no coincide con el origen",
                        "hash_mismatch",
                    )
                    .with_resource(item.artifact_location_id.clone()));
                }
                p.placed.push(item.artifact_location_id.clone());
                self.write(job, MovePhase::Copying, &[MovePhase::Copying], p)
                    .await?;
                let d = dst.clone();
                self.blocking(move |fs| {
                    fs.create_dir_all(&parent(&d))?;
                    fs.rename_new(&staging, &d)
                })
                .await
                .map_err(|e| io_err("colocando la copia", &e))?;
            }
        }
        self.write(job, MovePhase::Verified, &[MovePhase::Copying], p)
            .await?;
        // Si el destino desaparece antes del cambio de registro, el origen
        // sigue siendo el válido.
        for item in &movable {
            let dst = world.path(&item.to_storage_location_id, &item.relative_path)?;
            let expected = item.file_size.map(larm_domain::units::DecimalU64::get);
            let present = self.blocking(move |fs| fs.size(&dst)).await.unwrap_or(None);
            if present.is_none() || present != expected {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El destino desapareció antes de cambiar el registro",
                    "destination_missing",
                )
                .with_resource(item.artifact_location_id.clone())
                .retryable(true));
            }
        }
        // DB_SWITCHED: CAS + fencing + journal + eventos en una transacción.
        let now = timestamp(chrono::Utc::now());
        let switches: Vec<LocationSwitch> = movable
            .iter()
            .map(|i| LocationSwitch {
                artifact_location_id: i.artifact_location_id.clone(),
                expected_row_version: i.row_version,
                storage_location_id: i.to_storage_location_id.clone(),
                relative_path: i.relative_path.clone(),
                sha256: p.hashes.get(&i.artifact_location_id).cloned(),
                file_size: i.file_size.map(larm_domain::units::DecimalU64::get),
                verified_at: p
                    .hashes
                    .contains_key(&i.artifact_location_id)
                    .then(|| now.clone()),
            })
            .collect();
        for i in &movable {
            p.switched
                .insert(i.artifact_location_id.clone(), i.row_version + 1);
        }
        let fence = self.fence(job, p);
        let step = Self::step(job, MovePhase::DbSwitched, &[MovePhase::Verified], p);
        if let Err(e) = self
            .ports
            .artifacts
            .switch(&switches, &fence, &step, &p.ctx())
            .await
        {
            p.switched.clear();
            return Err(e);
        }
        self.after_switch(job, p).await
    }

    fn fence(&self, job: &Job, p: &Payload) -> Fence {
        Fence {
            owner_id: job.id.clone(),
            generation: p.generation,
            resource: Resource::new(ResourceKind::Variant, p.resource_id.clone()),
        }
    }

    fn staging_file(&self, world: &World, job: &Job, item: &MoveItem) -> Result<String, LarmError> {
        Ok(join(
            &world.root(&item.to_storage_location_id)?,
            &format!("{STAGING_DIR}/{}/{}.partial", job.id, item.artifact_id),
        ))
    }

    /// ACCESS_TESTED → SOURCE_DELETED → COMPLETED (también desde la recuperación).
    async fn after_switch(&self, job: &Job, p: &mut Payload) -> Result<(), LarmError> {
        let world = self.world().await?;
        let movable: Vec<MoveItem> = p.movable().cloned().collect();
        // El destino se resuelve desde el registro nuevo, no desde la ruta usada al copiar.
        for item in &movable {
            let row = self
                .ports
                .artifacts
                .get(&item.artifact_location_id)
                .await?
                .ok_or_else(|| internal("artifact_location", "fila desaparecida"))?;
            let path = world.path(&row.storage_location_id, &row.relative_path)?;
            let size = self
                .blocking(move |fs| fs.open_test(&path))
                .await
                .map_err(|e| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "El destino no se pudo abrir tras cambiar el registro",
                        "access_test_failed",
                    )
                    .with_resource(item.artifact_location_id.clone())
                    .with_technical(e.to_string())
                })?;
            if Some(size) != item.file_size.map(larm_domain::units::DecimalU64::get) {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El destino no tiene el tamaño esperado",
                    "access_test_failed",
                )
                .with_resource(item.artifact_location_id.clone()));
            }
        }
        p.access_tested = true;
        self.write(
            job,
            MovePhase::AccessTested,
            &[MovePhase::DbSwitched, MovePhase::AccessTested],
            p,
        )
        .await?;
        self.delete_sources(job, p, &world).await?;
        self.complete(job, p).await
    }

    async fn delete_sources(
        &self,
        job: &Job,
        p: &mut Payload,
        world: &World,
    ) -> Result<(), LarmError> {
        // Fencing antes de la escritura irreversible.
        let still = self
            .ports
            .guard
            .holds(
                &job.id,
                p.generation,
                &Resource::new(ResourceKind::Variant, p.resource_id.clone()),
                LockMode::Exclusive,
            )
            .await?;
        if !still {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El movimiento perdió su lock antes de borrar el origen",
                "fencing_lost",
            ));
        }
        for item in p.movable().filter(|i| i.action == MoveAction::Copy) {
            let src = world.path(&item.from_storage_location_id, &item.relative_path)?;
            self.blocking(move |fs| match fs.remove_file(&src) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            })
            .await
            .map_err(|e| io_err("borrando el origen", &e))?;
        }
        self.clean_staging(job, p, world).await;
        self.write(
            job,
            MovePhase::SourceDeleted,
            &[MovePhase::AccessTested, MovePhase::SourceDeleted],
            p,
        )
        .await
    }

    async fn clean_staging(&self, job: &Job, p: &Payload, world: &World) {
        let mut roots: Vec<String> = p
            .movable()
            .filter(|i| i.action == MoveAction::Copy)
            .filter_map(|i| world.root(&i.to_storage_location_id).ok())
            .collect();
        roots.sort();
        roots.dedup();
        for root in roots {
            let dir = join(&root, &format!("{STAGING_DIR}/{}", job.id));
            let staging = join(&root, STAGING_DIR);
            let _ = self
                .blocking(move |fs| {
                    match fs.remove_dir_all(&dir) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                        _ => {}
                    }
                    // La carpeta común solo se quita si quedó vacía.
                    let _ = std::fs::remove_dir(&staging);
                    Ok(())
                })
                .await;
        }
    }

    async fn complete(&self, job: &Job, p: &mut Payload) -> Result<(), LarmError> {
        let moved = p.movable().count();
        p.result = Some(serde_json::json!({
            "moved": moved,
            "kept": p.items.len() - moved,
            "target_storage_location_id": p.target_storage_location_id,
        }));
        let step = Self::step(job, MovePhase::Completed, &[MovePhase::SourceDeleted], p);
        self.ports
            .guard
            .release(&job.id, p.generation, Some(&step))
            .await?;
        self.ports.reservations.release(&job.id).await?;
        Ok(())
    }

    /// Trata un fallo según la fase alcanzada.
    async fn fail(&self, job: &Job, p: &mut Payload, error: LarmError) {
        if error.details.reason == "fencing_lost" {
            // Otro titular manda ahora: no se toca nada.
            tracing_fallback("movimiento sin lock: se detiene sin escribir", &error);
            return;
        }
        if p.access_tested {
            // El destino ya es el válido: la recuperación terminará el borrado.
            tracing::error!(job = %job.id, reason = %error.details.reason, "borrado del origen pendiente: queda para la recuperación");
            return;
        }
        let world = match self.world().await {
            Ok(w) => w,
            Err(e) => {
                tracing_fallback(
                    "sin estado de volúmenes: el journal queda para la recuperación",
                    &e,
                );
                return;
            }
        };
        if p.switched.is_empty() {
            self.rollback_before_switch(job, p, &world).await;
            self.abort_locked(job, p, error).await;
        } else {
            self.revert_switch(job, p, &world, error).await;
        }
    }

    /// Antes del cambio de registro: deshace renombrados, retira las copias
    /// colocadas y lo provisional. El origen sigue siendo el válido.
    async fn rollback_before_switch(&self, job: &Job, p: &Payload, world: &World) {
        for item in p.movable() {
            let (Ok(src), Ok(dst)) = (
                world.path(&item.from_storage_location_id, &item.relative_path),
                world.path(&item.to_storage_location_id, &item.relative_path),
            ) else {
                continue;
            };
            let id = &item.artifact_location_id;
            if item.action == MoveAction::Rename
                && (p.renaming.contains(id) || p.renamed.contains(id))
            {
                let _ = self
                    .blocking(move |fs| {
                        if fs.size(&src)?.is_none() && fs.size(&dst)?.is_some() {
                            fs.rename_new(&dst, &src)?;
                        }
                        Ok(())
                    })
                    .await;
            } else if item.action == MoveAction::Copy && p.placed.contains(id) {
                let _ = self
                    .blocking(move |fs| {
                        // Solo si el origen sigue ahí: entonces la copia es nuestra.
                        if fs.size(&src)?.is_some() && fs.size(&dst)?.is_some() {
                            fs.remove_file(&dst)?;
                        }
                        Ok(())
                    })
                    .await;
            }
        }
        self.clean_staging(job, p, world).await;
    }

    /// Después del cambio: el destino no sirve, así que el registro vuelve
    /// al origen (que no se ha borrado) y se conservan ambas copias.
    async fn revert_switch(&self, job: &Job, p: &mut Payload, world: &World, error: LarmError) {
        let mut back = Vec::new();
        let movable: Vec<MoveItem> = p.movable().cloned().collect();
        for item in &movable {
            let (Ok(src), Ok(dst)) = (
                world.path(&item.from_storage_location_id, &item.relative_path),
                world.path(&item.to_storage_location_id, &item.relative_path),
            ) else {
                continue;
            };
            if item.action == MoveAction::Rename {
                let _ = self
                    .blocking(move |fs| {
                        if fs.size(&src)?.is_none() && fs.size(&dst)?.is_some() {
                            fs.rename_new(&dst, &src)?;
                        }
                        Ok(())
                    })
                    .await;
            } else {
                p.leftovers.push(item.artifact_location_id.clone());
            }
            if let Some(version) = p.switched.get(&item.artifact_location_id) {
                back.push(LocationSwitch {
                    artifact_location_id: item.artifact_location_id.clone(),
                    expected_row_version: *version,
                    storage_location_id: item.from_storage_location_id.clone(),
                    relative_path: item.relative_path.clone(),
                    sha256: None,
                    file_size: None,
                    verified_at: None,
                });
            }
        }
        p.error = Some(error);
        let fence = self.fence(job, p);
        let step = Self::step(job, MovePhase::Failed, &LIVE, p);
        match self
            .ports
            .artifacts
            .switch(&back, &fence, &step, &p.ctx())
            .await
        {
            Ok(_) => {
                let _ = self.ports.guard.release(&job.id, p.generation, None).await;
                let _ = self.ports.reservations.release(&job.id).await;
            }
            Err(e) => tracing_fallback(
                "no se pudo devolver el registro al origen: queda para la recuperación",
                &e,
            ),
        }
    }

    /// Recuperación al arrancar: cada movimiento empezado y sin terminar se
    /// cierra según su fase. Devuelve `(job, desenlace)`.
    ///
    /// # Errors
    /// Fallo al leer el journal.
    pub async fn recover(&self) -> Result<Vec<(String, String)>, LarmError> {
        let mut out = Vec::new();
        for record in self.ports.journal.unfinished().await? {
            let (job, phase, mut p) = self.load(&record.id).await?;
            let outcome = match phase {
                MovePhase::Locked | MovePhase::Copying | MovePhase::Verified => {
                    p.switched.clear();
                    let error = LarmError::new(
                        ErrorCode::Internal,
                        "El movimiento se interrumpió antes de cambiar el registro; el origen sigue siendo el válido",
                        "job_interrupted",
                    );
                    self.fail(&job, &mut p, error).await;
                    "rolled_back"
                }
                MovePhase::DbSwitched | MovePhase::AccessTested => {
                    match self.after_switch(&job, &mut p).await {
                        Ok(()) => "completed",
                        Err(e) => {
                            self.fail(&job, &mut p, e).await;
                            "reverted"
                        }
                    }
                }
                MovePhase::SourceDeleted => match self.world().await {
                    Ok(world) => {
                        self.clean_staging(&job, &p, &world).await;
                        match self.complete(&job, &mut p).await {
                            Ok(()) => "completed",
                            Err(_) => "pending",
                        }
                    }
                    Err(_) => "pending",
                },
                _ => "ignored",
            };
            let phase_now = self
                .ports
                .journal
                .get(&job.id)
                .await?
                .map_or_else(String::new, |r| r.phase);
            out.push((job.id, format!("{outcome}:{phase_now}")));
        }
        Ok(out)
    }

    fn job_state(&self, record: &MoveRecord) -> Option<JobState> {
        let payload: Payload = serde_json::from_value(record.payload.clone()).ok()?;
        let phase = MovePhase::parse(&record.phase).ok()?;
        let state = match phase {
            MovePhase::Planned => JobPhase::Queued,
            MovePhase::Completed => JobPhase::Completed,
            MovePhase::Failed | MovePhase::Cancelled => JobPhase::Failed,
            _ => JobPhase::Running,
        };
        let live = self
            .progress
            .lock()
            .ok()
            .and_then(|m| m.get(&record.id).copied());
        let mut progress = serde_json::Map::new();
        progress.insert("phase".into(), phase.as_str().into());
        progress.insert("items_total".into(), payload.movable().count().into());
        progress.insert("bytes_total".into(), payload.bytes_total.to_string().into());
        progress.insert(
            "bytes_copied".into(),
            live.unwrap_or(payload.bytes_copied).to_string().into(),
        );
        Some(JobState {
            job_id: record.id.clone(),
            state,
            resource_type: "model_variant".into(),
            resource_id: Some(payload.resource_id.clone()),
            progress,
            result: payload.result.clone(),
            error: payload.error.clone(),
            evidence_refs: vec![],
        })
    }
}

fn tracing_fallback(what: &str, error: &LarmError) {
    tracing::error!(reason = %error.details.reason, detail = %error.technical_detail(), "{what}");
}

#[async_trait]
impl JobSource for MoveService {
    /// Visible para quien lo pidió y para un administrador.
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        let Some(record) = self.ports.journal.get(id).await? else {
            return Ok(None);
        };
        let actor = record
            .payload
            .get("actor_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if actor != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        Ok(self.job_state(&record))
    }
}

/// Acceso al servicio desde la API (puerto `ModelMover`).
#[derive(Clone)]
pub struct MoveHandle(pub Arc<MoveService>);

#[async_trait]
impl larm_domain::ports::ModelMover for MoveHandle {
    async fn plan(
        &self,
        variant_id: &str,
        target_storage_location_id: &str,
        ctx: &MutationContext,
    ) -> Result<Plan, LarmError> {
        self.0
            .plan(variant_id, target_storage_location_id, ctx)
            .await
    }

    async fn planned(&self, plan_hash: &str) -> Result<Option<Plan>, LarmError> {
        Ok(self
            .0
            .ports
            .journal
            .find_planned(plan_hash)
            .await?
            .and_then(|r| serde_json::from_value::<Payload>(r.payload).ok())
            .and_then(|p| p.plan))
    }

    async fn execute(
        &self,
        variant_id: &str,
        request: &MoveRequest,
        ctx: &MutationContext,
    ) -> Result<(Plan, larm_domain::ports::BackgroundTask), LarmError> {
        self.0.execute(variant_id, request, ctx).await
    }
}
