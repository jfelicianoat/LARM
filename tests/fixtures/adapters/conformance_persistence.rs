// Sólo fixtures CODE_PASS de persistencia. No ejecuta ni cualifica workers.
use larm_domain::adapters::{AdapterInstallation, AdapterState};
use larm_domain::conformance::{hash, BindingHashes, Observation, Outcome, Proof, Run};
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterInstallationRepository, MutationContext,
};

/// Puerto sintético sólo para tests transaccionales, sin archivos ni workers.
/// Nunca se registra en la composición del daemon o las cinco recetas nativas.
pub fn physical_gate_fixture(
) -> Option<std::sync::Arc<dyn larm_domain::ports::AdapterQualification>> {
    Some(std::sync::Arc::new(PersistenceGate))
}
struct PersistenceGate;
struct PersistenceGuard;
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualification for PersistenceGate {
    async fn acquire(
        &self,
        _: &AdapterInstallation,
        _: &Run,
        _: &MutationContext,
    ) -> Result<Box<dyn larm_domain::ports::AdapterQualificationGuard>, larm_domain::error::LarmError>
    {
        Ok(Box::new(PersistenceGuard))
    }
}
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualificationGuard for PersistenceGuard {
    async fn recheck(&self) -> Result<(), larm_domain::error::LarmError> {
        Ok(())
    }
}

pub async fn qualify_fixture(
    pool: &sqlx::SqlitePool,
    installation: &AdapterInstallation,
    ctx: &MutationContext,
) -> AdapterInstallation {
    let installation = larm_db::adapters::SqlAdapterInstallations { pool: pool.clone() }
        .get(&installation.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(installation.state, AdapterState::Installed);
    let run = Run::new(
        &installation,
        BindingHashes {
            suite_sha256: "b".repeat(64),
            capability_profile_sha256: "c".repeat(64),
            execution_context_sha256: "d".repeat(64),
        },
        &ctx.actor_id,
    )
    .unwrap();
    let observations = run
        .required_checks()
        .into_iter()
        .map(|id| Observation {
            evidence_sha256: hash(
                &serde_json::json!({"persistence_fixture_only": id, "run": run.id}),
            )
            .unwrap(),
            check_id: id,
            passed: true,
        })
        .collect();
    let proof = Proof::evaluate(&run, observations).unwrap();
    larm_db::conformance::SqlAdapterConformance { pool: pool.clone() }
        .commit_conformance(&run, &Outcome::Passed { proof }, ctx)
        .await
        .unwrap();
    larm_db::adapters::SqlAdapterInstallations { pool: pool.clone() }
        .get(&installation.id)
        .await
        .unwrap()
        .unwrap()
}
