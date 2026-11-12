//! Repositorio `hardware_profile` (P010) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::hardware::SqlHardware;
use larm_domain::error::ErrorCode;
use larm_domain::hardware::{
    CpuInfo, HardwareProbe, HardwareSnapshot, MemoryInfo, ObservedMetrics, OsInfo, SYSTEM_RAM_POOL,
};
use larm_domain::ports::HardwareRepository;
use larm_domain::units::DecimalU64;

fn probe(build: &str, available: u64) -> HardwareProbe {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "profiler".into(),
        serde_json::json!({"name": "test", "version": "0"}),
    );
    HardwareProbe {
        snapshot: HardwareSnapshot {
            os: OsInfo {
                edition: Some("Professional".into()),
                build: Some(build.into()),
                architecture: Some("AMD64".into()),
            },
            cpu: CpuInfo {
                vendor: Some("AuthenticAMD".into()),
                model: Some("cpu".into()),
                physical_cores: Some(16),
                logical_cores: Some(32),
                instruction_sets: vec!["avx2".into(), "avx".into()],
            },
            ram: MemoryInfo {
                physical_bytes: DecimalU64::new(128 << 30),
                usable_bytes: DecimalU64::new(64 << 30),
                pool_id: SYSTEM_RAM_POOL.into(),
            },
            devices: vec![],
            dependencies: vec![],
            observed_metrics: ObservedMetrics {
                available_ram_bytes: DecimalU64::new(available),
                cpu_usage_percent: Some(3.5),
            },
        },
        metadata,
        evidence_refs: vec!["ev".into()],
        probed_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
    }
}

async fn count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM hardware_profile")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn misma_huella_actualiza_y_huella_nueva_crea_perfil() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlHardware {
        pool: db.pool.clone(),
    };
    assert!(repo.latest().await.unwrap().is_none());
    let first = repo.record(&probe("26200.6899", 40 << 30)).await.unwrap();
    assert_eq!(first.row_version, 1);
    assert_eq!(first.hardware_fingerprint.len(), 64);
    // Orden canónico persistido.
    assert_eq!(first.snapshot.cpu.instruction_sets, ["avx", "avx2"]);

    // Sólo cambian métricas observadas: mismo perfil, nueva revisión.
    let again = repo.record(&probe("26200.6899", 30 << 30)).await.unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.row_version, 2);
    assert_eq!(again.created_at, first.created_at);
    assert_eq!(again.hardware_fingerprint, first.hardware_fingerprint);
    assert_eq!(
        again.snapshot.observed_metrics.available_ram_bytes,
        DecimalU64::new(30 << 30)
    );

    // Cambia el build: configuración nueva, la anterior queda como historia.
    let updated_os = repo.record(&probe("26200.7000", 30 << 30)).await.unwrap();
    assert_ne!(updated_os.id, first.id);
    assert_ne!(updated_os.hardware_fingerprint, first.hardware_fingerprint);
    assert_eq!(count(&db.pool).await, 2);

    // Volver al build anterior no reutiliza el perfil viejo: sólo se compara
    // con el más reciente, así la historia refleja la secuencia real.
    let back = repo.record(&probe("26200.6899", 30 << 30)).await.unwrap();
    assert_ne!(back.id, first.id);
    assert_eq!(back.hardware_fingerprint, first.hardware_fingerprint);
    assert_eq!(count(&db.pool).await, 3);
    assert_eq!(repo.latest().await.unwrap(), Some(back.clone()));

    let page = repo.list(None, 2).await.unwrap();
    assert_eq!(
        page.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        [first.id.as_str(), updated_os.id.as_str()]
    );
    let last = page.last().unwrap();
    let rest = repo
        .list(Some((last.created_at.clone(), last.id.clone())), 10)
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, back.id);
}

#[tokio::test]
async fn sondeos_simultaneos_de_la_misma_configuracion_dan_un_perfil() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlHardware {
        pool: db.pool.clone(),
    };
    let mut tasks = Vec::new();
    for i in 0..10u64 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.record(&probe("26200.6899", (20 + i) << 30)).await
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    for t in tasks {
        ids.insert(t.await.unwrap().unwrap().id);
    }
    assert_eq!(ids.len(), 1, "un único perfil para una única configuración");
    assert_eq!(count(&db.pool).await, 1);
    let profile = repo.list(None, 10).await.unwrap().remove(0);
    assert_eq!(profile.row_version, 10);
}

#[tokio::test]
async fn snapshot_incoherente_se_rechaza_sin_escribir() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlHardware {
        pool: db.pool.clone(),
    };
    let mut bad = probe("26200.6899", 40 << 30);
    bad.snapshot.ram.usable_bytes = DecimalU64::new(256 << 30);
    let err = repo.record(&bad).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ValidationFailed);
    assert_eq!(err.details.reason, "hardware_snapshot_invalid");
    assert_eq!(count(&db.pool).await, 0);
}

#[tokio::test]
async fn una_fila_fuera_del_schema_es_error_y_no_dato() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlHardware {
        pool: db.pool.clone(),
    };
    repo.record(&probe("26200.6899", 40 << 30)).await.unwrap();
    sqlx::query("UPDATE hardware_profile SET snapshot = json_set(snapshot, '$.cpu.threads', 32)")
        .execute(&db.pool)
        .await
        .unwrap();
    let err = repo.list(None, 10).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
}

#[tokio::test]
async fn cada_configuracion_guarda_version_y_diff_con_la_anterior() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlHardware {
        pool: db.pool.clone(),
    };
    let first = repo.record(&probe("26200.6899", 40 << 30)).await.unwrap();
    let f = &first.metadata["fingerprint"];
    assert_eq!(f["version"], 1);
    assert!(f["previous_profile_id"].is_null());
    assert_eq!(f["changes"], serde_json::json!([]));

    let second = repo.record(&probe("26200.7000", 40 << 30)).await.unwrap();
    let f = &second.metadata["fingerprint"];
    assert_eq!(f["previous_profile_id"], first.id.as_str());
    assert_eq!(
        f["previous_fingerprint"],
        first.hardware_fingerprint.as_str()
    );
    assert_eq!(
        f["changes"],
        serde_json::json!([{"field": "os.build", "before": "26200.6899", "after": "26200.7000"}])
    );

    // Misma configuración: se actualiza el perfil y se conserva su diff.
    let again = repo.record(&probe("26200.7000", 20 << 30)).await.unwrap();
    assert_eq!(
        (again.id.as_str(), again.row_version),
        (second.id.as_str(), 2)
    );
    assert_eq!(
        again.metadata["fingerprint"],
        second.metadata["fingerprint"]
    );
    assert_eq!(again.metadata["profiler"], second.metadata["profiler"]);
}

mod jobs {
    use super::probe;
    use larm_db::hardware::SqlHardware;
    use larm_db::jobs::SqlJournalJobs;
    use larm_domain::bridge::JobPhase;
    use larm_domain::error::{ErrorCode, LarmError};
    use larm_domain::ports::{HardwareRepository, JournalJobRepository, MutationContext};

    fn ctx(actor: &str) -> MutationContext {
        MutationContext {
            actor_id: actor.into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        }
    }

    #[tokio::test]
    async fn diez_peticiones_simultaneas_encolan_un_solo_job() {
        let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
        let repo = SqlJournalJobs::hardware(db.pool.clone());
        let mut tasks = Vec::new();
        for i in 0..10 {
            let repo = repo.clone();
            tasks.push(tokio::spawn(async move {
                repo.enqueue(&ctx(&format!("a{i}")), None).await
            }));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut created = 0;
        for t in tasks {
            let (job, new) = t.await.unwrap().unwrap();
            ids.insert(job.state.job_id);
            created += usize::from(new);
        }
        assert_eq!((ids.len(), created), (1, 1));
    }

    #[tokio::test]
    async fn ciclo_completo_transiciones_invalidas_y_recuperacion() {
        let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
        let repo = SqlJournalJobs::hardware(db.pool.clone());
        let profiles = SqlHardware {
            pool: db.pool.clone(),
        };
        let (job, _) = repo.enqueue(&ctx("autor"), None).await.unwrap();
        let id = job.state.job_id.clone();
        assert_eq!(
            (job.state.state, job.actor_id.as_str()),
            (JobPhase::Queued, "autor")
        );

        // No se completa sin haber empezado.
        let profile = profiles.record(&probe("26200.1", 1 << 30)).await.unwrap();
        let result = serde_json::json!({"hardware_fingerprint": profile.hardware_fingerprint});
        let err = repo
            .complete(&id, &profile.id, result.clone(), &[])
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidStateTransition);

        repo.running(&id).await.unwrap();
        repo.complete(&id, &profile.id, result.clone(), &[])
            .await
            .unwrap();
        let done = repo.get(&id).await.unwrap().unwrap().state;
        assert_eq!(done.state, JobPhase::Completed);
        assert_eq!(done.resource_id.as_deref(), Some(profile.id.as_str()));
        assert_eq!(
            done.result.unwrap()["hardware_fingerprint"],
            profile.hardware_fingerprint
        );
        // Terminal: ni fallar ni volver a empezar.
        let boom = LarmError::new(ErrorCode::Internal, "x", "y");
        assert_eq!(
            repo.fail(&id, &boom).await.unwrap_err().code,
            ErrorCode::InvalidStateTransition
        );
        assert_eq!(
            repo.running(&id).await.unwrap_err().code,
            ErrorCode::InvalidStateTransition
        );

        // Un job que quedó a medias por un reinicio pasa a FAILED reintentable.
        let (second, created) = repo.enqueue(&ctx("autor"), None).await.unwrap();
        assert!(created);
        repo.running(&second.state.job_id).await.unwrap();
        assert_eq!(repo.recover().await.unwrap(), 1);
        let failed = repo.get(&second.state.job_id).await.unwrap().unwrap().state;
        assert_eq!(failed.state, JobPhase::Failed);
        let error = failed.error.unwrap();
        assert_eq!(
            (error.details.reason.as_str(), error.retryable),
            ("job_interrupted", true)
        );
        assert_eq!(repo.recover().await.unwrap(), 0);
        assert!(repo
            .get("0192aaaa-0000-7000-8000-0000000000ff")
            .await
            .unwrap()
            .is_none());
    }
}
