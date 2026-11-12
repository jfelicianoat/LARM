//! Libro durable de leases P100. Toda transición se escribe junto con su
//! evento de dominio y entrada de outbox; los secretos permanecen fuera de SQLite.

use async_trait::async_trait;
use larm_domain::bridge::{is_uuid_v7, timestamp, JobPhase, JobReceipt, LeaseState, RuntimeLease};
use larm_domain::error::{ApiError, ErrorCode};
use larm_domain::ports::{
    CreateLeaseOutcome, LeaseCreatePersistence, LeaseRenewal, LeaseStateUpdate, MutationContext,
    RuntimeLeaseRepository,
};
use serde_json::{Map, Value};
use sqlx::{SqlitePool, Transaction};

use super::pool::db_error;

const CREATE_OPERATION: &str = "P100_post__leases";
const RENEW_OPERATION: &str = "P100_post__leases__renew";
const IDEMPOTENCY_DAYS: i64 = 30;

type LeaseRow = (
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
);

/// Repositorio SQLite de leases.
#[derive(Clone)]
pub struct SqlRuntimeLeases {
    pub pool: SqlitePool,
}

impl SqlRuntimeLeases {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn lease_from_row(row: LeaseRow) -> Result<RuntimeLease, ApiError> {
    let state: LeaseState = serde_json::from_value(Value::String(row.9))
        .map_err(|error| db_error("estado de runtime_lease", &error))?;
    let metadata: Map<String, Value> = serde_json::from_str(&row.4)
        .map_err(|error| db_error("metadata de runtime_lease", &error))?;
    let constraints = serde_json::from_str(&row.12)
        .map_err(|error| db_error("constraints de runtime_lease", &error))?;
    Ok(RuntimeLease {
        id: row.0,
        created_at: row.1,
        updated_at: row.2,
        row_version: u64::try_from(row.3)
            .map_err(|error| db_error("row_version de runtime_lease", &error))?,
        metadata,
        owner_id: row.5,
        request_id: row.6,
        instance_id: row.7,
        generation: u64::try_from(row.8)
            .map_err(|error| db_error("generation de runtime_lease", &error))?,
        state,
        expires_at: row.10,
        endpoint_id: row.11,
        constraints,
    })
}

const SELECT_LEASE: &str =
    "SELECT id,created_at,updated_at,row_version,metadata,owner_id,request_id,\
    instance_id,generation,state,expires_at,endpoint_id,constraints FROM runtime_lease";

fn lease_state_name(state: LeaseState) -> &'static str {
    match state {
        LeaseState::Requested => "REQUESTED",
        LeaseState::Admitting => "ADMITTING",
        LeaseState::Active => "ACTIVE",
        LeaseState::Expiring => "EXPIRING",
        LeaseState::Expired => "EXPIRED",
        LeaseState::Released => "RELEASED",
        LeaseState::Failed => "FAILED",
    }
}

fn state_event(state: LeaseState) -> &'static str {
    match state {
        LeaseState::Requested => "runtime_lease.requested",
        LeaseState::Admitting => "runtime_lease.admitting",
        LeaseState::Active => "runtime_lease.active",
        LeaseState::Expiring => "runtime_lease.expiring",
        LeaseState::Expired => "runtime_lease.expired",
        LeaseState::Released => "runtime_lease.released",
        LeaseState::Failed => "runtime_lease.failed",
    }
}

fn idempotency_conflict(resource: &str) -> ApiError {
    ApiError::new(
        ErrorCode::IdempotencyConflict,
        "La solicitud de lease ya se registró con otros datos",
        "idempotency_conflict",
    )
    .with_resource(resource)
}

fn secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "token"
            | "accesstoken"
            | "refreshtoken"
            | "bearertoken"
            | "secret"
            | "clientsecret"
            | "password"
            | "authorization"
            | "apikey"
    )
}

fn contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(object) => object
            .iter()
            .any(|(key, value)| secret_key(key) || contains_secret(value)),
        Value::Array(items) => items.iter().any(contains_secret),
        _ => false,
    }
}

fn validate_transition(from: LeaseState, to: LeaseState) -> Result<(), ApiError> {
    let allowed = matches!(
        (from, to),
        (LeaseState::Requested, LeaseState::Admitting)
            | (LeaseState::Requested, LeaseState::Failed)
            | (LeaseState::Admitting, LeaseState::Active)
            | (LeaseState::Admitting, LeaseState::Failed)
            | (LeaseState::Active, LeaseState::Expiring)
            | (LeaseState::Active, LeaseState::Released)
            | (LeaseState::Expiring, LeaseState::Expired)
    );
    if allowed {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::InvalidStateTransition,
            "La lease no admite esa transición de estado",
            "lease_state_conflict",
        ))
    }
}

async fn validate_activation_references(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    instance_id: &str,
    endpoint_id: &str,
    generation: i64,
    expires_at: &str,
    lease_id: &str,
) -> Result<(), ApiError> {
    let expires = chrono::DateTime::parse_from_rfc3339(expires_at).map_err(|_| {
        ApiError::new(
            ErrorCode::ValidationFailed,
            "El vencimiento de la lease no es válido",
            "lease_activation_expiry_invalid",
        )
        .with_resource(lease_id)
    })?;
    if expires <= chrono::Utc::now() {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "El vencimiento de la lease debe estar en el futuro",
            "lease_activation_expiry_invalid",
        )
        .with_resource(lease_id));
    }

    let instance: Option<(String, i64)> =
        sqlx::query_as("SELECT state,generation FROM runtime_instance WHERE id=?")
            .bind(instance_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| db_error("verificando instancia para activar lease", &error))?;
    let Some((state, instance_generation)) = instance else {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "La instancia de la lease no existe",
            "lease_activation_instance_missing",
        )
        .with_resource(lease_id));
    };
    if !matches!(state.as_str(), "READY" | "BUSY") {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "La instancia de la lease no está lista para atender solicitudes",
            "lease_activation_instance_not_ready",
        )
        .with_resource(lease_id));
    }
    if instance_generation != generation {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "La generación de la instancia cambió durante la admisión",
            "lease_activation_generation_mismatch",
        )
        .with_resource(lease_id));
    }

    let endpoint: Option<(String, String)> =
        sqlx::query_as("SELECT instance_id,qualification FROM inference_endpoint WHERE id=?")
            .bind(endpoint_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| db_error("verificando endpoint para activar lease", &error))?;
    let Some((endpoint_instance_id, qualification)) = endpoint else {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "El endpoint de la lease no existe",
            "lease_activation_endpoint_missing",
        )
        .with_resource(lease_id));
    };
    if endpoint_instance_id != instance_id {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "El endpoint no pertenece a la instancia de la lease",
            "lease_activation_endpoint_instance_mismatch",
        )
        .with_resource(lease_id));
    }
    if qualification != "QUALIFIED" {
        return Err(ApiError::new(
            ErrorCode::ValidationFailed,
            "El endpoint de la lease no está cualificado",
            "lease_activation_endpoint_unqualified",
        )
        .with_resource(lease_id));
    }
    Ok(())
}

async fn append_state_event(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    lease: &RuntimeLease,
    ctx: &MutationContext,
) -> Result<(), ApiError> {
    append_lease_event(tx, lease, state_event(lease.state), ctx).await
}

async fn append_lease_event(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    lease: &RuntimeLease,
    event_type: &str,
    ctx: &MutationContext,
) -> Result<(), ApiError> {
    super::events::append(
        &mut *tx,
        &super::events::NewEvent {
            event_type,
            resource_type: "runtime_lease",
            resource_id: &lease.id,
            resource_version: i64::try_from(lease.row_version)
                .map_err(|error| db_error("versión de evento de lease", &error))?,
            data: serde_json::json!({
                "state": lease_state_name(lease.state),
                "generation": lease.generation,
                "expires_at": lease.expires_at,
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

#[async_trait]
impl RuntimeLeaseRepository for SqlRuntimeLeases {
    async fn create_requested(
        &self,
        lease: &RuntimeLease,
        body_hash: &str,
        idempotency_key: &str,
        initial_response: &CreateLeaseOutcome,
        ctx: &MutationContext,
    ) -> Result<LeaseCreatePersistence, ApiError> {
        let job_receipt = if initial_response.status == 202 {
            let receipt = serde_json::from_value::<JobReceipt>(initial_response.body.clone())
                .map_err(|error| db_error("JobReceipt inicial de lease", &error))?;
            let expected_url = format!("/api/v1/jobs/{}", receipt.job_id);
            let linked_job_id = lease.metadata.get("job_id").and_then(Value::as_str);
            if receipt.state != JobPhase::Queued
                || !is_uuid_v7(&receipt.job_id)
                || receipt.status_url != expected_url
                || linked_job_id != Some(receipt.job_id.as_str())
            {
                return Err(ApiError::new(
                    ErrorCode::ValidationFailed,
                    "El job inicial de lease no coincide con la solicitud durable",
                    "lease_job_invalid",
                ));
            }
            Some(receipt)
        } else {
            None
        };
        if lease.state != LeaseState::Requested
            || lease.row_version != 1
            || !is_uuid_v7(&lease.id)
            || lease.generation == 0
            || lease.instance_id.is_some()
            || lease.endpoint_id.is_some()
            || lease.expires_at.is_some()
            || lease.owner_id != ctx.actor_id
            || idempotency_key.is_empty()
            || idempotency_key.len() > 240
            || body_hash.len() != 64
            || !body_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !matches!(initial_response.status, 201 | 202)
            || contains_secret(&initial_response.body)
        {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "La solicitud de persistencia de lease no es válida",
                "lease_create_invalid",
            ));
        }

        let generation = i64::try_from(lease.generation)
            .map_err(|error| db_error("generation de lease", &error))?;
        let mut metadata = lease.metadata.clone();
        metadata.insert(
            "request_hash".into(),
            Value::String(body_hash.to_ascii_lowercase()),
        );
        let metadata = Value::Object(metadata).to_string();
        let constraints = serde_json::to_string(&lease.constraints)
            .map_err(|error| db_error("constraints de lease", &error))?;
        let state = lease_state_name(lease.state);
        let now = timestamp(chrono::Utc::now());
        let operation = CREATE_OPERATION;
        let replay_json = initial_response.body.to_string();
        let replay_expires =
            timestamp(chrono::Utc::now() + chrono::Duration::days(IDEMPOTENCY_DAYS));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN create runtime_lease", &error))?;
        // Toma el lock de escritura antes de consultar: dos retries simultáneos
        // no pueden crear leases distintos para el mismo Idempotency-Key.
        sqlx::query("UPDATE idempotency SET expires_at=expires_at WHERE principal_id=? AND operation=? AND key=?")
            .bind(&ctx.actor_id)
            .bind(operation)
            .bind(idempotency_key)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando Idempotency-Key de lease", &error))?;

        let replay: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash,status_code,response_json FROM idempotency \
             WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(&ctx.actor_id)
        .bind(operation)
        .bind(idempotency_key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("buscando replay de lease", &error))?;
        if let Some((stored_hash, status, body)) = replay {
            if stored_hash != body_hash.to_ascii_lowercase() {
                return Err(idempotency_conflict(idempotency_key));
            }
            let body: Value = serde_json::from_str(&body)
                .map_err(|error| db_error("decodificando replay de lease", &error))?;
            tx.commit()
                .await
                .map_err(|error| db_error("COMMIT replay de lease", &error))?;
            return Ok(LeaseCreatePersistence::Replayed(CreateLeaseOutcome {
                status: u16::try_from(status)
                    .map_err(|error| db_error("status de replay de lease", &error))?,
                body,
            }));
        }

        sqlx::query(
            "DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?",
        )
        .bind(&ctx.actor_id)
        .bind(operation)
        .bind(idempotency_key)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("limpiando replay caducado de lease", &error))?;

        let existing: Option<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{SELECT_LEASE} WHERE owner_id=? AND request_id=?"
        )))
        .bind(&lease.owner_id)
        .bind(&lease.request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("buscando request_id de lease", &error))?;
        if let Some(row) = existing {
            let existing = lease_from_row(row)?;
            let same_request = existing
                .metadata
                .get("request_hash")
                .and_then(Value::as_str)
                .is_some_and(|stored| stored.eq_ignore_ascii_case(body_hash));
            if !same_request {
                return Err(idempotency_conflict(&lease.request_id));
            }
            tx.commit()
                .await
                .map_err(|error| db_error("COMMIT request_id repetido", &error))?;
            return Ok(LeaseCreatePersistence::Existing(existing));
        }

        sqlx::query(
            "INSERT INTO runtime_lease(id,created_at,updated_at,row_version,metadata,owner_id,request_id,\
             instance_id,generation,state,expires_at,endpoint_id,constraints) \
             VALUES(?,?,?,?,?,?,?,NULL,?,?,NULL,NULL,?)",
        )
        .bind(&lease.id)
        .bind(&lease.created_at)
        .bind(&lease.updated_at)
        .bind(i64::try_from(lease.row_version).map_err(|error| db_error("row_version de lease", &error))?)
        .bind(&metadata)
        .bind(&lease.owner_id)
        .bind(&lease.request_id)
        .bind(generation)
        .bind(state)
        .bind(&constraints)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("insertando runtime_lease", &error))?;

        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES(?,?,?,?,?,?,?)",
        )
        .bind(&ctx.actor_id)
        .bind(operation)
        .bind(idempotency_key)
        .bind(body_hash.to_ascii_lowercase())
        .bind(i64::from(initial_response.status))
        .bind(replay_json)
        .bind(replay_expires)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("guardando replay de lease", &error))?;

        let mut stored = lease.clone();
        stored.metadata = serde_json::from_str(&metadata)
            .map_err(|error| db_error("metadata de lease", &error))?;
        append_state_event(&mut tx, &stored, ctx).await?;
        if let Some(receipt) = job_receipt {
            let job_payload = serde_json::json!({
                "actor_id": ctx.actor_id,
                "correlation_id": ctx.correlation_id,
                "resource_id": lease.id,
                "result": null,
                "error": null,
                "evidence_refs": [],
            });
            sqlx::query(
                "INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) \
                 VALUES(?,?,?,'QUEUED',?,?,?)",
            )
            .bind(&receipt.job_id)
            .bind(larm_domain::ports::BROKER_LEASE_ADMISSION_JOB_KIND)
            .bind(serde_json::json!([lease.id]).to_string())
            .bind(larm_domain::ports::BROKER_LEASE_ADMISSION_JOB_KIND)
            .bind(job_payload.to_string())
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("encolando admisión durable de lease", &error))?;
        }
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT create runtime_lease", &error))?;
        Ok(LeaseCreatePersistence::Created(stored))
    }

    async fn get_for_owner(
        &self,
        lease_id: &str,
        owner_id: &str,
    ) -> Result<Option<RuntimeLease>, ApiError> {
        let query = format!("{SELECT_LEASE} WHERE id=? AND owner_id=?");
        let row: Option<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(query))
            .bind(lease_id)
            .bind(owner_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| db_error("leyendo runtime_lease", &error))?;
        row.map(lease_from_row).transpose()
    }

    async fn list_for_owner(
        &self,
        owner_id: &str,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<RuntimeLease>, ApiError> {
        if limit == 0 || limit > 200 {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "El límite de leases debe estar entre 1 y 200",
                "lease_page_limit_invalid",
            ));
        }
        let query = match after {
            Some(_) => format!(
                "{SELECT_LEASE} WHERE owner_id=? AND (created_at,id)>(?,?) ORDER BY created_at,id LIMIT ?"
            ),
            None => format!("{SELECT_LEASE} WHERE owner_id=? ORDER BY created_at,id LIMIT ?"),
        };
        let mut builder = sqlx::query_as::<_, LeaseRow>(sqlx::AssertSqlSafe(query)).bind(owner_id);
        if let Some((created_at, id)) = after {
            builder = builder.bind(created_at).bind(id);
        }
        let rows = builder
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| db_error("listando runtime_lease", &error))?;
        rows.into_iter().map(lease_from_row).collect()
    }

    async fn transition(
        &self,
        lease_id: &str,
        owner_id: &str,
        if_match: u64,
        generation: u64,
        update: &LeaseStateUpdate,
        ctx: &MutationContext,
    ) -> Result<RuntimeLease, ApiError> {
        validate_transition(update.from, update.to)
            .map_err(|error| error.with_resource(lease_id))?;
        if update.to == LeaseState::Active
            && (update.instance_id.is_none()
                || update.endpoint_id.is_none()
                || update.expires_at.as_deref().is_none_or(str::is_empty))
        {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "Una lease ACTIVE necesita instancia, endpoint y vencimiento verificados",
                "lease_activation_incomplete",
            )
            .with_resource(lease_id));
        }
        let expected_version =
            i64::try_from(if_match).map_err(|error| db_error("If-Match de lease", &error))?;
        let expected_generation =
            i64::try_from(generation).map_err(|error| db_error("generation de lease", &error))?;
        let now = timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN transición de lease", &error))?;
        if update.to == LeaseState::Active {
            let locked = sqlx::query(
                "UPDATE runtime_lease SET state=state WHERE id=? AND owner_id=? AND row_version=? \
                 AND generation=? AND state=?",
            )
            .bind(lease_id)
            .bind(owner_id)
            .bind(expected_version)
            .bind(expected_generation)
            .bind(lease_state_name(update.from))
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("bloqueando activación de lease", &error))?;
            if locked.rows_affected() == 1 {
                match (
                    update.instance_id.as_deref(),
                    update.endpoint_id.as_deref(),
                    update.expires_at.as_deref(),
                ) {
                    (Some(instance_id), Some(endpoint_id), Some(expires_at)) => {
                        validate_activation_references(
                            &mut tx,
                            instance_id,
                            endpoint_id,
                            expected_generation,
                            expires_at,
                            lease_id,
                        )
                        .await?;
                    }
                    _ => {
                        return Err(ApiError::new(
                            ErrorCode::ValidationFailed,
                            "Una lease ACTIVE necesita instancia, endpoint y vencimiento verificados",
                            "lease_activation_incomplete",
                        )
                        .with_resource(lease_id));
                    }
                }
            }
        }
        let sql = "UPDATE runtime_lease SET state=?,updated_at=?,row_version=row_version+1,\
             instance_id=COALESCE(?,instance_id),endpoint_id=COALESCE(?,endpoint_id),\
             expires_at=COALESCE(?,expires_at) \
             WHERE id=? AND owner_id=? AND row_version=? AND generation=? AND state=? \
             RETURNING id,created_at,updated_at,row_version,metadata,owner_id,request_id,\
             instance_id,generation,state,expires_at,endpoint_id,constraints"
            .to_string();
        let row: Option<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(lease_state_name(update.to))
            .bind(&now)
            .bind(&update.instance_id)
            .bind(&update.endpoint_id)
            .bind(&update.expires_at)
            .bind(lease_id)
            .bind(owner_id)
            .bind(expected_version)
            .bind(expected_generation)
            .bind(lease_state_name(update.from))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("transición de runtime_lease", &error))?;
        let Some(row) = row else {
            let current: Option<(String, i64, i64, String)> = sqlx::query_as(
                "SELECT owner_id,row_version,generation,state FROM runtime_lease WHERE id=?",
            )
            .bind(lease_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("diagnosticando transición de lease", &error))?;
            let Some((owner, version, current_generation, state)) = current else {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    "No existe esa lease",
                    "lease_not_found",
                )
                .with_resource(lease_id));
            };
            if owner != owner_id {
                return Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "La lease pertenece a otro propietario",
                    "principal_not_allowed",
                )
                .with_resource(lease_id));
            }
            if version != expected_version || current_generation != expected_generation {
                return Err(ApiError::new(
                    ErrorCode::StaleRevision,
                    "La lease cambió desde la última lectura",
                    "stale_revision",
                )
                .with_resource(lease_id));
            }
            if state != lease_state_name(update.from) {
                return Err(ApiError::new(
                    ErrorCode::InvalidStateTransition,
                    "La lease ya no está en el estado esperado",
                    "lease_state_conflict",
                )
                .with_resource(lease_id));
            }
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "No se pudo cambiar el estado de la lease",
                "lease_state_conflict",
            )
            .with_resource(lease_id));
        };
        let stored = lease_from_row(row)?;
        append_state_event(&mut tx, &stored, ctx).await?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT transición de lease", &error))?;
        Ok(stored)
    }

    async fn renew_active(
        &self,
        lease_id: &str,
        renewal: &LeaseRenewal,
        ctx: &MutationContext,
    ) -> Result<RuntimeLease, ApiError> {
        let owner_id = renewal.owner_id.as_str();
        let if_match = renewal.if_match;
        let generation = renewal.generation;
        let idempotency_key = renewal.idempotency_key.as_str();
        let body_hash = renewal.body_hash.as_str();
        let expires_at = renewal.expires_at.as_str();
        if idempotency_key.is_empty()
            || idempotency_key.len() > 240
            || body_hash.len() != 64
            || !body_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "La clave o huella de idempotencia de renovación no es válida",
                "lease_renew_idempotency_invalid",
            )
            .with_resource(lease_id));
        }
        let expires = chrono::DateTime::parse_from_rfc3339(expires_at).map_err(|_| {
            ApiError::new(
                ErrorCode::ValidationFailed,
                "El vencimiento de la lease no es válido",
                "lease_renew_expiry_invalid",
            )
            .with_resource(lease_id)
        })?;
        let now_dt = chrono::Utc::now();
        if expires <= now_dt {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "El vencimiento de la lease debe estar en el futuro",
                "lease_renew_expiry_invalid",
            )
            .with_resource(lease_id));
        }

        let expected_version =
            i64::try_from(if_match).map_err(|error| db_error("If-Match de lease", &error))?;
        let expected_generation =
            i64::try_from(generation).map_err(|error| db_error("generation de lease", &error))?;
        let now = timestamp(now_dt);
        let replay_expires = timestamp(now_dt + chrono::Duration::days(IDEMPOTENCY_DAYS));
        let body_hash = body_hash.to_ascii_lowercase();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN renovación de lease", &error))?;

        sqlx::query(
            "UPDATE idempotency SET expires_at=expires_at WHERE principal_id=? AND operation=? AND key=?",
        )
        .bind(owner_id)
        .bind(RENEW_OPERATION)
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("bloqueando Idempotency-Key de renovación", &error))?;

        let replay: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT body_hash,status_code,response_json FROM idempotency \
             WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
        )
        .bind(owner_id)
        .bind(RENEW_OPERATION)
        .bind(idempotency_key)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("buscando replay de renovación", &error))?;
        if let Some((stored_hash, _status, body)) = replay {
            if stored_hash != body_hash {
                return Err(idempotency_conflict(idempotency_key));
            }
            let stored: RuntimeLease = serde_json::from_str(&body)
                .map_err(|error| db_error("decodificando replay de renovación", &error))?;
            tx.commit()
                .await
                .map_err(|error| db_error("COMMIT replay de renovación", &error))?;
            return Ok(stored);
        }

        sqlx::query(
            "DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?",
        )
        .bind(owner_id)
        .bind(RENEW_OPERATION)
        .bind(idempotency_key)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("limpiando replay caducado de renovación", &error))?;

        let row: Option<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(
            "UPDATE runtime_lease SET expires_at=?,updated_at=?,row_version=row_version+1 \
             WHERE id=? AND owner_id=? AND row_version=? AND generation=? AND state='ACTIVE' AND expires_at>? \
             RETURNING id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,generation,state,expires_at,endpoint_id,constraints"
                .to_string(),
        ))
        .bind(expires_at)
        .bind(&now)
        .bind(lease_id)
        .bind(owner_id)
        .bind(expected_version)
        .bind(expected_generation)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| db_error("renovando runtime_lease", &error))?;
        let Some(row) = row else {
            let current: Option<(String, i64, i64, String, Option<String>)> = sqlx::query_as(
                "SELECT owner_id,row_version,generation,state,expires_at FROM runtime_lease WHERE id=?",
            )
            .bind(lease_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("diagnosticando renovación de lease", &error))?;
            let Some((owner, version, current_generation, state, current_expiry)) = current else {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    "No existe esa lease",
                    "lease_not_found",
                )
                .with_resource(lease_id));
            };
            if owner != owner_id {
                return Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "La lease pertenece a otro propietario",
                    "principal_not_allowed",
                )
                .with_resource(lease_id));
            }
            if version != expected_version || current_generation != expected_generation {
                return Err(ApiError::new(
                    ErrorCode::StaleRevision,
                    "La lease cambió desde la última lectura",
                    "stale_revision",
                )
                .with_resource(lease_id));
            }
            let is_expired = current_expiry
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|value| value <= now_dt);
            if state != "ACTIVE" || is_expired {
                return Err(ApiError::new(
                    ErrorCode::InvalidStateTransition,
                    "La lease no está ACTIVE o ya venció",
                    "lease_not_active",
                )
                .with_resource(lease_id));
            }
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "No se pudo renovar la lease",
                "lease_state_conflict",
            )
            .with_resource(lease_id));
        };

        let stored = lease_from_row(row)?;
        append_lease_event(&mut tx, &stored, "runtime_lease.renewed", ctx).await?;
        let response_json = serde_json::to_string(&stored)
            .map_err(|error| db_error("serializando respuesta de renovación", &error))?;
        sqlx::query(
            "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
             VALUES(?,?,?,?,200,?,?)",
        )
        .bind(owner_id)
        .bind(RENEW_OPERATION)
        .bind(idempotency_key)
        .bind(body_hash)
        .bind(response_json)
        .bind(replay_expires)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("guardando replay de renovación", &error))?;
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT renovación de lease", &error))?;
        Ok(stored)
    }

    async fn expire_due(
        &self,
        grace_seconds: u64,
        ctx: &MutationContext,
    ) -> Result<Vec<RuntimeLease>, ApiError> {
        let grace_seconds = i64::try_from(grace_seconds)
            .map_err(|error| db_error("gracia de expiración de lease", &error))?;
        if grace_seconds > 3600 {
            return Err(ApiError::new(
                ErrorCode::ValidationFailed,
                "La gracia de expiración no puede superar una hora",
                "lease_expiry_grace_invalid",
            ));
        }
        let now_dt = chrono::Utc::now();
        let now = timestamp(now_dt);
        let grace_cutoff = timestamp(now_dt - chrono::Duration::seconds(grace_seconds));
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN expiración de leases", &error))?;
        let rows: Vec<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{SELECT_LEASE} WHERE (state='ACTIVE' AND expires_at IS NOT NULL AND expires_at<=?) \
             OR (state='EXPIRING' AND updated_at<=?) ORDER BY created_at,id"
        )))
        .bind(&now)
        .bind(&grace_cutoff)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| db_error("buscando leases vencidas", &error))?;

        let mut changed = Vec::with_capacity(rows.len());
        for row in rows {
            let mut lease = lease_from_row(row)?;
            let old_state = lease.state;
            lease.state = match old_state {
                LeaseState::Active => LeaseState::Expiring,
                LeaseState::Expiring => LeaseState::Expired,
                _ => continue,
            };
            lease.updated_at = now.clone();
            lease.row_version = lease.row_version.saturating_add(1);
            let row = sqlx::query_as::<_, LeaseRow>(
                "UPDATE runtime_lease SET state=?,updated_at=?,row_version=row_version+1 \
                 WHERE id=? AND state=? AND row_version=? \
                 RETURNING id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,generation,state,expires_at,endpoint_id,constraints",
            )
            .bind(lease_state_name(lease.state))
            .bind(&now)
            .bind(&lease.id)
            .bind(lease_state_name(old_state))
            .bind(i64::try_from(lease.row_version.saturating_sub(1)).map_err(|error| db_error("row_version de lease", &error))?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("actualizando expiración de lease", &error))?;
            let Some(row) = row else {
                continue;
            };
            let stored = lease_from_row(row)?;
            append_state_event(&mut tx, &stored, ctx).await?;
            changed.push(stored);
        }
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT expiración de leases", &error))?;
        Ok(changed)
    }

    async fn expire_unreconciled(
        &self,
        current_generation: u64,
        ctx: &MutationContext,
    ) -> Result<Vec<RuntimeLease>, ApiError> {
        let current_generation = i64::try_from(current_generation)
            .map_err(|error| db_error("generation actual de lease", &error))?;
        let now = timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("BEGIN recuperación de leases", &error))?;
        // Devuelve también leases terminales de generaciones anteriores para
        // que el daemon pueda reintentar la revocación del secreto después de
        // un fallo del Credential Manager en un arranque previo.
        let query = format!("{SELECT_LEASE} WHERE generation<>? ORDER BY created_at,id");
        let rows: Vec<LeaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(query))
            .bind(current_generation)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| db_error("buscando leases sin reconciliar", &error))?;
        let mut expired = Vec::with_capacity(rows.len());
        for row in rows {
            let mut lease = lease_from_row(row)?;
            if lease.state.is_terminal() {
                expired.push(lease);
                continue;
            }
            let old_state = lease.state;
            lease.state = LeaseState::Expired;
            lease.row_version = lease.row_version.saturating_add(1);
            lease.updated_at = now.clone();
            let updated = sqlx::query(
                "UPDATE runtime_lease SET state='EXPIRED',updated_at=?,row_version=row_version+1 \
                 WHERE id=? AND generation=? AND row_version=? AND state=?",
            )
            .bind(&now)
            .bind(&lease.id)
            .bind(
                i64::try_from(lease.generation)
                    .map_err(|error| db_error("generation persistida", &error))?,
            )
            .bind(
                i64::try_from(lease.row_version.saturating_sub(1))
                    .map_err(|error| db_error("row_version persistido", &error))?,
            )
            .bind(lease_state_name(old_state))
            .execute(&mut *tx)
            .await
            .map_err(|error| db_error("expirando lease no reconciliada", &error))?;
            if updated.rows_affected() == 1 {
                append_state_event(&mut tx, &lease, ctx).await?;
                expired.push(lease);
            }
        }
        tx.commit()
            .await
            .map_err(|error| db_error("COMMIT recuperación de leases", &error))?;
        Ok(expired)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use larm_domain::bridge::Constraints;

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    fn context() -> MutationContext {
        MutationContext {
            actor_id: "broker-test".into(),
            correlation_id: "lease-test-correlation".into(),
        }
    }

    fn lease(owner: &str, generation: u64) -> RuntimeLease {
        let now = timestamp(chrono::Utc::now());
        let id = uuid::Uuid::now_v7().to_string();
        let mut metadata = Map::new();
        metadata.insert("job_id".into(), Value::String(id.clone()));
        RuntimeLease {
            id,
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            owner_id: owner.into(),
            request_id: uuid::Uuid::now_v7().to_string(),
            instance_id: None,
            generation,
            state: LeaseState::Requested,
            expires_at: None,
            endpoint_id: None,
            constraints: Constraints {
                runtime_version_id: None,
                backend_id: None,
                execution_fingerprint: None,
                allow_backend_fallback: false,
                max_memory_bytes: None,
            },
        }
    }

    fn initial_response(id: &str) -> CreateLeaseOutcome {
        CreateLeaseOutcome {
            status: 202,
            body: serde_json::json!({
                "job_id": id,
                "status_url": format!("/api/v1/jobs/{id}"),
                "state": "QUEUED"
            }),
        }
    }

    async fn repo() -> (tempfile::TempDir, SqlRuntimeLeases) {
        let (dir, db) = crate::testing::temp_migrated().await.unwrap();
        (dir, SqlRuntimeLeases::new(db.pool))
    }

    async fn seed_activation_records(repo: &SqlRuntimeLeases) -> (String, String) {
        let pool = &repo.pool;
        for statement in [
            "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-ai','t','t',1,'{}','source-adapter','1.0','source','hash','test','UNTRUSTED','{}','INSTALLED')",
            "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter','t','t',1,'{}','source-ai','HTTP','{}')",
            "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('canonical','t','t',1,'{}','test','{}','[]')",
            "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source','t','t',1,'{}','test','source-adapter','https://example.test',0,0,'NONE','{}')",
            "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','canonical','source','test','model')",
            "INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('revision','t','t',1,'{}','distribution','main','hash','CURRENT')",
            "INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES('variant','t','t',1,'{}','revision','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)",
            "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('runtime-ai','t','t',1,'{}','runtime-adapter','1.0','runtime','hash','test','UNTRUSTED','{}','INSTALLED')",
            "INSERT INTO runtime_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,runtime_family,capabilities) VALUES('runtime-adapter','t','t',1,'{}','runtime-ai','test','{}')",
            "INSERT INTO runtime(id,created_at,updated_at,row_version,metadata,channel_policy,name,adapter_id,capabilities,default_version_id) VALUES('runtime','t','t',1,'{}','{}','test','runtime-adapter','{}',NULL)",
            "INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES('runtime-version','t','t',1,'{}','runtime','1.0','stable','NONE')",
            "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,state,ownership) VALUES('MANAGED','VALIDATED','installation','t','t',1,'{}','runtime-version','CUSTOM','VALIDATED','LARM_MANAGED')",
            "INSERT INTO runtime_backend(limits,id,created_at,updated_at,row_version,metadata,runtime_installation_id,backend_type,status,evidence_refs) VALUES('{}','backend','t','t',1,'{}','installation','test','AVAILABLE','[]')",
            "INSERT INTO launch_profile(id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id) VALUES('profile','t','t',1,'{}','BALANCED','{}','hash','backend')",
        ] {
            sqlx::query(statement).execute(pool).await.unwrap();
        }
        let first = id(50);
        let second = id(51);
        for (instance_id, generation) in [(&first, 4_i64), (&second, 4_i64)] {
            sqlx::query(
                "INSERT INTO runtime_instance(id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) \
                 VALUES(?, 't','t',1,'{}','installation','variant','backend','profile','READY',1234,'t','hash',?,'fingerprint',NULL,'LARM_MANAGED')",
            )
            .bind(instance_id)
            .bind(generation)
            .execute(pool)
            .await
            .unwrap();
        }
        (first, second)
    }

    async fn insert_endpoint(
        repo: &SqlRuntimeLeases,
        endpoint_id: &str,
        instance_id: &str,
        qualification: &str,
    ) {
        let evidence = if qualification == "QUALIFIED" {
            "[\"test:evidence\"]"
        } else {
            "[]"
        };
        sqlx::query(
            "INSERT INTO inference_endpoint(id,created_at,updated_at,row_version,metadata,instance_id,protocol,protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs) \
             VALUES(?, 't','t',1,'{}',?,'openai-compatible',?,'http://127.0.0.1:1234',NULL,'[]',?,?)",
        )
        .bind(endpoint_id)
        .bind(instance_id)
        .bind("a".repeat(64))
        .bind(qualification)
        .bind(evidence)
        .execute(&repo.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn solicitud_replay_y_conflicto_son_atomicos_con_el_outbox() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let lease = lease(&ctx.actor_id, 4);
        let response = initial_response(&lease.id);
        let created = repo
            .create_requested(&lease, &"a".repeat(64), "same-key", &response, &ctx)
            .await
            .unwrap();
        assert!(matches!(created, LeaseCreatePersistence::Created(_)));

        let replay = repo
            .create_requested(&lease, &"a".repeat(64), "same-key", &response, &ctx)
            .await
            .unwrap();
        assert_eq!(replay, LeaseCreatePersistence::Replayed(response.clone()));

        let error = repo
            .create_requested(&lease, &"b".repeat(64), "same-key", &response, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IdempotencyConflict);
        let lease_count: i64 = sqlx::query_scalar("SELECT count(*) FROM runtime_lease")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(lease_count, 1);
        assert_eq!(outbox_count, 1);
        let job_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM operation_journal WHERE kind=? AND phase='QUEUED'",
        )
        .bind(larm_domain::ports::BROKER_LEASE_ADMISSION_JOB_KIND)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(job_count, 1);
        let job_repository =
            super::super::jobs::SqlJournalJobs::broker_lease_admission(repo.pool.clone());
        let job = larm_domain::ports::JournalJobRepository::get(&job_repository, &lease.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.actor_id, ctx.actor_id);
        assert_eq!(job.state.state, JobPhase::Queued);
    }

    #[tokio::test]
    async fn renovacion_durable_es_cas_idempotente_y_genera_outbox() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let lease = lease(&ctx.actor_id, 4);
        repo.create_requested(
            &lease,
            &"a".repeat(64),
            "renew-create",
            &initial_response(&lease.id),
            &ctx,
        )
        .await
        .unwrap();
        let admitting = LeaseStateUpdate {
            from: LeaseState::Requested,
            to: LeaseState::Admitting,
            instance_id: None,
            endpoint_id: None,
            expires_at: None,
        };
        let stored = repo
            .transition(&lease.id, &ctx.actor_id, 1, 4, &admitting, &ctx)
            .await
            .unwrap();
        let (instance_id, _) = seed_activation_records(&repo).await;
        let endpoint_id = id(63);
        insert_endpoint(&repo, &endpoint_id, &instance_id, "QUALIFIED").await;
        let active = repo
            .transition(
                &lease.id,
                &ctx.actor_id,
                stored.row_version,
                4,
                &LeaseStateUpdate {
                    from: LeaseState::Admitting,
                    to: LeaseState::Active,
                    instance_id: Some(instance_id.clone()),
                    endpoint_id: Some(endpoint_id.clone()),
                    expires_at: Some(timestamp(chrono::Utc::now() + chrono::Duration::minutes(2))),
                },
                &ctx,
            )
            .await
            .unwrap();
        let expiry = timestamp(chrono::Utc::now() + chrono::Duration::minutes(5));
        let renewed = repo
            .renew_active(
                &lease.id,
                &LeaseRenewal {
                    owner_id: ctx.actor_id.clone(),
                    if_match: active.row_version,
                    generation: active.generation,
                    idempotency_key: "renew-once".into(),
                    body_hash: "b".repeat(64),
                    expires_at: expiry.clone(),
                },
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(renewed.state, LeaseState::Active);
        assert_eq!(renewed.row_version, active.row_version + 1);
        assert_eq!(renewed.expires_at.as_deref(), Some(expiry.as_str()));

        let replay = repo
            .renew_active(
                &lease.id,
                &LeaseRenewal {
                    owner_id: ctx.actor_id.clone(),
                    if_match: active.row_version,
                    generation: active.generation,
                    idempotency_key: "renew-once".into(),
                    body_hash: "b".repeat(64),
                    expires_at: timestamp(chrono::Utc::now() + chrono::Duration::minutes(9)),
                },
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(replay, renewed);

        let conflict = repo
            .renew_active(
                &lease.id,
                &LeaseRenewal {
                    owner_id: ctx.actor_id.clone(),
                    if_match: active.row_version,
                    generation: active.generation,
                    idempotency_key: "renew-once".into(),
                    body_hash: "c".repeat(64),
                    expires_at: expiry,
                },
                &ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);

        let released = repo
            .transition(
                &lease.id,
                &ctx.actor_id,
                renewed.row_version,
                renewed.generation,
                &LeaseStateUpdate {
                    from: LeaseState::Active,
                    to: LeaseState::Released,
                    instance_id: None,
                    endpoint_id: None,
                    expires_at: None,
                },
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(released.state, LeaseState::Released);
        let renewed_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event WHERE resource_id=? AND event_type='runtime_lease.renewed'",
        )
        .bind(&lease.id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(renewed_events, 1);
        let outbox_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE event_record_id IN (SELECT id FROM event WHERE resource_id=?)",
        )
        .bind(&lease.id)
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        assert_eq!(outbox_count, 5);
    }

    #[tokio::test]
    async fn vencimiento_durable_pasa_por_expiring_y_respeta_gracia() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let lease = lease(&ctx.actor_id, 4);
        repo.create_requested(
            &lease,
            &"a".repeat(64),
            "expire-create",
            &initial_response(&lease.id),
            &ctx,
        )
        .await
        .unwrap();
        let admitting = LeaseStateUpdate {
            from: LeaseState::Requested,
            to: LeaseState::Admitting,
            instance_id: None,
            endpoint_id: None,
            expires_at: None,
        };
        let stored = repo
            .transition(&lease.id, &ctx.actor_id, 1, 4, &admitting, &ctx)
            .await
            .unwrap();
        let (instance_id, _) = seed_activation_records(&repo).await;
        let endpoint_id = id(64);
        insert_endpoint(&repo, &endpoint_id, &instance_id, "QUALIFIED").await;
        let active = repo
            .transition(
                &lease.id,
                &ctx.actor_id,
                stored.row_version,
                4,
                &LeaseStateUpdate {
                    from: LeaseState::Admitting,
                    to: LeaseState::Active,
                    instance_id: Some(instance_id.clone()),
                    endpoint_id: Some(endpoint_id.clone()),
                    expires_at: Some(timestamp(chrono::Utc::now() - chrono::Duration::seconds(1))),
                },
                &ctx,
            )
            .await;
        assert!(active.is_err(), "activation must reject an expired lease");

        let active = repo
            .transition(
                &lease.id,
                &ctx.actor_id,
                stored.row_version,
                4,
                &LeaseStateUpdate {
                    from: LeaseState::Admitting,
                    to: LeaseState::Active,
                    instance_id: Some(instance_id),
                    endpoint_id: Some(endpoint_id),
                    expires_at: Some(timestamp(chrono::Utc::now() + chrono::Duration::minutes(2))),
                },
                &ctx,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE runtime_lease SET expires_at=? WHERE id=?")
            .bind(timestamp(chrono::Utc::now() - chrono::Duration::seconds(1)))
            .bind(&lease.id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let expiring = repo.expire_due(30, &ctx).await.unwrap();
        assert_eq!(expiring.len(), 1);
        assert_eq!(expiring[0].state, LeaseState::Expiring);
        assert_eq!(expiring[0].row_version, active.row_version + 1);

        sqlx::query("UPDATE runtime_lease SET updated_at=? WHERE id=?")
            .bind(timestamp(
                chrono::Utc::now() - chrono::Duration::seconds(31),
            ))
            .bind(&lease.id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let expired = repo.expire_due(30, &ctx).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].state, LeaseState::Expired);
        assert!(repo.expire_due(30, &ctx).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transicion_exige_owner_revision_generacion_y_referencias_activas() {
        assert_eq!(
            validate_transition(LeaseState::Expiring, LeaseState::Released)
                .unwrap_err()
                .code,
            ErrorCode::InvalidStateTransition
        );
        let (_dir, repo) = repo().await;
        let ctx = context();
        let lease = lease(&ctx.actor_id, 4);
        repo.create_requested(
            &lease,
            &"a".repeat(64),
            "transition-key",
            &initial_response(&lease.id),
            &ctx,
        )
        .await
        .unwrap();
        let admitting = LeaseStateUpdate {
            from: LeaseState::Requested,
            to: LeaseState::Admitting,
            instance_id: None,
            endpoint_id: None,
            expires_at: None,
        };
        let stored = repo
            .transition(&lease.id, &ctx.actor_id, 1, 4, &admitting, &ctx)
            .await
            .unwrap();
        assert_eq!(stored.state, LeaseState::Admitting);
        assert_eq!(stored.row_version, 2);

        let mut active = LeaseStateUpdate {
            from: LeaseState::Admitting,
            to: LeaseState::Active,
            instance_id: None,
            endpoint_id: None,
            expires_at: None,
        };
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert_eq!(error.details.reason, "lease_activation_incomplete");

        let (instance_id, other_instance_id) = seed_activation_records(&repo).await;
        let expires_at = timestamp(chrono::Utc::now() + chrono::Duration::minutes(5));
        active.instance_id = Some(id(59));
        active.endpoint_id = Some(id(58));
        active.expires_at = Some(expires_at.clone());
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "lease_activation_instance_missing");

        active.instance_id = Some(instance_id.clone());
        active.endpoint_id = Some(id(57));
        sqlx::query("UPDATE runtime_instance SET state='STARTING' WHERE id=?")
            .bind(&instance_id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "lease_activation_instance_not_ready");
        sqlx::query("UPDATE runtime_instance SET state='READY' WHERE id=?")
            .bind(&instance_id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "lease_activation_endpoint_missing");

        let endpoint_id = id(60);
        let mismatched_endpoint_id = id(61);
        let qualified_endpoint_id = id(62);
        insert_endpoint(&repo, &endpoint_id, &instance_id, "UNTESTED").await;
        active.endpoint_id = Some(endpoint_id);
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(
            error.details.reason,
            "lease_activation_endpoint_unqualified"
        );

        insert_endpoint(
            &repo,
            &mismatched_endpoint_id,
            &other_instance_id,
            "QUALIFIED",
        )
        .await;
        active.endpoint_id = Some(mismatched_endpoint_id);
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(
            error.details.reason,
            "lease_activation_endpoint_instance_mismatch"
        );

        insert_endpoint(&repo, &qualified_endpoint_id, &instance_id, "QUALIFIED").await;
        active.endpoint_id = Some(qualified_endpoint_id.clone());
        sqlx::query("UPDATE runtime_instance SET generation=3 WHERE id=?")
            .bind(&instance_id)
            .execute(&repo.pool)
            .await
            .unwrap();
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "lease_activation_generation_mismatch");
        sqlx::query("UPDATE runtime_instance SET generation=4 WHERE id=?")
            .bind(&instance_id)
            .execute(&repo.pool)
            .await
            .unwrap();

        active.expires_at = Some(timestamp(chrono::Utc::now() - chrono::Duration::minutes(1)));
        let error = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.details.reason, "lease_activation_expiry_invalid");

        active.expires_at = Some(expires_at);
        let activated = repo
            .transition(&lease.id, &ctx.actor_id, 2, 4, &active, &ctx)
            .await
            .unwrap();
        assert_eq!(activated.state, LeaseState::Active);
        assert_eq!(activated.instance_id.as_deref(), Some(instance_id.as_str()));
        assert_eq!(
            activated.endpoint_id.as_deref(),
            Some(qualified_endpoint_id.as_str())
        );

        let stale = repo
            .transition(&lease.id, &ctx.actor_id, 1, 4, &admitting, &ctx)
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleRevision);

        let other = repo
            .transition(&lease.id, "another-owner", 2, 4, &admitting, &ctx)
            .await
            .unwrap_err();
        assert_eq!(other.code, ErrorCode::Forbidden);

        let stale_generation = repo
            .transition(&lease.id, &ctx.actor_id, 2, 3, &admitting, &ctx)
            .await
            .unwrap_err();
        assert_eq!(stale_generation.code, ErrorCode::StaleRevision);
        assert!(repo
            .get_for_owner(&lease.id, "another-owner")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn restart_expira_leases_no_reconciliadas_y_conserva_generacion_actual() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let old = lease(&ctx.actor_id, 3);
        let current = lease(&ctx.actor_id, 4);
        for record in [&old, &current] {
            repo.create_requested(
                record,
                &"a".repeat(64),
                &format!("key-{}", record.id),
                &initial_response(&record.id),
                &ctx,
            )
            .await
            .unwrap();
        }
        let expired = repo.expire_unreconciled(4, &ctx).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, old.id);
        assert_eq!(expired[0].state, LeaseState::Expired);
        assert_eq!(
            repo.get_for_owner(&current.id, &ctx.actor_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            LeaseState::Requested
        );
    }

    #[tokio::test]
    async fn nunca_persiste_replays_con_secretos_y_revierte_el_evento_fallido() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let lease = lease(&ctx.actor_id, 4);
        let with_secret = CreateLeaseOutcome {
            status: 201,
            body: serde_json::json!({ "apiKey": "must-not-be-stored" }),
        };
        assert_eq!(
            repo.create_requested(&lease, &"a".repeat(64), "secret-key", &with_secret, &ctx)
                .await
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );

        sqlx::query(
            "CREATE TRIGGER fail_lease_outbox BEFORE INSERT ON event_outbox \
             BEGIN SELECT RAISE(ABORT, 'injected outbox fault'); END",
        )
        .execute(&repo.pool)
        .await
        .unwrap();
        let result = repo
            .create_requested(
                &lease,
                &"a".repeat(64),
                "rollback-key",
                &initial_response(&lease.id),
                &ctx,
            )
            .await;
        assert!(result.is_err());
        let lease_count: i64 = sqlx::query_scalar("SELECT count(*) FROM runtime_lease")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let idempotency_count: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(lease_count, 0);
        assert_eq!(idempotency_count, 0);
    }

    #[tokio::test]
    async fn idempotency_key_caducada_se_puede_reutilizar_despues_de_treinta_dias() {
        let (_dir, repo) = repo().await;
        let ctx = context();
        let first = lease(&ctx.actor_id, 4);
        repo.create_requested(
            &first,
            &"a".repeat(64),
            "expired-key",
            &initial_response(&first.id),
            &ctx,
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE idempotency SET expires_at='2000-01-01T00:00:00.000Z' WHERE key='expired-key'",
        )
        .execute(&repo.pool)
        .await
        .unwrap();

        let second = lease(&ctx.actor_id, 4);
        let inserted = repo
            .create_requested(
                &second,
                &"b".repeat(64),
                "expired-key",
                &initial_response(&second.id),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(inserted, LeaseCreatePersistence::Created(_)));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runtime_lease")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }
}
