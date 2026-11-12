//! SQLite real: CAS, journal/outbox, replay y fault injection. Las observaciones
//! son fixtures de persistencia CODE_PASS; no cualifican un worker ni inferencia.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use larm_db::adapters::SqlAdapterInstallations;
use larm_db::conformance::SqlAdapterConformance;
use larm_domain::adapters::{
    AdapterInstallation, AdapterSelectionChange, AdapterState, AdapterTrust,
};
use larm_domain::conformance::{
    hash, BindingHashes, Observation, Outcome, Proof, Run, METADATA_KEY,
};
use larm_domain::error::ErrorCode;
use larm_domain::ports::{
    AdapterConformanceRepository, AdapterInstallationRepository, AdapterSelectionRepository,
    MutationContext,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
#[allow(dead_code)]
mod persistence;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "conformance-persistence-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn installed() -> AdapterInstallation {
    AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-18T00:00:00.000Z".into(),
        updated_at: "2026-10-18T00:00:00.000Z".into(),
        row_version: 1,
        metadata: json!({"preserved":true}).as_object().unwrap().clone(),
        adapter_id: "fixture.persistence".into(),
        version: "1.0.0".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "P026 persistence only".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.persistence","version":"1.0.0","family":"runtime","protocol_version":"1.0","publisher":"P026 persistence only","executable":{"relative_path":"bin/fixture.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":[],"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":["vendor.reference.echo"],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]}),
    }
}
fn run(a: &AdapterInstallation) -> Run {
    Run::new(
        a,
        BindingHashes {
            suite_sha256: "b".repeat(64),
            capability_profile_sha256: "c".repeat(64),
            execution_context_sha256: "d".repeat(64),
        },
        "conformance-persistence-fixture",
    )
    .unwrap()
}
fn passed(run: &Run) -> Outcome {
    let observations = run
        .required_checks()
        .into_iter()
        .map(|id| Observation {
            evidence_sha256: hash(&json!({"test_only":id,"run":run.id})).unwrap(),
            check_id: id,
            passed: true,
        })
        .collect();
    Outcome::Passed {
        proof: Proof::evaluate(run, observations).unwrap(),
    }
}
async fn seed(pool: &SqlitePool, a: &AdapterInstallation) {
    SqlAdapterInstallations { pool: pool.clone() }
        .insert(a, &ctx())
        .await
        .unwrap();
}
async fn get(pool: &SqlitePool, id: &str) -> AdapterInstallation {
    SqlAdapterInstallations { pool: pool.clone() }
        .get(id)
        .await
        .unwrap()
        .unwrap()
}
async fn counts(pool: &SqlitePool) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox),(SELECT count(*) FROM resource_lock),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM adapter_selection)")
        .fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn confirma_conformidad_evento_y_journal_sin_cambiar_trust_ni_seleccion_y_sobrevive_reinicio()
{
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let execution = run(&a);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let receipt = repo
        .commit_conformance(&execution, &passed(&execution), &ctx())
        .await
        .unwrap();
    assert!(receipt.passed);
    assert_eq!(receipt.installation_revision, "2");
    let row = get(&db.pool, &a.id).await;
    assert_eq!(row.state, AdapterState::Validated);
    assert_eq!(row.trust, AdapterTrust::Untrusted);
    assert_eq!(row.row_version, 2);
    assert_eq!(row.manifest, a.manifest);
    assert_eq!(row.metadata["preserved"], true);
    assert_eq!(
        row.metadata[METADATA_KEY],
        serde_json::to_value(&receipt).unwrap()
    );
    assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
    let event:(String,i64,String)=sqlx::query_as("SELECT event_type,resource_version,payload FROM event WHERE resource_id=? ORDER BY event_id DESC LIMIT 1").bind(&a.id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(event.0, "adapter.updated");
    assert_eq!(event.1, 2);
    assert_eq!(
        serde_json::from_str::<Value>(&event.2).unwrap()["action"],
        "conformance_passed"
    );
    let (consumer_installation, consumer_run) =
        repo.current_for_consumer(&a.id).await.unwrap().unwrap();
    assert_eq!(consumer_installation, row);
    assert_eq!(consumer_run, execution);
    assert!(repo
        .current_for_consumer(&uuid::Uuid::now_v7().to_string())
        .await
        .unwrap()
        .is_none());
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(&dir.path().join("larm.sqlite"))
        .await
        .unwrap();
    let receipt_again = SqlAdapterConformance {
        pool: reopened.pool.clone(),
    }
    .conformance_receipt(&execution.id)
    .await
    .unwrap();
    assert_eq!(receipt_again, Some(receipt));
    assert_eq!(get(&reopened.pool, &a.id).await, row);
    reopened.pool.close().await;
}

#[tokio::test]
async fn dos_runs_compiten_por_snapshot_solo_uno_confirma() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let first = run(&a);
    let second = run(&a);
    let one = passed(&first);
    let two = passed(&second);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let context = ctx();
    let (left, right) = tokio::join!(
        repo.commit_conformance(&first, &one, &context),
        repo.commit_conformance(&second, &two, &context)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let rejected = if let Err(error) = left {
        error
    } else {
        right.unwrap_err()
    };
    assert_eq!(rejected.code, ErrorCode::StaleRevision);
    assert_eq!(get(&db.pool, &a.id).await.row_version, 2);
    assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
}

#[tokio::test]
async fn cambios_de_snapshot_permisos_codigo_metadata_o_estado_no_se_validan() {
    for case in 0..8 {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = installed();
        seed(&db.pool, &a).await;
        let execution = run(&a);
        let outcome = passed(&execution);
        let sql=match case {
            0=>"UPDATE adapter_installation SET row_version=row_version+1",
            1=>"UPDATE adapter_installation SET package_hash=printf('%064d',0)",
            2=>"UPDATE adapter_installation SET manifest=json_set(manifest,'$.permissions.read_roots',json('[\"other\"]'))",
            3=>"UPDATE adapter_installation SET manifest=json_set(manifest,'$.capabilities',json('[\"vendor.other\"]'))",
            4=>"UPDATE adapter_installation SET metadata=json_set(metadata,'$.changed',1)",
            5=>"UPDATE adapter_installation SET trust='REVOKED'",
            6=>"UPDATE adapter_installation SET state='RETIRED'",
            _=>"UPDATE adapter_installation SET publisher='different publisher'",
        };
        sqlx::query(sql).execute(&db.pool).await.unwrap();
        let before = get(&db.pool, &a.id).await;
        let repo = SqlAdapterConformance {
            pool: db.pool.clone(),
        };
        assert!(
            repo.commit_conformance(&execution, &outcome, &ctx())
                .await
                .is_err(),
            "caso {case}"
        );
        assert_eq!(get(&db.pool, &a.id).await, before);
        assert_eq!(counts(&db.pool).await, (0, 1, 1, 0, 0, 0));
    }
}

#[tokio::test]
async fn replay_identico_es_historico_no_revalida_tras_revocar_y_cambiar_revision() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let execution = run(&a);
    let outcome = passed(&execution);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let context = ctx();
    let first = repo
        .commit_conformance(&execution, &outcome, &context)
        .await
        .unwrap();
    assert_eq!(
        repo.commit_conformance(&execution, &outcome, &ctx())
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED',row_version=3")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.commit_conformance(&execution, &outcome, &context)
            .await
            .unwrap(),
        first
    );
    let current = get(&db.pool, &a.id).await;
    assert_eq!(current.trust, AdapterTrust::Revoked);
    assert_eq!(current.row_version, 3);
    let selection = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let error = selection
        .change_selection(
            &a.adapter_id,
            0,
            &AdapterSelectionChange::Promote {
                installation_id: a.id.clone(),
            },
            &ctx(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
}

#[tokio::test]
async fn replay_no_cambia_veredicto_actor_o_binding_ni_reutiliza_prueba_de_otro_run() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let execution = run(&a);
    let outcome = passed(&execution);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    repo.commit_conformance(&execution, &outcome, &ctx())
        .await
        .unwrap();
    assert_eq!(
        repo.commit_conformance(&execution, &Outcome::failed(&ErrorCode::Timeout), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_replay_mismatch"
    );
    let mut actor = ctx();
    actor.actor_id = "other actor".into();
    assert!(repo
        .commit_conformance(&execution, &outcome, &actor)
        .await
        .is_err());
    let mut other = execution.clone();
    other.bindings.suite_sha256 = "f".repeat(64);
    assert_eq!(
        repo.commit_conformance(&other, &outcome, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_run_mismatch"
    );
    other = execution.clone();
    other.id = uuid::Uuid::now_v7().to_string();
    assert!(repo
        .commit_conformance(&other, &outcome, &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
}

#[tokio::test]
async fn fallo_se_persiste_sin_texto_privado_y_reintento_nuevo_no_reescribe_historia() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let execution = run(&a);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let failed = repo
        .commit_conformance(
            &execution,
            &Outcome::failed(&ErrorCode::Other("private-plugin-marker".into())),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!failed.passed);
    assert_eq!(get(&db.pool, &a.id).await.state, AdapterState::Installed);
    let payload: String = sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=?")
        .bind(&execution.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!payload.contains("private-plugin-marker"));
    assert!(payload.contains("INTERNAL"));
    let next = run(&get(&db.pool, &a.id).await);
    repo.commit_conformance(&next, &passed(&next), &ctx())
        .await
        .unwrap();
    assert_eq!(
        repo.conformance_receipt(&execution.id).await.unwrap(),
        Some(failed)
    );
    assert_eq!(get(&db.pool, &a.id).await.row_version, 3);
    assert_eq!(counts(&db.pool).await, (2, 3, 3, 0, 0, 0));
}

#[tokio::test]
async fn revalidacion_fallida_retira_validated_y_bloquea_promocion() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = installed();
    seed(&db.pool, &a).await;
    let first = run(&a);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    repo.commit_conformance(&first, &passed(&first), &ctx())
        .await
        .unwrap();
    let second = run(&get(&db.pool, &a.id).await);
    repo.commit_conformance(&second, &Outcome::failed(&ErrorCode::ProtocolError), &ctx())
        .await
        .unwrap();
    let current = get(&db.pool, &a.id).await;
    assert_eq!(current.state, AdapterState::Installed);
    let selection = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    assert_eq!(
        selection
            .change_selection(
                &a.adapter_id,
                0,
                &AdapterSelectionChange::Promote {
                    installation_id: a.id
                },
                &ctx()
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_not_validated"
    );
    assert_eq!(counts(&db.pool).await, (2, 3, 3, 0, 0, 0));
}

#[tokio::test]
async fn fallo_en_mutacion_journal_evento_u_outbox_revierte_todo_y_no_consume_run() {
    for (table, operation, condition) in [
        (
            "adapter_installation",
            "UPDATE",
            "WHEN NEW.row_version<>OLD.row_version",
        ),
        ("operation_journal", "INSERT", ""),
        ("event", "INSERT", ""),
        ("event_outbox", "INSERT", ""),
    ] {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = installed();
        seed(&db.pool, &a).await;
        let execution = run(&a);
        let outcome = passed(&execution);
        let repo = SqlAdapterConformance {
            pool: db.pool.clone(),
        };
        let sql=format!("CREATE TRIGGER fixture_commit_failure BEFORE {operation} ON {table} {condition} BEGIN SELECT RAISE(ABORT,'owned_conformance_failure'); END");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            repo.commit_conformance(&execution, &outcome, &ctx())
                .await
                .is_err(),
            "{table}"
        );
        assert_eq!(get(&db.pool, &a.id).await, a);
        assert_eq!(counts(&db.pool).await, (0, 1, 1, 0, 0, 0));
        assert!(repo
            .conformance_receipt(&execution.id)
            .await
            .unwrap()
            .is_none());
        sqlx::query("DROP TRIGGER fixture_commit_failure")
            .execute(&db.pool)
            .await
            .unwrap();
        repo.commit_conformance(&execution, &outcome, &ctx())
            .await
            .unwrap();
        assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
    }
}

#[tokio::test]
async fn recibo_alterado_no_se_acepta_ni_se_reconfirma() {
    for path in [
        "$.receipt.passed",
        "$.receipt.outcome_sha256",
        "$.receipt.installation_revision",
        "$.receipt.committed_installation_sha256",
        "$.receipt.completed_at",
        "$.actor_id",
        "$.outcome.proof.observations[0].evidence_sha256",
        "$.run.package_sha256",
    ] {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = installed();
        seed(&db.pool, &a).await;
        let execution = run(&a);
        let outcome = passed(&execution);
        let repo = SqlAdapterConformance {
            pool: db.pool.clone(),
        };
        repo.commit_conformance(&execution, &outcome, &ctx())
            .await
            .unwrap();
        sqlx::query("UPDATE operation_journal SET payload=json_set(payload,?,?) WHERE id=?")
            .bind(path)
            .bind("altered")
            .bind(&execution.id)
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            repo.conformance_receipt(&execution.id).await.is_err(),
            "{path}"
        );
        assert!(
            repo.commit_conformance(&execution, &outcome, &ctx())
                .await
                .is_err(),
            "{path}"
        );
        assert_eq!(get(&db.pool, &a.id).await.row_version, 2);
        assert_eq!(counts(&db.pool).await, (1, 2, 2, 0, 0, 0));
    }
}

#[tokio::test]
async fn revision_i64_maxima_se_confirma_sin_redondeo_y_luego_bloquea_overflow() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut a = installed();
    a.row_version = i64::MAX - 1;
    seed(&db.pool, &a).await;
    let execution = run(&a);
    let repo = SqlAdapterConformance {
        pool: db.pool.clone(),
    };
    let receipt = repo
        .commit_conformance(&execution, &passed(&execution), &ctx())
        .await
        .unwrap();
    assert_eq!(receipt.installation_revision, i64::MAX.to_string());
    assert_eq!(get(&db.pool, &a.id).await.row_version, i64::MAX);
    assert!(Run::new(
        &get(&db.pool, &a.id).await,
        execution.bindings,
        "conformance-persistence-fixture"
    )
    .is_err());
    assert_eq!(
        repo.conformance_receipt(&receipt.run_id).await.unwrap(),
        Some(receipt)
    );
}

#[tokio::test]
async fn validated_sin_recibo_no_permite_promocion_y_recibo_vigente_si() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut a = installed();
    a.state = AdapterState::Validated;
    seed(&db.pool, &a).await;
    let selection = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let promote = AdapterSelectionChange::Promote {
        installation_id: a.id.clone(),
    };
    let before = counts(&db.pool).await;
    assert_eq!(
        selection
            .change_selection(&a.adapter_id, 0, &promote, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_receipt_missing"
    );
    assert_eq!(counts(&db.pool).await, before);
    let execution = run(&a);
    let receipt = SqlAdapterConformance {
        pool: db.pool.clone(),
    }
    .commit_conformance(&execution, &passed(&execution), &ctx())
    .await
    .unwrap();
    assert!(receipt
        .matches_committed(&get(&db.pool, &a.id).await)
        .unwrap());
    assert_eq!(
        selection
            .change_selection(&a.adapter_id, 0, &promote, &ctx())
            .await
            .unwrap()
            .active_installation_id,
        Some(a.id)
    );
}

#[tokio::test]
async fn promocion_rechaza_cada_mutacion_del_estado_confirmado_incluso_sin_incrementar_revision() {
    for mutation in [
        "UPDATE adapter_installation SET row_version=row_version+1",
        "UPDATE adapter_installation SET trust='VERIFIED'",
        "UPDATE adapter_installation SET state='FAILED'",
        "UPDATE adapter_installation SET package_hash=printf('%064d',0)",
        "UPDATE adapter_installation SET publisher='changed'",
        "UPDATE adapter_installation SET family='source'",
        "UPDATE adapter_installation SET version='changed'",
        "UPDATE adapter_installation SET updated_at='2026-10-19T00:00:00.000Z'",
        "UPDATE adapter_installation SET created_at='2026-10-19T00:00:00.000Z'",
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.other',true)",
        "UPDATE adapter_installation SET manifest=json_set(manifest,'$.permissions.read_roots',json('[\"other\"]'))",
        "UPDATE adapter_installation SET manifest=json_set(manifest,'$.capabilities',json('[\"vendor.other\"]'))",
        "UPDATE adapter_installation SET manifest=json_set(manifest,'$.executable.args',json('[\"changed\"]'))",
        "UPDATE adapter_installation SET manifest=json_set(manifest,'$.publisher','changed')",
    ] {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = installed();
        seed(&db.pool, &a).await;
        let execution = run(&a);
        SqlAdapterConformance { pool: db.pool.clone() }
            .commit_conformance(&execution, &passed(&execution), &ctx()).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(mutation.to_owned())).execute(&db.pool).await.unwrap();
        let before = counts(&db.pool).await;
        let selection = larm_db::adapter_selection::SqlAdapterSelections { pool: db.pool.clone(), qualification: persistence::physical_gate_fixture() };
        assert!(selection.change_selection(&a.adapter_id, 0,
            &AdapterSelectionChange::Promote { installation_id: a.id }, &ctx()).await.is_err(), "{mutation}");
        assert_eq!(counts(&db.pool).await, before, "{mutation}");
    }
}

#[tokio::test]
async fn promocion_rechaza_recibo_trasplantado_o_journal_ausente_alterado_o_de_otro_tipo() {
    for mutation in [
        "UPDATE adapter_installation SET metadata=json_remove(metadata,'$.larm_conformance_v1')",
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.larm_conformance_v1.passed',false)",
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.larm_conformance_v1.run_sha256','altered')",
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.larm_conformance_v1.installation_id','other')",
        "DELETE FROM operation_journal",
        "UPDATE operation_journal SET kind='adapter.selection'",
        "UPDATE operation_journal SET phase='FAILED'",
        "UPDATE operation_journal SET payload=json_set(payload,'$.outcome.proof.observations[0].passed',false)",
        "UPDATE operation_journal SET payload=json_set(payload,'$.receipt.committed_installation_sha256',?)",
    ] {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = installed();
        seed(&db.pool, &a).await;
        let execution = run(&a);
        SqlAdapterConformance { pool: db.pool.clone() }
            .commit_conformance(&execution, &passed(&execution), &ctx()).await.unwrap();
        let query = sqlx::query(sqlx::AssertSqlSafe(mutation.to_owned()));
        if mutation.contains('?') {
            query.bind("e".repeat(64)).execute(&db.pool).await.unwrap();
        } else {
            query.execute(&db.pool).await.unwrap();
        }
        let before = counts(&db.pool).await;
        let selection = larm_db::adapter_selection::SqlAdapterSelections { pool: db.pool.clone(), qualification: persistence::physical_gate_fixture() };
        assert!(selection.change_selection(&a.adapter_id, 0,
            &AdapterSelectionChange::Promote { installation_id: a.id }, &ctx()).await.is_err(), "{mutation}");
        assert_eq!(counts(&db.pool).await, before, "{mutation}");
    }
}

struct PhysicalGate {
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
    live: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
struct PhysicalGuard {
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
    live: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for PhysicalGuard {
    fn drop(&mut self) {
        self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualification for PhysicalGate {
    async fn acquire(
        &self,
        a: &AdapterInstallation,
        r: &Run,
        c: &MutationContext,
    ) -> Result<Box<dyn larm_domain::ports::AdapterQualificationGuard>, larm_domain::error::LarmError>
    {
        use std::sync::atomic::Ordering::SeqCst;
        assert_eq!(r.installation_id, a.id);
        assert_eq!(r.actor_id, c.actor_id);
        let receipt: larm_domain::conformance::Receipt =
            serde_json::from_value(a.metadata[METADATA_KEY].clone()).unwrap();
        assert_eq!(r.digest().unwrap(), receipt.run_sha256);
        self.calls.fetch_add(1, SeqCst);
        if self.mode.load(SeqCst) == 1 {
            return Err(larm_domain::error::LarmError::new(
                ErrorCode::Forbidden,
                "fixture denial",
                "physical_fixture_denied",
            ));
        }
        self.live.fetch_add(1, SeqCst);
        Ok(Box::new(PhysicalGuard {
            mode: self.mode.clone(),
            live: self.live.clone(),
        }))
    }
}
#[async_trait::async_trait]
impl larm_domain::ports::AdapterQualificationGuard for PhysicalGuard {
    async fn recheck(&self) -> Result<(), larm_domain::error::LarmError> {
        use std::sync::atomic::Ordering::SeqCst;
        assert!(self.live.load(SeqCst) > 0);
        if self.mode.load(SeqCst) == 2 {
            return Err(larm_domain::error::LarmError::new(
                ErrorCode::Other("VENDOR_PHYSICAL_FIXTURE".into()),
                "fixture late failure",
                "physical_fixture_changed",
            ));
        }
        Ok(())
    }
}
#[tokio::test]
async fn gate_fisico_promocion_enable_noop_rollback_sin_mutaciones_tras_fallo_y_disable_siempre_disponible(
) {
    use std::sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering::SeqCst},
        Arc,
    };
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut items = Vec::new();
    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        let mut a = installed();
        a.version = version.into();
        a.manifest["version"] = json!(version);
        seed(&db.pool, &a).await;
        let r = run(&a);
        SqlAdapterConformance {
            pool: db.pool.clone(),
        }
        .commit_conformance(&r, &passed(&r), &ctx())
        .await
        .unwrap();
        items.push(a);
    }
    let mode = Arc::new(AtomicU8::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let repo = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(Arc::new(PhysicalGate {
            mode: mode.clone(),
            live: live.clone(),
            calls: calls.clone(),
        })),
    };
    let promote = |a: &AdapterInstallation| AdapterSelectionChange::Promote {
        installation_id: a.id.clone(),
    };
    let a = &items[0];
    let b = &items[1];
    let c = &items[2];
    repo.change_selection(&a.adapter_id, 0, &promote(a), &ctx())
        .await
        .unwrap();
    repo.change_selection(
        &a.adapter_id,
        1,
        &AdapterSelectionChange::SetEnabled { enabled: true },
        &ctx(),
    )
    .await
    .unwrap();
    let before = repo
        .change_selection(&a.adapter_id, 2, &promote(b), &ctx())
        .await
        .unwrap();
    assert_eq!(before.previous_installation_id, Some(a.id.clone()));
    for failure in [1, 2] {
        mode.store(failure, SeqCst);
        for change in [
            promote(c),
            AdapterSelectionChange::Rollback,
            AdapterSelectionChange::SetEnabled { enabled: true },
        ] {
            let counts_before = counts(&db.pool).await;
            let error = repo
                .change_selection(&a.adapter_id, before.row_version, &change, &ctx())
                .await
                .unwrap_err();
            assert_eq!(
                error.details.reason,
                if failure == 1 {
                    "physical_fixture_denied"
                } else {
                    "physical_fixture_changed"
                }
            );
            if failure == 2 {
                assert_eq!(
                    error.code,
                    ErrorCode::Other("VENDOR_PHYSICAL_FIXTURE".into())
                );
            }
            assert_eq!(counts(&db.pool).await, counts_before);
            assert_eq!(
                repo.selection(&a.adapter_id).await.unwrap(),
                Some(before.clone())
            );
            assert_eq!(live.load(SeqCst), 0);
        }
    }
    let unavailable = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: None,
    };
    let saved = counts(&db.pool).await;
    assert_eq!(
        unavailable
            .change_selection(
                &a.adapter_id,
                before.row_version,
                &AdapterSelectionChange::Rollback,
                &ctx()
            )
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_physical_gate_unavailable"
    );
    assert_eq!(counts(&db.pool).await, saved);
    let total = calls.load(SeqCst);
    let disabled = unavailable
        .change_selection(
            &a.adapter_id,
            before.row_version,
            &AdapterSelectionChange::SetEnabled { enabled: false },
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(calls.load(SeqCst), total);
    mode.store(0, SeqCst);
    let rollback = repo
        .change_selection(
            &a.adapter_id,
            disabled.row_version,
            &AdapterSelectionChange::Rollback,
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(rollback.active_installation_id, Some(a.id.clone()));
    assert_eq!(live.load(SeqCst), 0);
}
