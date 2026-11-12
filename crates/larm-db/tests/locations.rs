//! Repositorio de `storage_location` (P014) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::locations::SqlStorageLocations;
use larm_domain::error::ErrorCode;
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{LocationFilter, MutationContext, StorageLocationRepository};
use larm_domain::storage::PerformanceClass;
use larm_domain::units::DecimalU64;
use sqlx::SqlitePool;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "operador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn location(name: &str, at: &str) -> StorageLocation {
    StorageLocation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: at.into(),
        updated_at: at.into(),
        row_version: 1,
        metadata: serde_json::Map::new(),
        name: name.into(),
        root_path: format!(r"C:\IA\{name}"),
        canonical_path: format!(r"C:\IA\{name}"),
        volume_guid: Some("fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3".into()),
        drive_letter: Some("C:".into()),
        unc_path: None,
        filesystem: "NTFS".into(),
        storage_type: "LOCAL".into(),
        capacity_bytes: DecimalU64::new(2_000_000).unwrap(),
        free_bytes: DecimalU64::new(1_000_000).unwrap(),
        reserved_bytes: DecimalU64::new(100).unwrap(),
        priority: 1,
        auto_placement: true,
        state: LocationState::Online,
        last_seen_at: Some(at.into()),
        volume_key: "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3".into(),
        performance_class: PerformanceClass::Nvme,
    }
}

async fn events(pool: &SqlitePool, id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT json_extract(e.payload, '$.action') FROM event e JOIN event_outbox o ON o.event_record_id = e.id \
         WHERE e.resource_id = ? ORDER BY e.id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn crud_con_if_match_y_eventos() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    let l = location("modelos", "2026-10-11T10:00:00.000Z");
    repo.insert(&l, &ctx()).await.unwrap();
    assert_eq!(
        repo.get(&l.id).await.unwrap().unwrap(),
        l,
        "ida y vuelta exacta"
    );

    let mut edited = l.clone();
    edited.name = "Modelos grandes".into();
    edited.reserved_bytes = DecimalU64::new(500).unwrap();
    let updated = repo.update(&edited, 1, &ctx()).await.unwrap();
    assert_eq!(
        (updated.row_version, updated.name.as_str()),
        (2, "Modelos grandes")
    );
    let stale = repo.update(&edited, 1, &ctx()).await.unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleRevision);
    let missing = location("otra", "t");
    assert_eq!(
        repo.update(&missing, 1, &ctx()).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    assert_eq!(
        repo.delete(&l.id, 1, &ctx()).await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    repo.delete(&l.id, 2, &ctx()).await.unwrap();
    assert!(repo.get(&l.id).await.unwrap().is_none());
    assert_eq!(
        events(&db.pool, &l.id).await,
        ["created", "updated", "deleted"]
    );
    assert_eq!(
        repo.delete(&l.id, 3, &ctx()).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn borrar_con_artefactos_o_en_uso_se_rechaza() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    let l = location("modelos", "t");
    repo.insert(&l, &ctx()).await.unwrap();
    // Un artefacto que vive en la ubicación (la cadena de FK del modelo no
    // importa aquí: se inserta con las FK desactivadas en una conexión aparte).
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO artifact_location (ownership, id, created_at, updated_at, row_version, metadata, artifact_id, \
         storage_location_id, relative_path, file_size, sha256, state, verified_at) \
         VALUES ('LARM_MANAGED', ?, 't', 't', 1, '{}', 'a1', ?, 'blobs/x', 1, NULL, 'ONLINE', NULL)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&l.id)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    conn.close().await.unwrap();

    assert_eq!(repo.artifact_count(&l.id).await.unwrap(), 1);
    let err = repo.delete(&l.id, 1, &ctx()).await.unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::Conflict, "location_has_artifacts")
    );
    assert!(
        repo.get(&l.id).await.unwrap().is_some(),
        "la ubicación sigue"
    );
    assert_eq!(
        events(&db.pool, &l.id).await,
        ["created"],
        "sin evento de un borrado rechazado"
    );
}

#[tokio::test]
async fn lista_paginada_y_filtros() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    let a = location("Modelos", "2026-10-11T10:00:01.000Z");
    let mut b = location("Cache", "2026-10-11T10:00:02.000Z");
    b.state = LocationState::Offline;
    let c = location("Difusion", "2026-10-11T10:00:03.000Z");
    for l in [&a, &b, &c] {
        repo.insert(l, &ctx()).await.unwrap();
    }
    let all = repo
        .list(None, 10, &LocationFilter::default())
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["Modelos", "Cache", "Difusion"]
    );
    let page = repo
        .list(
            Some((a.created_at.clone(), a.id.clone())),
            1,
            &LocationFilter::default(),
        )
        .await
        .unwrap();
    assert_eq!(page[0].name, "Cache");
    let q = repo
        .list(
            None,
            10,
            &LocationFilter {
                q: Some("dif".into()),
                state: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        q.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["Difusion"]
    );
    let offline = repo
        .list(
            None,
            10,
            &LocationFilter {
                q: None,
                state: Some(LocationState::Offline),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        offline.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["Cache"]
    );
    let empty_q = repo
        .list(
            None,
            10,
            &LocationFilter {
                q: Some(String::new()),
                state: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(empty_q.len(), 3, "q vacío omite el filtro");
}
