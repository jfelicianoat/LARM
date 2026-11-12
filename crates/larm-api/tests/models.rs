//! Registro de modelos de solo lectura (P040) con el guard real: scopes,
//! filtros permitidos por ruta, alias de capacidad, cursores y 404. La
//! conducta de base se prueba en `larm-models/tests/registry.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use larm_api::models::ModelsState;
use larm_api::security::guard;
use larm_core::security::memory::{MemoryCredentials, MemoryIdempotency, MemorySecrets};
use larm_core::security::{CursorSigner, Principals};
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::bridge::ApiError;
use larm_domain::model::{
    ArtifactProvenance, CanonicalModel, ModelDistribution, ModelRevision, RevisionState,
};
use larm_domain::model::{
    ArtifactSizeEstimate, BundleState, ModelArtifact, ModelBundle, ModelVariant,
};
use larm_domain::model_verification::ModelVerificationSubmitter;
use larm_domain::ports::{
    DistributionFilter, ModelFilter, ModelRepository, MutationContext, PrincipalAuthenticator,
    RevisionFilter,
};
use larm_domain::ports::{VariantFilter, VariantRepository};
use parking_lot::Mutex;
use tower::ServiceExt;

const SOURCE: &str = "0192f000-0000-7000-8000-00000000f001";

fn id(n: u8) -> String {
    format!("0192f000-0000-7000-8000-0000000000{n:02x}")
}

fn ts(n: u8) -> String {
    format!("2026-10-11T00:00:{n:02}.000Z")
}

/// Tres modelos, una distribución y dos revisiones; recuerda el último filtro.
struct Memory {
    last_model_filter: Mutex<Option<ModelFilter>>,
}

fn models() -> Vec<CanonicalModel> {
    (1..=3)
        .map(|n| CanonicalModel {
            id: id(n),
            created_at: ts(n),
            updated_at: ts(n),
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: format!("Modelo {n}"),
            upstream_identity: None,
            capabilities: if n == 2 {
                vec!["vision.ocr".into()]
            } else {
                vec!["text.generate".into()]
            },
            possible_duplicates: vec![],
        })
        .collect()
}

fn revisions() -> Vec<ModelRevision> {
    [
        (0x21, RevisionState::Current),
        (0x22, RevisionState::Retired),
    ]
    .into_iter()
    .map(|(n, state)| ModelRevision {
        id: id(n),
        created_at: ts(n % 60),
        updated_at: ts(n % 60),
        row_version: 1,
        metadata: serde_json::Map::new(),
        distribution_id: id(0x11),
        revision_key: format!("r{n}"),
        upstream_commit: None,
        manifest_hash: "1".repeat(64),
        state,
    })
    .collect()
}

#[async_trait]
impl ModelRepository for Memory {
    async fn list_models(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        f: &ModelFilter,
    ) -> Result<Vec<CanonicalModel>, ApiError> {
        *self.last_model_filter.lock() = Some(f.clone());
        let after = after.unwrap_or_default();
        Ok(models()
            .into_iter()
            .filter(|m| (m.created_at.clone(), m.id.clone()) > after)
            .filter(|m| {
                f.capability
                    .as_ref()
                    .is_none_or(|c| m.capabilities.contains(c))
            })
            .take(limit as usize)
            .collect())
    }
    async fn get_model(&self, id: &str) -> Result<Option<CanonicalModel>, ApiError> {
        Ok(models().into_iter().find(|m| m.id == id))
    }
    async fn model_by_upstream(&self, _: &str) -> Result<Option<CanonicalModel>, ApiError> {
        unreachable!()
    }
    async fn models_named(&self, _: &str) -> Result<Vec<CanonicalModel>, ApiError> {
        unreachable!()
    }
    async fn models_by_evidence(&self, _: &str, _: &str) -> Result<Vec<CanonicalModel>, ApiError> {
        unreachable!()
    }
    async fn insert_model(&self, _: &CanonicalModel, _: &MutationContext) -> Result<(), ApiError> {
        unreachable!("la API es de solo lectura")
    }
    async fn add_possible_duplicate(
        &self,
        _: &str,
        _: &str,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        unreachable!()
    }
    async fn list_distributions(
        &self,
        _: Option<(String, String)>,
        _: u32,
        f: &DistributionFilter,
    ) -> Result<Vec<ModelDistribution>, ApiError> {
        let d = ModelDistribution {
            id: id(0x11),
            created_at: ts(11),
            updated_at: ts(11),
            row_version: 1,
            metadata: serde_json::Map::new(),
            canonical_model_id: id(1),
            source_id: SOURCE.into(),
            upstream_namespace: "Qwen".into(),
            upstream_model_id: "Qwen-X".into(),
            license_id: None,
        };
        Ok(if f.source_id.as_deref().is_none_or(|s| s == SOURCE) {
            vec![d]
        } else {
            vec![]
        })
    }
    async fn get_distribution(&self, _: &str) -> Result<Option<ModelDistribution>, ApiError> {
        Ok(None)
    }
    async fn distribution_by_identity(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Option<ModelDistribution>, ApiError> {
        unreachable!()
    }
    async fn insert_distribution(
        &self,
        _: &ModelDistribution,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        unreachable!()
    }
    async fn list_revisions(
        &self,
        _: Option<(String, String)>,
        _: u32,
        f: &RevisionFilter,
    ) -> Result<Vec<ModelRevision>, ApiError> {
        Ok(revisions()
            .into_iter()
            .filter(|r| f.state.is_none_or(|s| s == r.state))
            .collect())
    }
    async fn get_revision(&self, id: &str) -> Result<Option<ModelRevision>, ApiError> {
        Ok(revisions().into_iter().find(|r| r.id == id))
    }
    async fn revision_by_key(&self, _: &str, _: &str) -> Result<Option<ModelRevision>, ApiError> {
        unreachable!()
    }
    async fn insert_revision(
        &self,
        _: &ModelRevision,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        unreachable!()
    }
    async fn replace_current_revision(
        &self,
        _: &ModelRevision,
        _: &MutationContext,
    ) -> Result<Option<String>, ApiError> {
        unreachable!()
    }
    async fn insert_provenance(
        &self,
        _: &ArtifactProvenance,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        unreachable!()
    }
    async fn provenance_for(&self, _: &str) -> Result<Vec<ArtifactProvenance>, ApiError> {
        unreachable!()
    }
}

struct Api {
    app: axum::Router,
    reader: String,
    writer: String,
    memory: Arc<Memory>,
}

async fn setup() -> Api {
    let principals = Arc::new(Principals::new(
        Arc::new(MemoryCredentials::default()),
        Arc::new(MemorySecrets::default()),
    ));
    let ctx = MutationContext {
        actor_id: "t".into(),
        correlation_id: "c".into(),
    };
    let (_, reader) = principals
        .enroll(
            PrincipalKind::Operator,
            "lector",
            vec![ApiScope::Read],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let (_, writer) = principals
        .enroll(
            PrincipalKind::Operator,
            "editor",
            vec![ApiScope::Read, ApiScope::ModelsWrite],
            vec![],
            None,
            &ctx,
        )
        .await
        .unwrap();
    let auth: Arc<dyn PrincipalAuthenticator> = principals;
    let memory = Arc::new(Memory {
        last_model_filter: Mutex::new(None),
    });
    let app = larm_api::models::routes(ModelsState {
        repo: memory.clone(),
        variants: Arc::new(Variants),
        cursors: Arc::new(CursorSigner::default()),
        idempotency: Arc::new(MemoryIdempotency::default()),
        verification: Arc::new(Variants),
    })
    .layer(axum::middleware::from_fn_with_state(auth, guard));
    Api {
        app,
        reader,
        writer,
        memory,
    }
}

async fn get(api: &Api, path: &str, token: &str) -> (StatusCode, serde_json::Value) {
    let res = api
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn post(
    api: &Api,
    path: &str,
    token: &str,
    key: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let res = api
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn listas_y_detalle() {
    let api = setup().await;
    let (s, page) = get(&api, "/api/v1/models?limit=2", &api.reader).await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let (_, page2) = get(
        &api,
        &format!("/api/v1/models?limit=2&cursor={cursor}"),
        &api.reader,
    )
    .await;
    assert_eq!(page2["items"].as_array().unwrap().len(), 1);
    // El cursor queda ligado al filtro.
    let (s, _) = get(
        &api,
        &format!("/api/v1/models?limit=2&capability=text.generate&cursor={cursor}"),
        &api.reader,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, m) = get(&api, &format!("/api/v1/models/{}", id(2)), &api.reader).await;
    assert_eq!(
        (s, m["capabilities"][0].as_str()),
        (StatusCode::OK, Some("vision.ocr"))
    );
    let (s, _) = get(&api, &format!("/api/v1/models/{}", id(9)), &api.reader).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = get(&api, "/api/v1/models/qwen", &api.reader).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, d) = get(
        &api,
        &format!("/api/v1/model-distributions?source_id={SOURCE}"),
        &api.reader,
    )
    .await;
    assert_eq!(
        (s, d["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
    let (s, _) = get(
        &api,
        &format!("/api/v1/model-distributions/{}", id(0x11)),
        &api.reader,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, r) = get(&api, "/api/v1/model-revisions?state=CURRENT", &api.reader).await;
    assert_eq!(
        (s, r["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
    let (s, r) = get(
        &api,
        &format!("/api/v1/model-revisions/{}", id(0x22)),
        &api.reader,
    )
    .await;
    assert_eq!((s, r["state"].as_str()), (StatusCode::OK, Some("RETIRED")));
}

#[tokio::test]
async fn filtros_por_ruta_y_alias() {
    let api = setup().await;
    // El alias de solo lectura filtra por su id.
    let (s, page) = get(&api, "/api/v1/models?capability=ocr", &api.reader).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        api.memory
            .last_model_filter
            .lock()
            .clone()
            .unwrap()
            .capability
            .as_deref(),
        Some("vision.ocr")
    );
    // Una capacidad desconocida pero bien formada es un filtro válido (vacío).
    let (s, page) = get(&api, "/api/v1/models?capability=audio.diarize", &api.reader).await;
    assert_eq!(
        (s, page["items"].as_array().unwrap().len()),
        (StatusCode::OK, 0)
    );
    for bad in [
        "/api/v1/models?capability=Vision%20OCR",
        "/api/v1/models?state=CURRENT",
        "/api/v1/model-distributions?source_id=hf",
        "/api/v1/model-distributions?capability=text.generate",
        "/api/v1/model-revisions?state=ACTIVE",
        "/api/v1/model-revisions?q=x",
        "/api/v1/models?limit=0",
        "/api/v1/models?limit=201",
    ] {
        let (s, e) = get(&api, bad, &api.reader).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("VALIDATION_FAILED")),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn sin_token_401() {
    let api = setup().await;
    let (s, _) = get(&api, "/api/v1/models", "falso").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// Una variante del modelo 1 con su bundle y un artefacto.
struct Variants;

fn variant() -> ModelVariant {
    ModelVariant {
        id: id(0x31),
        created_at: ts(31),
        updated_at: ts(31),
        row_version: 1,
        metadata: serde_json::Map::new(),
        revision_id: id(0x21),
        format: Some("GGUF".into()),
        quantization: Some("Q4_K_M".into()),
        architecture: Some("qwen2".into()),
        bundle_hash: Some("a".repeat(64)),
        lifecycle: "DISCOVERED".into(),
        availability: "UNKNOWN".into(),
        ownership: larm_domain::artifacts::Ownership::LarmManaged,
        promotion: "NONE".into(),
        system_role: "GENERAL".into(),
        capabilities: vec!["text.generate".into()],
        model_limits: larm_domain::model::ModelLimits::default(),
        system1_state: None,
        update_available: false,
        status: "DISCOVERED".into(),
    }
}

fn bundle() -> ModelBundle {
    ModelBundle {
        id: id(0x41),
        created_at: ts(41),
        updated_at: ts(41),
        row_version: 1,
        metadata: serde_json::Map::new(),
        variant_id: id(0x31),
        required_artifact_ids: vec![id(0x51)],
        optional_artifact_ids: vec![],
        manifest_hash: "b".repeat(64),
        state: BundleState::Incomplete,
    }
}

fn model_artifact() -> ModelArtifact {
    ModelArtifact {
        id: id(0x51),
        created_at: ts(51),
        updated_at: ts(51),
        row_version: 1,
        metadata: serde_json::Map::new(),
        variant_id: id(0x31),
        filename: "qwen-x-q4_k_m.gguf".into(),
        format: Some("GGUF".into()),
        quantization: Some("Q4_K_M".into()),
        expected_size: larm_domain::units::DecimalU64::new(4_683_073_856),
        actual_size: None,
        size_estimate: ArtifactSizeEstimate::unknown(
            "Fixture sin evidencia de confianza del tamaño",
            "test:api-model-artifact",
        ),
        sha256: Some("c".repeat(64)),
        source_url: None,
        required: true,
    }
}

#[async_trait]
impl ModelVerificationSubmitter for Variants {
    async fn enqueue_verification(
        &self,
        _variant_id: &str,
        _: &MutationContext,
    ) -> Result<larm_domain::bridge::JobReceipt, ApiError> {
        Ok(larm_domain::bridge::JobReceipt {
            job_id: id(0x71),
            status_url: format!("/api/v1/jobs/{}", id(0x71)),
            state: larm_domain::bridge::JobPhase::Queued,
        })
    }
}

#[async_trait]
impl VariantRepository for Variants {
    async fn list_variants(
        &self,
        _: Option<(String, String)>,
        _: u32,
        f: &VariantFilter,
    ) -> Result<Vec<ModelVariant>, ApiError> {
        let v = variant();
        let keep = f.state.as_deref().is_none_or(|s| s == v.lifecycle)
            && f.capability
                .as_ref()
                .is_none_or(|c| v.capabilities.contains(c))
            && f.model_id.as_deref().is_none_or(|m| m == id(1));
        Ok(if keep { vec![v] } else { vec![] })
    }
    async fn get_variant(&self, i: &str) -> Result<Option<ModelVariant>, ApiError> {
        Ok((i == id(0x31)).then(variant))
    }
    async fn variant_detail(&self, i: &str) -> Result<Option<serde_json::Value>, ApiError> {
        Ok((i == id(0x31)).then(|| serde_json::json!({"variant": variant()})))
    }
    async fn select_variant(&self, i: &str, _: &MutationContext) -> Result<ModelVariant, ApiError> {
        let Some(mut selected) = (i == id(0x31)).then(variant) else {
            return Err(ApiError::new(
                "NOT_FOUND",
                "Variante desconocida",
                "not_found",
            ));
        };
        selected.lifecycle = "SELECTED".into();
        selected.status = ModelVariant::status_for(
            &selected.lifecycle,
            &selected.availability,
            selected.update_available,
        )
        .to_string();
        Ok(selected)
    }
    async fn find_variant(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Option<ModelVariant>, ApiError> {
        unreachable!()
    }
    async fn insert_variant(
        &self,
        _: &ModelVariant,
        _: &[ModelArtifact],
        _: Option<&ModelBundle>,
        _: &MutationContext,
    ) -> Result<(), ApiError> {
        unreachable!("la API es de solo lectura")
    }
    async fn list_artifacts(
        &self,
        _: Option<(String, String)>,
        _: u32,
        q: Option<&str>,
    ) -> Result<Vec<ModelArtifact>, ApiError> {
        let a = model_artifact();
        Ok(if q.is_none_or(|q| a.filename.contains(q)) {
            vec![a]
        } else {
            vec![]
        })
    }
    async fn get_artifact(&self, i: &str) -> Result<Option<ModelArtifact>, ApiError> {
        Ok((i == id(0x51)).then(model_artifact))
    }
    async fn artifacts_of(&self, _: &str) -> Result<Vec<ModelArtifact>, ApiError> {
        unreachable!()
    }
    async fn list_bundles(
        &self,
        _: Option<(String, String)>,
        _: u32,
        state: Option<BundleState>,
    ) -> Result<Vec<ModelBundle>, ApiError> {
        let b = bundle();
        Ok(if state.is_none_or(|s| s == b.state) {
            vec![b]
        } else {
            vec![]
        })
    }
    async fn get_bundle(&self, i: &str) -> Result<Option<ModelBundle>, ApiError> {
        Ok((i == id(0x41)).then(bundle))
    }
    async fn bundle_of(&self, _: &str) -> Result<Option<ModelBundle>, ApiError> {
        unreachable!()
    }
    async fn set_availability(
        &self,
        _: &str,
        _: i64,
        _: &str,
        _: serde_json::Value,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn set_promotion(
        &self,
        _: &str,
        _: i64,
        _: &str,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn mark_variant_corrupt(
        &self,
        _: &str,
        _: i64,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn finish_variant_reverification(
        &self,
        _: &str,
        _: i64,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn delete_variant(&self, _: &str, _: i64, _: &MutationContext) -> Result<(), ApiError> {
        unreachable!()
    }
    async fn set_bundle_state(
        &self,
        _: &str,
        _: i64,
        _: BundleState,
        _: serde_json::Value,
        _: &MutationContext,
    ) -> Result<i64, ApiError> {
        unreachable!()
    }
    async fn models_with_artifact_hashes(
        &self,
        _: &[String],
    ) -> Result<Vec<CanonicalModel>, ApiError> {
        unreachable!()
    }
}

#[tokio::test]
async fn variantes_bundles_y_artefactos() {
    let api = setup().await;
    let (s, page) = get(
        &api,
        "/api/v1/variants?state=DISCOVERED&capability=text.generate",
        &api.reader,
    )
    .await;
    assert_eq!(
        (s, page["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
    let (s, page) = get(
        &api,
        &format!("/api/v1/models/{}/variants", id(1)),
        &api.reader,
    )
    .await;
    assert_eq!(
        (s, page["items"][0]["id"].as_str()),
        (StatusCode::OK, Some(id(0x31).as_str()))
    );
    let (s, page) = get(
        &api,
        &format!("/api/v1/models/{}/variants", id(2)),
        &api.reader,
    )
    .await;
    assert_eq!(
        (s, page["items"].as_array().unwrap().len()),
        (StatusCode::OK, 0)
    );
    let (s, _) = get(
        &api,
        &format!("/api/v1/models/{}/variants", id(9)),
        &api.reader,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "modelo canónico desconocido");
    let (s, v) = get(&api, &format!("/api/v1/variants/{}", id(0x31)), &api.reader).await;
    assert_eq!(
        (s, v["model_limits"]["parameter_count"].clone()),
        (StatusCode::OK, serde_json::Value::Null)
    );
    let (s, b) = get(&api, "/api/v1/model-bundles?state=INCOMPLETE", &api.reader).await;
    assert_eq!(
        (s, b["items"][0]["state"].as_str()),
        (StatusCode::OK, Some("INCOMPLETE"))
    );
    let (s, b) = get(
        &api,
        &format!("/api/v1/model-bundles/{}", id(0x41)),
        &api.reader,
    )
    .await;
    assert_eq!(
        (s, b["manifest_hash"].as_str().map(str::len)),
        (StatusCode::OK, Some(64))
    );
    let (s, a) = get(&api, "/api/v1/model-artifacts?q=q4_k_m", &api.reader).await;
    assert_eq!(
        (s, a["items"][0]["expected_size"].as_str()),
        (StatusCode::OK, Some("4683073856"))
    );
    let (s, _) = get(
        &api,
        &format!("/api/v1/model-artifacts/{}", id(0x51)),
        &api.reader,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for bad in [
        "/api/v1/variants?state=LISTO",
        "/api/v1/variants?capability=Texto",
        "/api/v1/variants?q=x",
        "/api/v1/model-bundles?state=OK",
        "/api/v1/model-artifacts?state=VERIFIED",
        "/api/v1/variants/x",
    ] {
        let (s, _) = get(&api, bad, &api.reader).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    for missing in [
        "/api/v1/variants/",
        "/api/v1/model-bundles/",
        "/api/v1/model-artifacts/",
    ] {
        let (s, _) = get(&api, &format!("{missing}{}", id(0x99)), &api.reader).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{missing}");
    }
}

#[tokio::test]
async fn seleccion_idempotente_y_detalle_de_variante() {
    let api = setup().await;
    let body = format!("{{\"variant_id\":\"{}\"}}", id(0x31));
    let (status, selected) = post(
        &api,
        "/api/v1/models/select",
        &api.writer,
        "selection-1",
        &body,
    )
    .await;
    assert_eq!(
        (status, selected["lifecycle"].as_str()),
        (StatusCode::CREATED, Some("SELECTED"))
    );
    let (status, replay) = post(
        &api,
        "/api/v1/models/select",
        &api.writer,
        "selection-1",
        &body,
    )
    .await;
    assert_eq!((status, replay), (StatusCode::CREATED, selected));
    let changed_body = format!("{{\"variant_id\":\"{}\"}}", id(0x32));
    let (status, error) = post(
        &api,
        "/api/v1/models/select",
        &api.writer,
        "selection-1",
        &changed_body,
    )
    .await;
    assert_eq!(
        (status, error["code"].as_str()),
        (StatusCode::CONFLICT, Some("IDEMPOTENCY_CONFLICT"))
    );
    let (status, _) = post(
        &api,
        "/api/v1/models/select",
        &api.reader,
        "selection-2",
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, detail) = get(
        &api,
        &format!("/api/v1/variants/{}/detail", id(0x31)),
        &api.reader,
    )
    .await;
    assert_eq!(
        (status, detail["variant"]["id"].as_str()),
        (StatusCode::OK, Some(id(0x31).as_str()))
    );
}

#[tokio::test]
async fn verify_encola_con_probe_estricto_y_replay_idempotente() {
    let api = setup().await;
    let path = format!("/api/v1/models/{}/verify", id(0x31));
    let body = r#"{"resource_id":null,"profile_id":null}"#;
    let (status, first) = post(&api, &path, &api.writer, "verify-1", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["state"], "QUEUED");
    assert_eq!(first["status_url"], format!("/api/v1/jobs/{}", id(0x71)));
    let (status, replay) = post(&api, &path, &api.writer, "verify-1", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{replay}");
    assert_eq!(first, replay);
    let other_path = format!("/api/v1/models/{}/verify", id(0x32));
    let (status, conflict) = post(&api, &other_path, &api.writer, "verify-1", body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");

    let (status, missing) = post(
        &api,
        &path,
        &api.writer,
        "verify-missing",
        r#"{"resource_id":null}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    let (status, mismatch) = post(
        &api,
        &path,
        &api.writer,
        "verify-mismatch",
        &format!(r#"{{"resource_id":"{}","profile_id":null}}"#, id(0x33)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{mismatch}");
}
