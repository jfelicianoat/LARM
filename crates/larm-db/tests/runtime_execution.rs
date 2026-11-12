//! SQLite real y guards sintéticos CODE_PASS. Estas pruebas no cualifican un
//! adaptador, no instalan motores y nunca se componen en el daemon.
#![allow(clippy::unwrap_used, clippy::panic)]
use async_trait::async_trait;
use larm_db::{adapters::SqlAdapterInstallations, pool::Db, runtimes::SqlRuntimes};
use larm_domain::{
    adapters::{AdapterInstallation, AdapterState, AdapterTrust},
    error::{ErrorCode, LarmError},
    ports::{AdapterInstallationRepository, AdapterQualificationGuard, MutationContext},
    runtime::*,
    runtime_execution::*,
    runtime_ports::RuntimeRepository,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
#[allow(dead_code)]
mod persistence;

const AT: &str = "2026-10-20T13:00:00.000Z";
fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn actor() -> MutationContext {
    MutationContext {
        actor_id: "runtime-execution-persistence-fixture".into(),
        correlation_id: uid(),
    }
}
struct Guard {
    binding: RuntimeSessionBinding,
    calls: AtomicUsize,
    fail_at: usize,
}
impl Guard {
    fn new(context: &RuntimeExecutionContext, fail_at: usize) -> Self {
        Self {
            binding: context.binding().unwrap(),
            calls: AtomicUsize::new(0),
            fail_at,
        }
    }
}
#[async_trait]
impl AdapterQualificationGuard for Guard {
    async fn recheck(&self) -> Result<(), LarmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at {
            Err(LarmError::new(
                ErrorCode::Forbidden,
                "Guard sintético caducado",
                "fixture_recheck_failed",
            ))
        } else {
            Ok(())
        }
    }
}
impl RuntimeSessionGuard for Guard {
    fn binding(&self) -> &RuntimeSessionBinding {
        &self.binding
    }
}
async fn setup() -> (tempfile::TempDir, Db, SqlRuntimes, RuntimeExecutionContext) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimes {
        pool: db.pool.clone(),
    };
    let installation = AdapterInstallation {
        id: uid(),
        created_at: AT.into(),
        updated_at: AT.into(),
        row_version: 1,
        metadata: json!({"fixture_only":true}).as_object().unwrap().clone(),
        adapter_id: "fixture.runtime.discovery".into(),
        version: "b7142".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "P031 persistence fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.runtime.discovery","version":"b7142","family":"runtime",
            "publisher":"P031 persistence fixture","package_sha256":"a".repeat(64),
            "capabilities":[],"permissions":{},"methods":[],"installation_strategies":["PREBUILT_BINARY"]}),
    };
    SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&installation, &actor())
    .await
    .unwrap();
    // Publicar el índice cambia el snapshot: se hace antes de la conformidad.
    let adapter = repo
        .register_runtime_adapter(&installation.id, "fixture.engine", &actor())
        .await
        .unwrap();
    let request = RuntimeCreate {
        name: "P031 SQLite fixture".into(),
        adapter_id: adapter.id,
        capabilities: vec![],
        default_version_id: None,
        channel_policy: RuntimeChannelPolicy {
            allowed_channels: channels().to_vec(),
        },
        retention_policy: Default::default(),
    };
    let hash =
        larm_domain::canonical::sha256_hex(&serde_json::to_value(&request).unwrap()).unwrap();
    let runtime = repo
        .create_runtime(&request, &uid(), &hash, &actor())
        .await
        .unwrap();
    persistence::qualify_fixture(&db.pool, &installation, &actor()).await;
    let context = repo.prepare_execution(&runtime.id, &actor()).await.unwrap();
    (dir, db, repo, context)
}
fn channels() -> [RuntimeChannel; 5] {
    [
        RuntimeChannel::Stable,
        RuntimeChannel::Prerelease,
        RuntimeChannel::Nightly,
        RuntimeChannel::Commit,
        RuntimeChannel::Custom,
    ]
}
fn observation(context: &RuntimeExecutionContext, channel: RuntimeChannel) -> RuntimeVersion {
    RuntimeVersion {
        id: uid(),
        created_at: AT.into(),
        updated_at: AT.into(),
        row_version: 9,
        metadata: json!({"fixture_only":true}).as_object().unwrap().clone(),
        runtime_id: context.runtime.id.clone(),
        version_string: "b7142-no-semver".into(),
        commit: Some("vendor-revision".into()),
        build: None,
        channel,
        released_at: None,
        artifact_hash: None,
        promotion: RuntimePromotion::None,
        lifecycle: Default::default(),
    }
}
async fn counts(pool: &SqlitePool) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "runtime_version",
        "operation_journal",
        "event",
        "event_outbox",
        "event_sequence_alloc",
        "idempotency",
        "resource_lock",
        "volume_reservation",
        "memory_reservation",
    ] {
        counts.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    counts
}
async fn commit(
    repo: &SqlRuntimes,
    context: &RuntimeExecutionContext,
    versions: &[RuntimeVersion],
) -> Vec<RuntimeVersion> {
    repo.commit_discovery(context, versions, &Guard::new(context, 0), &actor())
        .await
        .unwrap()
}

#[tokio::test]
async fn lote_cinco_canales_minta_identidad_local_conserva_unknown_y_sobrevive_reinicio() {
    let (dir, db, repo, context) = setup().await;
    let before = counts(&db.pool).await;
    let mut observed: Vec<_> = channels()
        .into_iter()
        .map(|c| observation(&context, c))
        .collect();
    observed[4].metadata.insert(
        "upstream_channel".into(),
        ObservedChannel::from_upstream("vendor.experimental/42").metadata(),
    );
    // El hash soporta las revisiones i64 de observaciones, sin pérdida JCS.
    observed[4].row_version = i64::MAX;
    let guard = Guard::new(&context, 0);
    let result = repo
        .commit_discovery(&context, &observed, &guard, &actor())
        .await
        .unwrap();
    assert_eq!(guard.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.len(), 5);
    for (value, source) in result.iter().zip(&observed) {
        assert_ne!(value.id, source.id);
        assert_eq!(value.row_version, 1);
        assert_eq!(value.promotion, RuntimePromotion::None);
        assert_eq!(value.channel, source.channel);
        assert_eq!(
            value.metadata["runtime_discovery"]["source_record_id"],
            source.id
        );
    }
    assert_eq!(result[4].metadata["upstream_channel"]["reason"], "UNKNOWN");
    assert_eq!(
        result[4].metadata["upstream_channel"]["value"],
        "vendor.experimental/42"
    );
    assert_eq!(
        repo.runtime(&context.runtime.id).await.unwrap(),
        Some(context.runtime.clone())
    );
    let after = counts(&db.pool).await;
    assert_eq!(
        after,
        before
            .iter()
            .enumerate()
            .map(|(i, n)| n + match i {
                0 | 2 | 3 | 4 => 5,
                1 => 6,
                _ => 0,
            })
            .collect::<Vec<_>>()
    );
    db.pool.close().await;
    let reopened = Db::open(&dir.path().join("larm.sqlite")).await.unwrap();
    let repo = SqlRuntimes {
        pool: reopened.pool.clone(),
    };
    for value in &result {
        assert_eq!(repo.version(&value.id).await.unwrap(), Some(value.clone()));
    }
    assert_eq!(commit(&repo, &context, &observed).await, result);
    assert_eq!(counts(&reopened.pool).await, after);
    reopened.pool.close().await;
}

#[tokio::test]
async fn replay_concurrente_devuelve_el_original_y_otro_cuerpo_no_reutiliza_operacion() {
    let (_dir, db, repo, context) = setup().await;
    let observed = [observation(&context, RuntimeChannel::Stable)];
    let g1 = Guard::new(&context, 0);
    let g2 = Guard::new(&context, 0);
    let ctx = actor();
    let (a, b) = tokio::join!(
        repo.commit_discovery(&context, &observed, &g1, &ctx),
        repo.commit_discovery(&context, &observed, &g2, &ctx)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let before = counts(&db.pool).await;
    let mut changed = observed.clone();
    changed[0].metadata.insert("different".into(), json!(true));
    assert_eq!(
        repo.commit_discovery(&context, &changed, &Guard::new(&context, 0), &ctx)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn nuevo_descubrimiento_preserva_pin_default_y_metadata_local_y_archiva_fuente() {
    let (_dir, db, repo, context) = setup().await;
    let observed = [observation(&context, RuntimeChannel::Stable)];
    let original = commit(&repo, &context, &observed).await.remove(0);
    // Estado local previo explícito; no implementa ni cualifica P117.
    sqlx::query("UPDATE runtime_version SET promotion='PINNED',metadata=json_set(metadata,'$.local_note','preserved'),row_version=row_version+1 WHERE id=?").bind(&original.id).execute(&db.pool).await.unwrap();
    sqlx::query("UPDATE runtime SET default_version_id=?,row_version=row_version+1 WHERE id=?")
        .bind(&original.id)
        .bind(&context.runtime.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let fresh = repo
        .prepare_execution(&context.runtime.id, &actor())
        .await
        .unwrap();
    let mut new_source = [observation(&fresh, RuntimeChannel::Stable)];
    new_source[0]
        .metadata
        .insert("upstream_new".into(), json!(true));
    let local = repo.version(&original.id).await.unwrap().unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        commit(&repo, &fresh, &new_source).await,
        vec![local.clone()]
    );
    assert_eq!(
        repo.runtime(&fresh.runtime.id).await.unwrap(),
        Some(fresh.runtime.clone())
    );
    let payload: String = sqlx::query_scalar("SELECT payload FROM operation_journal WHERE id=?")
        .bind(&fresh.operation_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        payload["observations"][0],
        serde_json::to_value(&new_source[0]).unwrap()
    );
    assert_eq!(payload["result"][0], serde_json::to_value(&local).unwrap());
    let mut expected = before;
    expected[1] += 1;
    assert_eq!(counts(&db.pool).await, expected);
    assert_eq!(commit(&repo, &fresh, &new_source).await, vec![local]);
}

#[tokio::test]
async fn actor_sesion_ids_y_contexto_cambiado_se_rechazan_sin_escrituras() {
    for case in 0..7 {
        let (_dir, db, repo, mut context) = setup().await;
        let observed = [observation(&context, RuntimeChannel::Stable)];
        let mut ctx = actor();
        let mut guard = Guard::new(&context, 0);
        match case {
            0 => ctx.actor_id = "another-actor".into(),
            1 => guard.binding.runtime_id = uid(),
            2 => context.operation_id = "invalid".into(),
            3 => {
                sqlx::query("UPDATE runtime SET row_version=row_version+1 WHERE id=?")
                    .bind(&context.runtime.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
            4 => {
                sqlx::query("UPDATE runtime_adapter SET runtime_family='changed',row_version=row_version+1 WHERE id=?").bind(&context.adapter.id).execute(&db.pool).await.unwrap();
            }
            5 => {
                sqlx::query("UPDATE adapter_installation SET manifest=json_set(manifest,'$.permissions.read_roots',json('[\"changed\"]')) WHERE id=?").bind(&context.installation.id).execute(&db.pool).await.unwrap();
            }
            _ => {
                sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
                    .bind(&context.installation.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
        }
        let before = counts(&db.pool).await;
        assert!(
            repo.commit_discovery(&context, &observed, &guard, &ctx)
                .await
                .is_err(),
            "case {case}"
        );
        assert_eq!(counts(&db.pool).await, before, "case {case}");
    }
}

#[tokio::test]
async fn observaciones_invalidas_no_otorgan_promocion_ni_aceptan_secretos() {
    let (_dir, db, repo, context) = setup().await;
    let before = counts(&db.pool).await;
    for case in 0..7 {
        let mut observed = vec![observation(&context, RuntimeChannel::Stable)];
        match case {
            0 => observed[0].promotion = RuntimePromotion::Default,
            1 => observed[0].runtime_id = uid(),
            2 => observed[0].row_version = 0,
            3 => observed[0]
                .metadata
                .insert("api_token".into(), json!("fixture-not-a-secret"))
                .map(|_| ())
                .unwrap_or(()),
            4 => {
                let mut duplicate = observed[0].clone();
                duplicate.id = uid();
                observed.push(duplicate);
            }
            5 => observed[0].version_string = "".into(),
            _ => observed[0].commit = Some("".into()),
        }
        assert!(
            repo.commit_discovery(&context, &observed, &Guard::new(&context, 0), &actor())
                .await
                .is_err(),
            "case {case}"
        );
        assert_eq!(counts(&db.pool).await, before);
    }
    let updated = repo
        .change_channel_policy(
            &context.runtime.id,
            context.runtime.row_version,
            &RuntimeChannelPolicy::default(),
            &actor(),
        )
        .await
        .unwrap();
    let fresh = repo.prepare_execution(&updated.id, &actor()).await.unwrap();
    let before = counts(&db.pool).await;
    assert!(repo
        .commit_discovery(
            &fresh,
            &[observation(&fresh, RuntimeChannel::Nightly)],
            &Guard::new(&fresh, 0),
            &actor()
        )
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn guard_antes_de_tx_y_antes_de_commit_revierte_lote_incluso_en_replay() {
    let (_dir, db, repo, context) = setup().await;
    let observed = channels()
        .into_iter()
        .map(|c| observation(&context, c))
        .collect::<Vec<_>>();
    let before = counts(&db.pool).await;
    for at in [1, 2] {
        let guard = Guard::new(&context, at);
        assert_eq!(
            repo.commit_discovery(&context, &observed, &guard, &actor())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        assert_eq!(guard.calls.load(Ordering::SeqCst), at);
        assert_eq!(counts(&db.pool).await, before);
    }
    let original = commit(&repo, &context, &observed).await;
    let before = counts(&db.pool).await;
    for at in [1, 2] {
        assert!(repo
            .commit_discovery(&context, &observed, &Guard::new(&context, at), &actor())
            .await
            .is_err());
        assert_eq!(counts(&db.pool).await, before);
    }
    assert_eq!(commit(&repo, &context, &observed).await, original);
}

#[tokio::test]
async fn abort_e_ignore_tardios_revierten_filas_journal_eventos_y_outbox_y_admiten_reintento() {
    for (table, predicate) in [
        ("runtime_version", "NEW.channel='nightly'"),
        (
            "operation_journal",
            "NEW.kind='runtime.catalog' AND json_extract(NEW.payload,'$.record.channel')='nightly'",
        ),
        ("operation_journal", "NEW.kind='runtime.discovery'"),
        (
            "event_sequence_alloc",
            "(SELECT count(*) FROM runtime_version)>=2",
        ),
        ("event", "(SELECT count(*) FROM runtime_version)>=2"),
        ("event_outbox", "(SELECT count(*) FROM runtime_version)>=2"),
    ] {
        for behavior in ["ABORT,'injected'", "IGNORE"] {
            let (_dir, db, repo, context) = setup().await;
            let observed = [
                observation(&context, RuntimeChannel::Stable),
                observation(&context, RuntimeChannel::Nightly),
            ];
            let before = counts(&db.pool).await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER inject_discovery BEFORE INSERT ON {table} WHEN {predicate} BEGIN SELECT RAISE({behavior}); END;"))).execute(&db.pool).await.unwrap();
            assert!(
                repo.commit_discovery(&context, &observed, &Guard::new(&context, 0), &actor())
                    .await
                    .is_err(),
                "{table}/{predicate}/{behavior}"
            );
            assert_eq!(
                counts(&db.pool).await,
                before,
                "{table}/{predicate}/{behavior}"
            );
            sqlx::raw_sql("DROP TRIGGER inject_discovery")
                .execute(&db.pool)
                .await
                .unwrap();
            let original = commit(&repo, &context, &observed).await;
            let after = counts(&db.pool).await;
            assert_eq!(after[0], 2);
            assert_eq!(commit(&repo, &context, &observed).await, original);
            assert_eq!(counts(&db.pool).await, after);
        }
    }
}

#[tokio::test]
async fn journal_alterado_nunca_devuelve_resultado_silenciosamente() {
    for case in 0..10 {
        let (_dir, db, repo, context) = setup().await;
        let observed = [observation(&context, RuntimeChannel::Stable)];
        commit(&repo, &context, &observed).await;
        let (mut payload, mut hash, mut phase, mut ids): (String, String, String, String) =
            sqlx::query_as(
                "SELECT payload,plan_hash,phase,resource_ids FROM operation_journal WHERE id=?",
            )
            .bind(&context.operation_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let mut value: Value = serde_json::from_str(&payload).unwrap();
        match case {
            0 => value["result"][0]["metadata"]["altered"] = json!(true),
            1 => value["observations"][0]["metadata"]["altered"] = json!(true),
            2 => value["result"][0]["id"] = json!(uid()),
            3 => value["result"][0]["row_version"] = json!(0),
            4 => value["request_sha256"] = json!("f".repeat(64)),
            5 => value["actor_id"] = json!("another"),
            6 => hash = "f".repeat(64),
            7 => phase = "FAILED".into(),
            8 => ids = "[]".into(),
            _ => value["result"] = Value::Null,
        }
        payload = value.to_string();
        sqlx::query(
            "UPDATE operation_journal SET payload=?,plan_hash=?,phase=?,resource_ids=? WHERE id=?",
        )
        .bind(payload)
        .bind(hash)
        .bind(phase)
        .bind(ids)
        .bind(&context.operation_id)
        .execute(&db.pool)
        .await
        .unwrap();
        let before = counts(&db.pool).await;
        assert_eq!(
            repo.commit_discovery(&context, &observed, &Guard::new(&context, 0), &actor())
                .await
                .unwrap_err()
                .code,
            ErrorCode::IntegrityFailed,
            "case {case}"
        );
        assert_eq!(counts(&db.pool).await, before);
    }
}

#[tokio::test]
async fn sondeo_activo_o_sin_autoridad_confirmada_y_journal_de_conformidad_corrupto_denegan_consumo(
) {
    use larm_db::adapter_probes::SqlAdapterProbes;
    use larm_domain::{hardware::Probe, ports::AdapterProbeRepository};
    for case in 0..4 {
        let (_dir, db, repo, context) = setup().await;
        if case == 3 {
            sqlx::query("UPDATE operation_journal SET plan_hash=? WHERE id=?")
                .bind("f".repeat(64))
                .bind(&context.run.id)
                .execute(&db.pool)
                .await
                .unwrap();
        } else {
            let probes = SqlAdapterProbes {
                pool: db.pool.clone(),
            };
            let job = probes
                .enqueue_probe(
                    &context.installation.id,
                    &Probe {
                        resource_id: None,
                        profile_id: None,
                    },
                    &"b".repeat(64),
                    &uid(),
                    &actor(),
                )
                .await
                .unwrap();
            if case >= 1 {
                probes.claim_probe().await.unwrap().unwrap();
            }
            if case == 2 {
                probes
                    .fail_probe(
                        &job.id,
                        &LarmError::new(ErrorCode::Internal, "fixture", "fixture"),
                        true,
                    )
                    .await
                    .unwrap();
            }
        }
        let before = counts(&db.pool).await;
        assert!(
            repo.prepare_execution(&context.runtime.id, &actor())
                .await
                .is_err(),
            "case {case}"
        );
        assert!(
            repo.commit_discovery(
                &context,
                &[observation(&context, RuntimeChannel::Stable)],
                &Guard::new(&context, 0),
                &actor()
            )
            .await
            .is_err(),
            "case {case}"
        );
        assert_eq!(counts(&db.pool).await, before);
    }
}

#[tokio::test]
async fn lote_vacio_registra_consulta_completa_y_replay_no_crea_inventario() {
    let (_dir, db, repo, context) = setup().await;
    let mut expected = counts(&db.pool).await;
    expected[1] += 1;
    assert!(commit(&repo, &context, &[]).await.is_empty());
    assert_eq!(counts(&db.pool).await, expected);
    assert!(commit(&repo, &context, &[]).await.is_empty());
    assert_eq!(counts(&db.pool).await, expected);
}
