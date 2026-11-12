//! Persistencia/atomicidad real; las observaciones de Proof son CODE_PASS.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::{adapter_probes::SqlAdapterProbes, conformance::SqlAdapterConformance};
use larm_domain::adapter_probes::ProbeJob;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::JobPhase;
use larm_domain::conformance::{hash, BindingHashes, Observation, Outcome, Proof, Run};
use larm_domain::error::ErrorCode;
use larm_domain::hardware::Probe;
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterInstallationRepository, AdapterProbeRepository,
    MutationContext,
};
use serde_json::json;
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "probe-sqlite-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn request() -> Probe {
    Probe {
        resource_id: None,
        profile_id: None,
    }
}
async fn seed(db: &larm_db::pool::Db) -> AdapterInstallation {
    let mut a = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-19T00:00:00.000Z".into(),
        updated_at: "2026-10-19T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "fixture.probe".into(),
        version: "1.0".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.probe","version":"1.0","family":"runtime","package_sha256":"a".repeat(64),"publisher":"fixture","capabilities":[],"permissions":{}}),
    };
    a.version = a.id.clone();
    a.manifest["version"] = json!(a.version);
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&a, &ctx())
    .await
    .unwrap();
    a
}
fn run(a: &AdapterInstallation, job: &ProbeJob) -> Run {
    Run::new(
        a,
        BindingHashes {
            suite_sha256: "b".repeat(64),
            capability_profile_sha256: "c".repeat(64),
            execution_context_sha256: "d".repeat(64),
        },
        &job.actor_id,
    )
    .unwrap()
}
fn passed(run: &Run) -> Outcome {
    Outcome::Passed {
        proof: Proof::evaluate(
            run,
            run.required_checks()
                .into_iter()
                .map(|id| Observation {
                    evidence_sha256: hash(&json!({"CODE_PASS":id})).unwrap(),
                    check_id: id,
                    passed: true,
                })
                .collect(),
        )
        .unwrap(),
    }
}
async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM idempotency)").fetch_one(&db.pool).await.unwrap()
}
async fn installed(db: &larm_db::pool::Db, id: &str) -> AdapterInstallation {
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .get(id)
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn acepta_replay_aislado_por_actor_destino_y_cuerpo_sin_duplicar_ni_reautorizar() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let b = seed(&db).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let c = ctx();
    let first = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "key", &c)
        .await
        .unwrap();
    let before = counts(&db).await;
    assert_eq!(
        repo.enqueue_probe(&a.id, &request(), &"a".repeat(64), "key", &ctx())
            .await
            .unwrap()
            .accepted(),
        first.accepted()
    );
    for (id, hash) in [(&a.id, "b".repeat(64)), (&b.id, "a".repeat(64))] {
        assert_eq!(
            repo.enqueue_probe(id, &request(), &hash, "key", &c)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
    }
    assert_eq!(
        repo.enqueue_probe(&a.id, &request(), &"a".repeat(64), "another", &c)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(counts(&db).await, before);
    let mut other = ctx();
    other.actor_id = "other".into();
    assert_eq!(
        repo.enqueue_probe(&a.id, &request(), &"a".repeat(64), "key", &other)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let claimed = repo.claim_probe().await.unwrap().unwrap();
    assert_eq!(claimed.id, first.id);
    assert!(repo.claim_probe().await.unwrap().is_none());
    let error = larm_domain::error::LarmError::new(
        ErrorCode::Other("private-marker".into()),
        "private-message",
        "private-reason",
    );
    repo.fail_probe(&first.id, &error, false).await.unwrap();
    let failed = repo.probe_job(&first.id).await.unwrap().unwrap();
    assert_eq!(failed.phase, JobPhase::Failed);
    assert!(!serde_json::to_string(&failed).unwrap().contains("private-"));
    assert_eq!(installed(&db, &a.id).await, a);
    let replay = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "key", &c)
        .await
        .unwrap();
    assert_eq!(replay.accepted(), first.accepted());
    assert_eq!(
        repo.enqueue_probe(&a.id, &request(), &"a".repeat(64), "key", &other)
            .await
            .unwrap()
            .actor_id,
        "other"
    );
}

#[tokio::test]
async fn conformidad_y_job_terminal_se_confirman_juntos_y_replay_no_escribe() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let c = ctx();
    let job = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "atomic", &c)
        .await
        .unwrap();
    repo.claim_probe().await.unwrap();
    let run = run(&a, &job);
    let outcome = passed(&run);
    let evidence = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let receipt = evidence
        .commit_probe_conformance(&job.id, &run, &outcome, &c)
        .await
        .unwrap();
    let done = repo.probe_job(&job.id).await.unwrap().unwrap();
    assert_eq!(done.phase, JobPhase::Completed);
    assert_eq!(done.receipt, Some(receipt.clone()));
    assert_eq!(installed(&db, &a.id).await.state, AdapterState::Validated);
    let before = counts(&db).await;
    assert_eq!(
        evidence
            .commit_probe_conformance(&job.id, &run, &outcome, &c)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(counts(&db).await, before);
    assert_eq!(before.1, before.2);
    sqlx::query(
        "UPDATE operation_journal SET payload=json_set(payload,'$.actor_id','tampered') WHERE id=?",
    )
    .bind(&job.id)
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(repo.probe_job(&job.id).await.is_err());
}

#[tokio::test]
async fn fallo_en_job_terminal_o_outbox_revierte_tambien_validated_y_recibo() {
    for fault in ["CREATE TRIGGER probe_fault BEFORE UPDATE ON operation_journal WHEN NEW.kind='adapter.probe' AND NEW.phase='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned_probe_final'); END","CREATE TRIGGER probe_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned_probe_outbox'); END"]{
        let (_dir,db)=larm_db::testing::temp_migrated().await.unwrap();let a=seed(&db).await;let repo=SqlAdapterProbes{pool:db.pool.clone()};let c=ctx();let job=repo.enqueue_probe(&a.id,&request(),&"a".repeat(64),"fault",&c).await.unwrap();repo.claim_probe().await.unwrap();
        let run=run(&a,&job);let outcome=passed(&run);let before=counts(&db).await;
        sqlx::query(sqlx::AssertSqlSafe(fault.to_string())).execute(&db.pool).await.unwrap();let evidence=SqlAdapterConformance{pool:db.pool.clone()};assert!(evidence.commit_probe_conformance(&job.id,&run,&outcome,&c).await.is_err());
        assert_eq!(counts(&db).await,before);assert_eq!(installed(&db,&a.id).await,a);assert_eq!(repo.probe_job(&job.id).await.unwrap().unwrap().phase,JobPhase::Running);
        sqlx::query("DROP TRIGGER probe_fault").execute(&db.pool).await.unwrap();evidence.commit_probe_conformance(&job.id,&run,&outcome,&c).await.unwrap();assert_eq!(repo.probe_job(&job.id).await.unwrap().unwrap().phase,JobPhase::Completed);
    }
}

#[tokio::test]
async fn fallo_de_aceptacion_y_claim_no_deja_replay_eventos_ni_queued_perdido() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let c = ctx();
    let before = counts(&db).await;
    sqlx::query("CREATE TRIGGER probe_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned'); END").execute(&db.pool).await.unwrap();
    assert!(repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "rollback", &c)
        .await
        .is_err());
    assert_eq!(counts(&db).await, before);
    sqlx::query("DROP TRIGGER probe_fault")
        .execute(&db.pool)
        .await
        .unwrap();
    let job = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "rollback", &c)
        .await
        .unwrap();
    let saved = counts(&db).await;
    sqlx::query("CREATE TRIGGER probe_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned'); END").execute(&db.pool).await.unwrap();
    assert!(repo.claim_probe().await.is_err());
    assert_eq!(counts(&db).await, saved);
    assert_eq!(
        repo.probe_job(&job.id).await.unwrap().unwrap().phase,
        JobPhase::Queued
    );
}

#[tokio::test]
async fn rechazo_de_snapshot_actor_o_job_incorrectos_no_concede_autoridad() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let c = ctx();
    let job = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "binding", &c)
        .await
        .unwrap();
    repo.claim_probe().await.unwrap();
    let run = run(&a, &job);
    let evidence = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let before = counts(&db).await;
    let mut wrong = ctx();
    wrong.actor_id = "other".into();
    assert!(evidence
        .commit_probe_conformance(&job.id, &run, &passed(&run), &wrong)
        .await
        .is_err());
    assert!(evidence
        .commit_probe_conformance(&uuid::Uuid::now_v7().to_string(), &run, &passed(&run), &c)
        .await
        .is_err());
    assert_eq!(counts(&db).await, before);
    assert_eq!(installed(&db, &a.id).await, a);
    let outcome = Outcome::failed(&ErrorCode::Timeout);
    evidence
        .commit_probe_conformance(&job.id, &run, &outcome, &c)
        .await
        .unwrap();
    let failed = repo.probe_job(&job.id).await.unwrap().unwrap();
    assert_eq!(failed.phase, JobPhase::Failed);
    assert_eq!(failed.error.unwrap().code, ErrorCode::Timeout);
    assert_eq!(installed(&db, &a.id).await.state, AdapterState::Installed);
}

#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
mod persistence;
#[tokio::test]
async fn sondeo_activo_o_fallido_sin_confirmacion_bloquea_recibo_historico() {
    use larm_domain::{adapters::AdapterSelectionChange, ports::AdapterSelectionRepository};
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let c = ctx();
    let a = persistence::qualify_fixture(&db.pool, &a, &c).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let selection = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let change = AdapterSelectionChange::Promote {
        installation_id: a.id.clone(),
    };
    let job = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "revalidate", &c)
        .await
        .unwrap();
    assert_eq!(
        selection
            .change_selection(&a.adapter_id, 0, &change, &c)
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_probe_active"
    );
    repo.claim_probe().await.unwrap();
    repo.fail_probe(
        &job.id,
        &larm_domain::error::LarmError::new(ErrorCode::Internal, "failed commit", "owned_failure"),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        selection
            .change_selection(&a.adapter_id, 0, &change, &c)
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_probe_unconfirmed"
    );
    let fresh = repo
        .enqueue_probe(&a.id, &request(), &"b".repeat(64), "retry", &c)
        .await
        .unwrap();
    repo.claim_probe().await.unwrap();
    let r = run(&a, &fresh);
    SqlAdapterConformance {
        pool: db.pool.clone(),
    }
    .commit_probe_conformance(&fresh.id, &r, &passed(&r), &c)
    .await
    .unwrap();
    selection
        .change_selection(&a.adapter_id, 0, &change, &c)
        .await
        .unwrap();
}

#[tokio::test]
async fn rechazo_previo_de_permiso_no_invalida_conformidad_historica_confirmada() {
    use larm_domain::{adapters::AdapterSelectionChange, ports::AdapterSelectionRepository};
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let c = ctx();
    let a = persistence::qualify_fixture(&db.pool, &a, &c).await;
    let repo = SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let job = repo
        .enqueue_probe(&a.id, &request(), &"a".repeat(64), "no-approval", &c)
        .await
        .unwrap();
    repo.claim_probe().await.unwrap();
    repo.fail_probe(
        &job.id,
        &larm_domain::error::LarmError::new(
            ErrorCode::ApprovalRequired,
            "denied",
            "approval_required",
        ),
        false,
    )
    .await
    .unwrap();
    let selection = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    selection
        .change_selection(
            &a.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: a.id.clone(),
            },
            &c,
        )
        .await
        .unwrap();
    assert_eq!(installed(&db, &a.id).await, a);
}
