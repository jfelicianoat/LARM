//! P024: SQLite real, concurrencia, rollback, reinicio y fallos del outbox.
//! Recibos sintéticos CODE_PASS de persistencia; no cualifican workers P026.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::adapter_selection::SqlAdapterSelections;
use larm_db::adapters::SqlAdapterInstallations;
use larm_domain::adapters::{
    AdapterInstallation, AdapterSelectionChange as Change, AdapterState, AdapterTrust,
};
use larm_domain::error::ErrorCode;
use larm_domain::ports::{
    AdapterInstallationRepository, AdapterSelectionRepository, MutationContext,
};
use serde_json::json;
use sqlx::SqlitePool;

#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
mod persistence;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "operator-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn installation(version: &str, state: AdapterState) -> AdapterInstallation {
    AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-16T10:00:00.000Z".into(),
        updated_at: "2026-10-16T10:00:00.000Z".into(),
        row_version: 1,
        metadata: serde_json::Map::new(),
        adapter_id: "acme.test".into(),
        version: version.into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture publisher".into(),
        trust: AdapterTrust::Verified,
        manifest: json!({"id":"acme.test","version":version,"family":"runtime",
            "publisher":"fixture publisher","package_sha256":"a".repeat(64),
            "permissions":{},"capabilities":[]}),
        state,
    }
}

fn promote(a: &AdapterInstallation) -> Change {
    Change::Promote {
        installation_id: a.id.clone(),
    }
}

async fn seed(pool: &SqlitePool, a: &AdapterInstallation) {
    let qualify = a.state == AdapterState::Validated && a.trust != AdapterTrust::Revoked;
    let mut initial = a.clone();
    if qualify {
        initial.state = AdapterState::Installed;
    }
    SqlAdapterInstallations { pool: pool.clone() }
        .insert(&initial, &ctx())
        .await
        .unwrap();
    if qualify {
        persistence::qualify_fixture(pool, &initial, &ctx()).await;
    }
}

async fn counts(pool: &SqlitePool) -> (i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM adapter_selection),\
         (SELECT count(*) FROM operation_journal WHERE kind='adapter.selection'),\
         (SELECT count(*) FROM event), (SELECT count(*) FROM event_outbox),\
         (SELECT count(*) FROM volume_reservation)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn promocion_habilitacion_y_rollback_persisten_sin_reescribir_lifecycle() {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1.0", AdapterState::Validated);
    let b = installation("2.0", AdapterState::Validated);
    seed(&db.pool, &a).await;
    seed(&db.pool, &b).await;
    let s1 = repo
        .change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    assert_eq!(s1.active_installation_id, Some(a.id.clone()));
    assert!(
        !s1.enabled,
        "la primera selección no habilita implícitamente"
    );
    let s2 = repo
        .change_selection(
            &a.adapter_id,
            1,
            &Change::SetEnabled { enabled: true },
            &ctx(),
        )
        .await
        .unwrap();
    assert!(s2.enabled);
    let s3 = repo
        .change_selection(&a.adapter_id, 2, &promote(&b), &ctx())
        .await
        .unwrap();
    assert_eq!(s3.previous_installation_id, Some(a.id.clone()));
    assert_eq!(s3.active_installation_id, Some(b.id.clone()));
    assert!(s3.enabled);
    let state: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,state FROM adapter_installation WHERE adapter_id='acme.test' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        state,
        vec![
            (a.id.clone(), "VALIDATED".into()),
            (b.id.clone(), "VALIDATED".into())
        ]
    );
    let result = repo
        .change_selection(&a.adapter_id, 3, &Change::Rollback, &ctx())
        .await
        .unwrap();
    assert_eq!(result.active_installation_id, Some(a.id.clone()));
    assert_eq!(result.previous_installation_id, None);
    assert!(result.enabled);
    assert_eq!(result.row_version, 4);
    assert_eq!(counts(&db.pool).await, (1, 4, 8, 8, 0));
    drop(repo);
    db.pool.close().await;
    let db = larm_db::pool::Db::open(&dir.path().join("larm.sqlite"))
        .await
        .unwrap();
    assert!(larm_db::migrate::migrate(&db.pool)
        .await
        .unwrap()
        .is_empty());
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    assert_eq!(repo.selection("ACME.TEST").await.unwrap(), Some(result));
    assert_eq!(
        repo.change_selection(&a.adapter_id, 4, &Change::Rollback, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_rollback_unavailable"
    );
    let (phase, actor, target): (String, String, String) = sqlx::query_as(
        "SELECT phase,json_extract(payload,'$.actor_id'),json_extract(payload,'$.result.active_installation_id') \
         FROM operation_journal WHERE json_extract(payload,'$.change.action')='ROLLBACK'",
    ).fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        (phase.as_str(), actor.as_str(), target.as_str()),
        ("COMPLETED", "operator-fixture", a.id.as_str())
    );
    db.pool.close().await;
}

#[tokio::test]
async fn rechaza_estados_no_validados_revocacion_identidad_y_revision_sin_efectos() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    for state in [
        AdapterState::Staged,
        AdapterState::Verified,
        AdapterState::Installed,
        AdapterState::Retired,
        AdapterState::Failed,
    ] {
        let a = installation(state.as_str(), state);
        seed(&db.pool, &a).await;
        let before = counts(&db.pool).await;
        let error = repo
            .change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "adapter_not_validated");
        assert_eq!(counts(&db.pool).await, before);
    }
    let mut revoked = installation("revoked", AdapterState::Validated);
    revoked.trust = AdapterTrust::Revoked;
    seed(&db.pool, &revoked).await;
    assert_eq!(
        repo.change_selection(&revoked.adapter_id, 0, &promote(&revoked), &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let a = installation("good", AdapterState::Validated);
    seed(&db.pool, &a).await;
    assert_eq!(
        repo.change_selection("other", 0, &promote(&a), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_identity_mismatch"
    );
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.change_selection(&a.adapter_id, -1, &promote(&a), &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    assert_eq!(
        repo.change_selection(&a.adapter_id, 1, &promote(&a), &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    assert_eq!(counts(&db.pool).await, before);
    let selected = repo
        .change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.change_selection(
            &a.adapter_id,
            1,
            &Change::SetEnabled { enabled: false },
            &ctx()
        )
        .await
        .unwrap(),
        selected
    );
    assert_eq!(
        counts(&db.pool).await,
        before,
        "no-op no crea eventos ni revisiones"
    );
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&a.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.change_selection(
            &a.adapter_id,
            1,
            &Change::SetEnabled { enabled: true },
            &ctx()
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Forbidden
    );
    assert_eq!(repo.selection(&a.adapter_id).await.unwrap(), Some(selected));
}

#[tokio::test]
async fn rollback_revalida_anterior_y_desactivar_sigue_permitido_tras_revocacion() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    seed(&db.pool, &a).await;
    seed(&db.pool, &b).await;
    repo.change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    repo.change_selection(
        &a.adapter_id,
        1,
        &Change::SetEnabled { enabled: true },
        &ctx(),
    )
    .await
    .unwrap();
    let selected = repo
        .change_selection(&a.adapter_id, 2, &promote(&b), &ctx())
        .await
        .unwrap();
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&a.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.change_selection(&a.adapter_id, 3, &Change::Rollback, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert_eq!(repo.selection(&a.adapter_id).await.unwrap(), Some(selected));
    assert_eq!(counts(&db.pool).await, before);
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&b.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !repo
            .change_selection(
                &a.adapter_id,
                3,
                &Change::SetEnabled { enabled: false },
                &ctx()
            )
            .await
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn habilitar_y_rollback_exigen_recibo_vigente_y_desactivar_sigue_permitido() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    let c = installation("3", AdapterState::Validated);
    for row in [&a, &b, &c] {
        seed(&db.pool, row).await;
    }
    repo.change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    repo.change_selection(
        &a.adapter_id,
        1,
        &Change::SetEnabled { enabled: true },
        &ctx(),
    )
    .await
    .unwrap();
    let selected = repo
        .change_selection(&a.adapter_id, 2, &promote(&b), &ctx())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.changed',true) WHERE id=?",
    )
    .bind(&a.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let before = counts(&db.pool).await;
    assert!(repo
        .change_selection(&a.adapter_id, 3, &Change::Rollback, &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
    assert_eq!(repo.selection(&a.adapter_id).await.unwrap(), Some(selected));
    sqlx::query(
        "UPDATE adapter_installation SET metadata=json_set(metadata,'$.changed',true) WHERE id=?",
    )
    .bind(&b.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let before = counts(&db.pool).await;
    assert!(repo
        .change_selection(
            &a.adapter_id,
            3,
            &Change::SetEnabled { enabled: true },
            &ctx()
        )
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
    let disabled = repo
        .change_selection(
            &a.adapter_id,
            3,
            &Change::SetEnabled { enabled: false },
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
    let changed = repo
        .change_selection(&a.adapter_id, 4, &promote(&c), &ctx())
        .await
        .unwrap();
    assert_eq!(
        changed.previous_installation_id,
        Some(a.id),
        "una activa obsoleta no se conserva como último bueno"
    );
}

#[tokio::test]
async fn journal_corrupto_de_activa_no_impide_sustituirla_por_una_instalacion_sana() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    let c = installation("3", AdapterState::Validated);
    for row in [&a, &b, &c] {
        seed(&db.pool, row).await;
    }
    repo.change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    repo.change_selection(&a.adapter_id, 1, &promote(&b), &ctx())
        .await
        .unwrap();
    sqlx::query("UPDATE operation_journal SET payload='{}' WHERE id=(SELECT json_extract(metadata,'$.larm_conformance_v1.run_id') FROM adapter_installation WHERE id=?)")
        .bind(&b.id).execute(&db.pool).await.unwrap();
    let selected = repo
        .change_selection(&a.adapter_id, 2, &promote(&c), &ctx())
        .await
        .unwrap();
    assert_eq!(selected.active_installation_id, Some(c.id));
    assert_eq!(selected.previous_installation_id, Some(a.id.clone()));
    let rollback = repo
        .change_selection(&a.adapter_id, 3, &Change::Rollback, &ctx())
        .await
        .unwrap();
    assert_eq!(rollback.active_installation_id, Some(a.id));
}

#[tokio::test]
async fn dos_promociones_concurrentes_solo_una_cas_gana_incluso_con_distintas_mayusculas() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    seed(&db.pool, &a).await;
    seed(&db.pool, &b).await;
    let (pa, pb, ca, cb) = (promote(&a), promote(&b), ctx(), ctx());
    let (ra, rb) = tokio::join!(
        repo.change_selection("acme.test", 0, &pa, &ca),
        repo.change_selection("ACME.TEST", 0, &pb, &cb),
    );
    assert_ne!(ra.is_ok(), rb.is_ok());
    let (winner, loser) = if let Ok(winner) = ra {
        (winner, rb.unwrap_err())
    } else {
        (rb.unwrap(), ra.unwrap_err())
    };
    assert_eq!(loser.code, ErrorCode::StaleRevision);
    assert_eq!(repo.selection("Acme.Test").await.unwrap(), Some(winner));
    assert_eq!(counts(&db.pool).await, (1, 1, 5, 5, 0));
}

#[tokio::test]
async fn activa_fallida_no_sustituye_al_ultimo_bueno_y_familia_distinta_se_rechaza() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    let mut other_family = installation("3", AdapterState::Validated);
    other_family.family = "protocol".into();
    other_family.manifest["family"] = json!("protocol");
    let c = installation("4", AdapterState::Validated);
    for row in [&a, &b, &other_family, &c] {
        seed(&db.pool, row).await;
    }
    repo.change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    let selected = repo
        .change_selection(&a.adapter_id, 1, &promote(&b), &ctx())
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.change_selection(&a.adapter_id, 2, &promote(&other_family), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_family_mismatch"
    );
    assert_eq!(counts(&db.pool).await, before);
    assert_eq!(repo.selection(&a.adapter_id).await.unwrap(), Some(selected));
    sqlx::query("UPDATE adapter_installation SET state='FAILED' WHERE id=?")
        .bind(&b.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let selected = repo
        .change_selection(&a.adapter_id, 2, &promote(&c), &ctx())
        .await
        .unwrap();
    assert_eq!(
        selected.previous_installation_id,
        Some(a.id.clone()),
        "la activa fallida no se guarda como buena"
    );
    let rollback = repo
        .change_selection(&a.adapter_id, 3, &Change::Rollback, &ctx())
        .await
        .unwrap();
    assert_eq!(rollback.active_installation_id, Some(a.id));
}

#[tokio::test]
async fn fallo_en_journal_y_outbox_revierte_seleccion_evento_y_revision() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let a = installation("1", AdapterState::Validated);
    let b = installation("2", AdapterState::Validated);
    seed(&db.pool, &a).await;
    seed(&db.pool, &b).await;
    let initial = repo
        .change_selection(&a.adapter_id, 0, &promote(&a), &ctx())
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    for target in ["operation_journal", "event_outbox"] {
        let mut injection = db.pool.begin().await.unwrap();
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *injection)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE TRIGGER inject_fault BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'fixture fault'); END;"
        ))).execute(&mut *injection).await.unwrap_or_else(|e| panic!("create trigger for {target}: {e}"));
        injection.commit().await.unwrap();
        assert_eq!(
            repo.change_selection(&a.adapter_id, 1, &promote(&b), &ctx())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        assert_eq!(
            repo.selection(&a.adapter_id).await.unwrap(),
            Some(initial.clone())
        );
        assert_eq!(counts(&db.pool).await, before);
        let mut injection = db.pool.begin().await.unwrap();
        sqlx::query("UPDATE operation_journal SET updated_at=updated_at WHERE 0")
            .execute(&mut *injection)
            .await
            .unwrap();
        sqlx::raw_sql("DROP TRIGGER inject_fault;")
            .execute(&mut *injection)
            .await
            .unwrap();
        injection.commit().await.unwrap();
        let triggers: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='inject_fault'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(triggers, 0, "se elimina la inyección de {target}");
    }
    let changed = repo
        .change_selection(&a.adapter_id, 1, &promote(&b), &ctx())
        .await
        .unwrap();
    assert_eq!(changed.row_version, 2, "un fallo no consume revisión CAS");
    assert_eq!(counts(&db.pool).await, (1, 2, 6, 6, 0));
}

#[tokio::test]
async fn migracion_de_bootstrap_preserva_filas_y_rechaza_identidades_ambiguas_atomicamente() {
    let dir = tempfile::tempdir().unwrap();
    let db = larm_db::pool::Db::open(&dir.path().join("old.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate_with(&db.pool, &larm_db::migrate::MIGRATIONS[..1])
        .await
        .unwrap();
    let a = installation("1", AdapterState::Installed);
    let mut duplicate = a.clone();
    duplicate.id = uuid::Uuid::now_v7().to_string();
    duplicate.adapter_id = "ACME.TEST".into();
    seed(&db.pool, &a).await;
    seed(&db.pool, &duplicate).await;
    assert!(larm_db::migrate::migrate(&db.pool).await.is_err());
    let new_tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='adapter_selection'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(new_tables, 0);
    assert_eq!(
        larm_db::migrate::applied_versions(&db.pool).await.unwrap(),
        vec!["0001_bootstrap"]
    );
    sqlx::query("DELETE FROM adapter_installation WHERE id=?")
        .bind(&duplicate.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        larm_db::migrate::migrate(&db.pool).await.unwrap(),
        vec![
            "0002_adapter_selection",
            "0003_runtime_channel_identity",
            "0004_runtime_profile_locks",
            "0005_runtime_profile_qualifications",
            "0006_p052_artifact_size_estimate",
            "0007_p098_local_import_source",
            "0008_p093_failed_recoverable",
            "0009_p028_signed_adapter_registry",
            "0010_p028_registry_url_binding",
            "0011_p127_source_operational_snapshot",
            "0012_p128_low_disk_transition",
            "0013_p032_runtime_version_pinning_and_retention",
            "0014_model_acquisition_manifest"
        ]
    );
    assert_eq!(
        SqlAdapterInstallations {
            pool: db.pool.clone()
        }
        .get(&a.id)
        .await
        .unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        counts(&db.pool).await,
        (0, 0, 2, 2, 0),
        "migrar no selecciona ni borra datos ni eventos previos"
    );
    assert_eq!(
        SqlAdapterInstallations {
            pool: db.pool.clone()
        }
        .insert(&duplicate, &ctx())
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(counts(&db.pool).await, (0, 0, 2, 2, 0));
}
