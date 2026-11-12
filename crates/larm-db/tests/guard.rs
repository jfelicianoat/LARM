//! Resource Guard y reservas (P019) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use larm_db::guard::{SqlPoolReservations, SqlResourceGuard};
use larm_db::reservations::SqlVolumeReservations;
use larm_domain::error::ErrorCode;
use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
use larm_domain::ports::{
    PoolReservationRequest, PoolReservations, ResourceGuard, VolumeDemand,
    VolumeReservationRepository,
};
use sqlx::SqlitePool;

fn art(id: &str) -> Resource {
    Resource::new(ResourceKind::Artifact, id)
}
fn vol(id: &str) -> Resource {
    Resource::new(ResourceKind::Volume, id)
}

async fn raw(pool: &SqlitePool, sql: &str, binds: &[&str]) {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
    for b in binds {
        q = q.bind(*b);
    }
    q.execute(&mut *conn).await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
}

async fn rows(pool: &SqlitePool) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT resource_id, owner_id, mode FROM resource_lock ORDER BY resource_id, owner_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn step(id: &str, phase: &str) -> JournalStep {
    JournalStep {
        id: id.into(),
        kind: "artifact.move".into(),
        phase: phase.into(),
        plan_hash: "0".repeat(64),
        resource_ids: vec!["artifact:a".into()],
        payload: serde_json::json!({}),
        from_phases: vec![],
    }
}

#[tokio::test]
async fn compartido_exclusivo_y_lease_activa() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    g.acquire("lease-1", 1, &[(art("a"), LockMode::Shared)], None)
        .await
        .unwrap();
    g.acquire("job-r", 1, &[(art("a"), LockMode::Shared)], None)
        .await
        .unwrap();
    let err = g
        .acquire("job-move", 1, &[(art("a"), LockMode::Exclusive)], None)
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::Conflict, "resource_locked")
    );
    // Si el titular que bloquea es una lease viva: ACTIVE_LEASE (409).
    g.release("job-r", 1, None).await.unwrap();
    raw(&db.pool, "INSERT INTO runtime_lease (id, created_at, updated_at, row_version, metadata, owner_id, request_id, instance_id, \
        generation, state, expires_at, endpoint_id, constraints) VALUES (?, 't', 't', 1, '{}', 'broker', 'r', NULL, 1, 'ACTIVE', NULL, NULL, '{}')",
        &["lease-1"]).await;
    let err = g
        .acquire("job-move", 1, &[(art("a"), LockMode::Exclusive)], None)
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::ActiveLease, "active_lease")
    );
    // Con un exclusivo, nadie más entra, tampoco en compartido.
    g.release("lease-1", 1, None).await.unwrap();
    g.acquire("job-move", 1, &[(art("a"), LockMode::Exclusive)], None)
        .await
        .unwrap();
    assert!(g
        .acquire("job-r", 2, &[(art("a"), LockMode::Shared)], None)
        .await
        .is_err());
}

#[tokio::test]
async fn todo_o_nada_y_journal_en_la_misma_transaccion() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    g.acquire("otro", 1, &[(art("b"), LockMode::Exclusive)], None)
        .await
        .unwrap();
    let err = g
        .acquire(
            "job",
            1,
            &[
                (vol("v"), LockMode::Shared),
                (art("a"), LockMode::Exclusive),
                (art("b"), LockMode::Exclusive),
            ],
            Some(&step("job", "LOCKED")),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(
        rows(&db.pool).await,
        [(
            "artifact:b".to_string(),
            "otro".to_string(),
            "EXCLUSIVE".to_string()
        )],
        "nada a medias"
    );
    let journal: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operation_journal")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        journal, 0,
        "el journal no se escribe si los locks no se toman"
    );

    g.acquire(
        "job",
        1,
        &[
            (vol("v"), LockMode::Shared),
            (art("a"), LockMode::Exclusive),
        ],
        Some(&step("job", "LOCKED")),
    )
    .await
    .unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = 'job'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(phase, "LOCKED");
    g.release("job", 1, Some(&step("job", "COMPLETED")))
        .await
        .unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = 'job'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(phase, "COMPLETED");
}

#[tokio::test]
async fn fencing_por_generacion() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    g.acquire("job", 1, &[(art("a"), LockMode::Exclusive)], None)
        .await
        .unwrap();
    assert!(g
        .holds("job", 1, &art("a"), LockMode::Exclusive)
        .await
        .unwrap());
    assert!(
        g.holds("job", 1, &art("a"), LockMode::Shared)
            .await
            .unwrap(),
        "exclusivo cubre compartido"
    );
    assert!(
        !g.holds("job", 2, &art("a"), LockMode::Exclusive)
            .await
            .unwrap(),
        "otra generación no escribe"
    );
    assert_eq!(
        g.release("job", 2, None).await.unwrap(),
        0,
        "liberar otra generación no toca nada"
    );
    assert_eq!(g.release("job", 1, None).await.unwrap(), 1);
    assert!(
        !g.holds("job", 1, &art("a"), LockMode::Exclusive)
            .await
            .unwrap(),
        "tras liberar, la generación vieja no escribe"
    );
    g.acquire("job", 1, &[(art("a"), LockMode::Shared)], None)
        .await
        .unwrap();
    assert!(
        !g.holds("job", 1, &art("a"), LockMode::Exclusive)
            .await
            .unwrap(),
        "compartido no da permiso de escritura"
    );
}

#[tokio::test]
async fn reconciliacion_solo_con_journal_o_lease_terminados() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    for owner in ["job-hecho", "job-vivo", "huerfano", "lease-fin"] {
        g.acquire(owner, 1, &[(art(owner), LockMode::Exclusive)], None)
            .await
            .unwrap();
    }
    raw(&db.pool, "INSERT INTO operation_journal (id, kind, resource_ids, phase, plan_hash, payload, updated_at) VALUES (?, 'x', '[]', 'COMPLETED', '', '{}', 't')", &["job-hecho"]).await;
    raw(&db.pool, "INSERT INTO operation_journal (id, kind, resource_ids, phase, plan_hash, payload, updated_at) VALUES (?, 'x', '[]', 'COPYING', '', '{}', 't')", &["job-vivo"]).await;
    raw(&db.pool, "INSERT INTO runtime_lease (id, created_at, updated_at, row_version, metadata, owner_id, request_id, instance_id, \
        generation, state, expires_at, endpoint_id, constraints) VALUES (?, 't', 't', 1, '{}', 'b', 'r', NULL, 1, 'RELEASED', NULL, NULL, '{}')",
        &["lease-fin"]).await;
    let r = g.reconcile().await.unwrap();
    assert_eq!(r.released, ["job-hecho", "lease-fin"]);
    assert_eq!(
        r.kept,
        ["job-vivo"],
        "un trabajo en COPYING conserva su lock (proceso caído o no)"
    );
    assert_eq!(
        r.unverified,
        ["huerfano"],
        "sin journal ni lease no se libera por tiempo"
    );
    // El trabajo caído en mitad de la copia: tras la recuperación del journal (FAILED) sí se libera.
    sqlx::query("UPDATE operation_journal SET phase = 'FAILED' WHERE id = 'job-vivo'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(g.reconcile().await.unwrap().released, ["job-vivo"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn carrera_de_diez_admisiones_contra_move_y_delete() {
    for round in 0..20 {
        let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
        let g = SqlResourceGuard {
            pool: db.pool.clone(),
        };
        let mut tasks = Vec::new();
        for i in 0..10 {
            let g = g.clone();
            tasks.push(tokio::spawn(async move {
                // Admisión: compartido en el volumen y el artefacto (orden pedido al revés a propósito).
                let r = g
                    .acquire(
                        &format!("lease-{i}"),
                        1,
                        &[(art("a"), LockMode::Shared), (vol("v"), LockMode::Shared)],
                        None,
                    )
                    .await;
                ("admision", r.map_err(|e| e.code))
            }));
        }
        for (owner, mode) in [
            ("move", LockMode::Exclusive),
            ("delete", LockMode::Exclusive),
        ] {
            let g = g.clone();
            tasks.push(tokio::spawn(async move {
                let r = g
                    .acquire(
                        owner,
                        1,
                        &[(vol("v"), LockMode::Shared), (art("a"), mode)],
                        None,
                    )
                    .await;
                (owner, r.map_err(|e| e.code))
            }));
        }
        let mut results = Vec::new();
        for t in tasks {
            results.push(
                tokio::time::timeout(Duration::from_secs(20), t)
                    .await
                    .expect("sin interbloqueo")
                    .unwrap(),
            );
        }
        // Quien pierde recibe un rechazo del guard (CONFLICT), no un error de base de datos.
        for (kind, r) in &results {
            if let Err(code) = r {
                assert_eq!(
                    *code,
                    ErrorCode::Conflict,
                    "ronda {round}: {kind} perdió con {code:?}"
                );
            }
        }
        let writers = results
            .iter()
            .filter(|(k, r)| *k != "admision" && r.is_ok())
            .count();
        let readers = results
            .iter()
            .filter(|(k, r)| *k == "admision" && r.is_ok())
            .count();
        assert!(writers <= 1, "ronda {round}: dos escritores a la vez");
        assert!(
            writers == 0 || readers == 0,
            "ronda {round}: escritor y lectores a la vez"
        );
        let held = rows(&db.pool).await;
        let exclusive = held
            .iter()
            .filter(|(r, _, m)| r == "artifact:a" && m == "EXCLUSIVE")
            .count();
        let shared = held
            .iter()
            .filter(|(r, _, m)| r == "artifact:a" && m == "SHARED")
            .count();
        assert!(
            exclusive == 0 || shared == 0,
            "ronda {round}: la tabla mezcla exclusivo y compartido"
        );
        assert_eq!(shared, readers);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orden_global_sin_interbloqueo() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    for round in 0..30 {
        let (a, b) = (g.clone(), g.clone());
        let ab = tokio::spawn(async move {
            a.acquire(
                "ab",
                round,
                &[
                    (art("x"), LockMode::Exclusive),
                    (art("y"), LockMode::Exclusive),
                ],
                None,
            )
            .await
        });
        let ba = tokio::spawn(async move {
            b.acquire(
                "ba",
                round,
                &[
                    (art("y"), LockMode::Exclusive),
                    (art("x"), LockMode::Exclusive),
                ],
                None,
            )
            .await
        });
        let r1 = tokio::time::timeout(Duration::from_secs(20), ab)
            .await
            .expect("sin interbloqueo")
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_secs(20), ba)
            .await
            .expect("sin interbloqueo")
            .unwrap();
        assert!(
            r1.is_ok() != r2.is_ok(),
            "ronda {round}: exactamente uno gana"
        );
        let loser = r1.err().or(r2.err()).unwrap();
        assert_eq!(
            loser.code,
            ErrorCode::Conflict,
            "ronda {round}: rechazo limpio, no error de base"
        );
        g.release("ab", round, None).await.unwrap();
        g.release("ba", round, None).await.unwrap();
    }
}

#[tokio::test]
async fn cas_compartido_referenciado_no_borrable() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    assert!(g.delete_blockers("blob").await.unwrap().is_empty());
    raw(
        &db.pool,
        "INSERT INTO artifact_reference (artifact_id, variant_id) VALUES (?, 'variante-1')",
        &["blob"],
    )
    .await;
    raw(
        &db.pool,
        "INSERT INTO artifact_reference (artifact_id, variant_id) VALUES (?, 'variante-2')",
        &["blob"],
    )
    .await;
    g.acquire("lease-9", 1, &[(art("blob"), LockMode::Shared)], None)
        .await
        .unwrap();
    assert_eq!(
        g.delete_blockers("blob").await.unwrap(),
        [
            "referenced_by_variant:variante-1",
            "referenced_by_variant:variante-2",
            "locked_by:lease-9:SHARED"
        ]
    );
}

#[tokio::test]
async fn dos_raices_del_mismo_volumen_reservan_una_vez_y_pools() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let r = SqlVolumeReservations {
        pool: db.pool.clone(),
    };
    let d = |required| VolumeDemand {
        volume_key: "v".into(),
        required_bytes: required,
        os_free: 100,
        policy_minimum: 0,
    };
    r.admit("job", &[d(30), d(20)], chrono::Utc::now(), None)
        .await
        .unwrap();
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT volume_key, total_bytes FROM volume_reservation")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [("v".to_string(), 50)],
        "una sola reserva por volumen con la suma"
    );

    let p = SqlPoolReservations {
        pool: db.pool.clone(),
    };
    p.reserve("lease-a", "gpu-0", 40, 100).await.unwrap();
    p.reserve("lease-b", "gpu-0", 50, 100).await.unwrap();
    let err = p.reserve("lease-c", "gpu-0", 20, 100).await.unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::ResourceUnavailable, "pool_budget_exceeded")
    );
    p.reserve("lease-a", "gpu-0", 10, 100).await.unwrap();
    assert_eq!(
        p.reserved("gpu-0").await.unwrap(),
        60,
        "repetir sustituye, no suma"
    );
    assert!(
        p.reserve("lease-d", "gpu-0", u64::MAX, u64::MAX)
            .await
            .is_err(),
        "desbordamiento comprobado"
    );
    assert_eq!(p.release("lease-a").await.unwrap(), 1);
    assert_eq!(p.reserved("gpu-0").await.unwrap(), 50);
}

#[tokio::test]
async fn reserva_de_pools_es_atomica_y_reemplaza_todas_las_reservas_del_titular() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let reservations = SqlPoolReservations {
        pool: db.pool.clone(),
    };
    reservations
        .reserve("owner", "gpu-0", 40, 100)
        .await
        .unwrap();

    let error = reservations
        .reserve_many(
            "owner",
            &[
                PoolReservationRequest {
                    pool_key: "gpu-0".into(),
                    bytes: 90,
                    budget: 100,
                },
                PoolReservationRequest {
                    pool_key: "system-ram".into(),
                    bytes: 20,
                    budget: 10,
                },
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.details.reason.as_str()),
        (ErrorCode::ResourceUnavailable, "pool_budget_exceeded")
    );
    assert_eq!(reservations.reserved("gpu-0").await.unwrap(), 40);
    assert_eq!(reservations.reserved("system-ram").await.unwrap(), 0);

    reservations
        .reserve_many(
            "owner",
            &[
                PoolReservationRequest {
                    pool_key: "system-ram".into(),
                    bytes: 10,
                    budget: 100,
                },
                PoolReservationRequest {
                    pool_key: "gpu-0".into(),
                    bytes: 30,
                    budget: 100,
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(reservations.reserved("gpu-0").await.unwrap(), 30);
    assert_eq!(reservations.reserved("system-ram").await.unwrap(), 10);
}

#[tokio::test]
async fn reserva_de_pools_rechaza_claves_duplicadas_sin_tocar_estado() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let reservations = SqlPoolReservations {
        pool: db.pool.clone(),
    };
    reservations
        .reserve("owner", "gpu-0", 40, 100)
        .await
        .unwrap();
    let error = reservations
        .reserve_many(
            "owner",
            &[
                PoolReservationRequest {
                    pool_key: "gpu-0".into(),
                    bytes: 10,
                    budget: 100,
                },
                PoolReservationRequest {
                    pool_key: "gpu-0".into(),
                    bytes: 20,
                    budget: 100,
                },
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(error.details.reason, "pool_reservation_invalid");
    assert_eq!(reservations.reserved("gpu-0").await.unwrap(), 40);
}

#[tokio::test]
async fn reserva_de_un_pool_conserva_otros_pools_del_titular() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let reservations = SqlPoolReservations {
        pool: db.pool.clone(),
    };
    reservations
        .reserve("owner", "gpu-0", 40, 100)
        .await
        .unwrap();
    reservations
        .reserve("owner", "system-ram", 30, 100)
        .await
        .unwrap();
    reservations
        .reserve("owner", "gpu-0", 20, 100)
        .await
        .unwrap();
    assert_eq!(reservations.reserved("gpu-0").await.unwrap(), 20);
    assert_eq!(reservations.reserved("system-ram").await.unwrap(), 30);
}

#[tokio::test]
async fn lease_de_la_variante_bajo_el_mismo_lock() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let guard = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    let n = "2026-10-11T00:00:00.000Z";
    raw(&db.pool, "INSERT INTO runtime_instance (id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,\
        backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) \
        VALUES ('inst',?,?,1,'{}','ri','var-1','be','lp','READY',NULL,NULL,NULL,1,NULL,NULL,'LARM_MANAGED')", &[n, n]).await;
    raw(&db.pool, "INSERT INTO runtime_lease (id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,generation,\
        state,expires_at,endpoint_id,constraints) VALUES ('lease-1',?,?,1,'{}','broker','r','inst',1,'ACTIVE',NULL,NULL,'{}')", &[n, n]).await;
    let variant = Resource::new(ResourceKind::Variant, "var-1");
    // Exclusivo sobre la variante: ACTIVE_LEASE aunque la lease no tenga locks.
    let err = guard
        .acquire(
            "move-1",
            1,
            &[(variant.clone(), LockMode::Exclusive)],
            Some(&step("move-1", "LOCKED")),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::ActiveLease, "active_lease")
    );
    assert!(err.details.resource_ids.contains(&"lease-1".to_string()));
    assert!(rows(&db.pool).await.is_empty(), "nada a medias");
    // Compartido (uso) sí convive con la lease; otra variante no se ve afectada.
    guard
        .acquire("reader", 1, &[(variant.clone(), LockMode::Shared)], None)
        .await
        .unwrap();
    guard
        .acquire(
            "move-2",
            1,
            &[(
                Resource::new(ResourceKind::Variant, "var-2"),
                LockMode::Exclusive,
            )],
            None,
        )
        .await
        .unwrap();
    guard.release("reader", 1, None).await.unwrap();
    // Lease terminada: el movimiento puede bloquear.
    raw(&db.pool, "UPDATE runtime_lease SET state = 'EXPIRED'", &[]).await;
    guard
        .acquire("move-1", 1, &[(variant, LockMode::Exclusive)], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn journal_solo_desde_las_fases_esperadas() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let guard = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    let mut planned = step("job-1", "PLANNED");
    guard.acquire("x", 1, &[], Some(&planned)).await.unwrap();
    guard.release("x", 1, None).await.unwrap();
    // Primer uso del plan: PLANNED → LOCKED con los locks.
    let mut locked = step("job-1", "LOCKED");
    locked.from_phases = vec!["PLANNED".into()];
    guard
        .acquire(
            "job-1",
            1,
            &[(art("a"), LockMode::Exclusive)],
            Some(&locked),
        )
        .await
        .unwrap();
    guard.release("job-1", 1, None).await.unwrap();
    // Segundo uso: el journal ya no está en PLANNED; ni locks ni cambio.
    let err = guard
        .acquire(
            "job-1",
            1,
            &[(art("a"), LockMode::Exclusive)],
            Some(&locked),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::Conflict, "journal_phase_conflict")
    );
    assert!(rows(&db.pool).await.is_empty());
    // Sin fila previa tampoco se crea.
    planned.id = "job-2".into();
    planned.from_phases = vec!["PLANNED".into()];
    assert!(guard.acquire("y", 1, &[], Some(&planned)).await.is_err());
    let phase: Option<String> =
        sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = 'job-2'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(phase, None);
}
