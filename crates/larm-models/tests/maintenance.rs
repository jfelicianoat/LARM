//! Mantenimiento del almacenamiento (P018) sobre SQLite real y ficheros
//! reales: volumen offline/online, huérfanos, descargas parciales,
//! limpieza con plan, borrado de una variante y reconstrucción del
//! registro desde los manifiestos.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use larm_db::artifacts::{SqlArtifactLocations, SqlJournal, SqlLiveJobs};
use larm_db::guard::SqlResourceGuard;
use larm_db::locations::SqlStorageLocations;
use larm_db::models::SqlModels;
use larm_db::variants::SqlVariants;
use larm_domain::artifacts::{ArtifactState, Ownership};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{
    ArtifactLocationRepository, MutationContext, StorageLocationRepository, StorageProber,
    VariantRepository,
};
use larm_domain::storage::{PerformanceClass, StorageObservation};
use larm_domain::units::DecimalU64;
use larm_models::import::{import_dir, ImportMode, ImportOptions};
use larm_storage::cleanup::{StoragePorts, StorageService, CLEANUP_KIND, DELETE_KIND};
use larm_storage::reconcile::Reconciler;
use sqlx::SqlitePool;

const SOURCE: &str = "0192f000-0000-7000-8000-00000000f001";
const LOC: &str = "0192f000-0000-7000-8000-00000000c001";
const EXT: &str = "0192f000-0000-7000-8000-00000000c002";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "operador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

/// Sin GUID: la raíz de cada ubicación es su ruta canónica.
struct NoVolumes;

#[async_trait]
impl StorageProber for NoVolumes {
    async fn probe(&self, _: &[String]) -> Result<StorageObservation, LarmError> {
        Ok(StorageObservation {
            volumes: vec![],
            unknown: vec![],
            observed_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        })
    }
}

fn gguf(name: &str, payload: usize) -> Vec<u8> {
    let mut b = b"GGUF".to_vec();
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    let k = "general.name";
    b.extend((k.len() as u64).to_le_bytes());
    b.extend(k.as_bytes());
    b.extend(8u32.to_le_bytes());
    b.extend((name.len() as u64).to_le_bytes());
    b.extend(name.as_bytes());
    b.extend((0..payload).map(|i| (i % 251) as u8));
    b
}

fn location(id: &str, root: &Path) -> StorageLocation {
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    StorageLocation {
        id: id.into(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: serde_json::Map::new(),
        name: id.into(),
        root_path: root.display().to_string(),
        canonical_path: root.display().to_string(),
        volume_guid: None,
        drive_letter: None,
        unc_path: None,
        filesystem: "NTFS".into(),
        storage_type: "LOCAL".into(),
        capacity_bytes: DecimalU64::new(1 << 40).unwrap(),
        free_bytes: DecimalU64::new(1 << 39).unwrap(),
        reserved_bytes: DecimalU64::default(),
        priority: 0,
        auto_placement: true,
        state: LocationState::Online,
        last_seen_at: None,
        volume_key: format!("vol-{id}"),
        performance_class: PerformanceClass::Ssd,
    }
}

struct Env {
    _db: tempfile::TempDir,
    files: tempfile::TempDir,
    pool: SqlitePool,
    service: Arc<StorageService>,
    reconciler: Reconciler,
    variants: SqlVariants,
    artifacts: SqlArtifactLocations,
}

async fn env() -> Env {
    let (d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let files = tempfile::Builder::new()
        .prefix("larm-maintenance-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    for dir in ["cas", "externos"] {
        std::fs::create_dir_all(files.path().join(dir)).unwrap();
    }
    let locations = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    locations
        .insert(&location(LOC, &files.path().join("cas")), &ctx())
        .await
        .unwrap();
    locations
        .insert(&location(EXT, &files.path().join("externos")), &ctx())
        .await
        .unwrap();
    larm_db::models::insert_recovered_sources(&db.pool, &[SOURCE.to_string()])
        .await
        .unwrap();
    let ports = StoragePorts {
        locations: Arc::new(locations.clone()),
        artifacts: Arc::new(SqlArtifactLocations {
            pool: db.pool.clone(),
        }),
        variants: Arc::new(SqlVariants {
            pool: db.pool.clone(),
        }),
        prober: Arc::new(NoVolumes),
        live: Arc::new(SqlLiveJobs {
            pool: db.pool.clone(),
        }),
        guard: Arc::new(SqlResourceGuard {
            pool: db.pool.clone(),
        }),
        cleanup_journal: Arc::new(SqlJournal {
            pool: db.pool.clone(),
            kind: CLEANUP_KIND,
        }),
        delete_journal: Arc::new(SqlJournal {
            pool: db.pool.clone(),
            kind: DELETE_KIND,
        }),
    };
    let reconciler = Reconciler {
        locations: ports.locations.clone(),
        artifacts: ports.artifacts.clone(),
        variants: ports.variants.clone(),
        prober: Arc::new(NoVolumes),
    };
    Env {
        _db: d,
        service: Arc::new(StorageService {
            ports,
            plan_ttl: chrono::Duration::seconds(900),
        }),
        reconciler,
        variants: SqlVariants {
            pool: db.pool.clone(),
        },
        artifacts: SqlArtifactLocations {
            pool: db.pool.clone(),
        },
        files,
        pool: db.pool,
    }
}

impl Env {
    /// Copia al CAS una carpeta con dos modelos (uno comparte el README).
    async fn copy_models(&self) -> larm_models::import::DirImportReport {
        let src = self.files.path().join("fuente");
        for (dir, name) in [("a", "Alfa"), ("b", "Beta")] {
            std::fs::create_dir_all(src.join(dir)).unwrap();
            std::fs::write(src.join(dir).join(format!("{dir}.gguf")), gguf(name, 2000)).unwrap();
            std::fs::write(src.join(dir).join("README.md"), b"# comun").unwrap();
        }
        let options = ImportOptions {
            dir: src,
            storage_location_id: LOC.into(),
            location_root: self.files.path().join("cas"),
            source_id: SOURCE.into(),
            mode: ImportMode::Copy,
            format: None,
            license_id: None,
            adopt_as: Ownership::External,
        };
        import_dir(
            &SqlModels {
                pool: self.pool.clone(),
            },
            &self.variants,
            &self.artifacts,
            &options,
            &ctx(),
        )
        .await
        .unwrap()
    }

    async fn set_location_state(&self, id: &str, state: &str) {
        sqlx::query("UPDATE storage_location SET state = ? WHERE id = ?")
            .bind(state)
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn states(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT state FROM artifact_location ORDER BY relative_path, id")
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    fn cas(&self) -> std::path::PathBuf {
        self.files.path().join("cas")
    }
}

#[tokio::test]
async fn volumen_offline_y_vuelta_con_reverificacion() {
    let e = env().await;
    let r = e.copy_models().await;
    let variant = r.variants[0].variant_id.clone();
    sqlx::query("UPDATE model_variant SET availability = 'AVAILABLE', lifecycle = 'AVAILABLE'")
        .execute(&e.pool)
        .await
        .unwrap();
    // Se desconecta el disco: OFFLINE sin borrar nada ni tocar la etapa.
    e.set_location_state(LOC, "OFFLINE").await;
    let rep = e.reconciler.run(&ctx()).await.unwrap();
    assert_eq!((rep.offline, rep.variants_offline), (4, 2));
    assert!(e.states().await.iter().all(|s| s == "OFFLINE"));
    let v = e.variants.get_variant(&variant).await.unwrap().unwrap();
    assert_eq!(
        (v.availability.as_str(), v.lifecycle.as_str()),
        ("OFFLINE", "AVAILABLE")
    );
    assert_eq!(v.status, "OFFLINE");
    assert!(v.metadata.get("availability_before_offline").is_none());
    assert_eq!(
        e.reconciler.run(&ctx()).await.unwrap(),
        Default::default(),
        "idempotente"
    );
    // Vuelve: se releen los bytes y la disponibilidad queda pendiente de
    // recalcular contra los gates técnicos y la huella vigentes.
    e.set_location_state(LOC, "ONLINE").await;
    let rep = e.reconciler.run(&ctx()).await.unwrap();
    assert_eq!((rep.reverified, rep.variants_restored), (4, 2));
    let v = e.variants.get_variant(&variant).await.unwrap().unwrap();
    assert_eq!(v.availability, "UNKNOWN");
    assert!(v.metadata.get("availability_before_offline").is_none());
    assert_eq!(v.status, "AVAILABLE", "status deriva de lifecycle");

    // La actualización disponible conserva la versión instalada y sólo
    // cambia el status de lectura; availability y lifecycle son independientes.
    sqlx::query("UPDATE model_variant SET update_available = 1, status = 'AVAILABLE' WHERE id = ?")
        .bind(&variant)
        .execute(&e.pool)
        .await
        .unwrap();
    let v = e.variants.get_variant(&variant).await.unwrap().unwrap();
    assert_eq!(
        (
            v.lifecycle.as_str(),
            v.availability.as_str(),
            v.status.as_str()
        ),
        ("AVAILABLE", "UNKNOWN", "UPDATE_AVAILABLE")
    );
    e.variants
        .set_availability(
            &variant,
            v.row_version,
            "UNKNOWN",
            serde_json::json!({}),
            &ctx(),
        )
        .await
        .unwrap();
    let stored_status: String = sqlx::query_scalar("SELECT status FROM model_variant WHERE id = ?")
        .bind(&variant)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(stored_status, "UPDATE_AVAILABLE");
    // Un blob alterado mientras estaba fuera: CORRUPT, sin volver a AVAILABLE.
    e.set_location_state(LOC, "OFFLINE").await;
    e.reconciler.run(&ctx()).await.unwrap();
    let rows = e.artifacts.by_location(LOC).await.unwrap();
    let blob = rows
        .iter()
        .find(|r| r.relative_path.starts_with("blobs/") && r.file_size.unwrap().get() > 100)
        .unwrap();
    let corrupt_variant: String =
        sqlx::query_scalar("SELECT variant_id FROM model_artifact WHERE id = ?")
            .bind(&blob.artifact_id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    let path = blob
        .relative_path
        .split('/')
        .fold(e.cas(), |p, c| p.join(c));
    let original_bytes = std::fs::read(&path).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    let mut bytes = original_bytes.clone();
    bytes[50] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    e.set_location_state(LOC, "ONLINE").await;
    let rep = e.reconciler.run(&ctx()).await.unwrap();
    assert_eq!(
        (rep.corrupt, rep.variants_corrupt, rep.variants_restored),
        (1, 1, 1)
    );
    let v = e
        .variants
        .get_variant(&corrupt_variant)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            v.lifecycle.as_str(),
            v.availability.as_str(),
            v.status.as_str()
        ),
        ("CORRUPT", "CORRUPT", "CORRUPT")
    );
    // Reparar el contenido y volver a verificar permite sólo regresar a
    // VERIFIED; los gates de ejecución aún no se han recalculado.
    std::fs::write(&path, original_bytes).unwrap();
    e.set_location_state(LOC, "OFFLINE").await;
    e.reconciler.run(&ctx()).await.unwrap();
    e.set_location_state(LOC, "ONLINE").await;
    e.reconciler.run(&ctx()).await.unwrap();
    let v = e
        .variants
        .get_variant(&corrupt_variant)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            v.lifecycle.as_str(),
            v.availability.as_str(),
            v.status.as_str()
        ),
        ("VERIFIED", "UNKNOWN", "UPDATE_AVAILABLE")
    );
    // Raíz que desaparece con el volumen presente: MISSING.
    e.set_location_state(LOC, "MISSING").await;
    e.reconciler.run(&ctx()).await.unwrap();
    assert!(e.states().await.iter().all(|s| s == "MISSING"));
}

#[tokio::test]
async fn huerfanos_parciales_y_limpieza_aprobada() {
    let e = env().await;
    e.copy_models().await;
    // Un blob sin registro, una descarga parcial de un job muerto y otra de uno vivo.
    let orphan = e.cas().join("blobs/sha256/ff").join("f".repeat(64));
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"huerfano").unwrap();
    let live_job = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO operation_journal (id, kind, resource_ids, phase, plan_hash, payload, updated_at) VALUES (?, 'download', '[]', 'RUNNING', '0', '{}', 't')")
        .bind(&live_job)
        .execute(&e.pool)
        .await
        .unwrap();
    for (job, size) in [("job-muerto", 300usize), (live_job.as_str(), 500)] {
        std::fs::create_dir_all(e.cas().join("staging").join(job)).unwrap();
        std::fs::write(
            e.cas()
                .join("staging")
                .join(job)
                .join("modelo.gguf.partial"),
            vec![0u8; size],
        )
        .unwrap();
    }
    std::fs::write(e.cas().join("fotos.jpg"), vec![1u8; 70]).unwrap();
    let (analysis, items) = e.service.analysis(LOC).await.unwrap();
    let cat = |k: &str| {
        analysis
            .categories
            .iter()
            .find(|c| serde_json::to_value(c.kind).unwrap() == k)
            .unwrap()
            .clone()
    };
    let n = |d: Option<DecimalU64>| d.map(DecimalU64::get);
    assert_eq!(
        (
            n(cat("PARTIALS").used_bytes),
            n(cat("PARTIALS").reclaimable_bytes)
        ),
        (Some(800), Some(300))
    );
    assert_eq!(
        cat("PARTIALS").blocker_codes,
        vec!["job_active".to_string()]
    );
    assert_eq!(n(cat("ORPHANS").reclaimable_bytes), Some(8));
    assert!(cat("ORPHANS")
        .blocker_codes
        .contains(&"not_owned_by_larm".to_string()));
    assert_eq!(
        n(cat("MODELS").reclaimable_bytes),
        Some(0),
        "el README compartido no es una copia de más"
    );
    assert_eq!(items.len(), 2);
    // El plan exige aprobación y es de un solo uso.
    let plan = e.service.cleanup_plan(LOC, &ctx()).await.unwrap();
    assert!(plan.approval_required);
    assert_eq!(plan.steps.len(), 2);
    // Entre el plan y la ejecución, la parcial muerta crece: se salta.
    std::fs::write(
        e.cas().join("staging/job-muerto/modelo.gguf.partial"),
        vec![0u8; 301],
    )
    .unwrap();
    let (receipt, task) = e
        .service
        .cleanup(LOC, &plan.plan_id, &plan.plan_hash, &ctx())
        .await
        .unwrap();
    task.await;
    let phase: String = sqlx::query_scalar("SELECT phase FROM operation_journal WHERE id = ?")
        .bind(&receipt.job_id)
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(phase, "COMPLETED");
    assert!(!orphan.exists(), "el blob huérfano se borró");
    assert!(
        e.cas()
            .join("staging/job-muerto/modelo.gguf.partial")
            .exists(),
        "cambió: no se toca"
    );
    assert!(e
        .cas()
        .join("staging")
        .join(&live_job)
        .join("modelo.gguf.partial")
        .exists());
    assert!(e.cas().join("fotos.jpg").exists(), "lo del usuario nunca");
    let again = e
        .service
        .cleanup(LOC, &plan.plan_id, &plan.plan_hash, &ctx())
        .await
        .err()
        .unwrap();
    assert_eq!(again.details.reason, "plan_unknown");
}

#[tokio::test]
async fn borrar_una_variante() {
    let e = env().await;
    let r = e.copy_models().await;
    let (alfa, beta) = (
        r.variants[0].variant_id.clone(),
        r.variants[1].variant_id.clone(),
    );
    // Un fichero EXTERNAL adoptado en la variante Alfa no se borra nunca.
    let ext_file = e.files.path().join("externos").join("alfa-extra.gguf");
    std::fs::write(&ext_file, gguf("extra", 100)).unwrap();
    let v = e.variants.get_variant(&alfa).await.unwrap().unwrap();
    // Aunque una fila compartida no estuviera marcada SHARED, otra referencia
    // en la misma ruta basta para no borrar el fichero.
    sqlx::query("UPDATE artifact_location SET ownership = 'LARM_MANAGED'")
        .execute(&e.pool)
        .await
        .unwrap();
    let plan = e.service.delete_plan(&alfa, &ctx()).await.unwrap();
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.phase.as_str()).collect();
    assert_eq!(
        actions.iter().filter(|a| **a == "DELETE_FILE").count(),
        1,
        "el gguf propio"
    );
    assert_eq!(
        actions.iter().filter(|a| **a == "KEEP_SHARED").count(),
        1,
        "el README compartido"
    );
    assert!(plan.approval_required);
    // If-Match viejo.
    let err = e
        .service
        .delete(&alfa, v.row_version + 7, &plan.plan_hash, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleRevision);
    // Lease activa: ACTIVE_LEASE y nada cambia.
    let n = larm_domain::bridge::timestamp(chrono::Utc::now());
    let mut conn = e.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO runtime_instance (id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) VALUES ('inst',?1,?1,1,'{}','ri',?2,'be','lp','READY',NULL,NULL,NULL,1,NULL,NULL,'LARM_MANAGED')")
        .bind(&n).bind(&alfa).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO runtime_lease (id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,generation,state,expires_at,endpoint_id,constraints) VALUES ('lease',?1,?1,1,'{}','broker','r','inst',1,'ACTIVE',NULL,NULL,'{}')")
        .bind(&n).execute(&mut *conn).await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    let err = e
        .service
        .delete(&alfa, v.row_version, &plan.plan_hash, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ActiveLease);
    assert!(e.variants.get_variant(&alfa).await.unwrap().is_some());
    // Lease liberada pero la instancia sigue registrada: en uso, nada se borra.
    sqlx::query("UPDATE runtime_lease SET state = 'RELEASED'")
        .execute(&e.pool)
        .await
        .unwrap();
    let plan = e.service.delete_plan(&alfa, &ctx()).await.unwrap();
    let err = e
        .service
        .delete(&alfa, v.row_version, &plan.plan_hash, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.details.reason, "variant_in_use");
    assert_eq!(
        e.artifacts.by_location(LOC).await.unwrap().len(),
        4,
        "ni filas ni ficheros"
    );
    // Sin instancia: se borra el registro y después solo el fichero propio.
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&e.pool)
        .await
        .ok();
    sqlx::query("DELETE FROM runtime_lease")
        .execute(&e.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM runtime_instance")
        .execute(&e.pool)
        .await
        .unwrap();
    let plan = e.service.delete_plan(&alfa, &ctx()).await.unwrap();
    let report = e
        .service
        .delete(&alfa, v.row_version, &plan.plan_hash, &ctx())
        .await
        .unwrap();
    assert_eq!(report["deleted_files"].as_array().unwrap().len(), 1);
    assert!(e.variants.get_variant(&alfa).await.unwrap().is_none());
    assert!(e.variants.get_variant(&beta).await.unwrap().is_some());
    let left = e.artifacts.by_location(LOC).await.unwrap();
    assert_eq!(
        left.len(),
        2,
        "Beta conserva su gguf y el README compartido"
    );
    for l in &left {
        let p = l.relative_path.split('/').fold(e.cas(), |p, c| p.join(c));
        assert!(p.exists(), "{}", l.relative_path);
    }
    assert!(ext_file.exists());
    // Su manifiesto también se retiró; el de Beta sigue.
    assert!(!e
        .cas()
        .join("manifests")
        .join(format!("{alfa}.json"))
        .exists());
    assert!(e
        .cas()
        .join("manifests")
        .join(format!("{beta}.json"))
        .exists());
    // Repetir: la variante ya no existe (los planes de los intentos
    // fallidos siguen PLANNED, pero no queda nada que borrar).
    let err = e
        .service
        .delete(&alfa, v.row_version, &plan.plan_hash, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn reconstruir_el_registro_desde_manifiestos() {
    let e = env().await;
    let r = e.copy_models().await;
    assert_eq!(
        std::fs::read_dir(e.cas().join("manifests"))
            .unwrap()
            .count(),
        2
    );
    // Base nueva vacía: el registro se perdió.
    let (_d2, staging) = larm_db::testing::temp_migrated().await.unwrap();
    larm_db::models::insert_recovered_sources(&staging.pool, &[SOURCE.to_string()])
        .await
        .unwrap();
    SqlStorageLocations {
        pool: staging.pool.clone(),
    }
    .insert(&location(LOC, &e.cas()), &ctx())
    .await
    .unwrap();
    // Un blob citado se pierde y aparece un blob que nadie cita.
    let rows = e.artifacts.by_location(LOC).await.unwrap();
    let beta_artifacts = e
        .variants
        .artifacts_of(&r.variants[1].variant_id)
        .await
        .unwrap();
    let beta_gguf = beta_artifacts
        .iter()
        .find(|a| a.filename.ends_with(".gguf"))
        .unwrap();
    let beta_blob = rows
        .iter()
        .find(|l| l.artifact_id == beta_gguf.id)
        .unwrap()
        .relative_path
        .clone();
    let beta_path = beta_blob.split('/').fold(e.cas(), |p, c| p.join(c));
    let mut perms = std::fs::metadata(&beta_path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&beta_path, perms).unwrap();
    std::fs::remove_file(&beta_path).unwrap();
    let orphan = e.cas().join("blobs/sha256/ee").join("e".repeat(64));
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"sin manifiesto").unwrap();
    let scan = larm_storage::rebuild::scan_root(&e.cas()).unwrap();
    let report = larm_models::rebuild::rebuild(
        &SqlModels {
            pool: staging.pool.clone(),
        },
        &SqlVariants {
            pool: staging.pool.clone(),
        },
        &SqlArtifactLocations {
            pool: staging.pool.clone(),
        },
        LOC,
        &scan,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(report.recovered.len(), 1, "Alfa completo");
    assert_eq!(report.recovered[0].0, r.variants[0].variant_id);
    assert_eq!(report.incomplete.len(), 1, "a Beta le falta su gguf");
    assert_eq!(
        report.orphan_blobs,
        vec![format!("blobs/sha256/ee/{}", "e".repeat(64))]
    );
    let staging_rows = SqlArtifactLocations {
        pool: staging.pool.clone(),
    }
    .by_location(LOC)
    .await
    .unwrap();
    assert_eq!(staging_rows.len(), 2);
    assert!(staging_rows
        .iter()
        .all(|l| l.state == ArtifactState::Online && l.verified_at.is_some()));
    // La base viva no se tocó.
    assert_eq!(e.artifacts.by_location(LOC).await.unwrap().len(), 4);
}
