//! Perfil de hardware en el daemon (P010): sondeo al arrancar y comando
//! `hardware-probe`. El sondeo nunca instala ni cambia nada del sistema.

use parking_lot::RwLock;
use std::sync::Arc;

use larm_core::config::{LowDiskThresholds, Settings};
use larm_core::hardware::{HardwareRefresh, StorageRefresh};
use larm_core::locations::LocationProbe;
use larm_db::hardware::SqlHardware;
use larm_db::jobs::SqlJournalJobs;
use larm_db::pool::Db;
use larm_domain::error::LarmError;
use larm_domain::locations::DiskThresholds;
use larm_domain::ports::LocationService;
use larm_domain::ports::{
    HardwareProber, HardwareRepository, JournalJobRepository, MutationContext, StorageProber,
};

#[derive(Clone)]
pub struct UnavailableStorageProber;

#[async_trait::async_trait]
impl StorageProber for UnavailableStorageProber {
    async fn probe(
        &self,
        _: &[String],
    ) -> Result<larm_domain::storage::StorageObservation, LarmError> {
        Err(LarmError::new(
            larm_domain::error::ErrorCode::ResourceUnavailable,
            "El sondeo nativo de almacenamiento no está disponible",
            "storage_probe_unavailable",
        )
        .retryable(true))
    }
}

/// Perfilador con el sondeo nativo en un proceso hijo: este mismo binario con
/// `probe-accelerators` (ADR-0010).
#[must_use]
pub fn profiler() -> larm_hardware::WindowsProfiler {
    match std::env::current_exe() {
        Ok(exe) => {
            larm_hardware::WindowsProfiler::with_native(larm_hardware::devices::NativeCommand {
                program: exe.display().to_string(),
                args: vec!["probe-accelerators".into()],
                timeout: std::time::Duration::from_secs(30),
            })
        }
        Err(error) => {
            tracing::warn!(error = %error, "sin ruta del ejecutable: sondeo sin parte nativa");
            larm_hardware::WindowsProfiler::default()
        }
    }
}

/// Sondeo de almacenamiento en el proceso hijo `probe-storage` (ADR-0010).
/// 60 s: comprobar un servidor SMB caído tarda decenas de segundos.
#[must_use]
pub fn storage_prober() -> Option<larm_hardware::storage::NativeStorageProber> {
    std::env::current_exe()
        .ok()
        .map(|exe| larm_hardware::storage::NativeStorageProber {
            command: larm_hardware::devices::NativeCommand {
                program: exe.display().to_string(),
                args: vec!["probe-storage".into()],
                timeout: std::time::Duration::from_secs(60),
            },
        })
}

/// Sondea el almacenamiento con las raíces UNC dadas y devuelve el JSON.
///
/// # Errors
/// Fallo del sondeo.
pub async fn storage_probe(unc: &[String]) -> Result<serde_json::Value, LarmError> {
    use larm_domain::ports::StorageProber;
    let Some(prober) = storage_prober() else {
        return Err(LarmError::new(
            larm_domain::error::ErrorCode::Internal,
            "Sin ruta del ejecutable",
            "storage_probe_unavailable",
        ));
    };
    let observation = prober.probe(unc).await?;
    Ok(serde_json::to_value(observation).unwrap_or_default())
}

/// Sondea y, si `repo` existe, registra el perfil. Devuelve el JSON a imprimir.
///
/// # Errors
/// Fallo del sondeo o de la base.
pub async fn probe(repo: Option<&SqlHardware>) -> Result<serde_json::Value, LarmError> {
    let probe = profiler().probe().await?;
    if let Some(repo) = repo {
        let profile = repo.record(&probe).await?;
        return Ok(serde_json::to_value(profile).unwrap_or_default());
    }
    let fingerprint = probe.snapshot.fingerprint().unwrap_or_default();
    Ok(serde_json::json!({
        "hardware_fingerprint": fingerprint,
        "snapshot": probe.snapshot,
        "metadata": probe.metadata,
        "evidence_refs": probe.evidence_refs,
        "probed_at": probe.probed_at,
    }))
}

/// Umbrales de disco bajo de `Settings` (H09) en su forma de dominio.
#[must_use]
pub fn thresholds(settings: &Settings) -> DiskThresholds {
    match &settings.storage.low_disk_thresholds {
        LowDiskThresholds::Percent {
            warning_basis_points,
            critical_basis_points,
        } => DiskThresholds::Percent {
            warning_bp: u64::from(*warning_basis_points),
            critical_bp: u64::from(*critical_basis_points),
        },
        LowDiskThresholds::Bytes {
            warning_bytes,
            critical_bytes,
        } => DiskThresholds::Bytes {
            warning: warning_bytes.get(),
            critical: critical_bytes.get(),
        },
    }
}

async fn recover(jobs: &SqlJournalJobs, what: &str) {
    match jobs.recover().await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            jobs = n,
            tipo = what,
            "jobs interrumpidos por un reinicio marcados FAILED"
        ),
        Err(error) => {
            tracing::error!(tipo = what, detail = %error.technical_detail(), "no se pudieron recuperar los jobs")
        }
    }
}

/// Sondeo y estado de almacenamiento compartidos por el refresco de
/// hardware y el sondeo de ubicaciones.
#[must_use]
pub fn storage_refresh(
    db: &Db,
    thresholds: Arc<RwLock<(DiskThresholds, i64)>>,
) -> Option<StorageRefresh> {
    storage_prober().map(|prober| StorageRefresh {
        prober: Arc::new(prober),
        state: Arc::new(larm_db::storage::SqlStorageState {
            pool: db.pool.clone(),
        }),
        roots: Arc::new(larm_storage::locations::FsRootInspector),
        thresholds,
    })
}

/// Servicio de refresco sobre la base, tras marcar como FAILED los jobs
/// que un reinicio dejó a medias.
pub async fn refresh_service(
    db: &Db,
    thresholds: Arc<RwLock<(DiskThresholds, i64)>>,
) -> Arc<HardwareRefresh> {
    let jobs = SqlJournalJobs::hardware(db.pool.clone());
    recover(&jobs, "hardware.refresh").await;
    Arc::new(HardwareRefresh {
        prober: Arc::new(profiler()),
        profiles: Arc::new(SqlHardware {
            pool: db.pool.clone(),
        }),
        jobs: Arc::new(jobs),
        storage: storage_refresh(db, thresholds),
    })
}

/// Ubicaciones de almacenamiento (P014): servicio y job de sondeo.
pub async fn locations(
    db: &Db,
    thresholds: Arc<RwLock<(DiskThresholds, i64)>>,
) -> Option<(Arc<dyn LocationService>, Arc<LocationProbe>)> {
    let live_thresholds = *thresholds.read();
    let storage = storage_refresh(db, thresholds)?;
    let service: Arc<dyn LocationService> =
        Arc::new(larm_storage::locations::LocationServiceImpl {
            repo: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
            prober: storage.prober.clone(),
            thresholds: Some(live_thresholds.0),
        });
    let jobs = SqlJournalJobs::storage(db.pool.clone());
    recover(&jobs, "storage.probe").await;
    let probe = Arc::new(LocationProbe {
        storage,
        jobs: Arc::new(jobs),
        locations: service.clone(),
    });
    Some((service, probe))
}

/// Sondeo al arrancar, como un job más (visible en `GET /jobs/{id}` para
/// un administrador). La API responde desde el primer momento.
pub async fn start_on_boot(refresh: &Arc<HardwareRefresh>) {
    let ctx = MutationContext {
        actor_id: "larm-daemon".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    match refresh.start(&ctx, None).await {
        Ok((receipt, task)) => {
            tracing::info!(job_id = %receipt.job_id, "sondeo de hardware al arrancar");
            if let Some(task) = task {
                tokio::spawn(task);
            }
        }
        Err(error) => {
            tracing::error!(detail = %error.technical_detail(), "no se pudo encolar el sondeo de hardware")
        }
    }
}

/// Movimiento de modelos (P016) con su recuperación desde el journal: lo que
/// un reinicio dejó a medias se cierra antes de servir la API.
pub async fn mover(
    db: &Db,
    settings: &Settings,
) -> Option<Arc<larm_storage::move_job::MoveService>> {
    use larm_storage::move_job::{MovePorts, MoveService, StdMoveFs};
    let prober = storage_prober()?;
    let service = Arc::new(MoveService::new(
        MovePorts {
            artifacts: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            locations: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
            journal: Arc::new(larm_db::artifacts::SqlMoveJournal {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            reservations: Arc::new(larm_db::reservations::SqlVolumeReservations {
                pool: db.pool.clone(),
            }),
            prober: Arc::new(prober),
            fs: Arc::new(StdMoveFs),
        },
        larm_storage::estimate::SpacePolicy {
            margin: larm_domain::estimate::MarginPolicy {
                minimum_margin_bytes: settings.storage.minimum_margin_bytes.get(),
                margin_basis_points: settings.storage.margin_basis_points,
            },
            minimum_free_bytes: settings.storage.minimum_free_bytes.get(),
        },
        chrono::Duration::seconds(
            i64::try_from(settings.security.approval_ttl_seconds).unwrap_or(900),
        ),
    ));
    match service.recover().await {
        Ok(report) if report.is_empty() => {}
        Ok(report) => {
            for (job, outcome) in report {
                tracing::warn!(job = %job, desenlace = %outcome, "movimiento interrumpido recuperado desde el journal");
            }
        }
        Err(error) => {
            tracing::error!(detail = %error.technical_detail(), "no se pudo recuperar los movimientos")
        }
    }
    Some(service)
}

/// Reconciliación de artefactos con sus volúmenes (P018).
#[must_use]
pub fn reconciler(db: &Db) -> Option<larm_storage::reconcile::Reconciler> {
    let prober = storage_prober()?;
    Some(larm_storage::reconcile::Reconciler {
        locations: Arc::new(larm_db::locations::SqlStorageLocations {
            pool: db.pool.clone(),
        }),
        artifacts: Arc::new(larm_db::artifacts::SqlArtifactLocations {
            pool: db.pool.clone(),
        }),
        variants: Arc::new(larm_db::variants::SqlVariants {
            pool: db.pool.clone(),
        }),
        prober: Arc::new(prober),
    })
}

/// Análisis, limpieza y borrado de modelos (P018).
#[must_use]
pub fn maintenance(
    db: &Db,
    settings: &Settings,
) -> Option<Arc<larm_storage::cleanup::StorageService>> {
    use larm_storage::cleanup::{StoragePorts, StorageService, CLEANUP_KIND, DELETE_KIND};
    let prober = storage_prober()?;
    Some(Arc::new(StorageService {
        ports: StoragePorts {
            locations: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
            artifacts: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            variants: Arc::new(larm_db::variants::SqlVariants {
                pool: db.pool.clone(),
            }),
            prober: Arc::new(prober),
            live: Arc::new(larm_db::artifacts::SqlLiveJobs {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            cleanup_journal: Arc::new(larm_db::artifacts::SqlJournal {
                pool: db.pool.clone(),
                kind: CLEANUP_KIND,
            }),
            delete_journal: Arc::new(larm_db::artifacts::SqlJournal {
                pool: db.pool.clone(),
                kind: DELETE_KIND,
            }),
        },
        plan_ttl: chrono::Duration::seconds(
            i64::try_from(settings.security.approval_ttl_seconds).unwrap_or(900),
        ),
    }))
}

/// Reconciliación al arrancar y después cada `interval`.
pub fn spawn_reconcile(
    reconciler: larm_storage::reconcile::Reconciler,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let ctx = MutationContext {
                actor_id: "larm-daemon:reconcile".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            };
            match reconciler.run(&ctx).await {
                Ok(report) => {
                    if report != larm_storage::reconcile::ReconcileReport::default() {
                        tracing::info!(?report, "reconciliación de almacenamiento");
                    }
                }
                Err(error) => {
                    tracing::error!(detail = %error.technical_detail(), "la reconciliación falló")
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}
