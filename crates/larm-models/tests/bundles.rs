//! Bundles e identidad canónica (P041) sobre SQLite real y ficheros reales:
//! hashes idénticos, mismo nombre con otro modelo, cuantización derivada,
//! probable sin fusión y verificación del bundle en disco.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use larm_db::models::SqlModels;
use larm_db::variants::SqlVariants;
use larm_domain::model::{ArtifactRole, BundleState, RevisionState};
use larm_domain::ports::{ModelRepository, MutationContext, VariantFilter, VariantRepository};
use larm_domain::units::DecimalU64;
use larm_models::identity::IdentityGrade;
use larm_models::registry::{
    register, ArtifactImport, DistributionImport, EvidenceImport, ModelImport, RevisionImport,
    VariantImport,
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

const HF: &str = "0192f000-0000-7000-8000-00000000f001";
const MS: &str = "0192f000-0000-7000-8000-00000000f002";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "importador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

struct Env {
    _d: tempfile::TempDir,
    pool: SqlitePool,
    models: SqlModels,
    variants: SqlVariants,
}

async fn env() -> Env {
    let (d, db) = larm_db::testing::temp_migrated().await.unwrap();
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    for id in [HF, MS] {
        sqlx::query(
            "INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,\
             auth_mode,account_id,policy) VALUES (?, 't', 't', 1, '{}', 'fuente', 'adapter', 'https://example.invalid', 0, 1, 'NONE', NULL, '{}')",
        )
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    Env {
        _d: d,
        models: SqlModels {
            pool: db.pool.clone(),
        },
        variants: SqlVariants {
            pool: db.pool.clone(),
        },
        pool: db.pool,
    }
}

fn h(c: char) -> String {
    c.to_string().repeat(64)
}

fn artifact(
    name: &str,
    role: ArtifactRole,
    sha: &str,
    size: u64,
    required: bool,
) -> ArtifactImport {
    ArtifactImport {
        filename: name.into(),
        role,
        format: None,
        quantization: None,
        size_bytes: DecimalU64::new(size),
        sha256: Some(sha.into()),
        source_url: None,
        required,
        metadata: serde_json::Map::new(),
    }
}

fn import(
    name: &str,
    source: &str,
    repo: &str,
    evidence: EvidenceImport,
    artifacts: Vec<ArtifactImport>,
) -> ModelImport {
    ModelImport {
        name: name.into(),
        upstream_identity: None,
        capabilities: vec!["text.generate".into()],
        metadata: serde_json::Map::new(),
        evidence: Some(evidence),
        distributions: vec![DistributionImport {
            source_id: source.into(),
            upstream_namespace: "ns".into(),
            upstream_model_id: repo.into(),
            license_id: None,
            metadata: serde_json::Map::new(),
            revisions: vec![RevisionImport {
                revision_key: "r1".into(),
                upstream_commit: None,
                manifest_hash: h('0'),
                state: RevisionState::Current,
                metadata: serde_json::Map::new(),
                variants: vec![VariantImport {
                    format: Some("SafeTensors".into()),
                    quantization: Some("BF16".into()),
                    architecture: evidence_arch(name),
                    capabilities: vec!["text.generate".into()],
                    ownership: None,
                    model_limits: None,
                    metadata: serde_json::Map::new(),
                    artifacts,
                }],
                supersedes_current: false,
            }],
        }],
    }
}

fn evidence_arch(_: &str) -> Option<String> {
    Some("qwen2".into())
}

fn qwen_evidence() -> EvidenceImport {
    EvidenceImport {
        architecture: Some("qwen2".into()),
        base_model: None,
        author: Some("Qwen".into()),
        config_hash: Some(h('c')),
        tokenizer_hash: Some(h('t')),
        parent_hashes: vec![],
    }
}

fn qwen_files() -> Vec<ArtifactImport> {
    vec![
        artifact(
            "model-00001-of-00002.safetensors",
            ArtifactRole::Weights,
            &h('1'),
            100,
            true,
        ),
        artifact(
            "model-00002-of-00002.safetensors",
            ArtifactRole::Weights,
            &h('2'),
            50,
            true,
        ),
        artifact("config.json", ArtifactRole::Config, &h('3'), 2, true),
        artifact("tokenizer.json", ArtifactRole::Tokenizer, &h('4'), 3, true),
        artifact("README.md", ArtifactRole::Other, &h('5'), 1, false),
    ]
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn hashes_identicos_agrupan() {
    let e = env().await;
    let a = register(
        &e.models,
        &e.variants,
        &import("Qwen-X", HF, "Qwen-X", qwen_evidence(), qwen_files()),
        &ctx(),
    )
    .await
    .unwrap();
    // Otro nombre, otra fuente, los mismos bytes requeridos, config y tokenizador.
    let mut readme_distinto = qwen_files();
    readme_distinto[4].sha256 = Some(h('9'));
    let b = register(
        &e.models,
        &e.variants,
        &import(
            "espejo-modelscope",
            MS,
            "mirror",
            qwen_evidence(),
            readme_distinto,
        ),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!b.model_created, "IDENTICAL agrupa");
    assert_eq!(b.model_id, a.model_id);
    assert_eq!(b.resolutions[0].grade, IdentityGrade::Identical);
    assert!(b.resolutions[0]
        .reasons
        .contains(&"required_hashes_equal".to_string()));
    assert_eq!(
        count(&e.pool, "SELECT count(*) FROM canonical_model").await,
        1
    );
    assert_eq!(
        count(&e.pool, "SELECT count(*) FROM model_distribution").await,
        2
    );
    // Dos variantes (una por revisión) con el mismo hash de identidad: solo
    // cuentan los requeridos, el README opcional no.
    let vs = e
        .variants
        .list_variants(None, 50, &VariantFilter::default())
        .await
        .unwrap();
    assert_eq!(vs.len(), 2);
    assert_eq!(vs[0].bundle_hash, vs[1].bundle_hash);
    let b0 = e.variants.bundle_of(&vs[0].id).await.unwrap().unwrap();
    let b1 = e.variants.bundle_of(&vs[1].id).await.unwrap().unwrap();
    assert_ne!(
        b0.manifest_hash, b1.manifest_hash,
        "el manifiesto cubre también los opcionales"
    );
    assert_eq!(b0.required_artifact_ids.len(), 4);
    assert_eq!(b0.optional_artifact_ids.len(), 1);
    // Variantes del modelo canónico.
    let of_model = e
        .variants
        .list_variants(
            None,
            50,
            &VariantFilter {
                model_id: Some(a.model_id.clone()),
                ..VariantFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(of_model.len(), 2);
}

#[tokio::test]
async fn mismo_nombre_modelo_distinto() {
    let e = env().await;
    let a = register(
        &e.models,
        &e.variants,
        &import("Nova", HF, "nova-a", qwen_evidence(), qwen_files()),
        &ctx(),
    )
    .await
    .unwrap();
    let mut other = EvidenceImport {
        architecture: Some("llama".into()),
        author: Some("otro".into()),
        config_hash: Some(h('d')),
        tokenizer_hash: Some(h('e')),
        ..EvidenceImport::default()
    };
    other.base_model = None;
    let mut files = qwen_files();
    for (i, f) in files.iter_mut().enumerate() {
        f.sha256 = Some(format!("{:02x}", i + 10).repeat(32));
    }
    let b = register(
        &e.models,
        &e.variants,
        &import("Nova", MS, "nova-b", other, files),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(b.model_created);
    assert_ne!(a.model_id, b.model_id);
    assert_eq!(b.resolutions[0].grade, IdentityGrade::Unrelated);
    assert!(
        b.possible_duplicates.is_empty(),
        "distintos con evidencia: no se anotan como duplicados"
    );
    let stored = e.models.get_model(&b.model_id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["identity_resolutions"][0]["grade"],
        "UNRELATED"
    );
    assert_eq!(
        stored.metadata["identity_resolutions"][0]["reasons"][0],
        "architecture_differs"
    );
}

#[tokio::test]
async fn cuantizacion_derivada_conserva_linaje() {
    let e = env().await;
    let mut base = import("Qwen-X", HF, "Qwen-X", qwen_evidence(), qwen_files());
    base.upstream_identity = Some("hf:Qwen/Qwen-X".into());
    let base = register(&e.models, &e.variants, &base, &ctx())
        .await
        .unwrap();
    let quant = ModelImport {
        name: "Qwen-X-GGUF".into(),
        upstream_identity: Some("hf:bartowski/Qwen-X-GGUF".into()),
        evidence: Some(EvidenceImport {
            architecture: Some("qwen2".into()),
            base_model: Some("hf:Qwen/Qwen-X".into()),
            parent_hashes: vec![h('1'), h('2')],
            ..EvidenceImport::default()
        }),
        ..import(
            "Qwen-X-GGUF",
            HF,
            "Qwen-X-GGUF",
            EvidenceImport::default(),
            vec![artifact(
                "qwen-x-q4_k_m.gguf",
                ArtifactRole::Weights,
                &h('f'),
                40,
                true,
            )],
        )
    };
    let r = register(&e.models, &e.variants, &quant, &ctx())
        .await
        .unwrap();
    assert!(r.model_created, "una derivada es otro modelo");
    let d = r
        .resolutions
        .iter()
        .find(|x| x.model_id == base.model_id)
        .unwrap();
    assert_eq!(d.grade, IdentityGrade::DerivedModel);
    let stored = e.models.get_model(&r.model_id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["lineage"][0]["model_id"],
        base.model_id.as_str()
    );
    assert!(stored.possible_duplicates.is_empty());
}

#[tokio::test]
async fn probable_no_se_fusiona() {
    let e = env().await;
    let a = register(
        &e.models,
        &e.variants,
        &import("Qwen-X", HF, "Qwen-X", qwen_evidence(), qwen_files()),
        &ctx(),
    )
    .await
    .unwrap();
    // Misma arquitectura y autor, otros pesos: un fine-tune o una revisión distinta.
    let mut files = qwen_files();
    files[0].sha256 = Some(h('7'));
    let mut ev = qwen_evidence();
    ev.config_hash = Some(h('8'));
    let b = register(
        &e.models,
        &e.variants,
        &import("Qwen-X-Instruct", MS, "ft", ev, files),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(b.model_created, "PROBABLE nunca fusiona");
    assert_eq!(b.resolutions[0].grade, IdentityGrade::ProbableSameModel);
    assert_eq!(b.possible_duplicates, vec![a.model_id.clone()]);
    let first = e.models.get_model(&a.model_id).await.unwrap().unwrap();
    assert_eq!(
        first.possible_duplicates,
        vec![b.model_id.clone()],
        "revisión pendiente en los dos"
    );
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> (String, u64) {
    std::fs::write(dir.join(name), bytes).unwrap();
    (hex::encode(Sha256::digest(bytes)), bytes.len() as u64)
}

#[tokio::test]
async fn bundle_verificado_incompleto_y_corrupto() {
    let e = env().await;
    let dir = tempfile::Builder::new()
        .prefix("larm-bundle-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let files: Vec<(&str, ArtifactRole, Vec<u8>, bool)> = vec![
        (
            "model-00001-of-00002.gguf",
            ArtifactRole::Weights,
            vec![1u8; 4096],
            true,
        ),
        (
            "model-00002-of-00002.gguf",
            ArtifactRole::Weights,
            vec![2u8; 2048],
            true,
        ),
        (
            "mmproj-f16.gguf",
            ArtifactRole::Mmproj,
            vec![3u8; 512],
            true,
        ),
        ("README.md", ArtifactRole::Other, b"hola".to_vec(), false),
    ];
    let artifacts: Vec<ArtifactImport> = files
        .iter()
        .map(|(n, role, bytes, req)| {
            let (sha, size) = write(dir.path(), n, bytes);
            artifact(n, *role, &sha, size, *req)
        })
        .collect();
    let mut m = import(
        "Vision",
        HF,
        "vision",
        EvidenceImport::default(),
        artifacts.clone(),
    );
    m.distributions[0].revisions[0].variants[0].format = Some("GGUF".into());
    m.distributions[0].revisions[0].variants[0].capabilities = vec!["vision.understand".into()];
    let r = register(&e.models, &e.variants, &m, &ctx()).await.unwrap();
    let variant = r.variant_ids[0].clone();
    let bundle = e.variants.bundle_of(&variant).await.unwrap().unwrap();
    assert_eq!(
        bundle.state,
        BundleState::Incomplete,
        "sin verificar los bytes no es VERIFIED"
    );
    let v = larm_models::bundle::verify(&e.variants, &variant, dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!(v.state, BundleState::Verified);
    assert_eq!(v.missing_roles, Some(vec![]));
    // Un shard que falta: INCOMPLETE.
    std::fs::rename(
        dir.path().join("model-00002-of-00002.gguf"),
        dir.path().join("aparte.bin"),
    )
    .unwrap();
    let v = larm_models::bundle::verify(&e.variants, &variant, dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!(
        (v.state, v.missing.clone()),
        (
            BundleState::Incomplete,
            vec!["model-00002-of-00002.gguf".to_string()]
        )
    );
    // Vuelve y un byte cambia: CORRUPT.
    std::fs::rename(
        dir.path().join("aparte.bin"),
        dir.path().join("model-00002-of-00002.gguf"),
    )
    .unwrap();
    let mut bytes = vec![3u8; 512];
    bytes[100] = 9;
    std::fs::write(dir.path().join("mmproj-f16.gguf"), bytes).unwrap();
    let v = larm_models::bundle::verify(&e.variants, &variant, dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!(
        (v.state, v.mismatched.clone()),
        (BundleState::Corrupt, vec!["mmproj-f16.gguf".to_string()])
    );
    // El opcional ausente no impide VERIFIED.
    write(dir.path(), "mmproj-f16.gguf", &[3u8; 512]);
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    let v = larm_models::bundle::verify(&e.variants, &variant, dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!(v.state, BundleState::Verified);
    let stored = e.variants.get_bundle(&v.bundle_id).await.unwrap().unwrap();
    assert_eq!(stored.row_version, 5, "cada verificación con CAS");
    assert_eq!(
        stored.metadata["verification"]["missing"],
        serde_json::json!([])
    );
    // Reimportar no duplica la variante.
    let again = register(&e.models, &e.variants, &m, &ctx()).await.unwrap();
    assert_eq!(
        (again.variants_created, again.variant_ids.clone()),
        (0, vec![variant.clone()])
    );
}

#[tokio::test]
async fn completitud_segun_variante_y_negativos() {
    let e = env().await;
    let dir = tempfile::tempdir().unwrap();
    // GGUF de visión sin mmproj: los ficheros están, pero falta un rol necesario.
    let (sha, size) = write(dir.path(), "solo.gguf", &[7u8; 1000]);
    let mut m = import(
        "Ciego",
        HF,
        "ciego",
        EvidenceImport::default(),
        vec![artifact(
            "solo.gguf",
            ArtifactRole::Weights,
            &sha,
            size,
            true,
        )],
    );
    m.distributions[0].revisions[0].variants[0].format = Some("GGUF".into());
    m.distributions[0].revisions[0].variants[0].capabilities = vec!["vision.understand".into()];
    let r = register(&e.models, &e.variants, &m, &ctx()).await.unwrap();
    let v = larm_models::bundle::verify(&e.variants, &r.variant_ids[0], dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!(v.state, BundleState::Incomplete);
    assert_eq!(v.missing_roles, Some(vec![ArtifactRole::Mmproj]));
    // Sin hash no hay bundle: no se inventa.
    let mut unknown = import(
        "SinHash",
        MS,
        "sinhash",
        EvidenceImport::default(),
        vec![artifact("w.bin", ArtifactRole::Weights, &h('1'), 1, true)],
    );
    unknown.distributions[0].revisions[0].variants[0].artifacts[0].sha256 = None;
    let r = register(&e.models, &e.variants, &unknown, &ctx())
        .await
        .unwrap();
    let variant = e
        .variants
        .get_variant(&r.variant_ids[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(variant.bundle_hash, None);
    let err = larm_models::bundle::verify(&e.variants, &variant.id, dir.path(), &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.details.reason, "bundle_missing");
    // Rutas no POSIX o duplicadas: nada se escribe.
    let before = count(&e.pool, "SELECT count(*) FROM model_variant").await;
    for files in [
        vec![artifact(
            "dir\\w.gguf",
            ArtifactRole::Weights,
            &h('1'),
            1,
            true,
        )],
        vec![
            artifact("W.gguf", ArtifactRole::Weights, &h('1'), 1, true),
            artifact("w.GGUF", ArtifactRole::Weights, &h('2'), 1, true),
        ],
        vec![artifact(
            "../w.gguf",
            ArtifactRole::Weights,
            &h('1'),
            1,
            true,
        )],
    ] {
        let mut bad = import("Malo", HF, "malo", EvidenceImport::default(), files);
        bad.distributions[0].revisions[0].revision_key = format!("r-{}", uuid::Uuid::now_v7());
        assert!(register(&e.models, &e.variants, &bad, &ctx())
            .await
            .is_err());
    }
    assert_eq!(
        count(&e.pool, "SELECT count(*) FROM model_variant").await,
        before
    );
    // Formato desconocido: se registra sin inventar requisitos.
    let mut future = import(
        "Futuro",
        MS,
        "futuro",
        EvidenceImport::default(),
        vec![artifact("m.bin", ArtifactRole::Weights, &sha, size, true)],
    );
    future.distributions[0].revisions[0].variants[0].format = Some("MLC_FUTURE".into());
    std::fs::copy(dir.path().join("solo.gguf"), dir.path().join("m.bin")).unwrap();
    let r = register(&e.models, &e.variants, &future, &ctx())
        .await
        .unwrap();
    let v = larm_models::bundle::verify(&e.variants, &r.variant_ids[0], dir.path(), &ctx())
        .await
        .unwrap();
    assert_eq!((v.state, v.missing_roles), (BundleState::Verified, None));
}

#[tokio::test]
async fn probable_por_evidencia_sin_nombre_ni_bytes_comunes() {
    let e = env().await;
    let a = register(
        &e.models,
        &e.variants,
        &import("Qwen-X", HF, "Qwen-X", qwen_evidence(), qwen_files()),
        &ctx(),
    )
    .await
    .unwrap();
    // Otro nombre y ningún byte en común: solo la evidencia declarada lo
    // relaciona (defecto encontrado por la prueba real: no se comparaba).
    let files = vec![artifact(
        "model.safetensors",
        ArtifactRole::Weights,
        &h('6'),
        10,
        true,
    )];
    let mut ev = qwen_evidence();
    ev.config_hash = None;
    let b = register(
        &e.models,
        &e.variants,
        &import("Instruct sin parecido", MS, "otro", ev, files),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(b.model_created);
    assert_eq!(b.resolutions.len(), 1);
    assert_eq!(b.resolutions[0].grade, IdentityGrade::ProbableSameModel);
    assert_eq!(b.possible_duplicates, vec![a.model_id]);
}

#[tokio::test]
async fn sin_evidencia_suficiente_no_se_anota_duplicado() {
    let e = env().await;
    register(
        &e.models,
        &e.variants,
        &import("Qwen-X", HF, "Qwen-X", qwen_evidence(), qwen_files()),
        &ctx(),
    )
    .await
    .unwrap();
    // Solo comparte la arquitectura: candidato, pero UNKNOWN y sin anotar.
    let ev = EvidenceImport {
        architecture: Some("qwen2".into()),
        ..EvidenceImport::default()
    };
    let files = vec![artifact(
        "m.safetensors",
        ArtifactRole::Weights,
        &h('6'),
        10,
        true,
    )];
    let b = register(
        &e.models,
        &e.variants,
        &import("Otro", MS, "otro", ev, files),
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(b.resolutions[0].grade, IdentityGrade::Unknown);
    assert!(b.possible_duplicates.is_empty());
}
