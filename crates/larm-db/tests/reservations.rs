//! Reservas por volumen (P015) sobre SQLite real.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::reservations::SqlVolumeReservations;
use larm_domain::error::ErrorCode;
use larm_domain::ports::{VolumeDemand, VolumeReservationRepository};

const V: &str = "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3";

fn demand(required: u64, free: u64, minimum: u64) -> VolumeDemand {
    VolumeDemand {
        volume_key: V.into(),
        required_bytes: required,
        os_free: free,
        policy_minimum: minimum,
    }
}

#[tokio::test]
async fn admision_reserva_y_no_descuenta_dos_veces() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let r = SqlVolumeReservations {
        pool: db.pool.clone(),
    };
    let now = chrono::Utc::now();
    r.admit("job-a", &[demand(60, 100, 10)], now, None)
        .await
        .unwrap();
    assert_eq!(r.others(V, None).await.unwrap(), 60);
    assert_eq!(
        r.others(V, Some("job-a")).await.unwrap(),
        0,
        "la propia reserva no cuenta contra sí misma"
    );

    // Otro trabajo: 100 libres − 10 mínimo − 60 de A = 30 < 40.
    let err = r
        .admit("job-b", &[demand(40, 100, 10)], now, None)
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::StorageInsufficient, "insufficient_space")
    );
    assert_eq!(
        r.others(V, None).await.unwrap(),
        60,
        "un rechazo no reserva nada"
    );

    // A crea 50 bytes: el SO ya los refleja (libre 50) y A solo retiene 10.
    r.consume("job-a", V, 50).await.unwrap();
    assert_eq!(r.others(V, None).await.unwrap(), 10);
    r.admit("job-b", &[demand(25, 50, 10)], chrono::Utc::now(), None)
        .await
        .unwrap();
    assert_eq!(r.others(V, None).await.unwrap(), 35);

    // Readmitir A sustituye su reserva; liberar es idempotente.
    r.admit("job-a", &[demand(5, 50, 10)], chrono::Utc::now(), None)
        .await
        .unwrap();
    assert_eq!(r.others(V, None).await.unwrap(), 30);
    assert_eq!(r.release("job-a").await.unwrap(), 1);
    assert_eq!(r.release("job-a").await.unwrap(), 0);
    assert_eq!(
        r.consume("job-a", V, 1).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn instantanea_vieja_reservas_caducadas_y_varios_volumenes() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let r = SqlVolumeReservations {
        pool: db.pool.clone(),
    };
    let old = chrono::Utc::now() - chrono::Duration::seconds(6);
    let err = r
        .admit("j", &[demand(1, 100, 0)], old, None)
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::ResourceUnavailable, "free_snapshot_stale")
    );
    assert!(err.retryable);

    // Una reserva caducada ya no ocupa espacio.
    r.admit(
        "caducada",
        &[demand(90, 100, 0)],
        chrono::Utc::now(),
        Some("2000-01-01T00:00:00.000Z".into()),
    )
    .await
    .unwrap();
    assert_eq!(r.others(V, None).await.unwrap(), 0);

    // Todo o nada entre volúmenes: si uno no cabe, no se reserva ninguno.
    let other = VolumeDemand {
        volume_key: "otro".into(),
        required_bytes: 500,
        os_free: 100,
        policy_minimum: 0,
    };
    let err = r
        .admit(
            "multi",
            &[demand(10, 100, 0), other],
            chrono::Utc::now(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StorageInsufficient);
    assert_eq!(r.others(V, None).await.unwrap(), 0);
}

#[tokio::test]
async fn admisiones_simultaneas_no_reservan_el_mismo_espacio() {
    let (_d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let r = SqlVolumeReservations {
        pool: db.pool.clone(),
    };
    // 10 trabajos piden 60 de 100 a la vez: solo uno cabe.
    let mut tasks = Vec::new();
    for i in 0..10 {
        let r = r.clone();
        tasks.push(tokio::spawn(async move {
            r.admit(
                &format!("job-{i}"),
                &[demand(60, 100, 0)],
                chrono::Utc::now(),
                None,
            )
            .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(()) => ok += 1,
            Err(e) => assert_eq!(e.code, ErrorCode::StorageInsufficient),
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(r.others(V, None).await.unwrap(), 60);
}
