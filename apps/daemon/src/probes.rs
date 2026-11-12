//! Composition root del sondeo público. Sólo el host registra suites compiladas.
use larm_adapter_host::conformance::{service::ProbeService, NativeConformanceRunner};
use larm_domain::ports::AdapterConformanceRunner;
use std::sync::Arc;
pub fn service(
    db: &larm_db::pool::Db,
    runner: Arc<dyn AdapterConformanceRunner>,
) -> Arc<ProbeService> {
    ProbeService::new(
        Arc::new(larm_db::adapter_probes::SqlAdapterProbes {
            pool: db.pool.clone(),
        }),
        Arc::new(larm_core::conformance::AdapterConformance {
            installations: Arc::new(larm_db::adapters::SqlAdapterInstallations {
                pool: db.pool.clone(),
            }),
            evidence: Arc::new(larm_db::conformance::SqlAdapterConformance {
                pool: db.pool.clone(),
            }),
            runner,
        }),
    )
}
pub struct Probes {
    pub service: Arc<ProbeService>,
    pub runner: Arc<NativeConformanceRunner>,
}
pub fn runner(db: &larm_db::pool::Db) -> Arc<NativeConformanceRunner> {
    let catalog = Arc::new(crate::probe_profiles::PersistentTargets {
        repo: larm_db::conformance_profiles::SqlConformanceProfiles {
            // Physical gates run while catalog/admission holds SQLite's writer.
            // Do not queue their profile reads behind waiting ordinary writers.
            pool: db.qualification_reader(),
        },
    });
    Arc::new(NativeConformanceRunner::new(
        Arc::new(larm_adapter_host::policy::Policy {
            trust: Default::default(),
            roots: larm_adapter_sdk::permissions::RootMap(vec![]),
            elevation: Arc::new(larm_adapter_host::policy::WhoAmI),
        }),
        catalog.clone(),
        Default::default(),
    ))
}
pub fn open(db: &larm_db::pool::Db) -> Probes {
    let runner = runner(db);
    Probes {
        service: service(db, runner.clone()),
        runner,
    }
}
