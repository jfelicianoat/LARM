//! Servicio de la API (P005): aplicación unificada con guard (P009), rutas de
//! seguridad, puente Broker (P107) si hay `--bridge-config`, SSE alimentado por
//! los eventos persistidos, pipe de bootstrap del escritorio y canal de
//! credenciales de endpoint, ambos con DACL restringida al SID (ADR-0009).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use larm_api::app::{
    AppConfig, AppState, EventBus, EventEnvelope, EventReplayBatch, EventReplayStore, Readiness,
};
use larm_core::config::Settings;
use larm_core::security::{Approvals, Credentials, CursorSigner};
use larm_db::pool::Db;
use larm_db::security::{SqlApprovals, SqlCredentials, SqlIdempotency};
use larm_domain::bridge::ApiError;
use larm_domain::model_import::ModelImportJobs;
use larm_domain::model_verification::ModelVerificationJobs;
use larm_domain::ports::{JournalJobRepository, SettingsRepository};
use larm_domain::runtime_ports::RuntimeInstanceStopRepository;

use crate::bridge::{is_loopback, BridgeConfig, KeyringSecrets};
use crate::runtime_instance_process::{NativeRuntimeProcessController, RuntimeSessionRegistry};

/// Cada cuánto se intentan despachar eventos que siguen pendientes en SQLite.
const EVENT_POLL: Duration = Duration::from_millis(500);
const EVENT_RETENTION_CHECK: Duration = Duration::from_secs(24 * 60 * 60);
const EVENT_DISPATCH_BATCH: u32 = larm_db::events::MAX_DISPATCH_BATCH;
const RUNTIME_INSTALL_POLL: Duration = Duration::from_millis(500);
const RUNTIME_INSTALL_BATCH: u32 = 4;
const RUNTIME_INSTALL_RECOVERY_POLL: Duration = Duration::from_secs(10);
const RUNTIME_INSTALL_RECOVERY_BATCH: u32 = 1;
const MODEL_VERIFY_POLL: Duration = Duration::from_millis(500);
const MODEL_IMPORT_POLL: Duration = Duration::from_millis(500);
const DOWNLOAD_POLL: Duration = Duration::from_millis(500);
const RUNTIME_INSTANCE_STOP_POLL: Duration = Duration::from_millis(250);
const RUNTIME_INSTANCE_STOP_BATCH: u32 = 4;

#[async_trait]
trait DownloadConsumerRunner: Send + Sync {
    async fn run_cancellation_once(
        &self,
    ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>;
    async fn run_once_with_source_limit(
        &self,
        per_source_limit: u32,
    ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>;
}

#[async_trait]
impl DownloadConsumerRunner for larm_download::consumer::DownloadConsumer {
    async fn run_cancellation_once(
        &self,
    ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
    {
        self.run_pending_cancellation().await
    }
    async fn run_once_with_source_limit(
        &self,
        per_source_limit: u32,
    ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
    {
        larm_download::consumer::DownloadConsumer::run_once_with_source_limit(
            self,
            per_source_limit,
        )
        .await
    }
}

fn spawn_download_consumer(
    consumer: Arc<dyn DownloadConsumerRunner>,
    worker_count: u32,
    per_source_limit: u32,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        let cleanup = consumer.clone();
        let mut cleanup_shutdown = shutdown.clone();
        workers.spawn(async move {
            loop {
                match cleanup.run_cancellation_once().await {
                    Ok(larm_download::consumer::DownloadConsumerOutcome::Idle) => {},
                    Ok(larm_download::consumer::DownloadConsumerOutcome::Cancelled {job_id}) => tracing::info!(job_id=%job_id,"limpieza de cancelación completada"),
                    Ok(_) => tracing::error!("resultado inesperado en limpieza de descargas"),
                    Err(error) => tracing::error!(reason=%error.details.reason,"no se pudo completar la limpieza de descarga"),
                }
                tokio::select! {
                    changed = cleanup_shutdown.changed() => {
                        if changed.is_err() || *cleanup_shutdown.borrow() { break; }
                    }
                    _ = tokio::time::sleep(DOWNLOAD_POLL) => {},
                }
            }
        });
        for _ in 0..worker_count {
            let consumer = consumer.clone();
            let mut shutdown = shutdown.clone();
            workers.spawn(async move {
                loop {
                    match consumer
                        .run_once_with_source_limit(per_source_limit)
                        .await
                    {
                        Ok(larm_download::consumer::DownloadConsumerOutcome::Idle) => {}
                        Ok(larm_download::consumer::DownloadConsumerOutcome::Completed { job_id }) => {
                            tracing::info!(job_id = %job_id, "descarga completada y verificada")
                        }
                        Ok(larm_download::consumer::DownloadConsumerOutcome::Paused { job_id }) => {
                            tracing::info!(job_id = %job_id, "descarga pausada con checkpoint durable")
                        }
                        Ok(larm_download::consumer::DownloadConsumerOutcome::Cancelled { job_id }) => {
                            tracing::info!(job_id = %job_id, "descarga cancelada")
                        }
                        Ok(larm_download::consumer::DownloadConsumerOutcome::Failed { job_id, reason }) => {
                            tracing::warn!(job_id = %job_id, reason = %reason, "falló la descarga")
                        }
                        Err(error) => tracing::error!(
                            reason = %error.details.reason,
                            "falló el consumidor durable de descargas"
                        ),
                    }
                    tokio::select! {
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() { break; }
                        }
                        _ = tokio::time::sleep(DOWNLOAD_POLL) => {}
                    }
                }
            });
        }
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(reason = %error, "falló un worker de descargas");
            }
        }
    })
}

fn spawn_runtime_instance_stop_consumer(
    service: Arc<larm_runtime::stop_service::RuntimeInstanceStopService>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match service.consume_queued(RUNTIME_INSTANCE_STOP_BATCH).await {
                Ok(outcomes) => {
                    for (job_id, outcome) in outcomes {
                        match outcome {
                            Ok(result) => tracing::info!(
                                job_id = %job_id,
                                ?result,
                                "parada de RuntimeInstance confirmada"
                            ),
                            Err(error) => tracing::warn!(
                                job_id = %job_id,
                                reason = %error.details.reason,
                                "no se pudo detener RuntimeInstance"
                            ),
                        }
                    }
                }
                Err(error) => tracing::error!(
                    reason = %error.details.reason,
                    "falló la consulta del journal de paradas RuntimeInstance"
                ),
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(RUNTIME_INSTANCE_STOP_POLL) => {}
            }
        }
    })
}

fn spawn_model_verification_consumer(
    worker: Arc<larm_models::verification::ModelVerificationWorker>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match worker.run_once().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => tracing::error!(
                    reason = %error.details.reason,
                    "falló el consumidor de verificaciones model.verify"
                ),
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(MODEL_VERIFY_POLL) => {}
            }
        }
    })
}

fn spawn_model_import_consumer(
    worker: Arc<larm_models::import_worker::ModelImportWorker>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match worker.run_once().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => tracing::error!(
                    reason = %error.details.reason,
                    "falló el consumidor de importaciones model.import"
                ),
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tokio::time::sleep(MODEL_IMPORT_POLL) => {}
            }
        }
    })
}

fn spawn_runtime_install_consumer(
    service: Arc<larm_runtime::install_service::RuntimeInstallService>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let first_recovery = tokio::time::Instant::now() + RUNTIME_INSTALL_RECOVERY_POLL;
        let mut recovery = tokio::time::interval_at(first_recovery, RUNTIME_INSTALL_RECOVERY_POLL);
        loop {
            match service.consume_queued(RUNTIME_INSTALL_BATCH).await {
                Ok(outcomes) => {
                    for (job_id, outcome) in outcomes {
                        if let Err(error) = outcome {
                            tracing::warn!(
                                job_id = %job_id,
                                reason = %error.details.reason,
                                "no se pudo consumir un job runtime.install"
                            );
                        }
                    }
                }
                Err(error) => tracing::error!(
                    reason = %error.details.reason,
                    "falló la consulta de jobs runtime.install"
                ),
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = recovery.tick() => {
                    match service.reconcile_running(RUNTIME_INSTALL_RECOVERY_BATCH).await {
                        Ok(outcomes) => {
                            for (job_id, outcome) in outcomes {
                                if let Err(error) = outcome {
                                    tracing::warn!(
                                        job_id = %job_id,
                                        reason = %error.details.reason,
                                        "no se pudo reconciliar un job runtime.install; conserva sus reservas"
                                    );
                                }
                            }
                        }
                        Err(error) => tracing::error!(
                            reason = %error.details.reason,
                            "falló la consulta de jobs runtime.install en ejecución"
                        ),
                    }
                }
                _ = tokio::time::sleep(RUNTIME_INSTALL_POLL) => {}
            }
        }
    })
}

/// `health/ready`: la base responde y tiene los pragmas del contrato.
struct DbReadiness(Db);

#[derive(Clone)]
struct DownloadStorageRefresh(larm_core::hardware::StorageRefresh);

#[async_trait]
impl larm_domain::ports::StorageObservationRefresher for DownloadStorageRefresh {
    async fn refresh(
        &self,
        ctx: &larm_domain::ports::MutationContext,
    ) -> Result<larm_domain::storage::StorageObservation, ApiError> {
        self.0
            .refresh(ctx)
            .await
            .map(|(observation, _)| observation)
    }
}

#[async_trait]
impl Readiness for DbReadiness {
    async fn ready(&self) -> Result<bool, ApiError> {
        match self.0.health().await {
            Ok(health) => Ok(health.is_healthy()),
            Err(error) => Err(ApiError::new(
                "UPSTREAM_UNAVAILABLE",
                "La base de datos no responde",
                "database_unreachable",
            )
            .retryable(true)
            .with_technical(error.technical_detail().to_string())),
        }
    }
}

struct SqlEventReplayStore {
    db: Db,
    retention_days: u32,
    replay_limit: u32,
}

fn event_envelope(event: larm_db::events::StoredEvent) -> EventEnvelope {
    EventEnvelope {
        schema_version: 1,
        event_id: event.event_id.to_string(),
        event_type: event.event_type,
        occurred_at: event.occurred_at,
        correlation_id: event.correlation_id,
        actor_id: event.actor_id,
        resource_type: event.resource_type,
        resource_id: event.resource_id,
        resource_version: event.resource_version,
        data: event.data,
    }
}

#[async_trait]
impl EventReplayStore for SqlEventReplayStore {
    async fn replay_after(&self, after: Option<i64>) -> Result<EventReplayBatch, ApiError> {
        let window = larm_db::events::replay_window(
            &self.db.pool,
            after,
            self.retention_days,
            self.replay_limit,
        )
        .await?;
        Ok(EventReplayBatch {
            events: window.events.into_iter().map(event_envelope).collect(),
            through_event_id: window.through_event_id,
        })
    }
}

/// Publica el prefijo del outbox que aceptó el bus y lo confirma después. Si
/// SQLite falla al confirmar o el daemon cae en ese punto, el siguiente ciclo
/// vuelve a enviar los mismos `event_id`; los consumidores deduplican por ID.
async fn dispatch_pending_events(
    db: &Db,
    bus: &EventBus,
) -> Result<usize, larm_domain::error::LarmError> {
    let events = larm_db::events::pending(&db.pool, EVENT_DISPATCH_BATCH).await?;
    let mut dispatched = Vec::with_capacity(events.len());
    for event in events {
        let event_id = event.event_id;
        let accepted = bus.publish(event_envelope(event));
        if !accepted {
            break;
        }
        dispatched.push(event_id);
    }
    larm_db::events::mark_dispatched(&db.pool, &dispatched).await?;
    Ok(dispatched.len())
}

/// Recupera filas no confirmadas del outbox y las publica al bus.
fn spawn_event_feed(db: Db, bus: EventBus, retention_days: u32) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = larm_db::events::prune_expired(&db.pool, retention_days).await {
            tracing::warn!(detail = %e.technical_detail(), "poda inicial de eventos fallida; se reintenta")
        }
        let mut retention = tokio::time::interval(EVENT_RETENTION_CHECK);
        retention.tick().await;
        loop {
            if let Err(e) = dispatch_pending_events(&db, &bus).await {
                tracing::warn!(detail = %e.technical_detail(), "despacho de eventos fallido; se reintenta")
            }
            tokio::select! {
                _ = tokio::time::sleep(EVENT_POLL) => {},
                _ = retention.tick() => {
                    if let Err(e) = larm_db::events::prune_expired(&db.pool, retention_days).await {
                        tracing::warn!(detail = %e.technical_detail(), "poda periódica de eventos fallida; se reintenta")
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::items_after_test_module, clippy::unwrap_used, clippy::panic)]
mod event_feed_tests {
    use super::*;
    use larm_db::{events::NewEvent, testing::temp_migrated};
    use larm_domain::ports::MutationContext;

    async fn append_event(db: &Db, event_type: &str) -> i64 {
        let mut tx = db.pool.begin().await.unwrap();
        let event_id = larm_db::events::append(
            &mut tx,
            &NewEvent {
                event_type,
                resource_type: "fixture",
                resource_id: "p029-dispatcher",
                resource_version: 1,
                data: serde_json::json!({"action": event_type}),
            },
            &MutationContext {
                actor_id: "p029-test".into(),
                correlation_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        event_id.parse().unwrap()
    }

    #[tokio::test]
    async fn reintenta_tras_fallo_de_ack_y_reinicio_sin_perder_ni_cambiar_ids() {
        let (directory, db) = temp_migrated().await.unwrap();
        let expected = vec![
            append_event(&db, "fixture.first").await,
            append_event(&db, "fixture.second").await,
        ];
        let bus_before_restart = EventBus::default();
        let mut first_receiver = bus_before_restart.subscribe();

        sqlx::query(
            "CREATE TRIGGER fail_event_ack BEFORE UPDATE ON event_outbox \
             BEGIN SELECT RAISE(ABORT, 'fixture ack failure'); END",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        assert!(dispatch_pending_events(&db, &bus_before_restart)
            .await
            .is_err());
        let first_delivery = vec![
            first_receiver
                .recv()
                .await
                .unwrap()
                .event_id
                .parse::<i64>()
                .unwrap(),
            first_receiver
                .recv()
                .await
                .unwrap()
                .event_id
                .parse::<i64>()
                .unwrap(),
        ];
        assert_eq!(first_delivery, expected);
        assert_eq!(
            larm_db::events::pending(&db.pool, 10).await.unwrap().len(),
            2
        );

        let database_path = directory.path().join("larm.sqlite");
        db.pool.close().await;
        let restarted = Db::open(&database_path).await.unwrap();
        larm_db::migrate::migrate(&restarted.pool).await.unwrap();
        sqlx::query("DROP TRIGGER fail_event_ack")
            .execute(&restarted.pool)
            .await
            .unwrap();

        let bus_after_restart = EventBus::default();
        let mut retry_receiver = bus_after_restart.subscribe();
        assert_eq!(
            dispatch_pending_events(&restarted, &bus_after_restart)
                .await
                .unwrap(),
            2
        );
        let retry_delivery = vec![
            retry_receiver
                .recv()
                .await
                .unwrap()
                .event_id
                .parse::<i64>()
                .unwrap(),
            retry_receiver
                .recv()
                .await
                .unwrap()
                .event_id
                .parse::<i64>()
                .unwrap(),
        ];
        assert_eq!(retry_delivery, first_delivery);
        assert!(larm_db::events::pending(&restarted.pool, 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            dispatch_pending_events(&restarted, &bus_after_restart)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn no_confirma_eventos_hasta_que_haya_un_suscriptor() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let expected = append_event(&db, "fixture.waiting").await;
        let bus = EventBus::default();
        assert_eq!(dispatch_pending_events(&db, &bus).await.unwrap(), 0);
        assert_eq!(
            larm_db::events::pending(&db.pool, 10).await.unwrap().len(),
            1
        );

        let mut receiver = bus.subscribe();
        assert_eq!(dispatch_pending_events(&db, &bus).await.unwrap(), 1);
        assert_eq!(
            receiver
                .recv()
                .await
                .unwrap()
                .event_id
                .parse::<i64>()
                .unwrap(),
            expected
        );
        assert!(larm_db::events::pending(&db.pool, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn replay_sqlite_proyecta_el_envelope_y_cursor_actual() {
        let (_directory, db) = temp_migrated().await.unwrap();
        let expected = append_event(&db, "fixture.persisted").await;
        let store = SqlEventReplayStore {
            db,
            retention_days: 30,
            replay_limit: larm_db::events::MAX_EVENT_REPLAY,
        };

        let replay = store.replay_after(Some(0)).await.unwrap();
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.events[0].event_id, expected.to_string());
        assert_eq!(replay.events[0].event_type, "fixture.persisted");
        assert_eq!(replay.events[0].schema_version, 1);
        assert_eq!(replay.through_event_id, expected);

        let fresh = store.replay_after(None).await.unwrap();
        assert!(fresh.events.is_empty());
        assert_eq!(fresh.through_event_id, expected);
    }
}

/// `larm-generic-adapter.exe` junto al ejecutable del daemon (P023). Si
/// falta, el alta declarativa responde `generic_worker_missing`.
fn generic_worker() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("larm-generic-adapter.exe")))
        .unwrap_or_default()
}

/// Sirve la API hasta `shutdown`.
pub async fn run(
    startup_settings: &Settings,
    file_settings: Option<&serde_json::Value>,
    environment_overrides: &serde_json::Value,
    cli_overrides: &serde_json::Value,
    data_owner: &larm_adapter_host::daemon_owner::DaemonOwnership,
    bridge_config: Option<&std::path::Path>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    data_owner
        .recheck_database(std::path::Path::new(
            &startup_settings.paths.database_relative_path,
        ))
        .map_err(|e| format!("propiedad del daemon: {}", e.technical_detail()))?;
    let data_root = data_owner.root();
    if !is_loopback(&startup_settings.api.bind) && !startup_settings.api.remote_enabled {
        return Err(format!(
            "bind {} no es loopback y remote_enabled=false",
            startup_settings.api.bind
        ));
    }
    let db = crate::admin::open_db(startup_settings, data_root)
        .await
        .map_err(|e| format!("base de datos: {}", e.technical_detail()))?;
    let initial_settings = Settings::compose(file_settings, None, None)
        .map_err(|error| format!("configuración base: {}", error.technical_detail()))?;
    let initial_settings_value = serde_json::to_value(&initial_settings)
        .map_err(|error| format!("serializando Settings inicial: {error}"))?;
    let settings_repository = Arc::new(larm_db::settings::SqlSettings {
        pool: db.pool.clone(),
    });
    let persisted_settings = settings_repository
        .get_or_initialize(initial_settings_value.clone())
        .await
        .map_err(|error| format!("leyendo Settings persistido: {}", error.technical_detail()))?;
    let settings = Settings::compose_with_base(
        Some(&persisted_settings.payload),
        file_settings,
        Some(environment_overrides),
        Some(cli_overrides),
    )
    .map_err(|error| {
        format!(
            "componiendo Settings persistido: {}",
            error.technical_detail()
        )
    })?;
    // P132: registro del arranque en el orden contractual. La protección de
    // crash loop del contrato 04 es para los procesos runtime/adapter
    // (supervisor `CrashRestartTracker`, 3 reinicios por 10 min con backoff
    // [1, 5, 15] y FAILED después), no para el arranque del propio daemon.
    let mut startup_journal = larm_core::startup::StartupJournal::begin();
    startup_journal
        .complete("database_migrations")
        .map_err(|error| format!("orden de arranque: {}", error.technical_detail()))?;
    let disk_threshold_state = Arc::new(parking_lot::RwLock::new((
        crate::hardware::thresholds(&settings),
        persisted_settings.revision,
    )));
    let bandwidth_limiter = Arc::new(larm_download::policy::BandwidthLimiter::new(
        settings
            .storage
            .download_bandwidth_limit_bytes_per_second
            .and_then(|value| std::num::NonZeroU64::new(value.get())),
    ));
    let principals = crate::admin::principals(&db);
    let secrets = Arc::new(KeyringSecrets);
    let process_generation = larm_runtime::external_adoption::process_generation();
    let lease_repository = larm_db::runtime_leases::SqlRuntimeLeases {
        pool: db.pool.clone(),
    };
    let lease_jobs = larm_db::jobs::SqlJournalJobs::broker_lease_admission(db.pool.clone());
    let recovery_context = larm_domain::ports::MutationContext {
        actor_id: "larm:lease-recovery".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let invalidated_leases = larm_runtime::lease_recovery::invalidate_previous_generation(
        &lease_repository,
        secrets.as_ref(),
        process_generation,
        &recovery_context,
    )
    .await
    .map_err(|error| {
        format!(
            "no se pudieron reconciliar las leases anteriores: {}",
            error.details.reason
        )
    })?;
    if invalidated_leases > 0 {
        tracing::info!(
            invalidated_leases,
            "leases de procesos anteriores invalidadas"
        );
    }
    let recovered_lease_jobs = lease_jobs.recover().await.map_err(|error| {
        format!(
            "no se pudieron recuperar los jobs de leases: {}",
            error.details.reason
        )
    })?;
    if recovered_lease_jobs > 0 {
        tracing::info!(
            recovered_lease_jobs,
            "jobs de admisión interrumpidos marcados FAILED"
        );
    }
    let cursors = Arc::new(CursorSigner::default());
    let cursors_for_storage = cursors.clone();
    let approvals = Arc::new(Approvals {
        repo: Arc::new(SqlApprovals {
            pool: db.pool.clone(),
        }),
        ttl: chrono::Duration::seconds(
            i64::try_from(settings.security.approval_ttl_seconds).unwrap_or(900),
        ),
    });
    let security = larm_api::security::SecurityState {
        auth: principals.clone(),
        credentials: Arc::new(Credentials {
            repo: Arc::new(SqlCredentials {
                pool: db.pool.clone(),
            }),
            secrets: secrets.clone(),
        }),
        approvals: approvals.clone(),
        idempotency: Arc::new(SqlIdempotency {
            pool: db.pool.clone(),
        }),
        cursors: cursors.clone(),
    };
    let refresh = crate::hardware::refresh_service(&db, disk_threshold_state.clone()).await;
    let storage = crate::hardware::locations(&db, disk_threshold_state.clone()).await;
    let verification_jobs = Arc::new(larm_db::model_verification::SqlModelVerificationJobs {
        pool: db.pool.clone(),
    });
    let import_jobs = Arc::new(larm_db::model_import::SqlModelImportJobs {
        pool: db.pool.clone(),
    });
    let download_jobs = Arc::new(larm_db::download_jobs::SqlDownloadJobs {
        pool: db.pool.clone(),
    });
    let transformation_jobs = Arc::new(larm_db::transformations::SqlTransformationJobs {
        pool: db.pool.clone(),
    });
    download_jobs
        .recover_interrupted()
        .await
        .map_err(|error| format!("download recovery: {}", error.technical_detail()))?;
    #[cfg(windows)]
    let mut transformation_recovery = None;
    if let Err(error) = transformation_jobs.recover_interrupted().await {
        tracing::error!(reason = %error.details.reason, "no se pudieron recuperar los jobs transformation");
    } else {
        #[cfg(windows)]
        match data_owner.retain() {
            Ok(scope) => {
                transformation_recovery = Some(crate::transformation_recovery::start(
                    transformation_jobs.clone(),
                    scope,
                ))
            }
            Err(error) => {
                tracing::error!(reason=%error.details.reason,"no se pudo retener ownership para la recuperación transformation")
            }
        }
    }
    if let Err(error) = import_jobs.recover_imports().await {
        tracing::error!(reason = %error.details.reason, "no se pudieron recuperar los jobs model.import");
    }
    if let Err(error) = verification_jobs.recover_verifications().await {
        tracing::error!(reason = %error.details.reason, "no se pudieron recuperar los jobs model.verify");
    }
    let mut admin = larm_api::security::routes(security).merge(larm_api::hardware::routes(
        larm_api::hardware::HardwareState {
            repo: Arc::new(larm_db::hardware::SqlHardware {
                pool: db.pool.clone(),
            }),
            cursors: cursors.clone(),
            refresh: refresh.clone(),
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        },
    ));
    let limiter_for_settings = bandwidth_limiter.clone();
    let live_thresholds_for_settings = disk_threshold_state.clone();
    let storage_refresh_for_settings = refresh.storage.clone();
    admin = admin.merge(larm_api::settings::routes(
        larm_api::settings::SettingsState {
            repository: settings_repository.clone(),
            initial: initial_settings_value,
            file_overrides: file_settings
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            environment_overrides: environment_overrides.clone(),
            cli_overrides: cli_overrides.clone(),
            apply_bandwidth_limit: Arc::new(move |limit| {
                limiter_for_settings
                    .set_limit(limit.and_then(|value| std::num::NonZeroU64::new(value.get())));
            }),
            apply_low_disk_thresholds: Arc::new(move |thresholds, revision| {
                *live_thresholds_for_settings.write() = (thresholds, revision);
                let Some(storage_refresh) = storage_refresh_for_settings.clone() else {
                    return;
                };
                tokio::spawn(async move {
                    let ctx = larm_domain::ports::MutationContext {
                        actor_id: "larm-daemon:settings-h09".into(),
                        correlation_id: uuid::Uuid::now_v7().to_string(),
                    };
                    if let Err(error) = storage_refresh.refresh(&ctx).await {
                        tracing::error!(
                            reason = %error.details.reason,
                            "no se pudo reevaluar el espacio tras cambiar los umbrales H09"
                        );
                    }
                });
            }),
        },
    ));
    admin = admin.merge(larm_api::downloads::routes(
        larm_api::downloads::DownloadsState {
            service: download_jobs.clone(),
            cursors: cursors.clone(),
        },
    ));
    admin = admin.merge(larm_api::download_admissions::routes(
        larm_api::download_admissions::AdmissionState {
            store: Arc::new(SqlAdmissionStore {
                pool: db.pool.clone(),
            }),
            cursors: cursors.clone(),
        },
    ));
    crate::hardware::start_on_boot(&refresh).await;
    let mut job_sources: Vec<Arc<dyn larm_domain::ports::JobSource>> = vec![
        refresh,
        verification_jobs.clone(),
        import_jobs.clone(),
        download_jobs.clone(),
        transformation_jobs.clone(),
    ];
    #[cfg(windows)]
    let native_transformations = if transformation_recovery.is_some() {
        match data_owner.retain().and_then(|scope| {
            crate::transformations::Service::new(
                db.clone(),
                data_root.join(&startup_settings.paths.database_relative_path),
                scope,
                crate::transformations::CurrentPolicy {
                    repository: larm_db::settings::SqlSettings {
                        pool: db.qualification_reader(),
                    },
                    file: file_settings
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                    environment: environment_overrides.clone(),
                    cli: cli_overrides.clone(),
                },
            )
        }) {
            Ok(service) => Some(service),
            Err(e) => {
                tracing::error!(reason=%e.details.reason,"motor público de transformaciones no disponible");
                None
            }
        }
    } else {
        None
    };
    #[cfg(windows)]
    let transformation_service: Arc<dyn larm_domain::ports::TransformationService> =
        match &native_transformations {
            Some(service) => Arc::new(service.clone()),
            None => transformation_jobs.clone(),
        };
    #[cfg(not(windows))]
    let transformation_service: Arc<dyn larm_domain::ports::TransformationService> =
        transformation_jobs;
    admin = admin.merge(larm_api::transformations::routes(
        larm_api::transformations::TransformationsState {
            service: transformation_service.clone(),
            cursors: cursors.clone(),
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        },
    ));
    if let Some((locations, probe)) = storage {
        job_sources.push(probe.clone());
        admin = admin.merge(larm_api::storage::routes(larm_api::storage::StorageState {
            locations,
            probe,
            cursors: cursors_for_storage.clone(),
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        }));
    } else {
        tracing::error!(
            "sin ruta del ejecutable: las ubicaciones de almacenamiento no están disponibles"
        );
    }
    if let Some(service) = crate::hardware::mover(&db, &settings).await {
        job_sources.push(service.clone());
        admin = admin.merge(larm_api::artifacts::routes(
            larm_api::artifacts::ArtifactsState {
                locations: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                    pool: db.pool.clone(),
                }),
                mover: Arc::new(larm_storage::move_job::MoveHandle(service)),
                approvals: approvals.clone(),
                cursors: cursors_for_storage.clone(),
                idempotency: Arc::new(SqlIdempotency {
                    pool: db.pool.clone(),
                }),
            },
        ));
    }
    let model_variants = Arc::new(larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    });
    let adapter_probes = crate::probes::open(&db);
    let download_sources = Arc::new(crate::download_execution::QualifiedDownloadSourceResolver {
        sources: larm_db::sources::SqlSources {
            pool: db.pool.clone(),
            qualification: Some(adapter_probes.runner.clone()),
        },
        conformance: larm_db::conformance::SqlAdapterConformance {
            pool: db.pool.clone(),
        },
        models: larm_db::models::SqlModels {
            pool: db.pool.clone(),
        },
        variants: model_variants.clone(),
        runner: adapter_probes.runner.clone(),
    });
    if let Some(storage_refresh) =
        crate::hardware::storage_refresh(&db, disk_threshold_state.clone())
    {
        let plan_repository = Arc::new(larm_db::download_plans::SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: Some(adapter_probes.runner.clone()),
        });
        let acquisition_catalog = Arc::new(larm_db::acquisition::SqlAcquisitionManifests {
            pool: db.pool.clone(),
            qualification: Some(adapter_probes.runner.clone()),
        });
        let execution_gate = Arc::new(crate::download_execution::QualifiedDownloadExecutionGate {
            sources: download_sources.clone(),
        });
        let model_repository = Arc::new(larm_db::models::SqlModels {
            pool: db.pool.clone(),
        });
        let mirror_catalog: Arc<dyn larm_domain::ports::DownloadMirrorCatalog> =
            Arc::new(crate::download_execution::QualifiedDownloadMirrorCatalog {
                variants: model_variants.clone(),
                models: model_repository.clone(),
                sources: larm_db::sources::SqlSources {
                    pool: db.pool.clone(),
                    qualification: None,
                },
                conformance: larm_db::conformance::SqlAdapterConformance {
                    pool: db.pool.clone(),
                },
            });
        let location_repository = Arc::new(larm_db::locations::SqlStorageLocations {
            pool: db.pool.clone(),
        });
        let reservations = Arc::new(larm_db::reservations::SqlVolumeReservations {
            pool: db.pool.clone(),
        });
        let storage = Arc::new(DownloadStorageRefresh(storage_refresh));
        let policy = larm_storage::estimate::SpacePolicy {
            margin: larm_domain::estimate::MarginPolicy {
                minimum_margin_bytes: settings.storage.minimum_margin_bytes.get(),
                margin_basis_points: settings.storage.margin_basis_points,
            },
            minimum_free_bytes: settings.storage.minimum_free_bytes.get(),
        };
        let activation_service: Arc<dyn larm_domain::ports::DownloadPlanActivationService> =
            Arc::new(larm_download::activation_service::DownloadPlanActivator {
                acquisitions: acquisition_catalog.clone(),
                plans: plan_repository.clone(),
                activation: plan_repository.clone(),
                variants: model_variants.clone(),
                models: model_repository.clone(),
                locations: location_repository.clone(),
                storage: storage.clone(),
                reservations: reservations.clone(),
                mirrors: mirror_catalog.clone(),
                gate: execution_gate.clone(),
                policy,
            });
        let idempotency: Arc<dyn larm_domain::ports::IdempotencyStore> = Arc::new(SqlIdempotency {
            pool: db.pool.clone(),
        });
        admin = admin.merge(larm_api::download_plans::routes(
            larm_api::download_plans::DownloadPlansState {
                preview: Arc::new(larm_download::preview_service::DownloadPlanPreview {
                    acquisitions: acquisition_catalog,
                    gate: execution_gate,
                    variants: model_variants.clone(),
                    models: model_repository,
                    locations: location_repository,
                    storage,
                    reservations,
                    plans: plan_repository,
                    mirrors: mirror_catalog,
                    policy,
                }),
                activation: activation_service,
                idempotency,
            },
        ));
    } else {
        tracing::error!("sin ruta del ejecutable: el preflight de descargas no está disponible");
    }
    let verification_prober: Arc<dyn larm_domain::ports::StorageProber> =
        crate::hardware::storage_prober()
            .map(|prober| Arc::new(prober) as Arc<dyn larm_domain::ports::StorageProber>)
            .unwrap_or_else(|| Arc::new(crate::hardware::UnavailableStorageProber));
    let verification_worker = Arc::new(larm_models::verification::ModelVerificationWorker::new(
        larm_models::verification::VerificationPorts {
            jobs: verification_jobs.clone(),
            variants: model_variants.clone(),
            artifact_locations: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            storage_locations: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
            prober: verification_prober,
        },
    ));
    let import_worker = Arc::new(larm_models::import_worker::ModelImportWorker::new(
        larm_models::import_worker::ModelImportPorts {
            jobs: import_jobs.clone(),
            source: import_jobs.clone(),
            models: Arc::new(larm_db::models::SqlModels {
                pool: db.pool.clone(),
            }),
            variants: model_variants.clone(),
            artifact_locations: Arc::new(larm_db::artifacts::SqlArtifactLocations {
                pool: db.pool.clone(),
            }),
            storage_locations: Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
        },
    ));
    admin = admin.merge(larm_api::model_import::routes(
        larm_api::model_import::ModelImportState {
            submitter: import_jobs.clone(),
        },
    ));
    admin = admin.merge(larm_api::import_preview::routes(
        larm_api::import_preview::ModelImportPreviewState {
            previewer: Arc::new(crate::model_import_preview::LocalModelImportPreview),
        },
    ));
    admin = admin.merge(larm_api::models::routes(larm_api::models::ModelsState {
        repo: Arc::new(larm_db::models::SqlModels {
            pool: db.pool.clone(),
        }),
        variants: model_variants,
        cursors: cursors_for_storage.clone(),
        idempotency: Arc::new(SqlIdempotency {
            pool: db.pool.clone(),
        }),
        verification: verification_jobs.clone(),
    }));
    admin = admin.merge(larm_api::compatibility::routes(
        larm_api::compatibility::CompatibilityState {
            repository: Arc::new(larm_db::compatibility::SqlCompatibilityRepository {
                pool: db.pool.clone(),
            }),
        },
    ));
    let source_probes = crate::source_probes::service(&db, adapter_probes.runner.clone());
    let source_watch_worker = Arc::new(crate::source_watches::worker(
        &db,
        adapter_probes.runner.clone(),
        &settings.watchers,
    ));
    let download_consumer = Arc::new(larm_download::consumer::DownloadConsumer {
        jobs: (*download_jobs).clone(),
        plans: larm_db::download_plans::SqlDownloadPlans {
            pool: db.pool.clone(),
            qualification: Some(adapter_probes.runner.clone()),
        },
        progress: larm_download::transfer::SqliteTransferProgress::new(
            larm_db::download_progress::SqlDownloadProgress {
                pool: db.pool.clone(),
            },
        ),
        sources: download_sources,
        storage: Arc::new(
            crate::download_execution::OnlineDownloadStorageRootResolver {
                locations: larm_db::locations::SqlStorageLocations {
                    pool: db.pool.clone(),
                },
            },
        ),
        bandwidth_limiter: bandwidth_limiter.clone(),
    });
    let runtimes = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let runtime_instances = Arc::new(larm_db::instances::SqlRuntimeInstances {
        pool: db.pool.clone(),
    });
    let runtime_instance_stops = Arc::new(
        larm_db::runtime_instance_stops::SqlRuntimeInstanceStopJobs {
            pool: db.pool.clone(),
        },
    );
    runtime_instance_stops
        .recover_stop_jobs()
        .await
        .map_err(|error| format!("runtime instance stop recovery: {error}"))?;
    job_sources.push(runtime_instance_stops.clone());
    let runtime_instance_sessions = RuntimeSessionRegistry::default();
    let runtime_instance_stop_service = Arc::new(
        larm_runtime::stop_service::RuntimeInstanceStopService::new(
            runtime_instances.clone(),
            runtime_instance_stops.clone(),
            Arc::new(NativeRuntimeProcessController::new(
                runtime_instance_sessions.clone(),
            )),
            larm_runtime::supervisor::DEFAULT_STOP_GRACE,
        )
        .map_err(|error| format!("runtime instance stop controller: {error}"))?,
    );
    let runtime_install_jobs = Arc::new(larm_db::runtimes::SqlRuntimeInstallJobs {
        pool: db.pool.clone(),
    });
    job_sources.push(runtime_install_jobs.clone());
    let runtime_profiles = Arc::new(larm_db::runtime_profiles::SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    });
    let runtime_qualifications =
        Arc::new(larm_db::runtime_profiles::SqlRuntimeProfileQualifications {
            pool: db.pool.clone(),
        });
    let runtime_install_service =
        Arc::new(larm_runtime::install_service::RuntimeInstallService::new(
            runtimes.clone(),
            runtimes.clone(),
            runtime_profiles.clone(),
            runtime_qualifications,
            Arc::new(larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
                runner: adapter_probes.runner.clone(),
                access: Arc::new(crate::runtime_discovery::ManifestDiscoveryAccess),
            }),
            runtime_install_jobs,
            crate::hardware::storage_prober().map(|prober| {
                Arc::new(
                    larm_storage::runtime_install::ManagedRuntimeInstallPublisher {
                        locations: Arc::new(larm_db::locations::SqlStorageLocations {
                            pool: db.pool.clone(),
                        }),
                        prober: Arc::new(prober),
                    },
                )
                    as Arc<dyn larm_domain::runtime_install::ManagedRuntimeInstallStore>
            }),
        ));
    admin = admin.merge(larm_api::runtimes::routes(
        larm_api::runtimes::RuntimesState {
            repo: runtimes.clone(),
            service: Arc::new(larm_runtime::catalog::RuntimeCatalog { repo: runtimes }),
            cursors: cursors_for_storage.clone(),
            install_submitter: runtime_install_service.clone(),
        },
    ));
    admin = admin.merge(larm_api::instances::routes(
        larm_api::instances::InstancesState {
            repo: runtime_instances.clone(),
            stop_jobs: runtime_instance_stops.clone(),
            cursors: cursors_for_storage.clone(),
        },
    ));
    admin = admin.merge(larm_api::launch_profiles::routes(
        larm_api::launch_profiles::LaunchProfilesState {
            repo: Arc::new(larm_db::launch_profiles::SqlLaunchProfiles {
                pool: db.pool.clone(),
            }),
            cursors: Arc::new(CursorSigner::default()),
        },
    ));
    admin = admin.merge(larm_api::runtime_profiles::routes(
        larm_api::runtime_profiles::RuntimeProfilesState {
            profiles: runtime_profiles,
        },
    ));
    admin = admin.merge(larm_api::custom_rules::routes(
        larm_api::custom_rules::CustomRulesState {
            repo: Arc::new(larm_db::custom_rules::SqlCustomRules {
                pool: db.pool.clone(),
            }),
        },
    ));
    admin = admin.merge(larm_api::sources::routes(larm_api::sources::SourcesState {
        catalog: crate::sources::catalog(&db, Some(adapter_probes.runner.clone())),
        cursors: cursors_for_storage.clone(),
    }));
    admin = admin.merge(larm_api::source_watches::routes(
        larm_api::source_watches::SourceWatchesState {
            repository: Arc::new(larm_db::source_watches::SqlSourceWatches {
                pool: db.pool.clone(),
            }),
            cursors: cursors_for_storage.clone(),
        },
    ));
    admin = admin.merge(larm_api::notifications::routes(
        larm_api::notifications::NotificationsState {
            repository: Arc::new(larm_db::notifications::SqlNotifications {
                pool: db.pool.clone(),
            }),
            cursors: cursors_for_storage.clone(),
        },
    ));
    admin = admin.merge(larm_api::source_accounts::routes(
        larm_api::source_accounts::SourceAccountsState {
            service: Arc::new(larm_core::source_accounts::SourceAccounts {
                repo: Arc::new(larm_db::source_accounts::SqlSourceAccounts {
                    pool: db.pool.clone(),
                }),
            }),
            cursors: cursors_for_storage.clone(),
        },
    ));
    let basic_credentials = Arc::new(larm_core::source_credentials::SourceBasicCredentials::new(
        crate::sources::catalog(&db, Some(adapter_probes.runner.clone()))
            .repo
            .clone(),
        Arc::new(larm_db::source_accounts::SqlSourceAccounts {
            pool: db.pool.clone(),
        }),
        Arc::new(SqlCredentials {
            pool: db.pool.clone(),
        }),
        secrets.clone(),
    ));
    admin = admin.merge(larm_api::source_credentials::routes(basic_credentials));
    job_sources.push(source_probes.clone());
    admin = admin.merge(larm_api::source_probes::routes(source_probes.clone()));
    let model_search = crate::model_search::api_service(
        Arc::new(crate::source_search::QualifiedSourceSearch::new(
            &db,
            adapter_probes.runner.clone(),
        )),
        cursors_for_storage.clone(),
        larm_models::search::SearchConfig::default(),
    );
    admin = admin.merge(larm_api::search::routes(larm_api::search::SearchState {
        service: model_search,
        idempotency: Arc::new(SqlIdempotency {
            pool: db.pool.clone(),
        }),
    }));
    if let Some(service) = crate::hardware::maintenance(&db, &settings) {
        job_sources.push(service.clone());
        admin = admin.merge(larm_api::maintenance::routes(
            larm_api::maintenance::MaintenanceState {
                service: Arc::new(larm_storage::cleanup::StorageHandle(service)),
                approvals: approvals.clone(),
                idempotency: Arc::new(SqlIdempotency {
                    pool: db.pool.clone(),
                }),
            },
        ));
    }
    // P133: updater y recuperación (rebuild en staging) sobre la base real.
    admin = admin.merge(larm_api::recovery::routes(
        larm_api::recovery::RecoveryState {
            ops: crate::recovery::state(db.pool.clone(), settings.clone(), data_root.to_path_buf()),
            approvals: approvals.clone(),
        },
    ));
    admin = admin.merge(larm_api::adapters::routes(
        larm_api::adapters::AdaptersState {
            registry: Arc::new(larm_core::adapters::DeclarativeRegistry {
                repo: Arc::new(larm_db::adapters::SqlAdapterInstallations {
                    pool: db.pool.clone(),
                }),
                packages_root: data_root.join("adapters"),
                worker_exe: generic_worker(),
            }),
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        },
    ));
    admin = admin.merge(larm_api::adapters::catalog_routes(
        larm_api::adapters::AdapterCatalogState {
            repo: Arc::new(larm_db::adapters::SqlAdapterInstallations {
                pool: db.pool.clone(),
            }),
            cursors: Arc::new(CursorSigner::default()),
        },
    ));
    let adapter_coordinator = crate::adapters::coordinator(&db, &settings, data_root, None)
        .map_err(|e| format!("configuración de adaptadores: {}", e.details.reason))?;
    adapter_coordinator
        .clone()
        .recover_started()
        .await
        .map_err(|e| format!("recuperación de adaptadores: {}", e.details.reason))?;
    let adapter_import_coordinator =
        crate::adapters::import_coordinator(&db, &settings, data_root, None).map_err(|e| {
            format!(
                "configuración de importación de adaptadores: {}",
                e.details.reason
            )
        })?;
    adapter_import_coordinator
        .clone()
        .recover_started()
        .await
        .map_err(|e| {
            format!(
                "recuperación de importación de adaptadores: {}",
                e.details.reason
            )
        })?;
    let adapter_installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    });
    let adapter_export = crate::adapters::PackageExportService::new(
        db.clone(),
        adapter_installations,
        adapter_coordinator.root().to_path_buf(),
    )
    .map_err(|e| format!("exportación de adaptadores: {}", e.details.reason))?;
    admin = admin.merge(larm_api::adapters::export_routes(
        larm_api::adapters::AdapterExportState {
            exporter: adapter_export,
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        },
    ));
    let adapter_registry =
        crate::adapters::RegistrySyncService::new(db.clone(), &settings.security).map_err(
            |error| format!("registro firmado de adaptadores: {}", error.details.reason),
        )?;
    admin = admin.merge(larm_api::adapters::registry_routes(
        larm_api::adapters::AdapterRegistryState {
            manager: adapter_registry,
            idempotency: Arc::new(SqlIdempotency {
                pool: db.pool.clone(),
            }),
        },
    ));
    let adapter_imports = larm_adapter_host::import_service::ImportService::new(
        adapter_import_coordinator,
        Arc::new(larm_db::adapter_imports::SqlAdapterImports {
            pool: db.pool.clone(),
        }),
    );
    job_sources.push(adapter_imports.clone());
    admin = admin.merge(larm_api::adapters::import_routes(adapter_imports.clone()));
    let adapter_updates = larm_adapter_host::update_service::UpdateService::new(
        adapter_coordinator,
        Arc::new(larm_db::adapter_updates::SqlAdapterUpdates {
            pool: db.pool.clone(),
        }),
    );
    job_sources.push(adapter_updates.clone());
    admin = admin.merge(larm_api::adapters::update_routes(adapter_updates.clone()));
    job_sources.push(adapter_probes.service.clone());
    admin = admin.merge(larm_api::adapters::probe_routes(
        adapter_probes.service.clone(),
    ));
    let _reconcile = crate::hardware::reconciler(&db)
        .map(|r| crate::hardware::spawn_reconcile(r, Duration::from_secs(300)));
    let mut gateway = axum::Router::new();

    let address = format!("{}:{}", settings.api.bind, settings.api.port);
    let sid = crate::bridge::current_user_sid();
    #[cfg(windows)]
    let _bootstrap = match &sid {
        Some(sid) => {
            let (sid, principals, base) =
                (sid.clone(), principals.clone(), format!("http://{address}"));
            Some(tokio::spawn(async move {
                if let Err(error) = crate::bootstrap::serve(sid, principals, base).await {
                    tracing::error!(error = %error, "el pipe de bootstrap se detuvo");
                }
            }))
        }
        None => {
            tracing::warn!("sin SID de usuario: no hay pipe de bootstrap");
            None
        }
    };

    let mut _pipe = None;
    let mut _sweeper = None;
    if let Some(path) = bridge_config {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("no se pudo leer {}: {e}", path.display()))?;
        let config: BridgeConfig =
            serde_json::from_str(&text).map_err(|e| format!("bridge-config inválido: {e}"))?;
        let adoption = larm_runtime::external_adoption::ExternalAdoptionBridge::new_in_generation(
            config.adoption,
            secrets,
            process_generation,
        )?
        .with_hardware(Arc::new(larm_db::hardware::SqlHardware {
            pool: db.pool.clone(),
        }))
        .with_lease_repository(Arc::new(lease_repository.clone()))
        .with_job_repository(Arc::new(lease_jobs.clone()))
        .with_reference_store(Arc::new(
            crate::adoption_references::SqlQualifiedAdoptionReferenceStore::new(db.pool.clone())?,
        ))
        .with_runtime_version_resolver(Arc::new(
            crate::adoption_references::SqlPinnedRuntimeVersionResolver::new(db.pool.clone()),
        ));
        _sweeper = Some(adoption.spawn_sweeper());
        let shared: Arc<dyn larm_core::bridge::BrokerBridge> = Arc::new(adoption);
        #[cfg(windows)]
        if let Some(sid) = &sid {
            let name = larm_runtime::credentials_pipe::pipe_name(sid);
            let (pipe_sid, pipe_name) = (sid.clone(), name.clone());
            let create: larm_runtime::credentials_pipe::PipeFactory = Arc::new(move |first| {
                crate::winsec::create_pipe(
                    &pipe_name,
                    &pipe_sid,
                    first,
                    u32::try_from(larm_runtime::credentials_pipe::MESSAGE_MAX_BYTES)
                        .unwrap_or(4096),
                )
            });
            let (auth, served) = (principals.clone(), shared.clone());
            tracing::info!(pipe = %name, "canal de credenciales de endpoint (solo este usuario)");
            _pipe = Some(tokio::spawn(async move {
                if let Err(error) =
                    larm_runtime::credentials_pipe::serve(create, auth, served).await
                {
                    tracing::error!(error = %error, "el canal de credenciales se detuvo");
                }
            }));
        }
        job_sources.push(Arc::new(larm_api::broker::BridgeJobs(shared.clone())));
        let state = larm_api::broker::ApiState {
            bridge: shared,
            auth: principals.clone(),
        };
        admin = admin.merge(larm_api::broker::admin_routes(state.clone()));
        gateway = gateway.merge(larm_api::broker::gateway_routes(state));
    }

    admin = admin.merge(larm_api::jobs::routes(Arc::new(job_sources)));
    admin = admin.merge(larm_api::jobs::cancellation_routes(
        runtime_install_service.clone(),
        transformation_service.clone(),
    ));
    let bus = EventBus::default().with_replay_store(Arc::new(SqlEventReplayStore {
        db: db.clone(),
        retention_days: settings.events.retention_days,
        replay_limit: settings.events.replay_limit,
    }));
    let bus_for_shutdown = bus.clone();
    let _feed = spawn_event_feed(db.clone(), bus.clone(), settings.events.retention_days);
    let app = larm_api::app::app(
        AppState {
            auth: principals,
            auth_limiter: std::sync::Arc::new(larm_api::auth::AuthRateLimiter::new()),
            readiness: Arc::new(DbReadiness(db)),
            bus,
            config: AppConfig {
                request_body_bytes: usize::try_from(settings.api.request_body_bytes)
                    .unwrap_or(1_048_576),
                request_timeout: Duration::from_secs(settings.api.request_timeout_seconds),
                allowed_origins: settings.security.allowed_origins.clone(),
                heartbeat: Duration::from_secs(settings.events.heartbeat_seconds),
            },
        },
        admin,
        gateway,
    );

    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .map_err(|e| format!("bind {address}: {e}"))?;
    tracing::info!(address = %address, broker = bridge_config.is_some(), "API escuchando");
    adapter_imports.start().await;
    adapter_updates.start().await;
    adapter_probes.service.start().await;
    source_probes.start().await;
    let closing = bus_for_shutdown;
    let updates_for_shutdown = adapter_updates.clone();
    let imports_for_shutdown = adapter_imports.clone();
    let probes_for_shutdown = adapter_probes.service.clone();
    let source_probes_for_shutdown = source_probes.clone();
    let (install_shutdown, install_shutdown_rx) = tokio::sync::watch::channel(false);
    let install_consumer =
        spawn_runtime_install_consumer(runtime_install_service, install_shutdown_rx);
    let (verification_shutdown, verification_shutdown_rx) = tokio::sync::watch::channel(false);
    let verification_consumer =
        spawn_model_verification_consumer(verification_worker, verification_shutdown_rx);
    let (import_shutdown, import_shutdown_rx) = tokio::sync::watch::channel(false);
    let import_consumer = spawn_model_import_consumer(import_worker, import_shutdown_rx);
    let (download_shutdown, download_shutdown_rx) = tokio::sync::watch::channel(false);
    let download_consumer_task = spawn_download_consumer(
        download_consumer,
        settings.storage.download_global_concurrency,
        settings.storage.download_per_source_concurrency,
        download_shutdown_rx,
    );
    let (source_watch_shutdown, source_watch_shutdown_rx) = tokio::sync::watch::channel(false);
    let source_watch_consumer =
        crate::source_watches::spawn_consumer(source_watch_worker, source_watch_shutdown_rx);
    let (runtime_stop_shutdown, runtime_stop_shutdown_rx) = tokio::sync::watch::channel(false);
    let runtime_stop_consumer = spawn_runtime_instance_stop_consumer(
        runtime_instance_stop_service,
        runtime_stop_shutdown_rx,
    );
    #[cfg(windows)]
    let transformation_recovery_shutdown = transformation_recovery.as_ref().map(|r| r.signal());
    #[cfg(windows)]
    let transformation_consumer = match (&native_transformations, &transformation_recovery) {
        (Some(service), Some(recovery)) => Some(service.spawn_consumer(recovery.completion())),
        _ => None,
    };
    #[cfg(windows)]
    let transformation_service_shutdown = native_transformations.clone();
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            closing.close();
            #[cfg(windows)]
            if let Some(service) = transformation_service_shutdown {
                service.begin_shutdown();
            }
            #[cfg(windows)]
            if let Some(signal) = transformation_recovery_shutdown {
                signal.send_replace(true);
            }

            let _ = install_shutdown.send(true);
            let _ = verification_shutdown.send(true);
            let _ = import_shutdown.send(true);
            let _ = download_shutdown.send(true);
            let _ = source_watch_shutdown.send(true);
            let _ = runtime_stop_shutdown.send(true);
            updates_for_shutdown.begin_shutdown().await;
            imports_for_shutdown.begin_shutdown().await;
            probes_for_shutdown.begin_shutdown().await;
            source_probes_for_shutdown.begin_shutdown().await;
        })
        .await
        .map_err(|e| format!("servidor HTTP: {e}"));
    #[cfg(windows)]
    if let Some(recovery) = transformation_recovery.take() {
        recovery
            .shutdown()
            .await
            .map_err(|e| format!("supervisor transformation: {}", e.details.reason))?;
    }
    #[cfg(windows)]
    if let Some(consumer) = transformation_consumer {
        consumer
            .await
            .map_err(|e| format!("consumidor transformation: {e}"))?;
    }
    #[cfg(windows)]
    if let Some(service) = native_transformations {
        service
            .shutdown()
            .await
            .map_err(|e| format!("cierre transformation: {}", e.details.reason))?;
    }
    install_consumer
        .await
        .map_err(|e| format!("consumidor runtime.install: {e}"))?;
    verification_consumer
        .await
        .map_err(|e| format!("consumidor model.verify: {e}"))?;
    import_consumer
        .await
        .map_err(|e| format!("consumidor model.import: {e}"))?;
    download_consumer_task
        .await
        .map_err(|e| format!("consumidor download: {e}"))?;
    source_watch_consumer
        .await
        .map_err(|e| format!("consumidor source watcher: {e}"))?;
    runtime_stop_consumer
        .await
        .map_err(|e| format!("consumidor runtime.instance.stop: {e}"))?;
    runtime_instance_sessions
        .close_all()
        .await
        .map_err(|error| format!("cierre de sesiones RuntimeInstance: {error}"))?;
    adapter_updates.finish_shutdown().await;
    adapter_imports.finish_shutdown().await;
    adapter_probes.service.finish_shutdown().await;
    source_probes.finish_shutdown().await;
    adapter_probes
        .runner
        .shutdown()
        .await
        .map_err(|e| format!("cierre de sondeos: {}", e.details.reason))?;
    served
}

#[cfg(test)]
mod download_consumer_supervision_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingThenIdleConsumer {
        calls: AtomicUsize,
        second_call: tokio::sync::Notify,
    }

    #[async_trait]
    impl DownloadConsumerRunner for FailingThenIdleConsumer {
        async fn run_cancellation_once(
            &self,
        ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
        {
            Ok(larm_download::consumer::DownloadConsumerOutcome::Idle)
        }
        async fn run_once_with_source_limit(
            &self,
            _per_source_limit: u32,
        ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
        {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 1 {
                return Ok(larm_download::consumer::DownloadConsumerOutcome::Failed {
                    job_id: "0190f5c0-7a00-7000-8000-000000000055".into(),
                    reason: "download_storage_full".into(),
                });
            }
            self.second_call.notify_one();
            Ok(larm_download::consumer::DownloadConsumerOutcome::Idle)
        }
    }

    #[tokio::test]
    async fn supervisor_continua_despues_de_fallo_de_espacio_y_cierra_con_shutdown(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let consumer = Arc::new(FailingThenIdleConsumer {
            calls: AtomicUsize::new(0),
            second_call: tokio::sync::Notify::new(),
        });
        let second_call = consumer.second_call.notified();
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        let task = spawn_download_consumer(consumer.clone(), 1, 1, shutdown_receiver);

        tokio::time::timeout(Duration::from_secs(3), second_call).await?;
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), task).await??;
        assert_eq!(consumer.calls.load(Ordering::SeqCst), 2);
        Ok(())
    }

    struct BusyConsumer {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        cleaned: tokio::sync::Notify,
    }

    #[async_trait]
    impl DownloadConsumerRunner for BusyConsumer {
        async fn run_once_with_source_limit(
            &self,
            _limit: u32,
        ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
        {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(larm_download::consumer::DownloadConsumerOutcome::Idle)
        }
        async fn run_cancellation_once(
            &self,
        ) -> Result<larm_download::consumer::DownloadConsumerOutcome, larm_domain::error::LarmError>
        {
            self.cleaned.notify_one();
            Ok(larm_download::consumer::DownloadConsumerOutcome::Idle)
        }
    }

    #[tokio::test]
    async fn limpieza_se_atiende_con_el_unico_worker_de_transferencia_ocupado(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let consumer = Arc::new(BusyConsumer {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            cleaned: tokio::sync::Notify::new(),
        });
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let task = spawn_download_consumer(consumer.clone(), 1, 1, receiver);
        tokio::time::timeout(Duration::from_secs(3), consumer.entered.notified()).await?;
        tokio::time::timeout(Duration::from_secs(3), consumer.cleaned.notified()).await?;
        sender.send_replace(true);
        consumer.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), task).await??;
        Ok(())
    }
}

/// Adaptador del repositorio SQL de admisiones al trait de la API.
struct SqlAdmissionStore {
    pool: sqlx::SqlitePool,
}

#[async_trait::async_trait]
impl larm_api::download_admissions::AdmissionStore for SqlAdmissionStore {
    async fn load(
        &self,
    ) -> Result<
        Option<(
            larm_domain::download_admission::DownloadAdmissionPolicy,
            i64,
            String,
        )>,
        larm_domain::error::LarmError,
    > {
        larm_db::download_admissions::SqlDownloadAdmissions {
            pool: self.pool.clone(),
        }
        .get_policy()
        .await
        .map(|stored| stored.map(|stored| (stored.policy, stored.row_version, stored.updated_at)))
    }

    async fn save(
        &self,
        policy: &larm_domain::download_admission::DownloadAdmissionPolicy,
        ctx: &larm_domain::ports::MutationContext,
    ) -> Result<(i64, String), larm_domain::error::LarmError> {
        larm_db::download_admissions::SqlDownloadAdmissions {
            pool: self.pool.clone(),
        }
        .put_policy(policy, ctx)
        .await
        .map(|stored| (stored.row_version, stored.updated_at))
    }

    async fn list(
        &self,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, larm_domain::error::LarmError> {
        larm_db::download_admissions::SqlDownloadAdmissions {
            pool: self.pool.clone(),
        }
        .list(limit)
        .await
        .and_then(|admissions| {
            admissions
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    larm_domain::error::LarmError::new(
                        larm_domain::error::ErrorCode::IntegrityFailed,
                        "Admisión persistida no supera la verificación",
                        "download_admission_corrupt",
                    )
                })
        })
    }

    async fn revoke(
        &self,
        id: &str,
        ctx: &larm_domain::ports::MutationContext,
    ) -> Result<serde_json::Value, larm_domain::error::LarmError> {
        larm_db::download_admissions::SqlDownloadAdmissions {
            pool: self.pool.clone(),
        }
        .revoke(id, ctx)
        .await
        .and_then(|admission| {
            serde_json::to_value(admission).map_err(|_| {
                larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::IntegrityFailed,
                    "Admisión persistida no supera la verificación",
                    "download_admission_corrupt",
                )
            })
        })
    }
}
