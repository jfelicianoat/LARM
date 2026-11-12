//! Último estado de `storage_location` (P012) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::storage::SqlStorageState;
use larm_domain::locations::DiskThresholds;
use larm_domain::ports::{MutationContext, ObservationContext, StorageStateRepository};
use larm_domain::storage::{
    BusKind, DriveKind, PerformanceClass, StorageObservation, StorageVolume, VolumeIdentity,
};
use larm_domain::units::DecimalU64;
use sqlx::SqlitePool;

const C: &str = "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3";
const NAS: &str = r"\\nas\modelos";

fn none() -> ObservationContext {
    ObservationContext::default()
}

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "larm-daemon".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

async fn location(
    pool: &SqlitePool,
    name: &str,
    key: &str,
    unc: Option<&str>,
    state: &str,
) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO storage_location (id, created_at, updated_at, row_version, metadata, name, root_path, canonical_path, \
         volume_guid, drive_letter, unc_path, filesystem, storage_type, capacity_bytes, free_bytes, reserved_bytes, priority, \
         auto_placement, state, last_seen_at, volume_key, performance_class) \
         VALUES (?, 't', 't', 1, '{}', ?, ?, ?, ?, NULL, ?, 'UNKNOWN', 'LOCAL', 0, 0, 0, 0, 0, ?, NULL, ?, 'UNKNOWN')",
    )
    .bind(&id)
    .bind(name)
    .bind(format!(r"D:\{name}"))
    .bind(format!(r"D:\{name}"))
    .bind(unc.is_none().then_some(key))
    .bind(unc)
    .bind(state)
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn local(
    key: &str,
    letter: Option<&str>,
    online: bool,
    read_only: bool,
    free: u64,
) -> StorageVolume {
    StorageVolume {
        identity: VolumeIdentity {
            volume_key: Some(key.into()),
            volume_guid: Some(key.into()),
            unc_path: None,
            mount_points: letter.iter().map(|l| format!("{l}\\")).collect(),
            drive_letter: letter.map(String::from),
        },
        label: None,
        filesystem: Some("NTFS".into()),
        drive_kind: DriveKind::Fixed,
        bus: Some(BusKind::Nvme),
        seek_penalty: Some(false),
        performance_class: PerformanceClass::Nvme,
        capacity_bytes: online.then(|| DecimalU64::new(2_000).unwrap()),
        free_bytes: online.then(|| DecimalU64::new(free).unwrap()),
        read_only,
        online,
        evidence_refs: vec![],
    }
}

fn obs(volumes: Vec<StorageVolume>, at: &str) -> StorageObservation {
    StorageObservation {
        volumes,
        unknown: vec![],
        observed_at: at.into(),
    }
}

#[derive(Debug, PartialEq)]
struct Row {
    state: String,
    row_version: i64,
    free: i64,
    capacity: i64,
    letter: Option<String>,
    class: String,
    last_seen: Option<String>,
}

async fn row(pool: &SqlitePool, id: &str) -> Row {
    let (state, row_version, free, capacity, letter, class, last_seen) = sqlx::query_as::<_, (String, i64, i64, i64, Option<String>, String, Option<String>)>(
        "SELECT state, row_version, free_bytes, capacity_bytes, drive_letter, performance_class, last_seen_at FROM storage_location WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    Row {
        state,
        row_version,
        free,
        capacity,
        letter,
        class,
        last_seen,
    }
}

async fn events(pool: &SqlitePool, id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id = e.id \
         WHERE e.event_type = 'storage.state_changed' AND e.resource_id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn ciclo_de_vida_de_una_ubicacion_segun_su_volumen() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    let a = location(&db.pool, "modelos", C, None, "UNKNOWN").await;
    let b = location(&db.pool, "cache", C, None, "UNKNOWN").await;

    // Primera observación: ONLINE, datos reales, versión+1 y evento; las dos
    // raíces del mismo volumen cambian juntas.
    let changes = repo
        .apply(
            &obs(
                vec![local(C, Some("C:"), true, false, 500)],
                "2026-10-11T10:00:00.000Z",
            ),
            &none(),
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(changes.len(), 2);
    let r = row(&db.pool, &a).await;
    assert_eq!(
        (
            r.state.as_str(),
            r.row_version,
            r.free,
            r.capacity,
            r.class.as_str()
        ),
        ("ONLINE", 2, 500, 2000, "NVME")
    );
    assert_eq!(r.letter.as_deref(), Some("C:"));
    assert_eq!(row(&db.pool, &b).await.row_version, 2);
    assert_eq!(events(&db.pool, &a).await, 1);

    // Solo cambia el espacio libre: se refresca sin versión ni evento.
    assert!(repo
        .apply(
            &obs(
                vec![local(C, Some("C:"), true, false, 400)],
                "2026-10-11T10:05:00.000Z"
            ),
            &none(),
            &ctx()
        )
        .await
        .unwrap()
        .is_empty());
    let r = row(&db.pool, &a).await;
    assert_eq!(
        (r.row_version, r.free, r.last_seen.as_deref()),
        (2, 400, Some("2026-10-11T10:05:00.000Z"))
    );
    assert_eq!(events(&db.pool, &a).await, 1);

    // Cambia la letra: misma identidad, nueva versión.
    repo.apply(
        &obs(
            vec![local(C, Some("F:"), true, false, 400)],
            "2026-10-11T10:06:00.000Z",
        ),
        &none(),
        &ctx(),
    )
    .await
    .unwrap();
    let r = row(&db.pool, &a).await;
    assert_eq!(
        (r.state.as_str(), r.letter.as_deref(), r.row_version),
        ("ONLINE", Some("F:"), 3)
    );

    // El volumen desaparece: OFFLINE conservando datos y la última vez visto.
    let changes = repo
        .apply(&obs(vec![], "2026-10-11T11:00:00.000Z"), &none(), &ctx())
        .await
        .unwrap();
    assert_eq!(changes[0].from, "ONLINE");
    assert_eq!(changes[0].to, "OFFLINE");
    let r = row(&db.pool, &a).await;
    assert_eq!(
        (r.state.as_str(), r.free, r.capacity),
        ("OFFLINE", 400, 2000)
    );
    assert_eq!(r.last_seen.as_deref(), Some("2026-10-11T10:06:00.000Z"));

    // Vuelve en solo lectura.
    repo.apply(
        &obs(
            vec![local(C, Some("F:"), true, true, 400)],
            "2026-10-11T12:00:00.000Z",
        ),
        &none(),
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(row(&db.pool, &a).await.state, "READ_ONLY");
    assert_eq!(events(&db.pool, &a).await, 4);
}

#[tokio::test]
async fn raices_unc_y_volumen_de_red_caido() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    let id = location(&db.pool, "nas", NAS, Some(r"\\NAS\Modelos"), "ONLINE").await;
    let roots = repo.roots().await.unwrap();
    assert_eq!(roots[0].unc_path.as_deref(), Some(r"\\NAS\Modelos"));
    let mut down = local(NAS, None, false, false, 0);
    down.identity.volume_guid = None;
    down.performance_class = PerformanceClass::Network;
    repo.apply(&obs(vec![down], "t"), &none(), &ctx())
        .await
        .unwrap();
    let r = row(&db.pool, &id).await;
    assert_eq!((r.state.as_str(), r.last_seen), ("OFFLINE", None));
}

#[tokio::test]
async fn observaciones_simultaneas_no_duplican_cambios() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    let id = location(&db.pool, "modelos", C, None, "UNKNOWN").await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        tasks.push(tokio::spawn(async move {
            repo.apply(
                &obs(vec![local(C, Some("C:"), true, false, 300)], "t"),
                &none(),
                &ctx(),
            )
            .await
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap().unwrap().len();
    }
    assert_eq!(total, 1, "solo la primera observación es un cambio");
    assert_eq!(row(&db.pool, &id).await.row_version, 2);
    assert_eq!(events(&db.pool, &id).await, 1);
}

#[tokio::test]
async fn raiz_borrada_es_missing_y_disco_critico_es_low_space() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    let id = location(&db.pool, "modelos", C, None, "UNKNOWN").await;
    let missing = ObservationContext {
        root_present: std::collections::BTreeMap::from([(id.clone(), false)]),
        thresholds: None,
        settings_revision: None,
        known: None,
    };
    repo.apply(
        &obs(vec![local(C, Some("C:"), true, false, 500)], "t1"),
        &missing,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(row(&db.pool, &id).await.state, "MISSING");
    // 50 libres de 2000 = 2,5 %: por debajo del crítico (5 %).
    let pct = ObservationContext {
        root_present: std::collections::BTreeMap::from([(id.clone(), true)]),
        thresholds: Some(DiskThresholds::Percent {
            warning_bp: 1500,
            critical_bp: 500,
        }),
        settings_revision: Some(7),
        known: None,
    };
    repo.apply(
        &obs(vec![local(C, Some("C:"), true, false, 50)], "t2"),
        &pct,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(row(&db.pool, &id).await.state, "LOW_SPACE");
    // 100 libres de 2000 = 5 %: la igualdad no cruza.
    repo.apply(
        &obs(vec![local(C, Some("C:"), true, false, 100)], "t3"),
        &pct,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(row(&db.pool, &id).await.state, "ONLINE");
    assert_eq!(events(&db.pool, &id).await, 3);
}

#[tokio::test]
async fn h09_emite_una_alerta_por_volumen_y_transicion_y_omite_offline() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    let first = location(&db.pool, "modelos", C, None, "ONLINE").await;
    let second = location(&db.pool, "cache", C, None, "ONLINE").await;
    let context_for = |warning_bp, critical_bp, revision| ObservationContext {
        root_present: std::collections::BTreeMap::from([
            (first.clone(), true),
            (second.clone(), true),
        ]),
        thresholds: Some(DiskThresholds::Percent {
            warning_bp,
            critical_bp,
        }),
        settings_revision: Some(revision),
        known: None,
    };
    let count = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notification WHERE message_code='LOW_DISK_SPACE'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };
    let context = context_for(1500, 500, 7);

    // La igualdad del 15 % crea una base NORMAL, sin alertar.
    repo.apply(
        &obs(vec![local(C, Some("D:"), true, false, 300)], "h09-0"),
        &context,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(count().await, 0);
    // Cruza warning una sola vez aunque haya dos raíces y se repita el refresh.
    let warning = obs(vec![local(C, Some("D:"), true, false, 299)], "h09-1");
    repo.apply(&warning, &context, &ctx()).await.unwrap();
    repo.apply(&warning, &context, &ctx()).await.unwrap();
    assert_eq!(count().await, 1);
    let warning_meta: (String, String) = sqlx::query_as(
        "SELECT severity,metadata FROM notification WHERE message_code='LOW_DISK_SPACE'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(warning_meta.0, "warning");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&warning_meta.1).unwrap()["settings_revision"],
        7
    );

    // Una observación offline conserva el último nivel y no genera otro aviso.
    repo.apply(
        &obs(vec![local(C, Some("D:"), false, false, 0)], "h09-offline"),
        &context,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(count().await, 1);

    repo.apply(
        &obs(vec![local(C, Some("D:"), true, false, 99)], "h09-2"),
        &context,
        &ctx(),
    )
    .await
    .unwrap();
    repo.apply(
        &obs(vec![local(C, Some("D:"), true, false, 300)], "h09-3"),
        &context,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(
        count().await,
        3,
        "WARNING, CRITICAL y recuperación por volumen físico"
    );
    let severities: Vec<String> = sqlx::query_scalar(
        "SELECT severity FROM notification WHERE message_code='LOW_DISK_SPACE' ORDER BY created_at,id",
    ).fetch_all(&db.pool).await.unwrap();
    assert!(severities.contains(&"error".into()));
    assert!(severities.contains(&"info".into()));

    // Cambiar umbrales en Settings vuelve a evaluar la misma observación y
    // vincula el aviso al nuevo número de revisión.
    let revised = context_for(2000, 500, 8);
    repo.apply(
        &obs(
            vec![local(C, Some("D:"), true, false, 399)],
            "h09-settings-change",
        ),
        &revised,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(count().await, 4);
    let revision: i64 = sqlx::query_scalar(
        "SELECT settings_revision FROM storage_volume_alert_state WHERE volume_key=?",
    )
    .bind(C)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(revision, 8);
}

#[tokio::test]
async fn ubicacion_creada_durante_el_sondeo_no_se_marca_offline() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageState {
        pool: db.pool.clone(),
    };
    // El sondeo empezó sin conocer la ubicación de red: su recurso no se miró.
    let id = location(&db.pool, "red", NAS, Some(NAS), "ONLINE").await;
    let before = row(&db.pool, &id).await;
    let started_without = ObservationContext {
        known: Some(std::collections::BTreeSet::new()),
        ..ObservationContext::default()
    };
    let changes = repo
        .apply(
            &obs(vec![local(C, Some("C:"), true, false, 500)], "t1"),
            &started_without,
            &ctx(),
        )
        .await
        .unwrap();
    assert!(changes.is_empty());
    assert_eq!(
        row(&db.pool, &id).await.state,
        before.state,
        "no se juzga con un sondeo que no la miró"
    );
    // Un sondeo que sí la conocía y no vio su recurso: OFFLINE.
    let knew = ObservationContext {
        known: Some(std::collections::BTreeSet::from([id.clone()])),
        ..ObservationContext::default()
    };
    repo.apply(
        &obs(vec![local(C, Some("C:"), true, false, 500)], "t2"),
        &knew,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(row(&db.pool, &id).await.state, "OFFLINE");
}
