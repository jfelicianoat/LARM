//! Registro de modelos (P040) sobre SQLite real: identidad del contrato 01,
//! agrupación, duplicados posibles, idempotencia, eventos y procedencia.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::models::SqlModels;
use larm_db::variants::SqlVariants;
use larm_domain::error::ErrorCode;
use larm_domain::model::{ArtifactProvenance, ArtifactRole, ModelRevision, RevisionState, Trust};
use larm_domain::ports::{
    DistributionFilter, ModelFilter, ModelRepository, MutationContext, RevisionFilter,
    VariantRepository,
};
use larm_models::registry::{
    register, ArtifactImport, DistributionImport, ModelImport, RevisionImport, VariantImport,
};
use sqlx::SqlitePool;

const HF: &str = "0192f000-0000-7000-8000-00000000f001";
const MS: &str = "0192f000-0000-7000-8000-00000000f002";
const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "importador".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

/// Fuentes: su API es de P042; aquí solo las filas que referencian las
/// distribuciones, sin comprobar el adaptador.
async fn sources(pool: &SqlitePool) {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    for (id, name) in [(HF, "huggingface"), (MS, "modelscope")] {
        sqlx::query(
            "INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,\
             auth_mode,account_id,policy) VALUES (?, 't', 't', 1, '{}', ?, 'adapter', 'https://example.invalid', 0, 1, 'NONE', NULL, '{}')",
        )
        .bind(id)
        .bind(name)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
}

async fn setup() -> (tempfile::TempDir, SqlitePool, SqlModels) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    sources(&db.pool).await;
    let repo = SqlModels {
        pool: db.pool.clone(),
    };
    (dir, db.pool, repo)
}

fn revision(key: &str, hash: &str, state: RevisionState) -> RevisionImport {
    let mut metadata = serde_json::Map::new();
    metadata.insert("sha".into(), key.into());
    metadata.insert("lastModified".into(), "2026-09-30T10:00:00Z".into());
    RevisionImport {
        revision_key: key.into(),
        upstream_commit: Some(key.into()),
        manifest_hash: hash.into(),
        state,
        metadata,
        variants: vec![],
        supersedes_current: false,
    }
}

fn distribution(
    source: &str,
    ns: &str,
    id: &str,
    revisions: Vec<RevisionImport>,
) -> DistributionImport {
    DistributionImport {
        source_id: source.into(),
        upstream_namespace: ns.into(),
        upstream_model_id: id.into(),
        license_id: Some("apache-2.0".into()),
        metadata: serde_json::Map::new(),
        revisions,
    }
}

fn qwen(distributions: Vec<DistributionImport>) -> ModelImport {
    ModelImport {
        name: "Qwen-X".into(),
        upstream_identity: Some("hf:Qwen/Qwen-X".into()),
        capabilities: vec![
            "text.generate".into(),
            "tool.call".into(),
            "audio.diarize".into(),
        ],
        metadata: serde_json::Map::new(),
        evidence: None,
        distributions,
    }
}

fn variants(repo: &SqlModels) -> SqlVariants {
    SqlVariants {
        pool: repo.pool.clone(),
    }
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn varias_distribuciones_un_modelo_canonico() {
    let (_d, pool, repo) = setup().await;
    let hf = register(
        &repo,
        &variants(&repo),
        &qwen(vec![distribution(
            HF,
            "Qwen",
            "Qwen-X",
            vec![revision("abc123", H1, RevisionState::Current)],
        )]),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(hf.model_created);
    assert_eq!(
        hf.unqualified_capabilities,
        vec!["audio.diarize".to_string()]
    );
    // La misma identidad upstream llega por otra fuente: se agrupa.
    let ms = register(
        &repo,
        &variants(&repo),
        &qwen(vec![distribution(
            MS,
            "qwen",
            "Qwen-X-SafeTensors",
            vec![revision("v1.0", H2, RevisionState::Current)],
        )]),
        &ctx(),
    )
    .await
    .unwrap();
    assert!(!ms.model_created);
    assert_eq!(ms.model_id, hf.model_id);
    assert_eq!(ms.distributions_created, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM canonical_model").await,
        1
    );
    let ds = repo
        .list_distributions(None, 50, &DistributionFilter::default())
        .await
        .unwrap();
    assert_eq!(ds.len(), 2);
    assert!(ds.iter().all(|d| d.canonical_model_id == hf.model_id));
    // Capacidades normalizadas y ordenadas; la desconocida se guarda.
    let model = repo.get_model(&hf.model_id).await.unwrap().unwrap();
    assert_eq!(
        model.capabilities,
        vec!["audio.diarize", "text.generate", "tool.call"]
    );
    // Filtros.
    let by_source = repo
        .list_distributions(
            None,
            50,
            &DistributionFilter {
                q: None,
                source_id: Some(MS.into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(by_source.len(), 1);
    let by_cap = repo
        .list_models(
            None,
            50,
            &ModelFilter {
                q: None,
                capability: Some("tool.call".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(by_cap.len(), 1);
    let none = repo
        .list_models(
            None,
            50,
            &ModelFilter {
                q: None,
                capability: Some("vision.ocr".into()),
            },
        )
        .await
        .unwrap();
    assert!(none.is_empty());
    let q = repo
        .list_models(
            None,
            50,
            &ModelFilter {
                q: Some("qwen-x".into()),
                capability: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(q.len(), 1);
    // Revisiones con su metadata original.
    let current = repo
        .list_revisions(
            None,
            50,
            &RevisionFilter {
                state: Some(RevisionState::Current),
            },
        )
        .await
        .unwrap();
    assert_eq!(current.len(), 2);
    assert_eq!(current[0].metadata["lastModified"], "2026-09-30T10:00:00Z");
    // Un evento por alta, en el outbox.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id = e.id WHERE e.event_type = 'model.state_changed'").await,
        5
    );
}

#[tokio::test]
async fn mismo_nombre_sin_upstream_no_se_fusiona() {
    let (_d, pool, repo) = setup().await;
    let mut a = qwen(vec![distribution(HF, "comunidad", "qwen-x-gguf", vec![])]);
    a.upstream_identity = None;
    let mut b = qwen(vec![distribution(MS, "otro", "Qwen-X", vec![])]);
    b.upstream_identity = None;
    b.name = "qwen-x".into();
    let ra = register(&repo, &variants(&repo), &a, &ctx()).await.unwrap();
    let rb = register(&repo, &variants(&repo), &b, &ctx()).await.unwrap();
    assert_ne!(ra.model_id, rb.model_id, "nunca se fusiona por nombre");
    assert_eq!(rb.possible_duplicates, vec![ra.model_id.clone()]);
    let first = repo.get_model(&ra.model_id).await.unwrap().unwrap();
    assert_eq!(
        first.possible_duplicates,
        vec![rb.model_id.clone()],
        "anotado en los dos"
    );
    assert_eq!(first.row_version, 2);
    let second = repo.get_model(&rb.model_id).await.unwrap().unwrap();
    assert_eq!(
        second.possible_duplicates,
        vec![ra.model_id.clone()],
        "guardado en el nuevo"
    );
    // Repetir la importación no crea otro modelo ni otro duplicado.
    let again = register(&repo, &variants(&repo), &b, &ctx()).await.unwrap();
    assert_eq!(again.model_id, rb.model_id);
    assert!(!again.model_created);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM canonical_model").await,
        2
    );
}

#[tokio::test]
async fn identidad_inmutable_y_conflictos() {
    let (_d, _pool, repo) = setup().await;
    let base = qwen(vec![distribution(
        HF,
        "Qwen",
        "Qwen-X",
        vec![revision("abc123", H1, RevisionState::Current)],
    )]);
    register(&repo, &variants(&repo), &base, &ctx())
        .await
        .unwrap();
    // Idempotente.
    let again = register(&repo, &variants(&repo), &base, &ctx())
        .await
        .unwrap();
    assert_eq!(
        (again.distributions_created, again.revisions_created),
        (0, 0)
    );
    // Misma clave de revisión con otro contenido: conflicto.
    let changed = qwen(vec![distribution(
        HF,
        "Qwen",
        "Qwen-X",
        vec![revision("abc123", H2, RevisionState::Current)],
    )]);
    let err = register(&repo, &variants(&repo), &changed, &ctx())
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.details.reason.as_str()),
        (ErrorCode::Conflict, "revision_key_immutable")
    );
    // Segunda revisión CURRENT en la misma distribución: conflicto.
    let second = qwen(vec![distribution(
        HF,
        "Qwen",
        "Qwen-X",
        vec![revision("def456", H2, RevisionState::Current)],
    )]);
    let err = register(&repo, &variants(&repo), &second, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.details.reason, "current_revision_exists");
    // La publicación ya es de otro modelo.
    let mut other = qwen(vec![distribution(HF, "Qwen", "Qwen-X", vec![])]);
    other.upstream_identity = Some("hf:Otro/Modelo".into());
    other.name = "Otro".into();
    let err = register(&repo, &variants(&repo), &other, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.details.reason, "distribution_belongs_to_other_model");
    // Revisión sin pin: solo PROVISIONAL.
    let pending = format!("pending:{}", uuid::Uuid::now_v7());
    let bad = qwen(vec![distribution(
        HF,
        "Qwen",
        "Qwen-X",
        vec![revision(&pending, H2, RevisionState::Candidate)],
    )]);
    let err = register(&repo, &variants(&repo), &bad, &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidStateTransition);
    let ok = qwen(vec![distribution(
        HF,
        "Qwen",
        "Qwen-X",
        vec![revision(&pending, H2, RevisionState::Provisional)],
    )]);
    assert_eq!(
        register(&repo, &variants(&repo), &ok, &ctx())
            .await
            .unwrap()
            .revisions_created,
        1
    );
    // Capacidad mal formada: nada se registra.
    let mut broken = qwen(vec![]);
    broken.upstream_identity = Some("hf:x/y".into());
    broken.capabilities = vec!["Text Generate".into()];
    assert_eq!(
        register(&repo, &variants(&repo), &broken, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "capability_invalid"
    );
    assert!(repo.model_by_upstream("hf:x/y").await.unwrap().is_none());
}

#[tokio::test]
async fn revision_roundtrip_y_paginacion() {
    let (_d, _pool, repo) = setup().await;
    let report = register(
        &repo,
        &variants(&repo),
        &qwen(vec![distribution(
            HF,
            "Qwen",
            "Qwen-X",
            (0..5)
                .map(|i| {
                    revision(
                        &format!("r{i}"),
                        H1,
                        if i == 4 {
                            RevisionState::Current
                        } else {
                            RevisionState::Retired
                        },
                    )
                })
                .collect(),
        )]),
        &ctx(),
    )
    .await
    .unwrap();
    assert_eq!(report.revisions_created, 5);
    let page1 = repo
        .list_revisions(None, 2, &RevisionFilter::default())
        .await
        .unwrap();
    let last = page1.last().unwrap();
    let page2 = repo
        .list_revisions(
            Some((last.created_at.clone(), last.id.clone())),
            10,
            &RevisionFilter::default(),
        )
        .await
        .unwrap();
    assert_eq!(page1.len() + page2.len(), 5);
    assert!(page2.iter().all(|r| !page1.iter().any(|p| p.id == r.id)));
    let one: ModelRevision = repo.get_revision(&page1[0].id).await.unwrap().unwrap();
    assert_eq!(one, page1[0]);
    let json = serde_json::to_value(&one).unwrap();
    assert_eq!(serde_json::from_value::<ModelRevision>(json).unwrap(), one);
}

#[tokio::test]
async fn seleccionar_variante_y_consultar_detalle_completo() {
    let (_d, pool, repo) = setup().await;
    let mut revision = revision("selectable", H1, RevisionState::Current);
    revision.variants.push(VariantImport {
        format: Some("GGUF".into()),
        quantization: Some("Q4_K_M".into()),
        architecture: Some("qwen2".into()),
        capabilities: vec!["text.generate".into()],
        ownership: None,
        model_limits: None,
        metadata: serde_json::Map::new(),
        artifacts: vec![ArtifactImport {
            filename: "weights.gguf".into(),
            role: ArtifactRole::Weights,
            format: Some("GGUF".into()),
            quantization: Some("Q4_K_M".into()),
            size_bytes: Some(larm_domain::units::DecimalU64::new(128).unwrap()),
            sha256: Some(H1.into()),
            source_url: None,
            required: true,
            metadata: serde_json::Map::new(),
        }],
    });
    let report = register(
        &repo,
        &variants(&repo),
        &qwen(vec![distribution(HF, "Qwen", "Qwen-X", vec![revision])]),
        &ctx(),
    )
    .await
    .unwrap();
    let variant_id = report.variant_ids.first().unwrap();
    let variants = variants(&repo);

    let selected = variants.select_variant(variant_id, &ctx()).await.unwrap();
    assert_eq!(selected.lifecycle, "SELECTED");
    assert_eq!(selected.row_version, 2);
    let replay = variants.select_variant(variant_id, &ctx()).await.unwrap();
    assert_eq!(
        replay.row_version, 2,
        "repetir selección no añade otra revisión"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM event WHERE event_type='model.state_changed' AND resource_type='model_variant' AND json_extract(payload, '$.to')='SELECTED'").await,
        1,
        "la transición queda auditada una sola vez"
    );

    let detail = variants.variant_detail(variant_id).await.unwrap().unwrap();
    assert_eq!(detail["variant"]["lifecycle"], "SELECTED");
    assert_eq!(detail["revision"]["revision_key"], "selectable");
    assert_eq!(detail["distribution"]["source_id"], HF);
    assert_eq!(detail["canonical_model"]["name"], "Qwen-X");
    assert_eq!(detail["artifacts"].as_array().unwrap().len(), 1);
    for field in [
        "bundle",
        "locations",
        "provenance",
        "compatibility",
        "evaluation_references",
        "certifications",
        "active_lease_ids",
    ] {
        assert!(detail.get(field).is_some(), "falta {field}");
    }
}

#[tokio::test]
async fn procedencia_completa() {
    let (_d, pool, repo) = setup().await;
    let artifact = uuid::Uuid::now_v7().to_string();
    let parent = uuid::Uuid::now_v7().to_string();
    {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        for id in [&artifact, &parent] {
            sqlx::query(
                "INSERT INTO model_artifact (id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,\
                 expected_size,actual_size,sha256,source_url,required) VALUES (?, 't', 't', 1, '{}', 'v', 'f', NULL, NULL, NULL, NULL, NULL, NULL, 1)",
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
    }
    let mut metadata = serde_json::Map::new();
    for (k, v) in [
        ("repository", "bartowski/Qwen-X-GGUF"),
        ("original_author", "Qwen"),
        ("uploader", "bartowski"),
        (
            "download_url",
            "https://huggingface.co/bartowski/Qwen-X-GGUF/resolve/abc/q4.gguf",
        ),
        ("base_model", "Qwen/Qwen-X"),
        ("quantizer", "bartowski"),
        ("downloaded_at", "2026-10-11T08:00:00.000Z"),
    ] {
        metadata.insert(k.into(), v.into());
    }
    metadata.insert("gated".into(), false.into());
    let mut parameters = serde_json::Map::new();
    parameters.insert("quantization".into(), "Q4_K_M".into());
    let p = ArtifactProvenance {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-11T08:00:00.000Z".into(),
        updated_at: "2026-10-11T08:00:00.000Z".into(),
        row_version: 1,
        metadata,
        artifact_id: artifact.clone(),
        source_id: Some(HF.into()),
        revision_key: Some("abc".into()),
        parents: vec![parent],
        tool_id: Some("llama.cpp/llama-quantize".into()),
        tool_version: Some("b4500".into()),
        parameters,
        license_id: Some("apache-2.0".into()),
        license_hash: Some(H1.into()),
        trust: Trust::Declared,
        actor_id: "importador".into(),
        evidence_refs: vec!["hf-api:abc".into()],
    };
    repo.insert_provenance(&p, &ctx()).await.unwrap();
    let back = repo.provenance_for(&artifact).await.unwrap();
    assert_eq!(back, vec![p.clone()], "roundtrip completo de §19");
    assert_eq!(
        larm_domain::model::origin_class(&back[0]),
        larm_domain::model::OriginClass::CommunityConversion
    );
    // Sin herramienta pero con versión: rechazada sin escribir.
    let mut bad = p;
    bad.id = uuid::Uuid::now_v7().to_string();
    bad.tool_id = None;
    assert!(repo.insert_provenance(&bad, &ctx()).await.is_err());
    assert_eq!(repo.provenance_for(&artifact).await.unwrap().len(), 1);
}
