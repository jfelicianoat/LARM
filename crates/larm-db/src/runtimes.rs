//! P030: inventario tipado sobre las tablas contractuales, con journal/event/outbox.
//! El SQL dinámico usa sólo nombres de tipos privados, nunca identificadores del cliente.
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::adapters::{AdapterState, AdapterTrust};
use larm_domain::bridge::{is_uuid_v7, timestamp};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::MutationContext;
use larm_domain::runtime::*;
use larm_domain::runtime_ports::{RuntimeFilter, RuntimeRepository};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{sqlite::SqliteRow, Column, Row, SqliteConnection, SqlitePool};

const CREATE: &str = "P030_post__runtimes";
const RETENTION_POLICY: &str = "P032_patch__runtimes_id_retention_policy";
const ROLLBACK_VERSION: &str = "P032_post__runtime_versions_id_rollback";
const PIN_VERSION: &str = "P032_post__runtime_versions_id_pin";
const UNPIN_VERSION: &str = "P032_delete__runtime_pins_variant_id_launch_profile_id";
const IDEMPOTENCY_DAYS: i64 = 30;
mod execution;
mod install;
pub use install::SqlRuntimeInstallJobs;
const JSON_FIELDS: &[&str] = &[
    "metadata",
    "capabilities",
    "channel_policy",
    "retention_policy",
    "limits",
    "evidence_refs",
];
#[derive(Clone)]
pub struct SqlRuntimes {
    pub pool: SqlitePool,
}

trait Record: Serialize + DeserializeOwned + Send + Unpin {
    const TABLE: &'static str;
    fn validate_record(&self) -> Result<(), LarmError>;
}
macro_rules! record {
    ($($kind:ty => $table:literal),+ $(,)?) => { $(impl Record for $kind {
        const TABLE:&'static str=$table;
        fn validate_record(&self)->Result<(),LarmError> {
            self.validate()?;
            larm_domain::sources::validate_public_config(
                &serde_json::to_value(self).map_err(|e|db_error("registro runtime",&e))?
            )
        }
    })+ };
}
record!(Runtime=>"runtime",RuntimeAdapter=>"runtime_adapter",RuntimeVersion=>"runtime_version",
    RuntimeInstallation=>"runtime_installation",RuntimeBackend=>"runtime_backend",ExternalHost=>"external_host",
    RuntimeVersionPin=>"runtime_version_pin");
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
fn missing(id: &str) -> LarmError {
    error(ErrorCode::NotFound, "runtime_resource_not_found").with_resource(id)
}
fn validate_mutation_identity(
    idempotency_key: &str,
    body_hash: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    if idempotency_key.is_empty()
        || idempotency_key.len() > 200
        || idempotency_key.chars().any(char::is_control)
        || !larm_domain::adapter_updates::is_sha256(body_hash)
        || ctx.actor_id.is_empty()
        || ctx.actor_id.len() > 256
        || ctx.actor_id.chars().any(char::is_control)
    {
        return Err(invalid("runtime_idempotency_invalid"));
    }
    Ok(())
}
async fn idempotent_replay<T: DeserializeOwned>(
    conn: &mut SqliteConnection,
    operation: &str,
    key: &str,
    body_hash: &str,
    actor: &str,
    now: &str,
) -> Result<Option<T>, LarmError> {
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT body_hash,response_json FROM idempotency \
         WHERE principal_id=? AND operation=? AND key=? AND expires_at>?",
    )
    .bind(actor)
    .bind(operation)
    .bind(key)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_error("consultando idempotencia runtime", &e))?;
    let Some((saved_hash, raw)) = existing else {
        return Ok(None);
    };
    if saved_hash != body_hash {
        return Err(error(
            ErrorCode::IdempotencyConflict,
            "idempotency_body_mismatch",
        ));
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| error(ErrorCode::IntegrityFailed, "runtime_replay_corrupt"))
}
struct IdempotencyWrite<'a> {
    operation: &'a str,
    key: &'a str,
    body_hash: &'a str,
    actor: &'a str,
    status: i64,
}
async fn save_idempotency<T: Serialize>(
    conn: &mut SqliteConnection,
    write: IdempotencyWrite<'_>,
    response: &T,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LarmError> {
    let IdempotencyWrite {
        operation,
        key,
        body_hash,
        actor,
        status,
    } = write;
    let at = timestamp(now);
    sqlx::query(
        "DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?",
    )
    .bind(actor)
    .bind(operation)
    .bind(key)
    .bind(&at)
    .execute(&mut *conn)
    .await
    .map_err(|e| db_error("limpiando idempotencia runtime caducada", &e))?;
    let changed = sqlx::query(
        "INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) \
         VALUES(?,?,?,?,?,?,?)",
    )
    .bind(actor)
    .bind(operation)
    .bind(key)
    .bind(body_hash)
    .bind(status)
    .bind(text_json(response)?)
    .bind(timestamp(now + chrono::Duration::days(IDEMPOTENCY_DAYS)))
    .execute(conn)
    .await
    .map_err(|e| db_error("guardando idempotencia runtime", &e))?
    .rows_affected();
    if changed != 1 {
        return Err(error(
            ErrorCode::Conflict,
            "runtime_idempotency_unconfirmed",
        ));
    }
    Ok(())
}
fn text_json<T: Serialize>(value: &T) -> Result<String, LarmError> {
    serde_json::to_string(value).map_err(|e| db_error("serializando runtime", &e))
}
fn row<T: Record>(row: &SqliteRow) -> Result<T, LarmError> {
    let mut fields = Map::new();
    for column in row.columns() {
        let name = column.name();
        let value = if name == "row_version" {
            json!(row
                .try_get::<i64, _>(name)
                .map_err(|e| db_error(name, &e))?)
        } else {
            let raw: Option<String> = row.try_get(name).map_err(|e| db_error(name, &e))?;
            match raw {
                None => Value::Null,
                Some(raw) if JSON_FIELDS.contains(&name) => serde_json::from_str(&raw)
                    .map_err(|_| error(ErrorCode::IntegrityFailed, "runtime_record_corrupt"))?,
                Some(raw) => json!(raw),
            }
        };
        fields.insert(name.to_owned(), value);
    }
    let record: T = serde_json::from_value(Value::Object(fields))
        .map_err(|_| error(ErrorCode::IntegrityFailed, "runtime_record_corrupt"))?;
    record
        .validate_record()
        .map_err(|_| error(ErrorCode::IntegrityFailed, "runtime_record_corrupt"))?;
    Ok(record)
}
async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE runtime SET row_version=row_version WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando catálogo runtime", &e))?;
    Ok(())
}
async fn load<T: Record>(conn: &mut SqliteConnection, id: &str) -> Result<Option<T>, LarmError> {
    let found = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT * FROM {} WHERE id=?",
        T::TABLE
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo runtime", &e))?;
    found.as_ref().map(row::<T>).transpose()
}
async fn insert<T: Record>(conn: &mut SqliteConnection, value: &T) -> Result<(), LarmError> {
    value.validate_record()?;
    let document = serde_json::to_value(value).map_err(|e| db_error("serializando runtime", &e))?;
    let fields = document
        .as_object()
        .ok_or_else(|| invalid("runtime_record_invalid"))?;
    // Sólo los seis structs privados anteriores implementan Record.
    let columns = fields
        .keys()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = vec!["?"; fields.len()].join(",");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {}({columns}) VALUES({placeholders})",
        T::TABLE
    )));
    for (name, value) in fields {
        query = if name == "row_version" {
            query.bind(
                value
                    .as_i64()
                    .ok_or_else(|| invalid("row_version_invalid"))?,
            )
        } else if JSON_FIELDS.contains(&name.as_str()) {
            query.bind(text_json(value)?)
        } else {
            query.bind(value.as_str().map(str::to_owned))
        };
    }
    let changed = query
        .execute(conn)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                error(ErrorCode::Conflict, "runtime_identity_exists")
            } else {
                db_error("insertando runtime", &e)
            }
        })?
        .rows_affected();
    if changed != 1 {
        return Err(error(ErrorCode::Conflict, "runtime_write_unconfirmed"));
    }
    Ok(())
}
async fn mutation<T: Record>(
    conn: &mut SqliteConnection,
    value: &T,
    action: &str,
    ctx: &MutationContext,
) -> Result<String, LarmError> {
    if ctx.actor_id.is_empty() {
        return Err(invalid("actor_invalid"));
    }
    let payload = serde_json::to_value(value).map_err(|e| db_error("journal runtime", &e))?;
    let id = payload["id"]
        .as_str()
        .ok_or_else(|| invalid("id_invalid"))?;
    let revision = payload["row_version"]
        .as_i64()
        .ok_or_else(|| invalid("row_version_invalid"))?;
    let journal = uuid::Uuid::now_v7().to_string();
    let hash = larm_domain::canonical::sha256_hex(&payload)
        .map_err(|_| invalid("runtime_hash_invalid"))?;
    let changed=sqlx::query("INSERT INTO operation_journal(id,kind,resource_ids,phase,plan_hash,payload,updated_at) VALUES(?,'runtime.catalog',?,'COMPLETED',?,?,?)")
        .bind(journal).bind(json!([id]).to_string()).bind(hash).bind(json!({"action":action,"record":payload,"actor_id":ctx.actor_id,"correlation_id":ctx.correlation_id}).to_string())
        .bind(timestamp(chrono::Utc::now())).execute(&mut *conn).await.map_err(|e|db_error("journal runtime",&e))?.rows_affected();
    if changed != 1 {
        return Err(error(ErrorCode::Conflict, "runtime_journal_unconfirmed"));
    }
    append(
        conn,
        &NewEvent {
            event_type: "runtime.state_changed",
            resource_type: T::TABLE,
            resource_id: id,
            resource_version: revision,
            data: json!({"action":action}),
        },
        ctx,
    )
    .await
}
async fn binding(conn: &mut SqliteConnection, id: &str) -> Result<RuntimeAdapter, LarmError> {
    let binding = load::<RuntimeAdapter>(conn, id)
        .await?
        .ok_or_else(|| missing(id))?;
    let installed = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(&binding.adapter_installation_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("binding runtime", &e))?
        .ok_or_else(|| missing(id))?;
    let installed = crate::adapters::to_installation(&installed)?;
    if installed.family != "runtime"
        || installed.trust == AdapterTrust::Revoked
        || !matches!(
            installed.state,
            AdapterState::Installed | AdapterState::Validated
        )
    {
        return Err(error(ErrorCode::Conflict, "runtime_adapter_unavailable"));
    }
    let capabilities: Vec<String> = serde_json::from_value(
        installed
            .manifest
            .get("capabilities")
            .cloned()
            .ok_or_else(|| invalid("runtime_manifest_capabilities"))?,
    )
    .map_err(|_| invalid("runtime_manifest_capabilities"))?;
    if binding.capabilities != capabilities {
        return Err(error(
            ErrorCode::Conflict,
            "runtime_adapter_binding_changed",
        ));
    }
    Ok(binding)
}

// Índice de presentación; runtime_adapter sigue siendo la relación autoritativa.
async fn publish_bindings(
    conn: &mut SqliteConnection,
    installed: &larm_domain::adapters::AdapterInstallation,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let rows =
        sqlx::query("SELECT * FROM runtime_adapter WHERE adapter_installation_id=? ORDER BY id")
            .bind(&installed.id)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_error("índice runtime", &e))?;
    let bindings: Vec<RuntimeAdapter> = rows
        .iter()
        .map(row::<RuntimeAdapter>)
        .collect::<Result<_, _>>()?;
    let index = json!(bindings
        .iter()
        .map(|r| json!({"id":r.id,"runtime_family":r.runtime_family,"capabilities":r.capabilities}))
        .collect::<Vec<_>>());
    if installed.metadata.get("runtime_bindings") == Some(&index) {
        return Ok(());
    }
    let mut metadata = installed.metadata.clone();
    metadata.insert("runtime_bindings".into(), index);
    let revision = installed
        .row_version
        .checked_add(1)
        .ok_or_else(|| invalid("row_version_overflow"))?;
    let changed=sqlx::query("UPDATE adapter_installation SET metadata=?,row_version=?,updated_at=? WHERE id=? AND row_version=?")
        .bind(text_json(&metadata)?).bind(revision).bind(timestamp(chrono::Utc::now())).bind(&installed.id).bind(installed.row_version)
        .execute(&mut *conn).await.map_err(|e|db_error("publicando índice runtime",&e))?.rows_affected();
    if changed != 1 {
        return Err(error(
            ErrorCode::StaleRevision,
            "runtime_binding_index_unconfirmed",
        ));
    }
    append(
        conn,
        &NewEvent {
            event_type: "adapter.updated",
            resource_type: "adapter_installation",
            resource_id: &installed.id,
            resource_version: revision,
            data: json!({"action":"runtime_bindings_updated"}),
        },
        ctx,
    )
    .await?;
    Ok(())
}
impl SqlRuntimes {
    async fn get<T: Record>(&self, id: &str) -> Result<Option<T>, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("pool runtime", &e))?;
        load(&mut conn, id).await
    }
    async fn record<T: Record>(&self, value: &T, ctx: &MutationContext) -> Result<(), LarmError> {
        value.validate_record()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN runtime", &e))?;
        lock(&mut tx).await?;
        insert(&mut tx, value).await?;
        mutation(&mut tx, value, "recorded", ctx).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT runtime", &e))
    }
    async fn page<T: Record>(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<T>, LarmError> {
        if !(1..=201).contains(&limit)
            || filter.runtime_id.as_deref().is_some_and(|v| !is_uuid_v7(v))
            || filter
                .storage_location_id
                .as_deref()
                .is_some_and(|v| !is_uuid_v7(v))
        {
            return Err(invalid("runtime_filter_invalid"));
        }
        let (join,q,state,runtime,storage,caps)=match T::TABLE {
            "runtime"=>("","r.name",None,None,None,Some("r.capabilities")),
            "runtime_version"=>("","r.version_string",None,Some("r.runtime_id"),None,None),
            "runtime_installation"=>(" JOIN runtime_version v ON v.id=r.runtime_version_id","NULL",Some("r.state"),Some("v.runtime_id"),Some("r.storage_location_id"),None),
            "runtime_backend"=>(" JOIN runtime_installation i ON i.id=r.runtime_installation_id JOIN runtime_version v ON v.id=i.runtime_version_id","r.backend_type",Some("r.status"),Some("v.runtime_id"),None,None),
            "external_host"=>("","r.name || r.base_url",Some("r.state"),None,None,None),
            _=>return Err(invalid("runtime_table_invalid")),
        };
        if (filter.q.is_some() && q == "NULL")
            || (filter.state.is_some() && state.is_none())
            || (filter.runtime_id.is_some() && runtime.is_none())
            || (filter.storage_location_id.is_some() && storage.is_none())
            || (filter.capability.is_some() && caps.is_none())
        {
            return Err(invalid("runtime_filter_invalid"));
        }
        let (at, id) = after.map_or((None, None), |(at, id)| (Some(at), Some(id)));
        let statement=format!("SELECT r.* FROM {} r{join} WHERE (? IS NULL OR (r.created_at,r.id)>(?,?)) AND (? IS NULL OR instr(lower({q}),lower(?))>0) AND (? IS NULL OR {}=?) AND (? IS NULL OR {}=?) AND (? IS NULL OR {}=?) AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each({}) WHERE value=?)) ORDER BY r.created_at,r.id LIMIT ?",T::TABLE,state.unwrap_or("NULL"),runtime.unwrap_or("NULL"),storage.unwrap_or("NULL"),caps.unwrap_or("'[]'"));
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(&at)
            .bind(&at)
            .bind(id)
            .bind(&filter.q)
            .bind(&filter.q)
            .bind(&filter.state)
            .bind(&filter.state)
            .bind(&filter.runtime_id)
            .bind(&filter.runtime_id)
            .bind(&filter.storage_location_id)
            .bind(&filter.storage_location_id)
            .bind(&filter.capability)
            .bind(&filter.capability)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error("paginando runtime", &e))?;
        rows.iter().map(row::<T>).collect()
    }
}

#[async_trait]
impl RuntimeRepository for SqlRuntimes {
    async fn register_runtime_adapter(
        &self,
        installation: &str,
        family: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeAdapter, LarmError> {
        if !is_uuid_v7(installation) || family.is_empty() {
            return Err(invalid("runtime_adapter_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN binding runtime", &e))?;
        lock(&mut tx).await?;
        let installed = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
            .bind(installation)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("adaptador runtime", &e))?
            .ok_or_else(|| missing(installation))?;
        let installed = crate::adapters::to_installation(&installed)?;
        if installed.family != "runtime"
            || installed.trust == AdapterTrust::Revoked
            || !matches!(
                installed.state,
                AdapterState::Installed | AdapterState::Validated
            )
        {
            return Err(error(ErrorCode::Conflict, "runtime_adapter_unavailable"));
        }
        let caps: Vec<String> = serde_json::from_value(
            installed
                .manifest
                .get("capabilities")
                .cloned()
                .ok_or_else(|| invalid("runtime_manifest_capabilities"))?,
        )
        .map_err(|_| invalid("runtime_manifest_capabilities"))?;
        let rows=sqlx::query("SELECT * FROM runtime_adapter WHERE adapter_installation_id=? AND runtime_family=? ORDER BY id LIMIT 2").bind(installation).bind(family).fetch_all(&mut *tx).await.map_err(|e|db_error("binding runtime",&e))?;
        if let Some(existing) = rows.first() {
            let existing: RuntimeAdapter = row(existing)?;
            if rows.len() != 1 || existing.capabilities != caps {
                return Err(error(
                    ErrorCode::Conflict,
                    "runtime_adapter_binding_changed",
                ));
            }
            publish_bindings(&mut tx, &installed, ctx).await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT binding runtime", &e))?;
            return Ok(existing);
        }
        let now = timestamp(chrono::Utc::now());
        let value = RuntimeAdapter {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: Map::new(),
            adapter_installation_id: installation.into(),
            runtime_family: family.into(),
            capabilities: caps,
        };
        insert(&mut tx, &value).await?;
        mutation(&mut tx, &value, "binding_registered", ctx).await?;
        publish_bindings(&mut tx, &installed, ctx).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT binding runtime", &e))?;
        Ok(value)
    }
    async fn runtime_adapter(&self, id: &str) -> Result<Option<RuntimeAdapter>, LarmError> {
        self.get(id).await
    }
    async fn create_runtime(
        &self,
        request: &RuntimeCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        request.validate()?;
        if key.is_empty()
            || key.len() > 240
            || !larm_domain::adapter_updates::is_sha256(body_hash)
            || ctx.actor_id.is_empty()
        {
            return Err(invalid("runtime_idempotency_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN runtime", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        let replay:Option<(String,String)>=sqlx::query_as("SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?")
            .bind(&ctx.actor_id).bind(CREATE).bind(key).bind(&at).fetch_optional(&mut *tx).await.map_err(|e|db_error("replay runtime",&e))?;
        if let Some((digest, raw)) = replay {
            let original: Runtime = serde_json::from_str(&raw)
                .map_err(|_| error(ErrorCode::IntegrityFailed, "runtime_replay_corrupt"))?;
            original.validate()?;
            if digest != body_hash || original.configuration() != *request {
                return Err(error(
                    ErrorCode::IdempotencyConflict,
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT runtime replay", &e))?;
            return Ok(original);
        }
        let bound = binding(&mut tx, &request.adapter_id).await?;
        if request
            .capabilities
            .iter()
            .any(|c| !bound.capabilities.contains(c))
        {
            return Err(invalid("runtime_capability_not_announced"));
        }
        let value = Runtime {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: at.clone(),
            updated_at: at.clone(),
            row_version: 1,
            metadata: Map::new(),
            name: request.name.clone(),
            adapter_id: request.adapter_id.clone(),
            capabilities: request.capabilities.clone(),
            default_version_id: request.default_version_id.clone(),
            channel_policy: request.channel_policy.clone(),
            retention_policy: request.retention_policy,
        };
        if let Some(default) = &value.default_version_id {
            let version = load::<RuntimeVersion>(&mut tx, default)
                .await?
                .ok_or_else(|| missing(default))?;
            if version.runtime_id != value.id {
                return Err(invalid("default_version_runtime_mismatch"));
            }
        }
        insert(&mut tx, &value).await?;
        mutation(&mut tx, &value, "registered", ctx).await?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?").bind(&ctx.actor_id).bind(CREATE).bind(key).bind(&at).execute(&mut *tx).await.map_err(|e|db_error("replay runtime caducado",&e))?;
        let count=sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES(?,?,?,?,201,?,?)")
            .bind(&ctx.actor_id).bind(CREATE).bind(key).bind(body_hash).bind(text_json(&value)?).bind(timestamp(now+chrono::Duration::days(30))).execute(&mut *tx).await.map_err(|e|db_error("guardando replay runtime",&e))?.rows_affected();
        if count != 1 {
            return Err(error(ErrorCode::Conflict, "runtime_replay_unconfirmed"));
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT runtime", &e))?;
        Ok(value)
    }
    async fn runtime(&self, id: &str) -> Result<Option<Runtime>, LarmError> {
        self.get(id).await
    }
    async fn runtimes(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<Runtime>, LarmError> {
        self.page(after, limit, filter).await
    }
    async fn change_channel_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeChannelPolicy,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        policy.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN política runtime", &e))?;
        lock(&mut tx).await?;
        let mut value = load::<Runtime>(&mut tx, id)
            .await?
            .ok_or_else(|| missing(id))?;
        if value.row_version != if_match {
            return Err(error(ErrorCode::StaleRevision, "stale_revision").with_resource(id));
        }
        value.row_version = value
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        value.updated_at = timestamp(chrono::Utc::now());
        value.channel_policy = policy.clone();
        let changed=sqlx::query("UPDATE runtime SET channel_policy=?,row_version=?,updated_at=? WHERE id=? AND row_version=?").bind(text_json(policy)?).bind(value.row_version).bind(&value.updated_at).bind(id).bind(if_match).execute(&mut *tx).await.map_err(|e|db_error("política runtime",&e))?.rows_affected();
        if changed != 1 {
            return Err(error(ErrorCode::StaleRevision, "runtime_write_unconfirmed"));
        }
        mutation(&mut tx, &value, "channel_policy_changed", ctx).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT política runtime", &e))?;
        Ok(value)
    }
    async fn change_retention_policy(
        &self,
        id: &str,
        if_match: i64,
        policy: &RuntimeRetentionPolicy,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Runtime, LarmError> {
        if !is_uuid_v7(id) || if_match < 1 {
            return Err(invalid("runtime_retention_policy_request_invalid"));
        }
        policy.validate()?;
        validate_mutation_identity(idempotency_key, body_hash, ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN política de retención runtime", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        if let Some(replay) = idempotent_replay::<Runtime>(
            &mut tx,
            RETENTION_POLICY,
            idempotency_key,
            body_hash,
            &ctx.actor_id,
            &at,
        )
        .await?
        {
            replay.validate()?;
            if replay.id != id || replay.retention_policy != *policy {
                return Err(error(ErrorCode::IntegrityFailed, "runtime_replay_mismatch"));
            }
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay retención runtime", &e))?;
            return Ok(replay);
        }
        let mut value = load::<Runtime>(&mut tx, id)
            .await?
            .ok_or_else(|| missing(id))?;
        if value.row_version != if_match {
            return Err(error(ErrorCode::StaleRevision, "stale_revision").with_resource(id));
        }
        value.row_version = value
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        value.updated_at = at;
        value.retention_policy = *policy;
        let changed = sqlx::query(
            "UPDATE runtime SET retention_policy=?,row_version=?,updated_at=? \
             WHERE id=? AND row_version=?",
        )
        .bind(text_json(policy)?)
        .bind(value.row_version)
        .bind(&value.updated_at)
        .bind(id)
        .bind(if_match)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando política de retención runtime", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(error(ErrorCode::StaleRevision, "runtime_write_unconfirmed"));
        }
        mutation(&mut tx, &value, "retention_policy_changed", ctx).await?;
        save_idempotency(
            &mut tx,
            IdempotencyWrite {
                operation: RETENTION_POLICY,
                key: idempotency_key,
                body_hash,
                actor: &ctx.actor_id,
                status: 200,
            },
            &value,
            now,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT política de retención runtime", &e))?;
        Ok(value)
    }
    async fn rollback_version(
        &self,
        version_id: &str,
        request: &RuntimeRollbackRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeMutationReceipt, LarmError> {
        if !is_uuid_v7(version_id) {
            return Err(invalid("runtime_version_id_invalid"));
        }
        request.validate()?;
        validate_mutation_identity(idempotency_key, body_hash, ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN rollback runtime", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        if let Some(receipt) = idempotent_replay::<RuntimeMutationReceipt>(
            &mut tx,
            ROLLBACK_VERSION,
            idempotency_key,
            body_hash,
            &ctx.actor_id,
            &at,
        )
        .await?
        {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay rollback runtime", &e))?;
            return Ok(receipt);
        }
        let mut target = load::<RuntimeVersion>(&mut tx, version_id)
            .await?
            .ok_or_else(|| missing(version_id))?;
        let mut runtime = load::<Runtime>(&mut tx, &target.runtime_id)
            .await?
            .ok_or_else(|| missing(&target.runtime_id))?;
        if runtime.default_version_id != request.expected_default_version_id {
            return Err(error(ErrorCode::StaleRevision, "runtime_default_changed")
                .with_resource(&runtime.id));
        }
        if runtime.default_version_id.as_deref() == Some(version_id) {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_version_already_default",
            ));
        }
        if !matches!(
            target.lifecycle,
            RuntimeVersionLifecycle::Validated | RuntimeVersionLifecycle::Retained
        ) {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_rollback_target_not_validated",
            ));
        }
        let qualified = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM runtime_installation \
             WHERE runtime_version_id=? AND state IN ('VALIDATED','ACTIVE','DEGRADED','ROLLED_BACK') \
             AND stage='VALIDATED')",
        )
        .bind(version_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("validando destino rollback runtime", &e))?;
        if qualified != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_rollback_target_not_installed",
            ));
        }
        let mut previous = match runtime.default_version_id.as_deref() {
            Some(id) => Some(
                load::<RuntimeVersion>(&mut tx, id)
                    .await?
                    .ok_or_else(|| missing(id))?,
            ),
            None => None,
        };
        runtime.row_version = runtime
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        runtime.updated_at = at.clone();
        runtime.default_version_id = Some(version_id.to_owned());
        let changed = sqlx::query(
            "UPDATE runtime SET default_version_id=?,row_version=?,updated_at=? \
             WHERE id=? AND default_version_id IS ?",
        )
        .bind(version_id)
        .bind(runtime.row_version)
        .bind(&runtime.updated_at)
        .bind(&runtime.id)
        .bind(&request.expected_default_version_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("cambiando default runtime", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(error(ErrorCode::StaleRevision, "runtime_default_changed"));
        }
        target.row_version = target
            .row_version
            .checked_add(1)
            .ok_or_else(|| invalid("row_version_overflow"))?;
        target.updated_at = at.clone();
        target.lifecycle = RuntimeVersionLifecycle::Default;
        target.promotion = RuntimePromotion::Default;
        let changed = sqlx::query(
            "UPDATE runtime_version SET lifecycle='DEFAULT',promotion='DEFAULT',row_version=?,updated_at=? \
             WHERE id=?",
        )
        .bind(target.row_version)
        .bind(&target.updated_at)
        .bind(version_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando default runtime", &e))?
        .rows_affected();
        if changed != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_default_write_unconfirmed",
            ));
        }
        let version_event = mutation(&mut tx, &target, "rollback_target_default", ctx).await?;
        if let Some(previous) = previous.as_mut() {
            previous.row_version = previous
                .row_version
                .checked_add(1)
                .ok_or_else(|| invalid("row_version_overflow"))?;
            previous.updated_at = at.clone();
            previous.lifecycle = RuntimeVersionLifecycle::Retained;
            previous.promotion = RuntimePromotion::Candidate;
            let changed = sqlx::query(
                "UPDATE runtime_version SET lifecycle='RETAINED',promotion='CANDIDATE',row_version=?,updated_at=? WHERE id=?",
            )
            .bind(previous.row_version)
            .bind(&previous.updated_at)
            .bind(&previous.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("marcando versión anterior retained", &e))?
            .rows_affected();
            if changed != 1 {
                return Err(error(
                    ErrorCode::Conflict,
                    "runtime_previous_version_write_unconfirmed",
                ));
            }
            mutation(&mut tx, previous, "rollback_previous_retained", ctx).await?;
        }
        mutation(&mut tx, &runtime, "rollback_default_changed", ctx).await?;
        let receipt = RuntimeMutationReceipt {
            resource_id: target.id.clone(),
            row_version: target.row_version,
            event_id: version_event,
        };
        save_idempotency(
            &mut tx,
            IdempotencyWrite {
                operation: ROLLBACK_VERSION,
                key: idempotency_key,
                body_hash,
                actor: &ctx.actor_id,
                status: 201,
            },
            &receipt,
            now,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT rollback runtime", &e))?;
        Ok(receipt)
    }
    async fn pin_version(
        &self,
        version_id: &str,
        request: &RuntimePinRequest,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeMutationReceipt, LarmError> {
        if !is_uuid_v7(version_id) {
            return Err(invalid("runtime_version_id_invalid"));
        }
        request.validate()?;
        validate_mutation_identity(idempotency_key, body_hash, ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN pin runtime", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        if let Some(receipt) = idempotent_replay::<RuntimeMutationReceipt>(
            &mut tx,
            PIN_VERSION,
            idempotency_key,
            body_hash,
            &ctx.actor_id,
            &at,
        )
        .await?
        {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay pin runtime", &e))?;
            return Ok(receipt);
        }
        let target = load::<RuntimeVersion>(&mut tx, version_id)
            .await?
            .ok_or_else(|| missing(version_id))?;
        if !matches!(
            target.lifecycle,
            RuntimeVersionLifecycle::Validated
                | RuntimeVersionLifecycle::Default
                | RuntimeVersionLifecycle::Retained
        ) {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_pin_target_not_validated",
            ));
        }
        let installed = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM runtime_installation \
             WHERE runtime_version_id=? AND state IN ('VALIDATED','ACTIVE','DEGRADED','ROLLED_BACK') \
             AND stage='VALIDATED')",
        )
        .bind(version_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("validando versión para pin runtime", &e))?;
        if installed != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_pin_target_not_installed",
            ));
        }
        let compatible = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM launch_profile lp \
             JOIN compatibility_record c ON c.launch_profile_hash=lp.profile_hash \
             WHERE lp.id=? AND c.variant_id=? AND c.runtime_version_id=? AND c.status='COMPATIBLE' \
             AND NOT EXISTS(SELECT 1 FROM json_each(c.hard_gates) g \
               WHERE COALESCE(json_extract(g.value,'$.status'),'UNKNOWN')<>'PASS'))",
        )
        .bind(&request.launch_profile_id)
        .bind(&request.variant_id)
        .bind(version_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("comprobando compatibilidad de pin runtime", &e))?;
        if compatible != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_pin_compatibility_not_proven",
            ));
        }
        let existing = sqlx::query(
            "SELECT * FROM runtime_version_pin WHERE variant_id=? AND launch_profile_id=?",
        )
        .bind(&request.variant_id)
        .bind(&request.launch_profile_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo pin runtime existente", &e))?;
        let pin = if let Some(existing_row) = existing.as_ref() {
            let mut value = row::<RuntimeVersionPin>(existing_row)?;
            value.row_version = value
                .row_version
                .checked_add(1)
                .ok_or_else(|| invalid("row_version_overflow"))?;
            value.updated_at = at.clone();
            value.runtime_version_id = version_id.to_owned();
            let changed = sqlx::query(
                "UPDATE runtime_version_pin SET runtime_version_id=?,row_version=?,updated_at=? WHERE id=? AND row_version=?",
            )
            .bind(version_id)
            .bind(value.row_version)
            .bind(&value.updated_at)
            .bind(&value.id)
            .bind(value.row_version - 1)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("actualizando pin runtime", &e))?
            .rows_affected();
            if changed != 1 {
                return Err(error(ErrorCode::StaleRevision, "runtime_pin_changed"));
            }
            value
        } else {
            let value = RuntimeVersionPin {
                id: uuid::Uuid::now_v7().to_string(),
                created_at: at.clone(),
                updated_at: at,
                row_version: 1,
                metadata: Map::new(),
                variant_id: request.variant_id.clone(),
                launch_profile_id: request.launch_profile_id.clone(),
                runtime_version_id: version_id.to_owned(),
            };
            insert(&mut tx, &value).await?;
            value
        };
        let event_id = mutation(&mut tx, &pin, "pinned", ctx).await?;
        let receipt = RuntimeMutationReceipt {
            resource_id: pin.id.clone(),
            row_version: pin.row_version,
            event_id,
        };
        save_idempotency(
            &mut tx,
            IdempotencyWrite {
                operation: PIN_VERSION,
                key: idempotency_key,
                body_hash,
                actor: &ctx.actor_id,
                status: 201,
            },
            &receipt,
            now,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT pin runtime", &e))?;
        Ok(receipt)
    }
    async fn pins(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        runtime_version_id: Option<&str>,
    ) -> Result<Vec<RuntimeVersionPin>, LarmError> {
        if !(1..=201).contains(&limit)
            || runtime_version_id.is_some_and(|id| !is_uuid_v7(id))
            || after.as_ref().is_some_and(|(at, id)| {
                chrono::DateTime::parse_from_rfc3339(at).is_err() || !is_uuid_v7(id)
            })
        {
            return Err(invalid("runtime_pin_query_invalid"));
        }
        let (at, id) = after.map_or((None, None), |(at, id)| (Some(at), Some(id)));
        let rows = sqlx::query(
            "SELECT * FROM runtime_version_pin WHERE (? IS NULL OR (created_at,id)>(?,?)) \
             AND (? IS NULL OR runtime_version_id=?) ORDER BY created_at,id LIMIT ?",
        )
        .bind(&at)
        .bind(&at)
        .bind(id)
        .bind(runtime_version_id)
        .bind(runtime_version_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("consultando pins runtime", &e))?;
        rows.iter().map(row::<RuntimeVersionPin>).collect()
    }
    async fn unpin(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
        idempotency_key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        if !is_uuid_v7(variant_id) || !is_uuid_v7(launch_profile_id) {
            return Err(invalid("runtime_pin_identity_invalid"));
        }
        validate_mutation_identity(idempotency_key, body_hash, ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN unpin runtime", &e))?;
        lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let at = timestamp(now);
        if idempotent_replay::<RuntimeMutationReceipt>(
            &mut tx,
            UNPIN_VERSION,
            idempotency_key,
            body_hash,
            &ctx.actor_id,
            &at,
        )
        .await?
        .is_some()
        {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay unpin runtime", &e))?;
            return Ok(());
        }
        let found = sqlx::query(
            "SELECT * FROM runtime_version_pin WHERE variant_id=? AND launch_profile_id=?",
        )
        .bind(variant_id)
        .bind(launch_profile_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo pin runtime a retirar", &e))?;
        let receipt = if let Some(found) = found {
            let pin: RuntimeVersionPin = row(&found)?;
            let event_id = mutation(&mut tx, &pin, "unpin_requested", ctx).await?;
            let changed =
                sqlx::query("DELETE FROM runtime_version_pin WHERE id=? AND row_version=?")
                    .bind(&pin.id)
                    .bind(pin.row_version)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| db_error("retirando pin runtime", &e))?
                    .rows_affected();
            if changed != 1 {
                return Err(error(ErrorCode::StaleRevision, "runtime_pin_changed"));
            }
            RuntimeMutationReceipt {
                resource_id: pin.id,
                row_version: pin.row_version,
                event_id,
            }
        } else {
            RuntimeMutationReceipt {
                resource_id: variant_id.to_owned(),
                row_version: 0,
                event_id: "0".into(),
            }
        };
        save_idempotency(
            &mut tx,
            IdempotencyWrite {
                operation: UNPIN_VERSION,
                key: idempotency_key,
                body_hash,
                actor: &ctx.actor_id,
                status: 204,
            },
            &receipt,
            now,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT unpin runtime", &e))?;
        Ok(())
    }
    async fn pin_for(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
    ) -> Result<Option<RuntimeVersionPin>, LarmError> {
        if !is_uuid_v7(variant_id) || !is_uuid_v7(launch_profile_id) {
            return Err(invalid("runtime_pin_identity_invalid"));
        }
        let found = sqlx::query(
            "SELECT * FROM runtime_version_pin WHERE variant_id=? AND launch_profile_id=?",
        )
        .bind(variant_id)
        .bind(launch_profile_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("consultando pin runtime", &e))?;
        found.as_ref().map(row::<RuntimeVersionPin>).transpose()
    }
    async fn record_version(
        &self,
        value: &RuntimeVersion,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        if value.lifecycle != RuntimeVersionLifecycle::Candidate {
            return Err(invalid("runtime_version_lifecycle_requires_use_case"));
        }
        if matches!(
            value.promotion,
            RuntimePromotion::Default | RuntimePromotion::Pinned
        ) {
            return Err(invalid("runtime_promotion_requires_use_case"));
        }
        self.record(value, ctx).await
    }
    async fn version(&self, id: &str) -> Result<Option<RuntimeVersion>, LarmError> {
        self.get(id).await
    }
    async fn version_by_runtime_and_string(
        &self,
        runtime_id: &str,
        version_string: &str,
    ) -> Result<Option<RuntimeVersion>, LarmError> {
        if !is_uuid_v7(runtime_id) || version_string.is_empty() {
            return Err(invalid("runtime_version_identity_invalid"));
        }
        let rows = sqlx::query(
            "SELECT * FROM runtime_version WHERE runtime_id=? AND version_string=? ORDER BY id LIMIT 2",
        )
        .bind(runtime_id)
        .bind(version_string)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("resolviendo versión upstream exacta", &e))?;
        if rows.len() > 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_version_identity_ambiguous",
            ));
        }
        rows.first().map(row::<RuntimeVersion>).transpose()
    }
    async fn versions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeVersion>, LarmError> {
        self.page(after, limit, filter).await
    }
    async fn record_installation(
        &self,
        value: &RuntimeInstallation,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.record(value, ctx).await
    }
    async fn installation(&self, id: &str) -> Result<Option<RuntimeInstallation>, LarmError> {
        self.get(id).await
    }
    async fn installations(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeInstallation>, LarmError> {
        self.page(after, limit, filter).await
    }
    async fn record_backend(
        &self,
        value: &RuntimeBackend,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.record(value, ctx).await
    }
    async fn backend(&self, id: &str) -> Result<Option<RuntimeBackend>, LarmError> {
        self.get(id).await
    }
    async fn backends(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<RuntimeBackend>, LarmError> {
        self.page(after, limit, filter).await
    }
    async fn record_external_host(
        &self,
        value: &ExternalHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.record(value, ctx).await
    }
    async fn external_host(&self, id: &str) -> Result<Option<ExternalHost>, LarmError> {
        self.get(id).await
    }
    async fn external_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExternalHost>, LarmError> {
        self.page(after, limit, filter).await
    }
}
