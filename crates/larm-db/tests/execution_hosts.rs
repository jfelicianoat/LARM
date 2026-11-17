//! P081: registro read-only de hosts de ejecución observados. El upsert
//! idempotente avanza row_version, journal/evento/outbox van en la misma
//! transacción, la lectura es por id exacto y el listado filtra por nombre y
//! estado. El registro nunca habilita ejecución: no hay ruta de promoción.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::{execution_hosts::SqlExecutionHosts, pool::Db};
use larm_domain::{
    ports::MutationContext,
    runtime::{ExecutionHost, ExecutionHostKind, ExternalHostState},
};
use serde_json::Map;

fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "p081-fixture".into(),
        correlation_id: uid(),
    }
}

fn host(kind: ExecutionHostKind) -> ExecutionHost {
    ExecutionHost {
        id: uid(),
        created_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        updated_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        row_version: 1,
        metadata: Map::new(),
        kind,
        name: if kind == ExecutionHostKind::Wsl2 {
            "Ubuntu".into()
        } else {
            "docker".into()
        },
        engine_version: Some("29.7.2".into()),
        wsl_version: (kind == ExecutionHostKind::Wsl2).then_some(2),
        default: kind == ExecutionHostKind::Wsl2,
        status: ExternalHostState::Online,
        evidence_refs: vec!["fixture://p081/wsl-status".into()],
    }
}

#[tokio::test]
async fn registro_replay_avanza_row_version_y_conserva_eventos_unicos() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlExecutionHosts {
        pool: db.pool.clone(),
    };
    let mut observed = host(ExecutionHostKind::Wsl2);
    repo.record(&observed, &ctx()).await.unwrap();
    // La misma observación repetida es idempotente en identidad y avanza.
    observed.updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
    repo.record(&observed, &ctx()).await.unwrap();
    let stored = repo.find(&observed.id).await.unwrap().unwrap();
    assert_eq!(stored.row_version, 2);
    let (journal, events, outbox): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM operation_journal WHERE kind='runtime.execution_host'), \
         (SELECT count(*) FROM event WHERE resource_type='execution_host'), \
         (SELECT count(*) FROM event_outbox WHERE event_record_id IN (SELECT id FROM event WHERE resource_type='execution_host'))",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((journal, events, outbox), (2, 2, 2));
}

#[tokio::test]
async fn roundtrip_conserva_todos_los_campos_del_dto() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlExecutionHosts {
        pool: db.pool.clone(),
    };
    let container = host(ExecutionHostKind::Container);
    repo.record(&container, &ctx()).await.unwrap();
    let stored = repo.find(&container.id).await.unwrap().unwrap();
    assert_eq!(stored, container);
}

#[tokio::test]
async fn listado_filtra_por_nombre_estado_y_cursor() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlExecutionHosts {
        pool: db.pool.clone(),
    };
    let wsl = host(ExecutionHostKind::Wsl2);
    let mut engine = host(ExecutionHostKind::Container);
    engine.status = ExternalHostState::Offline;
    repo.record(&wsl, &ctx()).await.unwrap();
    repo.record(&engine, &ctx()).await.unwrap();
    let filter_online = larm_domain::runtime_ports::RuntimeFilter {
        state: Some("ONLINE".into()),
        ..Default::default()
    };
    let online = repo.list(None, 50, &filter_online).await.unwrap();
    assert_eq!(online.len(), 1);
    assert_eq!(online[0].id, wsl.id);
    let filter_name = larm_domain::runtime_ports::RuntimeFilter {
        q: Some("dock".into()),
        ..Default::default()
    };
    let engines = repo.list(None, 50, &filter_name).await.unwrap();
    assert_eq!(engines.len(), 1);
    assert_eq!(engines[0].id, engine.id);
    // Paginación por cursor (created_at, id).
    let page = repo.list(None, 1, &Default::default()).await.unwrap();
    assert_eq!(page.len(), 1);
    let after = repo
        .list(
            Some((page[0].created_at.clone(), page[0].id.clone())),
            50,
            &Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].id, page[0].id);
    // Filtro inválido: estado fuera del enumerado.
    let bad = larm_domain::runtime_ports::RuntimeFilter {
        state: Some("PAUSED".into()),
        ..Default::default()
    };
    assert!(repo.list(None, 50, &bad).await.is_err());
}

#[tokio::test]
async fn record_invalido_es_rechazado_sin_escrituras() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlExecutionHosts {
        pool: db.pool.clone(),
    };
    // wsl_version en un host CONTAINER: la coherencia del DTO lo rechaza.
    let mut wrong = host(ExecutionHostKind::Container);
    wrong.wsl_version = Some(2);
    assert!(repo.record(&wrong, &ctx()).await.is_err());
    // Nombre vacío: rechazado.
    let mut nameless = host(ExecutionHostKind::Wsl2);
    nameless.name = String::new();
    assert!(repo.record(&nameless, &ctx()).await.is_err());
    let empty: i64 = sqlx::query_scalar("SELECT count(*) FROM execution_host")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(empty, 0);
}

#[tokio::test]
async fn el_registro_no_concede_soporte_y_la_referencia_exige_existencia() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlExecutionHosts {
        pool: db.pool.clone(),
    };
    let observed = host(ExecutionHostKind::Wsl2);
    repo.record(&observed, &ctx()).await.unwrap();
    // El registro no crea perfiles habilitados ni cualificación: la única
    // superficie es observación (no hay columna qualification ni promoción).
    let columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('execution_host') ORDER BY cid")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let names: Vec<&str> = columns.iter().map(|(name,)| name.as_str()).collect();
    assert!(!names.iter().any(|name| name.contains("qualification")));
    // La FK de runtime_installation.execution_host_id exige host existente.
    let missing = uid();
    let broken = sqlx::query(
        "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,state,ownership,execution_host_id) \
         VALUES('MANAGED','INSTALL_PLANNED',?,?,?,1,'{}','rv','WSL','INSTALL_PLANNED','LARM_MANAGED',?)",
    )
    .bind(uid())
    .bind(uid())
    .bind(uid())
    .bind(&missing)
    .execute(&db.pool)
    .await;
    assert!(broken.is_err(), "la FK debe rechazar un host inexistente");
}
