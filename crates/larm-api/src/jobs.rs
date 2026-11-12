//! `GET /api/v1/jobs/{id}` (P007): consulta el job en las fuentes registradas.
//! Va detrás del guard (scope `read`); cada fuente filtra por principal.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_domain::bridge::{is_uuid_v7, ApiError, JobReceipt};
use larm_domain::ports::{JobSource, MutationContext, TransformationService};
use larm_domain::runtime_install::RuntimeInstallSubmitter;

use crate::http::{json, Answer, Failure};
use crate::security::AuthContext;

/// Fuentes de jobs consultadas en orden.
pub type JobSources = Arc<Vec<Arc<dyn JobSource>>>;

pub fn routes(sources: JobSources) -> Router {
    Router::new()
        .route("/api/v1/jobs/{id}", get(get_job))
        .with_state(sources)
}

pub fn cancellation_routes(
    runtime_installs: Arc<dyn RuntimeInstallSubmitter>,
    transformations: Arc<dyn TransformationService>,
) -> Router {
    Router::new()
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .layer(Extension(runtime_installs))
        .layer(Extension(transformations))
}

fn fail(error: ApiError, ctx: &AuthContext) -> Failure {
    Failure::from(error).recorrelate(&ctx.correlation_id)
}

async fn get_job(
    State(sources): State<JobSources>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    if !is_uuid_v7(&id) {
        return Err(fail(
            ApiError::new(
                "VALIDATION_FAILED",
                "Identificador no es UUIDv7",
                "path_id_invalid",
            ),
            &ctx,
        ));
    }
    for source in sources.iter() {
        if let Some(job) = source
            .job(&ctx.principal, &id)
            .await
            .map_err(|e| fail(e, &ctx))?
        {
            return Ok(json(StatusCode::OK, &job));
        }
    }
    Err(fail(
        ApiError::new("NOT_FOUND", "Job desconocido", "not_found").with_resource(id),
        &ctx,
    ))
}

async fn cancel_job(
    Extension(ctx): Extension<AuthContext>,
    Extension(runtime_installs): Extension<Arc<dyn RuntimeInstallSubmitter>>,
    Extension(transformations): Extension<Arc<dyn TransformationService>>,
    Path(id): Path<String>,
) -> Answer {
    if !is_uuid_v7(&id) {
        return Err(fail(
            ApiError::new(
                "VALIDATION_FAILED",
                "Identificador no es UUIDv7",
                "path_id_invalid",
            ),
            &ctx,
        ));
    }
    let mutation = MutationContext {
        actor_id: ctx.principal.id.clone(),
        correlation_id: ctx.correlation_id.clone(),
    };
    // El job no declara su clase: la cancelación de transformación responde
    // NOT_FOUND para ids ajenos o desconocidos y el handler cae a la de
    // instalaciones de runtime, que aplica la misma ocultación.
    match transformations
        .cancel_job(&id, &ctx.principal.id, &mutation)
        .await
    {
        Ok(receipt) => return Ok(json(StatusCode::OK, &receipt)),
        Err(error) if error.details.reason != "transformation_job_missing" => {
            return Err(fail(error, &ctx));
        }
        Err(_) => {}
    }
    let receipt: JobReceipt = runtime_installs
        .cancel(&id, &ctx.principal.id, &mutation)
        .await
        .map_err(|error| Failure::from(error).recorrelate(&ctx.correlation_id))?;
    Ok(json(StatusCode::OK, &receipt))
}
