//! P119: pruebas SQL del repositorio de admisiones — política singleton
//! versionada (PUT con row_version+1 y journal/evento/outbox), evaluación con
//! cuota diaria agregada por day_key, revocación inmediata (REVOKED) con
//! triggers de inmutabilidad, listado y consultas de revocados.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::{download_admissions::SqlDownloadAdmissions, pool::Db};
use larm_domain::download_admission::{AdmissionCandidate, DownloadAdmissionPolicy};
use larm_domain::ports::MutationContext;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "p119-fixture".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn policy() -> DownloadAdmissionPolicy {
    DownloadAdmissionPolicy {
        enabled: true,
        source_allowlist: vec!["0192aaaa-0000-7000-8000-00000000aa01".into()],
        capability_allowlist: vec!["vendor.future".into()],
        license_allowlist: vec![],
        min_trust: "TECHNICALLY_APPROVED".into(),
        daily_quota_bytes: Some("1000".into()),
        storage_location_id: None,
    }
}

fn candidate() -> AdmissionCandidate {
    AdmissionCandidate {
        variant_id: "0192aaaa-0000-7000-8000-00000000bb01".into(),
        source_id: "0192aaaa-0000-7000-8000-00000000aa01".into(),
        capability: "vendor.future".into(),
        license_id: None,
        trust: "TECHNICALLY_APPROVED".into(),
        estimated_bytes: 400,
    }
}

#[tokio::test]
async fn politica_singleton_versiona_journal_y_evento() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlDownloadAdmissions {
        pool: db.pool.clone(),
    };
    let mut policy = policy();
    let first = repo.put_policy(&policy, &ctx()).await.unwrap();
    assert_eq!(first.row_version, 1);
    policy.daily_quota_bytes = Some("2000".into());
    let second = repo.put_policy(&policy, &ctx()).await.unwrap();
    assert_eq!(second.row_version, 2);
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM operation_journal WHERE kind='download.admission_policy'), \
         (SELECT count(*) FROM event WHERE event_type='download.admission_policy.updated'), \
         (SELECT count(*) FROM event_outbox WHERE event_record_id IN (SELECT id FROM event WHERE event_type='download.admission_policy.updated'))",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(counts, (2, 2, 2));
    let stored = repo.get_policy().await.unwrap().unwrap();
    assert_eq!(stored.policy.daily_quota_bytes, Some("2000".into()));
    assert_eq!(stored.row_version, 2);
}

#[tokio::test]
async fn admit_registra_bloque_y_admite_dentro_de_cuota_diaria() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlDownloadAdmissions {
        pool: db.pool.clone(),
    };
    repo.put_policy(&policy(), &ctx()).await.unwrap();
    // Fuente fuera de allowlist: BLOCKED sin job.
    let mut foreign = candidate();
    foreign.source_id = "0192aaaa-0000-7000-8000-00000000aa09".into();
    let (blocked, policy_version) = repo.admit(&foreign, &ctx()).await.unwrap();
    assert_eq!(blocked.decision, "BLOCKED");
    assert_eq!(blocked.reason, "admission_source_not_allowlisted");
    assert!(policy_version.is_none());
    // Dentro de cuota: ADMITTED con BACKGROUND implícito del evaluador.
    let (admitted, _) = repo.admit(&candidate(), &ctx()).await.unwrap();
    assert_eq!(admitted.decision, "ADMITTED");
    // El day_key es la fecha UTC del momento de la admisión (no una fecha fija):
    // el test lo deriva del reloj para no romper al cambiar el día.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(admitted.day_key, today);
    // Cuota diaria 1000: primero y segundo ADMITTED (0+400, 400+400),
    // tercero excede (800+400 > 1000) → BLOCKED sin job.
    let (second, _) = repo.admit(&candidate(), &ctx()).await.unwrap();
    assert_eq!(second.decision, "ADMITTED");
    let (third, _) = repo.admit(&candidate(), &ctx()).await.unwrap();
    assert_eq!(third.decision, "BLOCKED");
    assert_eq!(third.reason, "admission_quota_exhausted");
    let used: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(cast(estimated_bytes AS INTEGER)),0) FROM download_admission WHERE decision='ADMITTED'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(used, 800);
}

#[tokio::test]
async fn revocacion_inmediata_marca_revoked_y_trigger_protege() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlDownloadAdmissions {
        pool: db.pool.clone(),
    };
    repo.put_policy(&policy(), &ctx()).await.unwrap();
    let (admitted, _) = repo.admit(&candidate(), &ctx()).await.unwrap();
    let revoked = repo.revoke(&admitted.id, &ctx()).await.unwrap();
    assert_eq!(revoked.decision, "REVOKED");
    assert!(repo.is_revoked(&admitted.variant_id).await.unwrap());
    // Imposible borrar (trigger) ni re-revocar (sólo desde ADMITTED).
    assert!(sqlx::query("DELETE FROM download_admission WHERE id=?")
        .bind(&admitted.id)
        .execute(&db.pool)
        .await
        .is_err());
    assert!(repo.revoke(&admitted.id, &ctx()).await.is_err());
}

#[tokio::test]
async fn listado_devuelve_orden_descendente_y_valida() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlDownloadAdmissions {
        pool: db.pool.clone(),
    };
    repo.put_policy(&policy(), &ctx()).await.unwrap();
    for _ in 0..3 {
        repo.admit(&candidate(), &ctx()).await.unwrap();
    }
    let listed = repo.list(2).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].created_at >= listed[1].created_at);
}
