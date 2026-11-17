//! P081: registro read-only de hosts de ejecución observados (WSL2 y motor de
//! contenedores). La escritura es un upsert idempotente de observación con
//! journal/evento/outbox en la misma transacción; el registro no habilita
//! ejecución ni acredita compatibilidad.

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use larm_domain::{
    bridge::timestamp,
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    runtime::ExecutionHost,
    runtime_ports::RuntimeFilter,
};
use serde_json::json;
use sqlx::{Row, SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlExecutionHosts {
    pub pool: SqlitePool,
}

fn error(code: ErrorCode, reason: &str) -> LarmError {
    LarmError::new(
        code,
        "No se pudo actualizar el catálogo de runtimes",
        reason,
    )
}

fn invalid(reason: &str) -> LarmError {
    error(ErrorCode::ValidationFailed, reason)
}

fn row_to_host(row: &sqlx::sqlite::SqliteRow) -> Result<ExecutionHost, LarmError> {
    let id: String = row
        .try_get("id")
        .map_err(|e| db_error("id execution_host", &e))?;
    let metadata_raw: String = row
        .try_get("metadata")
        .map_err(|e| db_error("metadata execution_host", &e))?;
    let metadata: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&metadata_raw)
        .map_err(|_| error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt"))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| db_error("kind execution_host", &e))?;
    let wsl_version: Option<i64> = row
        .try_get("wsl_version")
        .map_err(|e| db_error("wsl_version execution_host", &e))?;
    let default_host: i64 = row
        .try_get("default_host")
        .map_err(|e| db_error("default_host execution_host", &e))?;
    let evidence_raw: String = row
        .try_get("evidence_refs")
        .map_err(|e| db_error("evidence_refs execution_host", &e))?;
    let evidence_refs: Vec<String> = serde_json::from_str(&evidence_raw)
        .map_err(|_| error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt"))?;
    let host =
        ExecutionHost {
            id,
            created_at: row
                .try_get("created_at")
                .map_err(|e| db_error("created_at execution_host", &e))?,
            updated_at: row
                .try_get("updated_at")
                .map_err(|e| db_error("updated_at execution_host", &e))?,
            row_version: row
                .try_get("row_version")
                .map_err(|e| db_error("row_version execution_host", &e))?,
            metadata,
            kind: serde_json::from_value(json!(kind))
                .map_err(|_| error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt"))?,
            name: row
                .try_get("name")
                .map_err(|e| db_error("name execution_host", &e))?,
            engine_version: row
                .try_get::<Option<String>, _>("engine_version")
                .map_err(|e| db_error("engine_version execution_host", &e))?,
            wsl_version: match wsl_version {
                None => None,
                Some(v) => Some(u8::try_from(v).map_err(|_| {
                    error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt")
                })?),
            },
            default: default_host != 0,
            status: serde_json::from_value(json!(row
                .try_get::<String, _>("status")
                .map_err(|e| db_error("status execution_host", &e))?))
            .map_err(|_| error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt"))?,
            evidence_refs,
        };
    host.validate()
        .map_err(|_| error(ErrorCode::IntegrityFailed, "execution_host_record_corrupt"))?;
    Ok(host)
}

async fn find(conn: &mut SqliteConnection, id: &str) -> Result<Option<ExecutionHost>, LarmError> {
    let found = sqlx::query("SELECT * FROM execution_host WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo execution_host", &e))?;
    found.as_ref().map(row_to_host).transpose()
}

impl SqlExecutionHosts {
    pub async fn record(
        &self,
        host: &ExecutionHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        host.validate()
            .map_err(|_| invalid("execution_host_record_invalid"))?;
        if ctx.actor_id.is_empty() || ctx.actor_id.len() > 256 {
            return Err(invalid("runtime_idempotency_invalid"));
        }
        let at = timestamp(chrono::Utc::now());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN execution_host", &e))?;
        let evidence = json!(host.evidence_refs).to_string();
        let metadata = json!(host.metadata).to_string();
        let updated = sqlx::query(
            "INSERT INTO execution_host(id,created_at,updated_at,row_version,metadata,kind,name,engine_version,wsl_version,default_host,status,evidence_refs) \
             VALUES(?,?,?,1,?,?,?,?,?,?,?,?) \
             ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,row_version=row_version+1,metadata=excluded.metadata,kind=excluded.kind,name=excluded.name,engine_version=excluded.engine_version,wsl_version=excluded.wsl_version,default_host=excluded.default_host,status=excluded.status,evidence_refs=excluded.evidence_refs \
             RETURNING row_version",
        )
        .bind(&host.id)
        .bind(&at)
        .bind(&at)
        .bind(&metadata)
        .bind(host.kind.as_str())
        .bind(&host.name)
        .bind(host.engine_version.as_deref())
        .bind(host.wsl_version.map(i64::from))
        .bind(i64::from(host.default))
        .bind(host.status.as_str())
        .bind(&evidence)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("escribiendo execution_host", &e))?;
        let row_version: i64 = updated
            .try_get("row_version")
            .map_err(|e| db_error("row_version execution_host", &e))?;
        let journal_id = uuid::Uuid::now_v7().to_string();
        let payload =
            json!({"host": host, "actor_id": ctx.actor_id, "correlation_id": ctx.correlation_id});
        let journal = sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?, 'runtime.execution_host', ?, 'COMPLETED', ?, ?, ?)")
            .bind(journal_id)
            .bind(json!([host.id]).to_string())
            .bind(larm_domain::canonical::sha256_hex(&payload).map_err(|_| invalid("runtime_hash_invalid"))?)
            .bind(payload.to_string())
            .bind(&at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("journal runtime.execution_host", &e))?;
        if journal.rows_affected() != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "execution_host_journal_unconfirmed",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.execution_host.observed",
                resource_type: "execution_host",
                resource_id: &host.id,
                resource_version: row_version,
                data: json!({
                    "kind": host.kind.as_str(),
                    "name": host.name,
                    "status": host.status.as_str(),
                    "wsl_version": host.wsl_version,
                    "default": host.default,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT execution_host", &e))
    }

    pub async fn find(&self, id: &str) -> Result<Option<ExecutionHost>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool execution_host", &e))?;
        find(&mut conn, id).await
    }

    pub async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExecutionHost>, LarmError> {
        if !(1..=201).contains(&limit)
            || filter.runtime_id.is_some()
            || filter.storage_location_id.is_some()
            || filter.capability.is_some()
            || filter
                .state
                .as_deref()
                .is_some_and(|state| !matches!(state, "ONLINE" | "OFFLINE" | "UNKNOWN"))
        {
            return Err(invalid("runtime_filter_invalid"));
        }
        let q: Option<&str> = filter.q.as_deref();
        let status: Option<&str> = filter.state.as_deref();
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool execution_host", &e))?;
        let rows = sqlx::query(
            "SELECT * FROM execution_host WHERE (? IS NULL OR (created_at,id)>(?,?)) \
             AND (? IS NULL OR instr(lower(name),lower(?))>0) AND (? IS NULL OR status=?) \
             ORDER BY created_at,id LIMIT ?",
        )
        .bind(after.as_ref().map(|(_, _)| true))
        .bind(after.as_ref().map(|(at, _)| at.clone()))
        .bind(after.as_ref().map(|(_, id)| id.clone()))
        .bind(q)
        .bind(q)
        .bind(status)
        .bind(status)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_error("listando execution_host", &e))?;
        rows.iter().map(row_to_host).collect()
    }
}
