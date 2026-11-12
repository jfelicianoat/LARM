//! Orquestación pura con puertos de fixture. No es cualificación de workers.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic)]
use async_trait::async_trait;
use larm_core::conformance::AdapterConformance;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{
    hash, BindingHashes, Execution, Observation, Outcome, Proof, Receipt, Run,
};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterConformanceRunner, AdapterInstallationRepository,
    MutationContext,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct Fixture {
    row: Mutex<Option<AdapterInstallation>>,
    mode: &'static str,
    executed: AtomicUsize,
    committed: AtomicUsize,
    category: Mutex<Option<String>>,
}
#[async_trait]
impl AdapterInstallationRepository for Fixture {
    async fn get(&self, _: &str) -> Result<Option<AdapterInstallation>, LarmError> {
        Ok(self.row.lock().unwrap().clone())
    }
    async fn find(&self, _: &str, _: &str) -> Result<Option<AdapterInstallation>, LarmError> {
        panic!("not used")
    }
    async fn insert(&self, _: &AdapterInstallation, _: &MutationContext) -> Result<(), LarmError> {
        panic!("not used")
    }
}
#[async_trait]
impl AdapterConformanceRunner for Fixture {
    async fn wait_conformance(&self, _: &str) -> Result<(), LarmError> {
        Ok(())
    }

    async fn execute_conformance(
        &self,
        a: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<Execution, LarmError> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(self.committed.load(Ordering::SeqCst), 0);
        if self.mode == "denied" {
            return Err(LarmError::new(
                ErrorCode::ApprovalRequired,
                "No grant",
                "trusted_only_approval_required",
            ));
        }
        let mut run = Run::new(
            a,
            BindingHashes {
                suite_sha256: "b".repeat(64),
                capability_profile_sha256: "c".repeat(64),
                execution_context_sha256: "d".repeat(64),
            },
            &ctx.actor_id,
        )
        .unwrap();
        if self.mode == "different" {
            run.installation_sha256 = "f".repeat(64);
        }
        if self.mode == "different-actor" {
            run.actor_id = "other actor".into();
        }
        let observations = run
            .required_checks()
            .into_iter()
            .map(|check_id| Observation {
                check_id,
                evidence_sha256: "e".repeat(64),
                passed: true,
            })
            .collect();
        let proof = if self.mode == "failed" {
            Err(LarmError::new(
                ErrorCode::Other("vendor.fixture.error".into()),
                "Original safe error",
                "fixture_failed",
            ))
        } else {
            Proof::evaluate(&run, observations)
        };
        Ok(Execution { run, proof })
    }
}
#[async_trait]
impl AdapterConformanceRepository for Fixture {
    async fn commit_probe_conformance(
        &self,
        _: &str,
        run: &Run,
        outcome: &Outcome,
        ctx: &MutationContext,
    ) -> Result<Receipt, LarmError> {
        self.commit_conformance(run, outcome, ctx).await
    }

    async fn commit_conformance(
        &self,
        run: &Run,
        outcome: &Outcome,
        _: &MutationContext,
    ) -> Result<Receipt, LarmError> {
        assert_eq!(self.executed.load(Ordering::SeqCst), 1);
        outcome.check_run(run)?;
        self.committed.fetch_add(1, Ordering::SeqCst);
        if self.mode == "stale" {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "Changed",
                "conformance_snapshot_stale",
            ));
        }
        if let Outcome::Failed { error_code } = outcome {
            *self.category.lock().unwrap() = Some(error_code.clone());
        }
        Ok(Receipt {
            run_id: run.id.clone(),
            installation_id: run.installation_id.clone(),
            installation_revision: "2".into(),
            committed_installation_sha256: "e".repeat(64),
            package_sha256: run.package_sha256.clone(),
            run_sha256: run.digest()?,
            outcome_sha256: hash(outcome)?,
            evidence_sha256: outcome.evidence_hash()?,
            passed: outcome.passed(),
            completed_at: "2026-10-18T00:00:00.000Z".into(),
        })
    }
    async fn conformance_receipt(&self, _: &str) -> Result<Option<Receipt>, LarmError> {
        panic!("not used")
    }
}
fn setup(mode: &'static str) -> (Arc<Fixture>, AdapterConformance, String) {
    let methods = larm_adapter_sdk::manifest::MethodTable::contract()
        .methods
        .iter()
        .filter(|m| m.required && (m.family == "all" || m.family == "runtime"))
        .map(|m| m.method.clone())
        .collect::<Vec<_>>();
    let id = uuid::Uuid::now_v7().to_string();
    let row = AdapterInstallation {
        id: id.clone(),
        created_at: "2026-10-18T00:00:00.000Z".into(),
        updated_at: "2026-10-18T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "fixture.core".into(),
        version: "1.0.0".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "Core fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.core","version":"1.0.0","family":"runtime","protocol_version":"1.0","publisher":"Core fixture","executable":{"relative_path":"bin/fixture.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":methods,"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":[],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]}),
    };
    let fixture = Arc::new(Fixture {
        row: Mutex::new(Some(row)),
        mode,
        executed: AtomicUsize::new(0),
        committed: AtomicUsize::new(0),
        category: Mutex::new(None),
    });
    let service = AdapterConformance {
        installations: fixture.clone(),
        evidence: fixture.clone(),
        runner: fixture.clone(),
    };
    (fixture, service, id)
}
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "fixture-core".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
#[tokio::test]
async fn solo_confirma_despues_de_ejecutar_con_instalacion_registrada() {
    let (fixture, service, id) = setup("ok");
    let result = service.validate(&id, &ctx()).await.unwrap();
    assert!(result.passed);
    assert_eq!(fixture.executed.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.committed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn id_invalido_ausencia_revocado_estado_y_manifiesto_ambiguo_no_ejecutan() {
    for case in 0..9 {
        let (fixture, service, mut id) = setup("ok");
        let mut context = ctx();
        match case {
            0 => id = "invalid".into(),
            1 => *fixture.row.lock().unwrap() = None,
            2 => fixture.row.lock().unwrap().as_mut().unwrap().trust = AdapterTrust::Revoked,
            3 => fixture.row.lock().unwrap().as_mut().unwrap().state = AdapterState::Retired,
            4 => fixture.row.lock().unwrap().as_mut().unwrap().manifest["unexpected"] = json!(true),
            5 => fixture.row.lock().unwrap().as_mut().unwrap().publisher = "other".into(),
            6 => context.actor_id.clear(),
            7 => context.correlation_id = "invalid".into(),
            _ => context.actor_id = "control\nactor".into(),
        }
        assert!(service.validate(&id, &context).await.is_err());
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.committed.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn permiso_denegado_o_snapshot_de_otro_paquete_no_producen_commit() {
    for mode in ["denied", "different", "different-actor"] {
        let (fixture, service, id) = setup(mode);
        assert!(service.validate(&id, &ctx()).await.is_err());
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.committed.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn conserva_error_original_mientras_auditoria_solo_guarda_categoria_segura() {
    let (fixture, service, id) = setup("failed");
    let error = service.validate(&id, &ctx()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Other("vendor.fixture.error".into()));
    assert_eq!(error.message, "Original safe error");
    assert_eq!(fixture.committed.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.category.lock().unwrap().as_deref(),
        Some("INTERNAL")
    );
}
#[tokio::test]
async fn conflicto_de_persistencia_no_se_presenta_como_validacion_aprobada() {
    let (fixture, service, id) = setup("stale");
    let error = service.validate(&id, &ctx()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleRevision);
    assert_eq!(fixture.committed.load(Ordering::SeqCst), 1);
}
