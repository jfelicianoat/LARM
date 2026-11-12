//! Refresco del perfil de hardware como job (P011, `POST /hardware/refresh`).
//!
//! `start` encola el job en `operation_journal` y devuelve el trabajo a
//! ejecutar; quien llama lo lanza en su runtime (este crate no depende de
//! Tokio). Si ya hay un refresco en curso, devuelve ese job y no lanza otro.
//! El sondeo solo lee el sistema: nunca instala ni actualiza drivers.

use parking_lot::RwLock;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::bridge::{JobReceipt, JobState};
use larm_domain::error::LarmError;
use larm_domain::locations::DiskThresholds;
use larm_domain::ports::{
    ApiScope, HardwareProber, HardwareRepository, JobSource, JournalJobRepository, MutationContext,
    ObservationContext, Principal, RootInspector, StorageProber, StorageStateChange,
    StorageStateRepository,
};
use larm_domain::storage::StorageObservation;

/// Trabajo pendiente de lanzar.
pub type RefreshTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone)]
pub struct HardwareRefresh {
    pub prober: Arc<dyn HardwareProber>,
    pub profiles: Arc<dyn HardwareRepository>,
    pub jobs: Arc<dyn JournalJobRepository>,
    /// Almacenamiento (P012): si existe, el refresco también sondea los
    /// volúmenes, guarda la observación en `metadata.storage` del perfil y
    /// aplica el último estado a `storage_location`.
    pub storage: Option<StorageRefresh>,
}

/// Sondeo y estado de almacenamiento que acompañan al refresco.
#[derive(Clone)]
pub struct StorageRefresh {
    pub prober: Arc<dyn StorageProber>,
    pub state: Arc<dyn StorageStateRepository>,
    pub roots: Arc<dyn RootInspector>,
    /// Umbrales de H09 de `Settings`.
    /// Umbrales y revisión en vivo; Settings puede actualizarlos y pedir un
    /// nuevo sondeo sin reiniciar el daemon.
    pub thresholds: Arc<RwLock<(DiskThresholds, i64)>>,
}

impl StorageRefresh {
    /// Sondea los volúmenes (con las raíces UNC registradas) y comprueba qué
    /// carpetas raíz existen.
    ///
    /// # Errors
    /// Fallo al leer las raíces o del sondeo.
    pub async fn observe(&self) -> Result<(StorageObservation, ObservationContext), LarmError> {
        let roots = self.state.roots().await?;
        let mut unc: Vec<String> = roots.iter().filter_map(|r| r.unc_path.clone()).collect();
        unc.sort();
        unc.dedup();
        let observation = self.prober.probe(&unc).await?;
        let mut root_present = std::collections::BTreeMap::new();
        for root in &roots {
            if let Some(present) = self.roots.present(&root.canonical_path).await {
                root_present.insert(root.id.clone(), present);
            }
        }
        let (thresholds, settings_revision) = *self.thresholds.read();
        Ok((
            observation,
            ObservationContext {
                root_present,
                thresholds: Some(thresholds),
                settings_revision: Some(settings_revision),
                known: Some(roots.iter().map(|r| r.id.clone()).collect()),
            },
        ))
    }

    /// Sondea y aplica el último estado a `storage_location`.
    ///
    /// # Errors
    /// Fallo del sondeo o de la base.
    pub async fn refresh(
        &self,
        ctx: &MutationContext,
    ) -> Result<(StorageObservation, Vec<StorageStateChange>), LarmError> {
        let (observation, context) = self.observe().await?;
        let changes = self.state.apply(&observation, &context, ctx).await?;
        Ok((observation, changes))
    }
}

impl HardwareRefresh {
    /// Encola un refresco. Devuelve el recibo y, si el job es nuevo, la tarea
    /// que hay que lanzar.
    ///
    /// # Errors
    /// Errores del repositorio de jobs.
    pub async fn start(
        &self,
        ctx: &MutationContext,
        resource_id: Option<&str>,
    ) -> Result<(JobReceipt, Option<RefreshTask>), LarmError> {
        let (job, created) = self.jobs.enqueue(ctx, resource_id).await?;
        let receipt = JobReceipt {
            status_url: format!("/api/v1/jobs/{}", job.state.job_id),
            job_id: job.state.job_id.clone(),
            state: job.state.state,
        };
        if !created {
            return Ok((receipt, None));
        }
        let service = self.clone();
        let id = job.state.job_id;
        let correlation = ctx.correlation_id.clone();
        let task: RefreshTask = Box::pin(async move { service.run(&id, &correlation).await });
        Ok((receipt, Some(task)))
    }

    async fn run(&self, id: &str, correlation_id: &str) {
        if let Err(error) = self.jobs.running(id).await {
            tracing::error!(job_id = %id, detail = %error.technical_detail(), "el job de hardware no pudo empezar");
            return;
        }
        let storage = match &self.storage {
            Some(refresh) => Some(refresh.observe().await),
            None => None,
        };
        let outcome = match self.prober.probe().await {
            Ok(mut probe) => {
                match &storage {
                    Some(Ok((observation, _))) => {
                        probe.metadata.insert(
                            "storage".into(),
                            serde_json::to_value(observation).unwrap_or_default(),
                        );
                    }
                    Some(Err(error)) => {
                        probe.metadata.insert(
                            "storage".into(),
                            serde_json::json!({"error": error.details.reason, "detail": error.technical_detail()}),
                        );
                    }
                    None => {}
                }
                self.profiles.record(&probe).await
            }
            Err(error) => Err(error),
        };
        if let (Some(refresh), Some(Ok((observation, context)))) = (&self.storage, &storage) {
            let ctx = MutationContext {
                actor_id: "larm-daemon".into(),
                correlation_id: correlation_id.to_string(),
            };
            match refresh.state.apply(observation, context, &ctx).await {
                Ok(changes) => {
                    tracing::info!(job_id = %id, cambios = changes.len(), "estado de almacenamiento aplicado")
                }
                Err(error) => {
                    tracing::error!(job_id = %id, detail = %error.technical_detail(), "no se pudo aplicar el estado de almacenamiento")
                }
            }
        }
        let stored = match outcome {
            Ok(profile) => {
                tracing::info!(
                    job_id = %id,
                    hardware_profile_id = %profile.id,
                    hardware_fingerprint = %profile.hardware_fingerprint,
                    "perfil de hardware registrado"
                );
                self.jobs
                    .complete(
                        id,
                        &profile.id,
                        serde_json::json!({
                            "hardware_profile_id": profile.id,
                            "hardware_fingerprint": profile.hardware_fingerprint,
                            "row_version": profile.row_version,
                            "probed_at": profile.probed_at,
                        }),
                        &profile.evidence_refs,
                    )
                    .await
            }
            Err(mut error) => {
                if let Ok(c) = larm_domain::correlation::CorrelationId::parse(correlation_id) {
                    error = error.with_correlation(c);
                }
                tracing::error!(job_id = %id, code = %error.code, detail = %error.technical_detail(), "el sondeo de hardware falló");
                self.jobs.fail(id, &error).await
            }
        };
        if let Err(error) = stored {
            tracing::error!(job_id = %id, detail = %error.technical_detail(), "no se pudo cerrar el job de hardware");
        }
    }
}

#[async_trait]
impl JobSource for HardwareRefresh {
    /// Visible para quien lo pidió y para un administrador; para los demás es
    /// indistinguible de un job inexistente.
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        Ok(self.jobs.get(id).await?.and_then(|job| {
            (job.actor_id == principal.id || principal.has_api_scope(ApiScope::Admin))
                .then_some(job.state)
        }))
    }
}

/// Implementaciones en memoria para pruebas de casos de uso y de la API sin
/// SQLite ni hardware. Imitan la conducta de producción (un único job activo,
/// transiciones válidas); las pruebas reales están en `larm-db` y P011.
pub mod memory {
    use std::time::Duration;

    use async_trait::async_trait;
    use larm_domain::bridge::{JobPhase, JobState};
    use larm_domain::error::{ErrorCode, LarmError};
    use larm_domain::hardware::{HardwareProbe, HardwareProfile};
    use larm_domain::ports::{
        HardwareProber, HardwareRepository, JournalJob, JournalJobRepository, MutationContext,
    };
    use parking_lot::Mutex;

    /// Prober que devuelve `probe` tras `delay` o, sin `probe`, falla.
    pub struct ScriptedProber {
        pub probe: Option<HardwareProbe>,
        pub delay: Duration,
    }

    #[async_trait]
    impl HardwareProber for ScriptedProber {
        async fn probe(&self) -> Result<HardwareProbe, LarmError> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            self.probe.clone().ok_or_else(|| {
                LarmError::new(
                    ErrorCode::Internal,
                    "El sondeo de hardware falló",
                    "hardware_probe_failed",
                )
            })
        }
    }

    /// Perfiles en memoria: cada `record` crea un perfil nuevo.
    #[derive(Default)]
    pub struct MemoryProfiles {
        pub rows: Mutex<Vec<HardwareProfile>>,
    }

    #[async_trait]
    impl HardwareRepository for MemoryProfiles {
        async fn latest(&self) -> Result<Option<HardwareProfile>, LarmError> {
            Ok(self
                .rows
                .lock()
                .iter()
                .max_by_key(|p| (&p.created_at, &p.id))
                .cloned())
        }

        async fn record(&self, probe: &HardwareProbe) -> Result<HardwareProfile, LarmError> {
            let now = larm_domain::bridge::timestamp(chrono::Utc::now());
            let profile = HardwareProfile {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: now.clone(),
                updated_at: now,
                row_version: 1,
                metadata: probe.metadata.clone(),
                hardware_fingerprint: probe.snapshot.fingerprint().unwrap_or_default(),
                snapshot: probe.snapshot.clone(),
                evidence_refs: probe.evidence_refs.clone(),
                probed_at: probe.probed_at.clone(),
            };
            self.rows.lock().push(profile.clone());
            Ok(profile)
        }
        async fn list(
            &self,
            after: Option<(String, String)>,
            limit: u32,
        ) -> Result<Vec<HardwareProfile>, LarmError> {
            let after = after.unwrap_or_default();
            Ok(self
                .rows
                .lock()
                .iter()
                .filter(|p| (p.created_at.clone(), p.id.clone()) > after)
                .take(limit as usize)
                .cloned()
                .collect())
        }
        async fn get(&self, id: &str) -> Result<Option<HardwareProfile>, LarmError> {
            Ok(self.rows.lock().iter().find(|p| p.id == id).cloned())
        }
    }

    /// Jobs en memoria con un único job activo.
    #[derive(Default)]
    pub struct MemoryJournalJobs {
        pub jobs: Mutex<Vec<JournalJob>>,
        /// `resource_type` de los jobs; vacío = `hardware_profile`.
        pub resource_type: &'static str,
    }

    impl MemoryJournalJobs {
        #[must_use]
        pub fn for_resource(resource_type: &'static str) -> Self {
            Self {
                jobs: Mutex::new(Vec::new()),
                resource_type,
            }
        }
    }

    impl MemoryJournalJobs {
        fn set(
            &self,
            id: &str,
            from: &[JobPhase],
            to: JobPhase,
            patch: impl FnOnce(&mut JobState),
        ) -> Result<(), LarmError> {
            let mut jobs = self.jobs.lock();
            let job = jobs
                .iter_mut()
                .find(|j| j.state.job_id == id && from.contains(&j.state.state))
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::InvalidStateTransition,
                        "Transición de job inválida",
                        "job_state_conflict",
                    )
                })?;
            job.state.state = to;
            patch(&mut job.state);
            Ok(())
        }
    }

    #[async_trait]
    impl JournalJobRepository for MemoryJournalJobs {
        async fn enqueue(
            &self,
            ctx: &MutationContext,
            resource_id: Option<&str>,
        ) -> Result<(JournalJob, bool), LarmError> {
            let mut jobs = self.jobs.lock();
            if let Some(active) = jobs
                .iter()
                .find(|j| matches!(j.state.state, JobPhase::Queued | JobPhase::Running))
            {
                return Ok((active.clone(), false));
            }
            let job = JournalJob {
                actor_id: ctx.actor_id.clone(),
                state: JobState {
                    job_id: uuid::Uuid::now_v7().to_string(),
                    state: JobPhase::Queued,
                    resource_type: if self.resource_type.is_empty() {
                        "hardware_profile"
                    } else {
                        self.resource_type
                    }
                    .into(),
                    resource_id: resource_id.map(String::from),
                    progress: serde_json::Map::new(),
                    result: None,
                    error: None,
                    evidence_refs: vec![],
                },
            };
            jobs.push(job.clone());
            Ok((job, true))
        }
        async fn running(&self, id: &str) -> Result<(), LarmError> {
            self.set(id, &[JobPhase::Queued], JobPhase::Running, |_| {})
        }
        async fn complete(
            &self,
            id: &str,
            resource_id: &str,
            result: serde_json::Value,
            _: &[String],
        ) -> Result<(), LarmError> {
            let resource_id = resource_id.to_string();
            self.set(id, &[JobPhase::Running], JobPhase::Completed, move |s| {
                s.result = Some(result);
                s.resource_id = Some(resource_id);
            })
        }
        async fn fail(&self, id: &str, error: &LarmError) -> Result<(), LarmError> {
            let error = error.clone();
            self.set(
                id,
                &[JobPhase::Queued, JobPhase::Running],
                JobPhase::Failed,
                move |s| s.error = Some(error),
            )
        }
        async fn get(&self, id: &str) -> Result<Option<JournalJob>, LarmError> {
            Ok(self
                .jobs
                .lock()
                .iter()
                .find(|j| j.state.job_id == id)
                .cloned())
        }
        async fn recover(&self) -> Result<u64, LarmError> {
            let mut n = 0;
            for j in self.jobs.lock().iter_mut() {
                if matches!(j.state.state, JobPhase::Queued | JobPhase::Running) {
                    j.state.state = JobPhase::Failed;
                    n += 1;
                }
            }
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::memory::{MemoryJournalJobs, MemoryProfiles, ScriptedProber};
    use super::*;
    use larm_domain::bridge::JobPhase;
    use larm_domain::hardware::{
        CpuInfo, HardwareProbe, HardwareSnapshot, MemoryInfo, ObservedMetrics, OsInfo,
    };
    use std::time::Duration;

    fn probe() -> HardwareProbe {
        HardwareProbe {
            snapshot: HardwareSnapshot {
                os: OsInfo {
                    edition: None,
                    build: None,
                    architecture: None,
                },
                cpu: CpuInfo {
                    vendor: None,
                    model: None,
                    physical_cores: None,
                    logical_cores: None,
                    instruction_sets: vec![],
                },
                ram: MemoryInfo {
                    physical_bytes: None,
                    usable_bytes: None,
                    pool_id: "system-ram".into(),
                },
                devices: vec![],
                dependencies: vec![],
                observed_metrics: ObservedMetrics {
                    available_ram_bytes: None,
                    cpu_usage_percent: None,
                },
            },
            metadata: serde_json::Map::new(),
            evidence_refs: vec![],
            probed_at: "2026-10-11T10:00:00.000Z".into(),
        }
    }

    fn service(probe: Option<HardwareProbe>) -> (HardwareRefresh, Arc<MemoryJournalJobs>) {
        let jobs = Arc::new(MemoryJournalJobs::default());
        (
            HardwareRefresh {
                prober: Arc::new(ScriptedProber {
                    probe,
                    delay: Duration::ZERO,
                }),
                profiles: Arc::new(MemoryProfiles::default()),
                jobs: jobs.clone(),
                storage: None,
            },
            jobs,
        )
    }

    fn ctx(actor: &str) -> MutationContext {
        MutationContext {
            actor_id: actor.into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    fn principal(id: &str, scope: ApiScope) -> Principal {
        Principal::new(
            id,
            id,
            larm_domain::auth::PrincipalKind::Operator,
            [scope],
            [],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn un_segundo_refresco_reutiliza_el_job_en_curso() {
        let (svc, _) = service(Some(probe()));
        let (first, task) = svc.start(&ctx("a"), None).await.unwrap();
        assert!(task.is_some());
        assert_eq!(first.status_url, format!("/api/v1/jobs/{}", first.job_id));
        let (second, again) = svc.start(&ctx("b"), None).await.unwrap();
        assert_eq!(second.job_id, first.job_id);
        assert!(again.is_none(), "no se lanza un segundo sondeo");
        task.unwrap().await;
        let done = svc
            .job(&principal("a", ApiScope::Read), &first.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.state, JobPhase::Completed);
        assert_eq!(done.result.unwrap()["row_version"], 1);
    }

    #[tokio::test]
    async fn un_sondeo_fallido_deja_el_job_en_failed_con_la_correlacion_de_la_peticion() {
        let (svc, jobs) = service(None);
        let c = ctx("a");
        let (receipt, task) = svc.start(&c, None).await.unwrap();
        task.unwrap().await;
        let job = jobs.jobs.lock()[0].clone();
        assert_eq!(job.state.state, JobPhase::Failed);
        let error = job.state.error.unwrap();
        assert_eq!(error.details.reason, "hardware_probe_failed");
        assert_eq!(error.correlation_id, c.correlation_id);
        // Tras terminar, un refresco nuevo crea otro job.
        let (next, task) = svc.start(&ctx("a"), None).await.unwrap();
        assert_ne!(next.job_id, receipt.job_id);
        assert!(task.is_some());
    }

    #[tokio::test]
    async fn el_job_solo_lo_ve_su_autor_o_un_administrador() {
        let (svc, _) = service(Some(probe()));
        let (receipt, _) = svc.start(&ctx("autor"), None).await.unwrap();
        let id = receipt.job_id;
        assert!(svc
            .job(&principal("autor", ApiScope::Read), &id)
            .await
            .unwrap()
            .is_some());
        assert!(svc
            .job(&principal("admin", ApiScope::Admin), &id)
            .await
            .unwrap()
            .is_some());
        assert!(svc
            .job(&principal("otro", ApiScope::Read), &id)
            .await
            .unwrap()
            .is_none());
    }

    /// Almacenamiento falso: devuelve `observation` o falla; registra lo aplicado.
    struct FakeStorage {
        observation: Option<StorageObservation>,
        roots: Vec<String>,
        asked: parking_lot::Mutex<Vec<String>>,
        applied: parking_lot::Mutex<Vec<StorageObservation>>,
        contexts: parking_lot::Mutex<Vec<ObservationContext>>,
    }

    #[async_trait]
    impl RootInspector for FakeStorage {
        async fn present(&self, path: &str) -> Option<bool> {
            (!path.contains("nas")).then_some(true)
        }
    }

    #[async_trait]
    impl StorageProber for FakeStorage {
        async fn probe(&self, roots: &[String]) -> Result<StorageObservation, LarmError> {
            self.asked.lock().extend(roots.iter().cloned());
            self.observation.clone().ok_or_else(|| {
                LarmError::new(
                    larm_domain::error::ErrorCode::Internal,
                    "x",
                    "storage_probe_timeout",
                )
            })
        }
    }

    #[async_trait]
    impl StorageStateRepository for FakeStorage {
        async fn roots(&self) -> Result<Vec<larm_domain::ports::LocationRoot>, LarmError> {
            Ok(self
                .roots
                .iter()
                .enumerate()
                .map(|(i, r)| larm_domain::ports::LocationRoot {
                    id: format!("loc{i}"),
                    canonical_path: r.clone(),
                    unc_path: Some(r.clone()),
                })
                .collect())
        }
        async fn apply(
            &self,
            observation: &StorageObservation,
            context: &ObservationContext,
            _: &MutationContext,
        ) -> Result<Vec<larm_domain::ports::StorageStateChange>, LarmError> {
            self.contexts.lock().push(context.clone());
            self.applied.lock().push(observation.clone());
            Ok(vec![])
        }
    }

    async fn refresh_with_storage(
        observation: Option<StorageObservation>,
    ) -> (HardwareRefresh, Arc<FakeStorage>, Arc<MemoryProfiles>) {
        let fake = Arc::new(FakeStorage {
            observation,
            roots: vec![r"\\nas\modelos".into()],
            asked: parking_lot::Mutex::new(vec![]),
            applied: parking_lot::Mutex::new(vec![]),
            contexts: parking_lot::Mutex::new(vec![]),
        });
        let profiles = Arc::new(MemoryProfiles::default());
        let svc = HardwareRefresh {
            prober: Arc::new(ScriptedProber {
                probe: Some(probe()),
                delay: Duration::ZERO,
            }),
            profiles: profiles.clone(),
            jobs: Arc::new(MemoryJournalJobs::default()),
            storage: Some(StorageRefresh {
                prober: fake.clone(),
                state: fake.clone(),
                roots: fake.clone(),
                thresholds: Arc::new(RwLock::new((
                    DiskThresholds::Percent {
                        warning_bp: 1500,
                        critical_bp: 500,
                    },
                    1,
                ))),
            }),
        };
        let (_, task) = svc.start(&ctx("a"), None).await.unwrap();
        task.unwrap().await;
        (svc, fake, profiles)
    }

    #[tokio::test]
    async fn el_refresco_sondea_guarda_y_aplica_el_almacenamiento() {
        let observation = StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: "t".into(),
        };
        let (_, fake, profiles) = refresh_with_storage(Some(observation.clone())).await;
        assert_eq!(
            *fake.asked.lock(),
            [r"\\nas\modelos"],
            "se piden las raíces UNC registradas"
        );
        assert_eq!(*fake.applied.lock(), std::slice::from_ref(&observation));
        let context = fake.contexts.lock()[0].clone();
        assert!(
            context.root_present.is_empty(),
            "una ruta de red que no se pudo comprobar no se marca MISSING"
        );
        assert!(
            context.thresholds.is_some(),
            "los umbrales de Settings llegan a la aplicación"
        );
        assert_eq!(context.settings_revision, Some(1));
        let stored = profiles.rows.lock()[0].metadata["storage"].clone();
        assert_eq!(stored, serde_json::to_value(observation).unwrap());
    }

    #[tokio::test]
    async fn el_siguiente_sondeo_usa_umbrales_y_revision_actualizados_en_vivo() {
        let observation = StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: "t".into(),
        };
        let (service, _, _) = refresh_with_storage(Some(observation)).await;
        let storage = service.storage.as_ref().unwrap();
        *storage.thresholds.write() = (
            DiskThresholds::Bytes {
                warning: 1000,
                critical: 200,
            },
            2,
        );
        let (_, context) = storage.observe().await.unwrap();
        assert_eq!(
            context.thresholds,
            Some(DiskThresholds::Bytes {
                warning: 1000,
                critical: 200
            })
        );
        assert_eq!(context.settings_revision, Some(2));
    }

    #[tokio::test]
    async fn un_fallo_de_almacenamiento_se_declara_y_no_tumba_el_perfil() {
        let (_, fake, profiles) = refresh_with_storage(None).await;
        assert!(
            fake.applied.lock().is_empty(),
            "sin observación no se toca storage_location"
        );
        let rows = profiles.rows.lock();
        assert_eq!(rows.len(), 1, "el perfil de hardware se registra igual");
        assert_eq!(
            rows[0].metadata["storage"]["error"],
            "storage_probe_timeout"
        );
    }
}
