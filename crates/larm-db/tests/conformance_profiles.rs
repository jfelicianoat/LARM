//! SQLite real y fault injection de perfiles. No ejecuta ni cualifica workers.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::conformance_profiles::SqlConformanceProfiles;
use larm_domain::{
    adapters::{AdapterInstallation, AdapterState, AdapterTrust},
    approval::ApprovalRequest,
    conformance::hash,
    conformance_profiles::ConformanceProfile,
    ports::{
        AdapterInstallationRepository, ApprovalRepository, ConformanceProfileRepository,
        MutationContext,
    },
};
use serde_json::json;
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "profile-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
async fn seed(db: &larm_db::pool::Db) -> AdapterInstallation {
    let id = uuid::Uuid::now_v7().to_string();
    let at = larm_domain::bridge::timestamp(chrono::Utc::now());
    let a = AdapterInstallation {
        id: id.clone(),
        created_at: at.clone(),
        updated_at: at,
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "fixture.profile".into(),
        version: id.clone(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.profile","version":id,"family":"runtime","package_sha256":"a".repeat(64),"publisher":"fixture","permissions":{},"capabilities":[]}),
    };
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&a, &ctx())
    .await
    .unwrap();
    a
}
async fn profile(db: &larm_db::pool::Db, a: &AdapterInstallation) -> ConformanceProfile {
    let now = chrono::Utc::now();
    let approval=ApprovalRequest{operation_digest:"b".repeat(64),resource_ids:vec![a.id.clone()],code_hash:Some(a.package_hash.clone()),permissions:json!({"publisher":"fixture","permissions":{},"expires_at":larm_domain::bridge::timestamp(now+chrono::Duration::hours(1)),"secret_refs":[]}).as_object().unwrap().clone(),approved:true}.grant(uuid::Uuid::now_v7().to_string(),ctx().actor_id,now,chrono::Duration::minutes(15)).unwrap();
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .insert(&approval, &ctx())
    .await
    .unwrap();
    ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: a.id.clone(),
        actor_id: ctx().actor_id,
        manifest_sha256: hash(&a.manifest).unwrap(),
        configuration: json!({"CODE_PASS":"repository-only, no native oracle"}),
        approval,
        created_at: larm_domain::bridge::timestamp(now),
    }
}
async fn consumed(db: &larm_db::pool::Db, id: &str) -> bool {
    larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .get(id)
    .await
    .unwrap()
    .unwrap()
    .consumed_at
    .is_some()
}
async fn counts(db: &larm_db::pool::Db) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM operation_journal WHERE kind='adapter.conformance.profile'),(SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetch_one(&db.pool).await.unwrap()
}
#[tokio::test]
async fn perfil_y_consumo_sobreviven_reapertura_replay_no_duplica_ni_reutiliza_aprobacion() {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let p = profile(&db, &a).await;
    let repo = SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    repo.store_conformance_profile(&p, None, &ctx())
        .await
        .unwrap();
    let before = counts(&db).await;
    assert!(consumed(&db, &p.approval.id).await);
    repo.store_conformance_profile(&p, None, &ctx())
        .await
        .unwrap();
    assert_eq!(counts(&db).await, before);
    let mut duplicate = p.clone();
    duplicate.id = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        repo.store_conformance_profile(&duplicate, Some(&p.id), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_profile_approval_unavailable"
    );
    let mut wrong = ctx();
    wrong.actor_id = "other".into();
    assert!(repo
        .store_conformance_profile(&p, None, &wrong)
        .await
        .is_err());
    let path: String =
        sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(std::path::Path::new(&path))
        .await
        .unwrap();
    let repo = SqlConformanceProfiles {
        pool: reopened.pool.clone(),
    };
    assert_eq!(repo.conformance_profile(&a.id).await.unwrap(), Some(p));
    assert_eq!(counts(&reopened).await, before);
    assert_eq!(before.1, before.2);
    reopened.pool.close().await;
    drop(dir);
}
#[tokio::test]
async fn fallo_de_outbox_revierte_perfil_consumo_y_sustitucion_y_permite_retry() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let p = profile(&db, &a).await;
    let repo = SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    repo.store_conformance_profile(&p, None, &ctx())
        .await
        .unwrap();
    let next = profile(&db, &a).await;
    let before = counts(&db).await;
    sqlx::query("CREATE TRIGGER profile_outbox_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'owned profile outbox fault'); END").execute(&db.pool).await.unwrap();
    assert!(repo
        .store_conformance_profile(&next, Some(&p.id), &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db).await, before);
    assert!(!consumed(&db, &next.approval.id).await);
    assert_eq!(
        repo.conformance_profile(&a.id).await.unwrap(),
        Some(p.clone())
    );
    sqlx::query("DROP TRIGGER profile_outbox_fault")
        .execute(&db.pool)
        .await
        .unwrap();
    repo.store_conformance_profile(&next, Some(&p.id), &ctx())
        .await
        .unwrap();
    assert!(consumed(&db, &next.approval.id).await);
    assert_eq!(repo.conformance_profile(&a.id).await.unwrap(), Some(next));
    let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id=?")
        .bind(&p.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(phase, "SUPERSEDED");
}
#[tokio::test]
async fn cas_concurrente_confirma_un_solo_perfil_y_consume_solo_su_aprobacion() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let first = profile(&db, &a).await;
    let second = profile(&db, &a).await;
    let repo = SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    let c = ctx();
    let (x, y) = tokio::join!(
        repo.store_conformance_profile(&first, None, &c),
        repo.store_conformance_profile(&second, None, &c)
    );
    assert_ne!(x.is_ok(), y.is_ok());
    assert_ne!(
        consumed(&db, &first.approval.id).await,
        consumed(&db, &second.approval.id).await
    );
    assert_eq!(counts(&db).await.0, 1);
    assert!(repo.conformance_profile(&a.id).await.unwrap().is_some());
}
#[tokio::test]
async fn sondeo_activo_impide_cambiar_su_contexto_y_cas_exige_perfil_vigente() {
    use larm_domain::{hardware::Probe, ports::AdapterProbeRepository};
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let a = seed(&db).await;
    let p = profile(&db, &a).await;
    let next = profile(&db, &a).await;
    let repo = SqlConformanceProfiles {
        pool: db.pool.clone(),
    };
    repo.store_conformance_profile(&p, None, &ctx())
        .await
        .unwrap();
    assert_eq!(
        repo.store_conformance_profile(&next, None, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_profile_stale"
    );
    let jobs = larm_db::adapter_probes::SqlAdapterProbes {
        pool: db.pool.clone(),
    };
    let job = jobs
        .enqueue_probe(
            &a.id,
            &Probe {
                resource_id: None,
                profile_id: None,
            },
            &"c".repeat(64),
            "profile-active",
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(
        repo.store_conformance_profile(&next, Some(&p.id), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "adapter_probe_active"
    );
    assert!(!consumed(&db, &next.approval.id).await);
    jobs.claim_probe().await.unwrap();
    jobs.fail_probe(
        &job.id,
        &larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::ApprovalRequired,
            "fixture",
            "owned",
        ),
        false,
    )
    .await
    .unwrap();
    repo.store_conformance_profile(&next, Some(&p.id), &ctx())
        .await
        .unwrap();
}
#[tokio::test]
async fn corrupcion_de_hash_recursos_o_consumo_no_reconstruye_concesion() {
    for mode in 0..4 {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = seed(&db).await;
        let p = profile(&db, &a).await;
        let repo = SqlConformanceProfiles {
            pool: db.pool.clone(),
        };
        repo.store_conformance_profile(&p, None, &ctx())
            .await
            .unwrap();
        match mode {
            0 => {
                sqlx::query("UPDATE operation_journal SET plan_hash=? WHERE id=?")
                    .bind("f".repeat(64))
                    .bind(&p.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
            1 => {
                sqlx::query("UPDATE operation_journal SET resource_ids='[]' WHERE id=?")
                    .bind(&p.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE operation_journal SET phase='APPROVED' WHERE id=?")
                    .bind(&p.approval.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE operation_journal SET payload=json_set(payload,'$.permissions.publisher','foreign') WHERE id=?").bind(&p.approval.id).execute(&db.pool).await.unwrap();
            }
        }
        assert!(repo.conformance_profile(&a.id).await.is_err());
    }
}
#[tokio::test]
async fn snapshot_actor_caducidad_o_paquete_ajenos_no_consumen_aprobacion() {
    for mode in 0..4 {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = seed(&db).await;
        let mut p = profile(&db, &a).await;
        match mode {
            0 => p.manifest_sha256 = "d".repeat(64),
            1 => p.actor_id = "foreign".into(),
            2 => {
                sqlx::query("UPDATE operation_journal SET payload=json_set(payload,'$.expires_at','2000-01-01T00:00:00.000Z') WHERE id=?").bind(&p.approval.id).execute(&db.pool).await.unwrap();
                p.approval.expires_at = "2000-01-01T00:00:00.000Z".into();
            }
            _ => p.approval.code_hash = Some("e".repeat(64)),
        }
        let repo = SqlConformanceProfiles {
            pool: db.pool.clone(),
        };
        let before = counts(&db).await;
        assert!(repo
            .store_conformance_profile(&p, None, &ctx())
            .await
            .is_err());
        assert_eq!(counts(&db).await, before);
        assert!(!consumed(&db, &p.approval.id).await);
    }
}

#[tokio::test]
async fn triggers_que_ignoran_insert_o_cas_no_consumen_ni_confirman_exito() {
    for trigger in [
        "CREATE TRIGGER ignored_profile_write BEFORE INSERT ON operation_journal WHEN NEW.kind='adapter.conformance.profile' BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER ignored_profile_write BEFORE UPDATE ON operation_journal WHEN NEW.kind='adapter.conformance.profile' BEGIN SELECT RAISE(IGNORE); END",
    ] {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let a = seed(&db).await;
        let p = profile(&db, &a).await;
        let repo = SqlConformanceProfiles {
            pool: db.pool.clone(),
        };
        repo.store_conformance_profile(&p, None, &ctx())
            .await
            .unwrap();
        let next = profile(&db, &a).await;
        let before = counts(&db).await;
        sqlx::query(trigger).execute(&db.pool).await.unwrap();
        assert!(repo
            .store_conformance_profile(&next, Some(&p.id), &ctx())
            .await
            .is_err());
        assert_eq!(counts(&db).await, before);
        assert!(!consumed(&db, &next.approval.id).await);
        assert_eq!(repo.conformance_profile(&a.id).await.unwrap(), Some(p));
    }
}
