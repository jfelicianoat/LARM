//! Importación de carpetas (P017) sobre SQLite real y ficheros reales:
//! carpeta mixta, desconocidos informados, adopción sin tocar nada,
//! reimportación con caché, duplicados por hash, copia al CAS y vista.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use larm_db::artifacts::SqlArtifactLocations;
use larm_db::locations::SqlStorageLocations;
use larm_db::models::SqlModels;
use larm_db::variants::SqlVariants;
use larm_domain::artifacts::Ownership;
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{
    ArtifactLocationRepository, MutationContext, StorageLocationRepository, VariantRepository,
};
use larm_domain::storage::PerformanceClass;
use larm_domain::units::DecimalU64;
use larm_models::import::{import_dir, ImportMode, ImportOptions};
use sqlx::SqlitePool;

const SOURCE: &str = "0192f000-0000-7000-8000-00000000f001";
const ADOPT_LOC: &str = "0192f000-0000-7000-8000-00000000c001";
const CAS_LOC: &str = "0192f000-0000-7000-8000-00000000c002";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "importador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn gguf(arch: &str, name: &str, payload: usize) -> Vec<u8> {
    let mut b = b"GGUF".to_vec();
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(2u64.to_le_bytes());
    for (k, v) in [("general.architecture", arch), ("general.name", name)] {
        b.extend((k.len() as u64).to_le_bytes());
        b.extend(k.as_bytes());
        b.extend(8u32.to_le_bytes());
        b.extend((v.len() as u64).to_le_bytes());
        b.extend(v.as_bytes());
    }
    b.extend((0..payload).map(|i| (i % 253) as u8));
    b
}

fn safetensors(payload: usize) -> Vec<u8> {
    let mut b = 12u64.to_le_bytes().to_vec();
    b.extend(b"{\"a\":{}}    ");
    b.extend(vec![1u8; payload]);
    b
}

/// Carpeta mixta: GGUF con shards, otro GGUF, mmproj, README, una variante
/// SafeTensors y ficheros que no son modelos.
fn mixed(dir: &Path) {
    let w = |rel: &str, bytes: &[u8]| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    w(
        "qwen/qwen-x-Q4_K_M-00001-of-00002.gguf",
        &gguf("qwen2", "Qwen X", 4000),
    );
    w(
        "qwen/qwen-x-Q4_K_M-00002-of-00002.gguf",
        &gguf("qwen2", "Qwen X", 3000),
    );
    w("qwen/qwen-x-Q8_0.gguf", &gguf("qwen2", "Qwen X", 5000));
    w("qwen/mmproj-qwen-x-f16.gguf", &gguf("clip", "proj", 500));
    w("qwen/README.md", b"# Qwen X");
    w("st/model.safetensors", &safetensors(2000));
    w("st/config.json", br#"{"model_type":"llama"}"#);
    w("st/tokenizer.json", b"{}");
    w("st/notas.txt", b"apuntes");
    w("setup.exe", b"MZ\x90\x00");
    w("roto.gguf", b"NOPE");
}

fn location(id: &str, root: &Path, volume: &str) -> StorageLocation {
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
        volume_key: volume.into(),
        performance_class: PerformanceClass::Ssd,
    }
}

struct Env {
    _db: tempfile::TempDir,
    files: tempfile::TempDir,
    pool: SqlitePool,
    models: SqlModels,
    variants: SqlVariants,
    locations: SqlArtifactLocations,
}

async fn env() -> Env {
    let (d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let files = tempfile::Builder::new()
        .prefix("larm-import-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    std::fs::create_dir_all(files.path().join("modelos")).unwrap();
    std::fs::create_dir_all(files.path().join("cas")).unwrap();
    let repo = SqlStorageLocations {
        pool: db.pool.clone(),
    };
    repo.insert(
        &location(ADOPT_LOC, &files.path().join("modelos"), "vol-a"),
        &ctx(),
    )
    .await
    .unwrap();
    repo.insert(
        &location(CAS_LOC, &files.path().join("cas"), "vol-a"),
        &ctx(),
    )
    .await
    .unwrap();
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,\
         auth_mode,account_id,policy) VALUES (?, 't', 't', 1, '{}', 'carpeta local', 'local', 'file:', 0, 1, 'NONE', NULL, '{}')",
    )
    .bind(SOURCE)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    Env {
        _db: d,
        files,
        models: SqlModels {
            pool: db.pool.clone(),
        },
        variants: SqlVariants {
            pool: db.pool.clone(),
        },
        locations: SqlArtifactLocations {
            pool: db.pool.clone(),
        },
        pool: db.pool,
    }
}

impl Env {
    fn options(&self, dir: &Path, mode: ImportMode, location: &str) -> ImportOptions {
        ImportOptions {
            dir: dir.to_path_buf(),
            storage_location_id: location.into(),
            location_root: self.files.path().join(if location == CAS_LOC {
                "cas"
            } else {
                "modelos"
            }),
            source_id: SOURCE.into(),
            mode,
            format: None,
            license_id: None,
            adopt_as: Ownership::External,
        }
    }

    async fn rows(&self) -> Vec<(String, String, String, Option<String>)> {
        sqlx::query_as(
            "SELECT relative_path, ownership, json_extract(metadata, '$.registration_mode'), verified_at \
             FROM artifact_location ORDER BY relative_path, id",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let (found, _) = larm_storage::import::walk(dir).unwrap();
    found
        .into_iter()
        .map(|f| {
            let meta = std::fs::metadata(&f.path).unwrap();
            (
                f.relative,
                std::fs::read(&f.path).unwrap(),
                meta.modified().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn carpeta_mixta_adoptada_sin_tocar_nada() {
    let e = env().await;
    let dir = e.files.path().join("modelos").join("descargas");
    mixed(&dir);
    let before = snapshot(&dir);
    let r = import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&dir, ImportMode::Adopt, ADOPT_LOC),
        &ctx(),
    )
    .await
    .unwrap();
    // Tres variantes: Q4 (2 shards + mmproj + README), Q8 (+ mmproj + README), SafeTensors.
    assert_eq!(r.variants.len(), 3);
    let formats: Vec<_> = r
        .variants
        .iter()
        .map(|v| (v.format.as_str(), v.files))
        .collect();
    assert!(
        formats.contains(&("GGUF", 4))
            && formats.contains(&("GGUF", 3))
            && formats.contains(&("SafeTensors", 3))
    );
    // Desconocidos ignorados e informados.
    let ignored: Vec<_> = r
        .ignored
        .iter()
        .map(|i| (i.relative.as_str(), i.reason.as_str()))
        .collect();
    assert_eq!(
        ignored,
        vec![
            ("roto.gguf", "gguf_magic_missing"),
            ("setup.exe", "unknown_format"),
            ("st/notas.txt", "unknown_format")
        ]
    );
    // Q4 y Q8 del mismo nombre y carpeta: un modelo, dos variantes.
    assert_eq!(r.models.len(), 2);
    let rows = e.rows().await;
    assert_eq!(
        rows.len(),
        10,
        "4 + 3 + 3 ficheros registrados (mmproj y README en dos variantes)"
    );
    assert!(rows
        .iter()
        .all(|(path, own, mode, verified)| path.starts_with("descargas/")
            && own == "EXTERNAL"
            && mode == "ADOPTED"
            && verified.is_some()));
    // Nada se movió, copió ni modificó.
    assert_eq!(snapshot(&dir), before);
    assert!(std::fs::read_dir(e.files.path().join("cas"))
        .unwrap()
        .next()
        .is_none());
    // La arquitectura de la cabecera GGUF llega a la variante.
    let v = e
        .variants
        .get_variant(&r.variants[0].variant_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (v.architecture.as_deref(), v.ownership),
        (Some("qwen2"), Ownership::External)
    );
    assert_eq!(v.metadata["registration_mode"], "ADOPTED");
}

#[tokio::test]
async fn filtro_de_formato_y_licencia_se_aplican_a_la_distribucion_local() {
    let e = env().await;
    let dir = e.files.path().join("modelos").join("descargas");
    mixed(&dir);
    let mut options = e.options(&dir, ImportMode::Adopt, ADOPT_LOC);
    options.format = Some("gguf".into());
    options.license_id = Some("license-local".into());

    let report = import_dir(&e.models, &e.variants, &e.locations, &options, &ctx())
        .await
        .unwrap();

    assert_eq!(report.variants.len(), 2, "solo entran variantes GGUF");
    assert!(report
        .variants
        .iter()
        .all(|variant| variant.format == "GGUF"));
    let license: Option<String> = sqlx::query_scalar(
        "SELECT license_id FROM model_distribution WHERE source_id=? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(SOURCE)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(license.as_deref(), Some("license-local"));
}

#[tokio::test]
async fn reimportar_usa_la_cache_y_no_duplica() {
    let e = env().await;
    let dir = e.files.path().join("modelos").join("descargas");
    mixed(&dir);
    let opts = e.options(&dir, ImportMode::Adopt, ADOPT_LOC);
    let first = import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
        .await
        .unwrap();
    assert_eq!((first.hashed, first.hashed_from_cache), (8, 0));
    let again = import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
        .await
        .unwrap();
    assert_eq!(
        (
            again.hashed,
            again.hashed_from_cache,
            again.locations_created
        ),
        (0, 8, 0)
    );
    assert!(again
        .models
        .iter()
        .all(|m| !m.model_created && m.variants_created == 0));
    // Un fichero cambiado se relee y es otra variante (otros bytes).
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(dir.join("st/tokenizer.json"), b"{\"v\":2}").unwrap();
    let changed = import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
        .await
        .unwrap();
    assert_eq!((changed.hashed, changed.hashed_from_cache), (1, 7));
    // Las filas nuevas con hash de la caché no cuentan como verificadas.
    let unverified: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact_location WHERE verified_at IS NULL")
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        unverified, 2,
        "model.safetensors y config.json de la variante nueva"
    );
    // La revisión nueva sustituye a la anterior de esa carpeta.
    let states: Vec<(String,)> = sqlx::query_as(
        "SELECT state FROM model_revision r JOIN model_distribution d ON d.id = r.distribution_id \
         WHERE d.upstream_model_id = 'st#st' ORDER BY r.created_at",
    )
    .fetch_all(&e.pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        vec![("SUPERSEDED".to_string(),), ("CURRENT".to_string(),)]
    );
}

#[tokio::test]
async fn duplicados_por_hash_sin_borrar() {
    let e = env().await;
    let dir = e.files.path().join("modelos").join("descargas");
    mixed(&dir);
    std::fs::create_dir_all(dir.join("copia")).unwrap();
    std::fs::copy(
        dir.join("qwen/qwen-x-Q8_0.gguf"),
        dir.join("copia/qwen-x-Q8_0.gguf"),
    )
    .unwrap();
    import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&dir, ImportMode::Adopt, ADOPT_LOC),
        &ctx(),
    )
    .await
    .unwrap();
    let dups = e.locations.duplicates().await.unwrap();
    assert_eq!(
        dups.len(),
        1,
        "solo el Q8 copiado: el mmproj compartido es la misma ruta"
    );
    let size = std::fs::metadata(dir.join("copia/qwen-x-Q8_0.gguf"))
        .unwrap()
        .len();
    assert_eq!(dups[0].copies.len(), 2);
    assert_eq!(
        (dups[0].duplicate_bytes, dups[0].reclaimable_bytes),
        (size, size)
    );
    assert!(
        dir.join("copia/qwen-x-Q8_0.gguf").exists() && dir.join("qwen/qwen-x-Q8_0.gguf").exists(),
        "no se borra nada"
    );
}

#[tokio::test]
async fn copia_al_cas_compartido_y_vista() {
    let e = env().await;
    let ext = e.files.path().join("externo");
    mixed(&ext);
    let r = import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&ext, ImportMode::Copy, CAS_LOC),
        &ctx(),
    )
    .await
    .unwrap();
    // 8 ficheros distintos al CAS; mmproj y README, que usan dos variantes, una sola vez.
    assert_eq!((r.blobs_copied, r.blobs_already_present), (8, 2));
    let rows = e.rows().await;
    assert!(rows
        .iter()
        .all(|(path, _, mode, _)| path.starts_with("blobs/sha256/") && mode == "MANAGED"));
    let shared = rows.iter().filter(|(_, own, _, _)| own == "SHARED").count();
    assert_eq!(shared, 4, "mmproj y README: dos filas cada uno, SHARED");
    let blob = &rows[0].0;
    let blob_path = blob
        .split('/')
        .fold(e.files.path().join("cas"), |p, c| p.join(c));
    assert!(
        std::fs::metadata(&blob_path)
            .unwrap()
            .permissions()
            .readonly(),
        "blob inmutable"
    );
    // El original externo sigue intacto.
    assert!(ext.join("qwen/qwen-x-Q8_0.gguf").exists());
    // Vista para el runtime con los nombres originales (mismo volumen: enlaces).
    let v = e
        .variants
        .get_variant(&r.variants[0].variant_id)
        .await
        .unwrap()
        .unwrap();
    let files: Vec<(String, String)> = e
        .variants
        .artifacts_of(&v.id)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.filename, a.sha256.unwrap()))
        .collect();
    let view = e.files.path().join("cas").join("views").join(&v.id);
    let entries =
        larm_storage::cas::runtime_view(&e.files.path().join("cas"), &view, &files).unwrap();
    assert!(entries.iter().all(|entry| match entry {
        larm_storage::cas::ViewEntry::Linked { .. } => true,
        larm_storage::cas::ViewEntry::Reference { blob, .. } =>
            std::path::Path::new(blob).is_file(),
    }));
    // Copiar otra vez la misma carpeta no copia ni crea filas.
    let again = import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&ext, ImportMode::Copy, CAS_LOC),
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!((again.blobs_copied, again.locations_created), (0, 0));
}

#[tokio::test]
async fn reconstruye_registro_en_una_base_nueva_desde_manifiestos_y_blobs() {
    let source = env().await;
    let external = source.files.path().join("recuperacion");
    mixed(&external);
    let mut options = source.options(&external, ImportMode::Copy, CAS_LOC);
    options.license_id = None;
    let imported = import_dir(
        &source.models,
        &source.variants,
        &source.locations,
        &options,
        &ctx(),
    )
    .await
    .unwrap();
    let root = source.files.path().join("cas");
    let scan_root = root.clone();
    let scan = tokio::task::spawn_blocking(move || larm_storage::rebuild::scan_root(&scan_root))
        .await
        .unwrap()
        .unwrap();
    assert!(scan.invalid.is_empty());
    assert!(scan
        .manifests
        .iter()
        .all(larm_storage::rebuild::ScannedManifest::complete));

    // Base nueva: sólo los sidecars y los blobs de la raíz contienen el registro.
    let (_staging_dir, staging) = larm_db::testing::temp_migrated().await.unwrap();
    let recovered_location_id = uuid::Uuid::now_v7().to_string();
    let recovered_location = location(&recovered_location_id, &root, "volume-recovered");
    SqlStorageLocations {
        pool: staging.pool.clone(),
    }
    .insert(&recovered_location, &ctx())
    .await
    .unwrap();
    let mut recovered_sources: Vec<String> = scan
        .manifests
        .iter()
        .map(|manifest| manifest.manifest.distribution.source_id.clone())
        .collect();
    recovered_sources.sort();
    recovered_sources.dedup();
    larm_db::models::insert_recovered_sources(&staging.pool, &recovered_sources)
        .await
        .unwrap();

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
        &recovered_location_id,
        &scan,
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(report.recovered.len(), imported.variants.len());
    assert!(report.incomplete.is_empty());
    assert!(report.invalid_manifests.is_empty());
    assert!(report.conflicts.is_empty());

    let variant_count: i64 = sqlx::query_scalar("SELECT count(*) FROM model_variant")
        .fetch_one(&staging.pool)
        .await
        .unwrap();
    let location_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact_location WHERE storage_location_id=?")
            .bind(&recovered_location_id)
            .fetch_one(&staging.pool)
            .await
            .unwrap();
    let unknown_licenses: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM model_distribution WHERE source_id=? AND license_id IS NULL",
    )
    .bind(SOURCE)
    .fetch_one(&staging.pool)
    .await
    .unwrap();
    assert_eq!(variant_count as usize, imported.variants.len());
    assert!(location_count > 0);
    assert_eq!(
        unknown_licenses as usize,
        imported.models.len(),
        "no se inventan licencias ausentes"
    );
}

#[tokio::test]
async fn negativos() {
    let e = env().await;
    let outside = e.files.path().join("fuera");
    mixed(&outside);
    let err = import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&outside, ImportMode::Adopt, ADOPT_LOC),
        &ctx(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.details.reason, "outside_location");
    let mut opts = e.options(
        &e.files.path().join("modelos"),
        ImportMode::Adopt,
        ADOPT_LOC,
    );
    opts.adopt_as = Ownership::LarmManaged;
    assert_eq!(
        import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "adopt_ownership_invalid"
    );
    let missing = e.options(
        &e.files.path().join("modelos").join("no-existe"),
        ImportMode::Adopt,
        ADOPT_LOC,
    );
    assert!(
        import_dir(&e.models, &e.variants, &e.locations, &missing, &ctx())
            .await
            .is_err()
    );
    // Adoptar como HOST_MANAGED.
    let host = e.files.path().join("modelos").join("lemonade");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::write(host.join("m.gguf"), gguf("llama", "M", 100)).unwrap();
    let mut opts = e.options(&host, ImportMode::Adopt, ADOPT_LOC);
    opts.adopt_as = Ownership::HostManaged;
    import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
        .await
        .unwrap();
    assert!(e
        .rows()
        .await
        .iter()
        .all(|(_, own, _, _)| own == "HOST_MANAGED"));
}

#[tokio::test]
async fn entre_volumenes_no_se_asume_ahorro_fisico() {
    let e = env().await;
    // Otra ubicación en otro volumen con una copia del mismo fichero.
    const OTHER: &str = "0192f000-0000-7000-8000-00000000c003";
    let other_root = e.files.path().join("otro-volumen");
    std::fs::create_dir_all(&other_root).unwrap();
    SqlStorageLocations {
        pool: e.pool.clone(),
    }
    .insert(&location(OTHER, &other_root, "vol-b"), &ctx())
    .await
    .unwrap();
    let a = e.files.path().join("modelos").join("a");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("m.gguf"), gguf("llama", "M", 1000)).unwrap();
    std::fs::create_dir_all(other_root.join("b")).unwrap();
    std::fs::copy(a.join("m.gguf"), other_root.join("b").join("m.gguf")).unwrap();
    import_dir(
        &e.models,
        &e.variants,
        &e.locations,
        &e.options(&a, ImportMode::Adopt, ADOPT_LOC),
        &ctx(),
    )
    .await
    .unwrap();
    let mut opts = e.options(&other_root.join("b"), ImportMode::Adopt, OTHER);
    opts.location_root = other_root.clone();
    import_dir(&e.models, &e.variants, &e.locations, &opts, &ctx())
        .await
        .unwrap();
    let dups = e.locations.duplicates().await.unwrap();
    let size = std::fs::metadata(a.join("m.gguf")).unwrap().len();
    assert_eq!(dups.len(), 1);
    assert_eq!(
        (dups[0].duplicate_bytes, dups[0].reclaimable_bytes),
        (size, 0)
    );
}
