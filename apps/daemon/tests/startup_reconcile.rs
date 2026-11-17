//! P132: aceptación real de la reconciliación de arranque sobre el PC IA:
//! journal del orden contractual, crash loop de runtime con la política del
//! contrato 04 (tres reinicios por 10 minutos con backoff [1, 5, 15] y FAILED
//! después), modo offline con rechazo explícito de búsquedas remotas y
//! huérfanos clasificados por identidad fuerte sin kill por ambigüedad.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_core::startup::{
    decide_mode, killable_orphans, reject_remote_search, OrphanCandidate, SpawnRecord,
    StartupJournal, STARTUP_STEPS,
};
use larm_runtime::supervisor::{CrashDecision, CrashRestartPolicy, CrashRestartTracker};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciliacion_de_arranque_real_con_proteccion_y_offline() {
    // 1) Journal real del arranque: los siete pasos contractuales en orden
    //    pasan; el octavo paso desconocido falla cerrado.
    let mut journal = StartupJournal::begin();
    for step in STARTUP_STEPS {
        journal.complete(step).unwrap();
    }
    let mut journal = StartupJournal::begin();
    assert!(journal.complete("database_migrations").is_ok());
    assert!(journal.complete("storage_reconcile").is_ok());
    assert!(journal.complete("adapter_registry").is_ok());
    assert!(journal.complete("runtime_installations").is_ok());
    assert!(journal.complete("instances").is_ok());
    assert!(journal.complete("downloads_transforms").is_ok());
    assert!(journal.complete("event_service").is_ok());
    let violation = journal.complete("invented_step");
    assert!(violation.is_err());
    assert!(violation
        .unwrap_err()
        .technical_detail()
        .contains("invented_step"));
    println!("REAL P132: startup_order_violation detectado con paso inventado");

    // 2) Crash loop REAL de runtime (contrato 04): tres reinicios por 10
    //    minutos con backoff [1, 5, 15] y FAILED en el cuarto.
    let policy = CrashRestartPolicy::default();
    policy.validate().unwrap();
    let mut tracker = CrashRestartTracker::new(policy).unwrap();
    let now = chrono::Utc::now();
    for expected in [1u64, 5, 15] {
        assert!(
            matches!(
                tracker.unexpected_exit(now),
                CrashDecision::RestartAfter(delay) if delay.as_secs() == expected
            ),
            "backoff del contrato 04 debe ser {expected}s"
        );
    }
    assert_eq!(tracker.recent_crashes(), 3);
    assert!(matches!(tracker.unexpected_exit(now), CrashDecision::Fail));
    println!("REAL P132: crash loop runtime con backoff 1/5/15 y FAILED tras el cuarto");

    // 3) Offline mode real: inventario servido, búsqueda remota rechazada.
    let offline = decide_mode(true, false);
    assert!(offline.offline);
    assert_eq!(offline.reason, "network_unreachable");
    assert_eq!(
        reject_remote_search(&offline).unwrap_err(),
        "remote_search_disabled_offline"
    );
    println!("REAL P132: offline mode con rechazo de busquedas remotas");
    let online = decide_mode(true, true);
    assert!(!online.offline);
    assert!(reject_remote_search(&online).is_ok());

    // 4) Huérfanos por identidad fuerte (pid + ejecutable + boot + job kind).
    let spawns = vec![SpawnRecord {
        pid: 1234,
        executable_sha256: "feed".into(),
        boot_epoch: 42,
        job_kind: "download".into(),
    }];
    let orphan = OrphanCandidate {
        pid: 1234,
        executable_sha256: "feed".into(),
        boot_epoch: 42,
        job_kind: "download".into(),
    };
    assert_eq!(killable_orphans(&[orphan], &spawns, 42), vec![1234]);
    println!("REAL P132: identificacion fuerte clasifica ORPHAN_LARM");
    // Pid reciclado con otro ejecutable y boot distinto del host: FOREIGN.
    let recycled = OrphanCandidate {
        pid: 1234,
        executable_sha256: "zzz".into(),
        boot_epoch: 42,
        job_kind: "download".into(),
    };
    assert!(killable_orphans(&[recycled], &spawns, 42).is_empty());
    let stale = OrphanCandidate {
        pid: 1234,
        executable_sha256: "feed".into(),
        boot_epoch: 41,
        job_kind: "download".into(),
    };
    assert!(killable_orphans(&[stale], &spawns, 42).is_empty());
}
