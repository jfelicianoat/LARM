//! Trabajador de prueba de P019 (no se distribuye): toma locks reales del
//! Resource Guard sobre una base SQLite real y opera sobre ficheros reales,
//! para probar la concurrencia entre procesos.
//!
//! ```text
//! guard_worker <db> read  <owner> <ruta_a> <ruta_b>
//! guard_worker <db> move  <owner> <ruta_a> <ruta_b> [--crash-after-lock]
//!
//! El fichero vive en `ruta_a` o en `ruta_b` (dos volúmenes); `move` lo pasa
//! de donde esté a la otra.
//! guard_worker <db> reconcile
//! ```
//!
//! Imprime una línea JSON con los instantes (ns desde UNIX) en que tuvo el lock.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use larm_db::guard::SqlResourceGuard;
use larm_db::pool::Db;
use larm_domain::error::ErrorCode;
use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
use larm_domain::ports::ResourceGuard;
use sha2::{Digest, Sha256};

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn sha(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

fn step(owner: &str, phase: &str) -> JournalStep {
    JournalStep {
        id: owner.into(),
        kind: "p019.move".into(),
        phase: phase.into(),
        plan_hash: "0".repeat(64),
        resource_ids: vec!["artifact:modelo".into()],
        payload: serde_json::json!({}),
        from_phases: vec![],
    }
}

/// Reintenta mientras el recurso esté ocupado (rechazo limpio del guard).
async fn acquire(
    g: &SqlResourceGuard,
    owner: &str,
    wanted: &[(Resource, LockMode)],
    journal: Option<&JournalStep>,
) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut attempts = 0;
    loop {
        attempts += 1;
        match g.acquire(owner, 1, wanted, journal).await {
            Ok(()) => return attempts,
            Err(e)
                if matches!(e.code, ErrorCode::Conflict | ErrorCode::ActiveLease)
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            Err(e) => panic!("{owner}: {} {}", e.code, e.details.reason),
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db = Db::open(Path::new(&args[1])).await.unwrap();
    let g = SqlResourceGuard {
        pool: db.pool.clone(),
    };
    let artifact = Resource::new(ResourceKind::Artifact, "modelo");
    match args[2].as_str() {
        "read" => {
            let (owner, a, b) = (&args[3], Path::new(&args[4]), Path::new(&args[5]));
            let wanted = [(artifact.clone(), LockMode::Shared)];
            let attempts = acquire(&g, owner, &wanted, None).await;
            let start = now_ns();
            // Con el lock compartido el fichero está entero en exactamente un sitio.
            let places = [a, b].iter().filter(|p| p.exists()).count();
            let file = if a.exists() { a } else { b };
            let found = places == 1;
            let hash = found.then(|| sha(file));
            tokio::time::sleep(Duration::from_millis(20)).await;
            let end = now_ns();
            g.release(owner, 1, None).await.unwrap();
            println!(
                "{}",
                serde_json::json!({"owner": owner, "kind": "read", "start": start.to_string(), "end": end.to_string(),
                "found": found, "places": places, "hash": hash, "attempts": attempts})
            );
        }
        "move" => {
            let (owner, a, b) = (&args[3], Path::new(&args[4]), Path::new(&args[5]));
            let crash = args.iter().any(|a| a == "--crash-after-lock");
            let wanted = [
                (Resource::new(ResourceKind::Volume, "c"), LockMode::Shared),
                (
                    Resource::new(ResourceKind::Volume, "unc-temp"),
                    LockMode::Shared,
                ),
                (artifact.clone(), LockMode::Exclusive),
            ];
            let attempts = acquire(&g, owner, &wanted, Some(&step(owner, "LOCKED"))).await;
            let start = now_ns();
            if crash {
                // Proceso que muere con el lock tomado y el journal en LOCKED.
                std::process::exit(9);
            }
            let (from, to) = if a.exists() { (a, b) } else { (b, a) };
            let source_hash = sha(from);
            std::fs::copy(from, to).unwrap();
            assert_eq!(sha(to), source_hash, "copia verificada");
            // Fencing antes de borrar el origen: seguimos teniendo el exclusivo.
            assert!(
                g.holds(owner, 1, &artifact, LockMode::Exclusive)
                    .await
                    .unwrap(),
                "fencing"
            );
            std::fs::remove_file(from).unwrap();
            let end = now_ns();
            g.release(owner, 1, Some(&step(owner, "COMPLETED")))
                .await
                .unwrap();
            println!(
                "{}",
                serde_json::json!({"owner": owner, "kind": "move", "start": start.to_string(), "end": end.to_string(),
                "hash": source_hash, "attempts": attempts})
            );
        }
        "reconcile" => {
            let r = g.reconcile().await.unwrap();
            println!(
                "{}",
                serde_json::json!({"released": r.released, "kept": r.kept, "unverified": r.unverified})
            );
        }
        other => panic!("orden desconocida {other}"),
    }
}
