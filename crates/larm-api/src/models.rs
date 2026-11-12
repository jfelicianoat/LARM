//! Registro de modelos de solo lectura (P040), detrás del guard (`read`):
//!
//! - `GET /models` (grupos `CanonicalModel`; filtros `q`, `capability`) y
//!   `GET /models/{id}`;
//! - `GET /model-distributions` (`q`, `source_id`) y `/{id}`;
//! - `GET /model-revisions` (`state`) y `/{id}`;
//! - P041: `GET /variants` y `/models/{id}/variants` (`state` = ciclo de
//!   vida, `capability`), `/variants/{id}`, `/model-bundles` (`state`) y
//!   `/{id}`, `/model-artifacts` (`q`) y `/{id}`.
//!
//! Cursores firmados ligados a los filtros; un filtro no previsto es 400.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Extension, Router};
use larm_core::security::CursorSigner;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::model::{
    BundleState, CanonicalModel, ModelArtifact, ModelBundle, ModelDistribution, ModelRevision,
    ModelVariant, RevisionState,
};
use larm_domain::model_verification::ModelVerificationSubmitter;
use larm_domain::ports::{
    DistributionFilter, IdempotencyStore, ModelFilter, ModelRepository, RevisionFilter,
    VariantFilter, VariantRepository,
};

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct ModelsState {
    pub repo: Arc<dyn ModelRepository>,
    pub variants: Arc<dyn VariantRepository>,
    pub cursors: Arc<CursorSigner>,
    pub idempotency: Arc<dyn IdempotencyStore>,
    pub verification: Arc<dyn ModelVerificationSubmitter>,
}

pub fn routes(state: ModelsState) -> Router {
    Router::new()
        .route("/api/v1/models", get(list_models))
        .route("/api/v1/models/{id}", get(read_model))
        .route("/api/v1/model-distributions", get(list_distributions))
        .route("/api/v1/model-distributions/{id}", get(read_distribution))
        .route("/api/v1/model-revisions", get(list_revisions))
        .route("/api/v1/model-revisions/{id}", get(read_revision))
        .route("/api/v1/variants", get(list_variants))
        .route("/api/v1/variants/{id}", get(read_variant))
        .route("/api/v1/models/{id}/variants", get(model_variants))
        .route("/api/v1/model-bundles", get(list_bundles))
        .route("/api/v1/model-bundles/{id}", get(read_bundle))
        .route("/api/v1/model-artifacts", get(list_artifacts))
        .route("/api/v1/model-artifacts/{id}", get(read_artifact))
        .route("/api/v1/models/select", post(select_variant))
        .route("/api/v1/models/{id}/verify", post(verify_variant))
        .route("/api/v1/variants/{id}/detail", get(variant_detail))
        .route("/api/v1/variants/{id}/promotion", patch(promote_variant))
        .with_state(state)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    variant_id: String,
}

async fn select_variant(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request: Selection =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if !is_uuid_v7(&request.variant_id) {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "variant_id no es UUIDv7",
            "schema_violation",
        )));
    }
    let (variants, mutation, id) = (state.variants.clone(), ctx.mutation(), request.variant_id);
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            let selected = variants.select_variant(&id, &mutation).await?;
            Ok((201, serde_json::to_value(selected).unwrap_or_default()))
        },
    )
    .await
}

async fn variant_detail(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let detail = state
        .variants
        .variant_detail(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Variante", &id)))?;
    Ok(json(StatusCode::OK, &detail))
}

async fn verify_variant(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let probe: larm_domain::hardware::Probe =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if probe
        .resource_id
        .as_deref()
        .is_some_and(|resource_id| resource_id != id)
        || probe.profile_id.is_some()
    {
        return Err(fail(&ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "Probe debe apuntar a esta variante y no admite profile_id",
            "verification_probe_invalid",
        )));
    }
    let verification = state.verification.clone();
    let mutation = ctx.mutation();
    // El id del recurso forma parte de la identidad idempotente porque vive
    // en la ruta, fuera del cuerpo Probe.
    let mut idempotency_body = id.as_bytes().to_vec();
    idempotency_body.push(0);
    idempotency_body.extend_from_slice(&body);
    let idempotency_body = Bytes::from(idempotency_body);
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &idempotency_body,
        || async move {
            let receipt = verification.enqueue_verification(&id, &mutation).await?;
            Ok((202, serde_json::to_value(receipt).unwrap_or_default()))
        },
    )
    .await
}

fn fail(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |e| Failure::from(e).recorrelate(&ctx.correlation_id)
}

fn id_param(id: &str, ctx: &AuthContext) -> Result<(), Failure> {
    if is_uuid_v7(id) {
        Ok(())
    } else {
        Err(fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "Identificador no es UUIDv7",
            "path_id_invalid",
        )))
    }
}

fn not_found(what: &str, id: &str) -> ApiError {
    ApiError::new("NOT_FOUND", format!("{what} desconocido"), "not_found")
        .with_resource(id.to_string())
}

fn filter_error(message: String) -> ApiError {
    ApiError::new("VALIDATION_FAILED", message, "filter_invalid")
}

async fn list_models(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(&params, &["q", "capability"]).map_err(fail(&ctx))?;
    // Un alias de capacidad filtra por su id (alias de solo lectura).
    let capability = params
        .get("capability")
        .map(|c| larm_domain::model::capability(c).map(|c| c.id))
        .transpose()
        .map_err(|e| fail(&ctx)(filter_error(e.message)))?;
    let filter = ModelFilter {
        q: params.get("q").cloned(),
        capability,
    };
    let digest = format!(
        "models:q={}:capability={}",
        filter.q.clone().unwrap_or_default(),
        filter.capability.clone().unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |m: &CanonicalModel| (m.created_at.clone(), m.id.clone()),
            |after, n| state.repo.list_models(after, n, &filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_model(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let model = state
        .repo
        .get_model(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Modelo", &id)))?;
    Ok(json(StatusCode::OK, &model))
}

async fn list_distributions(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(&params, &["q", "source_id"]).map_err(fail(&ctx))?;
    if params.get("source_id").is_some_and(|s| !is_uuid_v7(s)) {
        return Err(fail(&ctx)(filter_error("source_id no es UUIDv7".into())));
    }
    let filter = DistributionFilter {
        q: params.get("q").cloned(),
        source_id: params.get("source_id").cloned(),
    };
    let digest = format!(
        "model-distributions:q={}:source_id={}",
        filter.q.clone().unwrap_or_default(),
        filter.source_id.clone().unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |d: &ModelDistribution| (d.created_at.clone(), d.id.clone()),
            |after, n| state.repo.list_distributions(after, n, &filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_distribution(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let d = state
        .repo
        .get_distribution(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Distribución", &id)))?;
    Ok(json(StatusCode::OK, &d))
}

async fn list_revisions(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &["state"]).map_err(fail(&ctx))?;
    let filter = RevisionFilter {
        state: params
            .get("state")
            .map(|s| {
                serde_json::from_value::<RevisionState>(serde_json::Value::String(s.clone()))
                    .map_err(|_| filter_error(format!("Estado desconocido: {s}")))
            })
            .transpose()
            .map_err(fail(&ctx))?,
    };
    let digest = format!(
        "model-revisions:state={}",
        filter.state.map(RevisionState::as_str).unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |r: &ModelRevision| (r.created_at.clone(), r.id.clone()),
            |after, n| state.repo.list_revisions(after, n, &filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_revision(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let r = state
        .repo
        .get_revision(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Revisión", &id)))?;
    Ok(json(StatusCode::OK, &r))
}

fn variant_filter(
    params: &BTreeMap<String, String>,
    model_id: Option<String>,
) -> Result<VariantFilter, ApiError> {
    let state = params.get("state").cloned();
    if let Some(s) = &state {
        if !larm_domain::model::LIFECYCLE.contains(&s.as_str()) {
            return Err(filter_error(format!("Estado desconocido: {s}")));
        }
    }
    let capability = params
        .get("capability")
        .map(|c| larm_domain::model::capability(c).map(|c| c.id))
        .transpose()
        .map_err(|e| filter_error(e.message))?;
    Ok(VariantFilter {
        state,
        capability,
        model_id,
    })
}

async fn variants_page(
    state: &ModelsState,
    ctx: &AuthContext,
    params: &BTreeMap<String, String>,
    model_id: Option<String>,
) -> Answer {
    let (cursor, limit) =
        crate::http::list_params(params, &["state", "capability"]).map_err(fail(ctx))?;
    let filter = variant_filter(params, model_id).map_err(fail(ctx))?;
    let digest = format!(
        "variants:model={}:state={}:capability={}",
        filter.model_id.clone().unwrap_or_default(),
        filter.state.clone().unwrap_or_default(),
        filter.capability.clone().unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |v: &ModelVariant| (v.created_at.clone(), v.id.clone()),
            |after, n| state.variants.list_variants(after, n, &filter),
        )
        .await
        .map_err(fail(ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn list_variants(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    variants_page(&state, &ctx, &params, None).await
}

/// Variantes de un modelo canónico (aquí `{id}` es el del grupo).
async fn model_variants(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    id_param(&id, &ctx)?;
    if state
        .repo
        .get_model(&id)
        .await
        .map_err(fail(&ctx))?
        .is_none()
    {
        return Err(fail(&ctx)(not_found("Modelo", &id)));
    }
    variants_page(&state, &ctx, &params, Some(id)).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionRequest {
    promotion: String,
    expected_row_version: i64,
}

/// Promueve una variante en la máquina de estados P120. La exclusividad de
/// `DEFAULT` por (revision, format, quantization) la impone el repositorio.
async fn promote_variant(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let request: PromotionRequest =
        crate::http::strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    let mutation = ctx.mutation();
    let row_version = state
        .variants
        .set_promotion(
            &id,
            request.expected_row_version,
            &request.promotion,
            &mutation,
        )
        .await
        .map_err(fail(&ctx))?;
    let variant = state
        .variants
        .get_variant(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Variante", &id)))?;
    Ok(json(
        StatusCode::OK,
        &serde_json::json!({"variant": variant, "row_version": row_version}),
    ))
}

async fn read_variant(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let v = state
        .variants
        .get_variant(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Variante", &id)))?;
    Ok(json(StatusCode::OK, &v))
}

async fn list_bundles(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &["state"]).map_err(fail(&ctx))?;
    let filter = params
        .get("state")
        .map(|s| {
            serde_json::from_value::<BundleState>(serde_json::Value::String(s.clone()))
                .map_err(|_| filter_error(format!("Estado desconocido: {s}")))
        })
        .transpose()
        .map_err(fail(&ctx))?;
    let digest = format!(
        "model-bundles:state={}",
        filter.map(BundleState::as_str).unwrap_or_default()
    );
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |b: &ModelBundle| (b.created_at.clone(), b.id.clone()),
            |after, n| state.variants.list_bundles(after, n, filter),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_bundle(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let b = state
        .variants
        .get_bundle(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Bundle", &id)))?;
    Ok(json(StatusCode::OK, &b))
}

async fn list_artifacts(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &["q"]).map_err(fail(&ctx))?;
    let q = params.get("q").cloned();
    let digest = format!("model-artifacts:q={}", q.clone().unwrap_or_default());
    let page = state
        .cursors
        .page(
            &digest,
            cursor.as_deref(),
            limit,
            |a: &ModelArtifact| (a.created_at.clone(), a.id.clone()),
            |after, n| state.variants.list_artifacts(after, n, q.as_deref()),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read_artifact(
    State(state): State<ModelsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let a = state
        .variants
        .get_artifact(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| fail(&ctx)(not_found("Artefacto", &id)))?;
    Ok(json(StatusCode::OK, &a))
}
