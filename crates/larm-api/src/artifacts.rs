//! Ubicaciones de artefactos y movimiento de modelos (P016), detrás del guard:
//!
//! - `POST /models/{id}/move` (`storage:write`, `Idempotency-Key`): `{id}` es
//!   el UUID de la **variante**. Con `dry_run` devuelve el plan (201) y lo
//!   guarda `PLANNED`; sin él, ejecuta ese plan por su `plan_hash`: el job
//!   arranca en segundo plano y se sigue en `GET /jobs/{plan_id}`.
//! - `GET /artifact-locations` y `GET /artifact-locations/{id}` (`read`).
//!
//! Si llega `approval_id`, se valida y consume ligada al `plan_hash` y a los
//! recursos del plan aunque el plan no la exija: nunca se ignora en silencio.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use larm_core::security::{Approvals, CursorSigner};
use larm_domain::approval::ApprovalUse;
use larm_domain::artifacts::{ArtifactLocation, MoveRequest};
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::ports::{ArtifactLocationRepository, IdempotencyStore, ModelMover};

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct ArtifactsState {
    pub locations: Arc<dyn ArtifactLocationRepository>,
    pub mover: Arc<dyn ModelMover>,
    pub approvals: Arc<Approvals>,
    pub cursors: Arc<CursorSigner>,
    pub idempotency: Arc<dyn IdempotencyStore>,
}

pub fn routes(state: ArtifactsState) -> Router {
    Router::new()
        .route("/api/v1/models/{id}/move", post(move_model))
        .route("/api/v1/artifact-locations", get(list))
        .route("/api/v1/artifact-locations/{id}", get(read))
        .with_state(state)
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

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Patrones del schema `Move` que serde no comprueba.
fn check_move(request: &MoveRequest) -> Result<(), ApiError> {
    let bad = |field: &str| {
        ApiError::new(
            "VALIDATION_FAILED",
            format!("{field} no cumple el schema Move"),
            "schema_violation",
        )
    };
    if !is_uuid_v7(&request.target_storage_location_id) {
        return Err(bad("target_storage_location_id"));
    }
    if request.plan_hash.as_deref().is_some_and(|h| !is_sha256(h)) {
        return Err(bad("plan_hash"));
    }
    if request
        .approval_id
        .as_deref()
        .is_some_and(|a| !is_uuid_v7(a))
    {
        return Err(bad("approval_id"));
    }
    Ok(())
}

async fn move_model(
    State(state): State<ArtifactsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    id_param(&id, &ctx)?;
    let request: MoveRequest =
        strict(&body, &headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    check_move(&request).map_err(fail(&ctx))?;
    let (mover, approvals, mutation) =
        (state.mover.clone(), state.approvals.clone(), ctx.mutation());
    crate::security::idempotent(
        state.idempotency.as_ref(),
        &ctx,
        &headers,
        &body,
        || async move {
            if request.dry_run {
                let plan = mover
                    .plan(&id, &request.target_storage_location_id, &mutation)
                    .await?;
                return Ok((201, serde_json::to_value(plan).unwrap_or_default()));
            }
            if let (Some(approval_id), Some(hash)) =
                (request.approval_id.as_deref(), request.plan_hash.as_deref())
            {
                let plan = mover.planned(hash).await?.ok_or_else(|| {
                    ApiError::new(
                        "STALE_REVISION",
                        "Plan desconocido, ya usado o cancelado: vuelve a planificar",
                        "plan_unknown",
                    )
                })?;
                approvals
                    .consume(
                        approval_id,
                        &ApprovalUse {
                            actor_id: &mutation.actor_id,
                            operation_digest: hash,
                            resource_ids: &plan.resource_ids,
                            code_hash: None,
                        },
                    )
                    .await?;
            }
            let (plan, task) = mover.execute(&id, &request, &mutation).await?;
            tokio::spawn(task);
            Ok((201, serde_json::to_value(plan).unwrap_or_default()))
        },
    )
    .await
}

async fn list(
    State(state): State<ArtifactsState>,
    Extension(ctx): Extension<AuthContext>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Answer {
    let (cursor, limit) = crate::http::list_params(&params, &[]).map_err(fail(&ctx))?;
    let page = state
        .cursors
        .page(
            "artifact-locations",
            cursor.as_deref(),
            limit,
            |l: &ArtifactLocation| (l.created_at.clone(), l.id.clone()),
            |after, n| state.locations.list(after, n),
        )
        .await
        .map_err(fail(&ctx))?;
    Ok(json(StatusCode::OK, &page))
}

async fn read(
    State(state): State<ArtifactsState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Answer {
    id_param(&id, &ctx)?;
    let location = state
        .locations
        .get(&id)
        .await
        .map_err(fail(&ctx))?
        .ok_or_else(|| {
            fail(&ctx)(
                ApiError::new(
                    "NOT_FOUND",
                    "Ubicación de artefacto desconocida",
                    "not_found",
                )
                .with_resource(id.clone()),
            )
        })?;
    Ok(json(StatusCode::OK, &location))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn patrones_del_schema_move() {
        let ok = MoveRequest {
            target_storage_location_id: "0192f000-0000-7000-8000-000000000001".into(),
            plan_hash: Some("a".repeat(64)),
            dry_run: false,
            approval_id: None,
        };
        assert!(check_move(&ok).is_ok());
        let mut bad = ok.clone();
        bad.plan_hash = Some("A".repeat(64));
        assert_eq!(
            check_move(&bad).unwrap_err().details.reason,
            "schema_violation"
        );
        let mut bad = ok.clone();
        bad.target_storage_location_id = "x".into();
        assert!(check_move(&bad).is_err());
        let mut bad = ok;
        bad.approval_id = Some("no-uuid".into());
        assert!(check_move(&bad).is_err());
    }
}
