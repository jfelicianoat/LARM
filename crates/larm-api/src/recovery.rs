//! Recuperación y updater de LARM (P133), detrás del guard:
//!
//! - `POST /recovery/rebuild` (`admin`, `ExecutePlan`, `Idempotency-Key`):
//!   202 `JobReceipt` — rebuild del registro desde las ubicaciones de
//!   almacenamiento hacia una base de staging (P018, spec §134);
//! - `POST /updates/larm` (`admin`, `ExecutePlan`, `Idempotency-Key`):
//!   202 `JobReceipt` — updater: guardas de leases/jobs activas (con
//!   confirmación vía `approval_id`), verificación del staged, backup previo
//!   de la base y bundle de config/manifests (spec §132; el reinicio y la
//!   migración los aplica el arranque siguiente).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Router};
use larm_core::security::Approvals;
use larm_domain::approval::ApprovalUse;
use larm_domain::bridge::{is_uuid_v7, ApiError};
use larm_domain::ports::RecoveryOps;

use crate::http::{json, strict, Answer, Failure};
use crate::security::AuthContext;

#[derive(Clone)]
pub struct RecoveryState {
    pub ops: Arc<dyn RecoveryOps>,
    pub approvals: Arc<Approvals>,
}

pub fn routes(state: RecoveryState) -> Router {
    Router::new()
        .route("/api/v1/recovery/rebuild", post(recovery_rebuild))
        .route("/api/v1/updates/larm", post(update_larm))
        .with_state(state)
}

fn fail(ctx: &AuthContext) -> impl Fn(ApiError) -> Failure + '_ {
    move |e| Failure::from(e).recorrelate(&ctx.correlation_id)
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutePlan {
    plan_id: String,
    plan_hash: String,
    approval_id: Option<String>,
}

fn parse_plan(
    body: &Bytes,
    headers: &HeaderMap,
    ctx: &AuthContext,
) -> Result<ExecutePlan, Failure> {
    let request: ExecutePlan =
        strict(body, headers).map_err(|f| f.recorrelate(&ctx.correlation_id))?;
    if !is_uuid_v7(&request.plan_id)
        || !is_sha256(&request.plan_hash)
        || request
            .approval_id
            .as_deref()
            .is_some_and(|a| !is_uuid_v7(a))
    {
        return Err(fail(ctx)(ApiError::new(
            "VALIDATION_FAILED",
            "El cuerpo no cumple el schema ExecutePlan",
            "schema_violation",
        )));
    }
    Ok(request)
}

async fn recovery_rebuild(
    State(state): State<RecoveryState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request = parse_plan(&body, &headers, &ctx)?;
    let (receipt, task) = state
        .ops
        .recovery_rebuild(&request.plan_id, &request.plan_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    tokio::spawn(task);
    Ok(json(StatusCode::ACCEPTED, &receipt))
}

async fn update_larm(
    State(state): State<RecoveryState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer {
    let request = parse_plan(&body, &headers, &ctx)?;
    // Guarda del updater (spec §132): con leases o jobs activas el reinicio
    // no se fuerza sin confirmación; la confirmación es el `approval_id`.
    state
        .ops
        .update_guard(request.approval_id.is_some())
        .await
        .map_err(fail(&ctx))?;
    if let Some(approval_id) = request.approval_id.as_deref() {
        state
            .approvals
            .consume(
                approval_id,
                &ApprovalUse {
                    actor_id: &ctx.principal.id,
                    operation_digest: &request.plan_hash,
                    resource_ids: &["larm".to_string()],
                    code_hash: None,
                },
            )
            .await
            .map_err(fail(&ctx))?;
    }
    let (receipt, task) = state
        .ops
        .update_larm(&request.plan_id, &request.plan_hash, &ctx.mutation())
        .await
        .map_err(fail(&ctx))?;
    tokio::spawn(task);
    Ok(json(StatusCode::ACCEPTED, &receipt))
}
