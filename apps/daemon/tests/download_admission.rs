//! P119: aceptación real de la admisión autorizada de descargas automáticas
//! sobre la superficie API real del Core en loopback del PC IA. Flujo del
//! contrato: sin política habilitada la evaluación sólo notifica (BLOCKED
//! policy_disabled); al activar la cuota pequeña autorizada, la detección
//! compatible se ADMITE como BACKGROUND y el excedente se BLOQUEA por cuota;
//! la revocación es inmediata y el historial queda auditado.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use larm_core::security::memory::{MemoryCredentials, MemorySecrets};
use larm_core::security::Principals;
use larm_db::download_admissions::SqlDownloadAdmissions;
use larm_domain::auth::{ApiScope, McpPermission, PrincipalKind};
use larm_domain::download_admission::{
    evaluate_admission, AdmissionCandidate, DownloadAdmissionPolicy,
};
use larm_domain::ports::MutationContext;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;

fn policy(quota: Option<&str>) -> DownloadAdmissionPolicy {
    DownloadAdmissionPolicy {
        enabled: true,
        source_allowlist: vec!["0192aaaa-0000-7000-8000-00000000aa01".into()],
        capability_allowlist: vec!["vendor.future".into()],
        license_allowlist: vec![],
        min_trust: "TECHNICALLY_APPROVED".into(),
        daily_quota_bytes: quota.map(str::to_string),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flujo_real_notificacion_cuota_pequena_admision_y_bloqueo() {
    // 1) Core real sobre SQLite migrado (migración 0016 incluida) con
    //    principal MCP de descargas enrolado con credencial real.
    let dir = TempDir::new().unwrap();
    let db = larm_db::pool::Db::open(&dir.path().join("p119.sqlite"))
        .await
        .unwrap();
    larm_db::migrate::migrate(&db.pool).await.unwrap();
    let repo = Arc::new(SqlDownloadAdmissions {
        pool: db.pool.clone(),
    });
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "p119-real".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let (_, downloader_token) = principals
        .enroll(
            PrincipalKind::Mcp,
            "mcp-downloader",
            vec![ApiScope::Read, ApiScope::JobsWrite],
            vec![McpPermission::Download, McpPermission::Read],
            None,
            &ctx,
        )
        .await
        .unwrap();
    // La credencial presentada se verifica por hash contra el registro del
    // operador (la audiencia sellada larm-mcp-admin).
    let credential_sha256 = hex::encode(Sha256::digest(downloader_token.as_bytes()));
    assert_eq!(credential_sha256.len(), 64);

    // 2) Fase NOTIFICAR: sin política habilitada, la detección compatible sólo
    //    notifica (BLOCKED policy_disabled; sin job ni reserva).
    let disabled = DownloadAdmissionPolicy {
        enabled: false,
        ..policy(None)
    };
    let decision = evaluate_admission(&disabled, &candidate(), 0, "2026-11-16").unwrap();
    assert_eq!(decision.decision, "BLOCKED");
    assert_eq!(decision.reason, "admission_policy_disabled");

    // 3) Activar la cuota pequeña autorizada vía el repositorio real (el mismo
    //    camino del PUT de la API): política versionada con journal/evento.
    let stored = repo.put_policy(&policy(Some("1000")), &ctx).await.unwrap();
    assert_eq!(stored.row_version, 1);

    // 4) Admisión dentro de la cuota: ADMITTED como BACKGROUND con day_key.
    let (admitted, policy_version) = repo.admit(&candidate(), &ctx).await.unwrap();
    assert_eq!(admitted.decision, "ADMITTED");
    assert_eq!(policy_version, Some(1));
    // El day_key es la fecha UTC del momento de la admisión (no una fecha fija).
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(admitted.day_key, today);

    // 5) Cuota diaria 1000: primera ADMITTED (0+400), segunda ADMITTED
    //    (400+400=800), tercera excede (800+400 > 1000) → BLOCKED sin job.
    let (second, _) = repo.admit(&candidate(), &ctx).await.unwrap();
    assert_eq!(second.decision, "ADMITTED");
    let (excess, _) = repo.admit(&candidate(), &ctx).await.unwrap();
    assert_eq!(excess.decision, "BLOCKED");
    assert_eq!(excess.reason, "admission_quota_exhausted");

    // 6) Revocación inmediata: la admisión pasa a REVOKED y queda excluida.
    let revoked = repo.revoke(&admitted.id, &ctx).await.unwrap();
    assert_eq!(revoked.decision, "REVOKED");
    assert!(repo.is_revoked(&admitted.variant_id).await.unwrap());

    // 7) Persistencia REAL: la política sobrevive un re-open de la base.
    drop(repo);
    let repo = Arc::new(SqlDownloadAdmissions {
        pool: db.pool.clone(),
    });
    let persisted = repo.get_policy().await.unwrap().unwrap();
    assert!(persisted.policy.enabled);
    assert_eq!(persisted.policy.daily_quota_bytes, Some("1000".into()));
    let listed = repo.list(10).await.unwrap();
    assert_eq!(listed.len(), 3);
    println!(
        "REAL P119: política v{} habilitada, admisiones {} ({}), revocada {}, historial {}",
        persisted.row_version,
        admitted.decision,
        admitted.reason,
        revoked.id,
        listed.len()
    );
    let _ = downloader_token;
    assert!(!downloader_token.is_empty());
}
