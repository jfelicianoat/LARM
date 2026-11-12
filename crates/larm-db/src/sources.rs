//! P043: configuración, replay y eventos atómicos; sin IO de fuentes/credenciales.
use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::adapter_updates::is_sha256;
use larm_domain::adapters::{AdapterState, AdapterTrust};
use larm_domain::bridge::{is_uuid_v7, timestamp};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{MutationContext, SourceCatalogRepository};
use larm_domain::sources::*;
use serde_json::{json, Map, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SqlSources {
    pub pool: SqlitePool,
    pub qualification: Option<std::sync::Arc<dyn larm_domain::ports::AdapterQualification>>,
}

/// Snapshot tipado de la relación P043 que un consumidor debe volver a
/// comprobar junto con cuenta/licencia y cualificación antes de abrir IO.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceExecutionBinding {
    pub source: ModelSource,
    pub adapter: ModelSourceAdapter,
    pub installation: larm_domain::adapters::AdapterInstallation,
}

impl SqlSources {
    /// Resuelve fuente, binding e instalación bajo una única transacción de
    /// lectura. No entrega secretos ni sustituye los gates P022/P045.
    pub async fn execution_binding(
        &self,
        source_id: &str,
    ) -> Result<SourceExecutionBinding, LarmError> {
        if !is_uuid_v7(source_id) {
            return Err(source_invalid("source_id_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN binding source", &e))?;
        let source = load(&mut tx, source_id).await?;
        if !source.enabled {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La fuente está deshabilitada",
                "source_disabled",
            )
            .with_resource(source_id));
        }
        let source_adapter = adapter(&mut tx, &source.adapter_id).await?;
        let installation = installation(&mut tx, &source_adapter.adapter_installation_id).await?;
        if installation.state != AdapterState::Validated {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El adaptador de fuente no está cualificado",
                "source_adapter_unqualified",
            )
            .with_resource(&installation.id));
        }
        if source_adapter.adapter_installation_id != installation.id
            || installation.manifest.get("capabilities")
                != Some(&json!(source_adapter.capabilities))
        {
            return Err(conflict(&source_adapter.id, "source_adapter_binding_stale"));
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT binding source", &e))?;
        Ok(SourceExecutionBinding {
            source,
            adapter: source_adapter,
            installation,
        })
    }
}

fn missing(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::NotFound,
        "Fuente o adaptador desconocidos",
        "not_found",
    )
    .with_resource(id)
}
fn stale(id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::StaleRevision,
        "La fuente cambió desde que la leíste",
        "stale_revision",
    )
    .with_resource(id)
}
fn conflict(id: &str, reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "La fuente tiene referencias o cambió de identidad",
        reason,
    )
    .with_resource(id)
}
fn json_text(row: &sqlx::sqlite::SqliteRow, name: &str) -> Result<Value, LarmError> {
    let text: String = row.try_get(name).map_err(|e| db_error(name, &e))?;
    serde_json::from_str(&text).map_err(|e| db_error(name, &e))
}
fn string(row: &sqlx::sqlite::SqliteRow, name: &str) -> Result<String, LarmError> {
    row.try_get(name).map_err(|e| db_error(name, &e))
}
fn integer(row: &sqlx::sqlite::SqliteRow, name: &str) -> Result<i64, LarmError> {
    row.try_get(name).map_err(|e| db_error(name, &e))
}
fn source_row(row: &sqlx::sqlite::SqliteRow) -> Result<ModelSource, LarmError> {
    let rate_limit_json: Option<String> = row
        .try_get("operational_rate_limit_json")
        .map_err(|e| db_error("operational_rate_limit_json", &e))?;
    let rate_limit = rate_limit_json
        .map(|text| {
            serde_json::from_str::<Value>(&text).map_err(|e| db_error("rate_limit_json", &e))
        })
        .transpose()?;
    let source: ModelSource=serde_json::from_value(json!({
        "id":string(row,"id")?,"created_at":string(row,"created_at")?,"updated_at":string(row,"updated_at")?,"row_version":integer(row,"row_version")?,
        "metadata":json_text(row,"metadata")?,"name":string(row,"name")?,"adapter_id":string(row,"adapter_id")?,"base_url":string(row,"base_url")?,
        "priority":integer(row,"priority")?,"enabled":integer(row,"enabled")?==1,"auth_mode":string(row,"auth_mode")?,
        "account_id":row.try_get::<Option<String>,_>("account_id").map_err(|e|db_error("account_id",&e))?,"policy":json_text(row,"policy")?,
        "operational":{"status":string(row,"operational_status")?,"last_sync_at":row.try_get::<Option<String>,_>("operational_last_sync_at").map_err(|e|db_error("operational_last_sync_at",&e))?,"rate_limit":rate_limit}
    })).map_err(|e|db_error("model_source",&e))?;
    source.validate()?;
    Ok(source)
}
fn adapter_row(row: &sqlx::sqlite::SqliteRow) -> Result<ModelSourceAdapter, LarmError> {
    let adapter: ModelSourceAdapter=serde_json::from_value(json!({
        "id":string(row,"id")?,"created_at":string(row,"created_at")?,"updated_at":string(row,"updated_at")?,"row_version":integer(row,"row_version")?,
        "metadata":json_text(row,"metadata")?,"adapter_installation_id":string(row,"adapter_installation_id")?,"source_type":string(row,"source_type")?,"capabilities":json_text(row,"capabilities")?
    })).map_err(|e|db_error("model_source_adapter",&e))?;
    adapter.validate()?;
    Ok(adapter)
}
async fn write_lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE model_source SET row_version=row_version WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando catálogo de fuentes", &e))?;
    Ok(())
}
async fn load(conn: &mut SqliteConnection, id: &str) -> Result<ModelSource, LarmError> {
    let row = sqlx::query("SELECT s.*,COALESCE(o.status,'UNKNOWN') AS operational_status,o.last_sync_at AS operational_last_sync_at,o.rate_limit_json AS operational_rate_limit_json FROM model_source s LEFT JOIN model_source_operational_snapshot o ON o.source_id=s.id WHERE s.id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo fuente", &e))?
        .ok_or_else(|| missing(id))?;
    source_row(&row)
}
async fn adapter(conn: &mut SqliteConnection, id: &str) -> Result<ModelSourceAdapter, LarmError> {
    let row = sqlx::query("SELECT * FROM model_source_adapter WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo adaptador de fuente", &e))?
        .ok_or_else(|| missing(id))?;
    adapter_row(&row)
}
async fn installation(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<larm_domain::adapters::AdapterInstallation, LarmError> {
    let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo instalación de fuente", &e))?
        .ok_or_else(|| missing(id))?;
    let installation = crate::adapters::to_installation(&row)?;
    if installation.family != "source"
        || !matches!(
            installation.state,
            AdapterState::Installed | AdapterState::Validated
        )
        || installation.trust == AdapterTrust::Revoked
    {
        return Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "La instalación no admite configuración de fuentes",
            "source_adapter_unavailable",
        )
        .with_resource(id));
    }
    Ok(installation)
}
async fn references(conn: &mut SqliteConnection, id: &str) -> Result<bool, LarmError> {
    sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM model_distribution WHERE source_id=?) OR EXISTS(SELECT 1 FROM artifact_provenance WHERE source_id=?) OR EXISTS(SELECT 1 FROM model_source_account WHERE source_id=?) OR EXISTS(SELECT 1 FROM source_watch WHERE source_id=?)")
        .bind(id).bind(id).bind(id).bind(id).fetch_one(conn).await.map_err(|e|db_error("referencias de fuente",&e))
}
async fn validate_binding(
    conn: &mut SqliteConnection,
    source_id: &str,
    request: &ModelSourceCreate,
    ctx: &MutationContext,
    gate: Option<&std::sync::Arc<dyn larm_domain::ports::AdapterQualification>>,
) -> Result<
    (
        ModelSourceAdapter,
        Option<Box<dyn larm_domain::ports::AdapterQualificationGuard>>,
    ),
    LarmError,
> {
    let binding = adapter(conn, &request.adapter_id).await?;
    let mut guard = None;
    if request.enabled {
        let candidate = installation(conn, &binding.adapter_installation_id).await?;
        if candidate.state != AdapterState::Validated {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "El adaptador aún no tiene conformidad validada",
                "source_adapter_unqualified",
            )
            .with_resource(&candidate.id));
        }
        guard = Some(crate::conformance::qualify_current(conn, &candidate, ctx, gate).await?);
        if json!(binding.capabilities) != candidate.manifest["capabilities"] {
            return Err(conflict(&binding.id, "source_adapter_binding_stale"));
        }
    }
    if let Some(account) = &request.account_id {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT source_id FROM model_source_account WHERE id=?")
                .bind(account)
                .fetch_optional(conn)
                .await
                .map_err(|e| db_error("cuenta de fuente", &e))?;
        if owner.as_deref() != Some(source_id) {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La cuenta no pertenece a esta fuente",
                "source_account_mismatch",
            ));
        }
    }
    Ok((binding, guard))
}
async fn event(
    conn: &mut SqliteConnection,
    source: &ModelSource,
    action: &str,
    version: i64,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    append(
        conn,
        &NewEvent {
            event_type: "source.updated",
            resource_type: "model_source",
            resource_id: &source.id,
            resource_version: version,
            data: json!({"action":action,"adapter_id":source.adapter_id}),
        },
        ctx,
    )
    .await?;
    Ok(())
}

#[async_trait]
impl SourceCatalogRepository for SqlSources {
    async fn source_replay(
        &self,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<Option<ModelSource>, LarmError> {
        if !is_sha256(body_hash) || key.is_empty() || key.len() > 240 || ctx.actor_id.is_empty() {
            return Err(source_invalid("idempotency"));
        }
        let replay:Option<(String,String)>=sqlx::query_as("SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?")
            .bind(&ctx.actor_id).bind(CREATE_SOURCE_OPERATION).bind(key).bind(timestamp(chrono::Utc::now())).fetch_optional(&self.pool).await.map_err(|e|db_error("replay de fuente",&e))?;
        replay
            .map(|(hash, body)| {
                if hash != body_hash {
                    return Err(LarmError::new(
                        ErrorCode::IdempotencyConflict,
                        "Idempotency-Key ya usada con otro cuerpo",
                        "idempotency_body_mismatch",
                    ));
                }
                serde_json::from_str(&body).map_err(|e| db_error("replay de fuente", &e))
            })
            .transpose()
    }
    async fn register_source_adapter(
        &self,
        id: &str,
        source_type: &str,
        metadata: &Map<String, Value>,
        ctx: &MutationContext,
    ) -> Result<ModelSourceAdapter, LarmError> {
        if !is_uuid_v7(id) || source_type.is_empty() {
            return Err(source_invalid("adapter_identity"));
        }
        validate_public_config(&Value::Object(metadata.clone()))?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let installed = installation(&mut tx, id).await?;
        let capabilities: Vec<String> = serde_json::from_value(
            installed
                .manifest
                .get("capabilities")
                .cloned()
                .ok_or_else(|| source_invalid("manifest_capabilities"))?,
        )
        .map_err(|e| db_error("capabilities", &e))?;
        let rows=sqlx::query("SELECT * FROM model_source_adapter WHERE adapter_installation_id=? AND source_type=? ORDER BY id LIMIT 2").bind(id).bind(source_type).fetch_all(&mut *tx).await.map_err(|e|db_error("binding existente",&e))?;
        if !rows.is_empty() {
            let previous = adapter_row(&rows[0])?;
            if rows.len() != 1
                || previous.metadata != *metadata
                || previous.capabilities != capabilities
            {
                return Err(conflict(id, "source_adapter_binding_exists"));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(previous);
        }
        let now = timestamp(chrono::Utc::now());
        let record = ModelSourceAdapter {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: metadata.clone(),
            adapter_installation_id: id.into(),
            source_type: source_type.into(),
            capabilities,
        };
        record.validate()?;
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES (?,?,?,1,?,?,?,?)")
            .bind(&record.id).bind(&record.created_at).bind(&record.updated_at).bind(Value::Object(record.metadata.clone()).to_string()).bind(id).bind(source_type).bind(json!(record.capabilities).to_string()).execute(&mut *tx).await.map_err(|e|db_error("registrando adaptador de fuente",&e))?;
        // Índice de presentación para GET /adapters. La relación tipada anterior
        // sigue siendo la autoridad: nunca se admiten fuentes desde este índice.
        let bindings: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,source_type FROM model_source_adapter WHERE adapter_installation_id=? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_error("indexando conexiones de fuente", &e))?;
        let mut public_metadata = installed.metadata.clone();
        public_metadata.insert(
            "source_bindings".into(),
            json!(bindings
                .iter()
                .map(|(id, source_type)| json!({"id":id,"source_type":source_type}))
                .collect::<Vec<_>>()),
        );
        let version = installed
            .row_version
            .checked_add(1)
            .ok_or_else(|| source_invalid("row_version"))?;
        sqlx::query("UPDATE adapter_installation SET metadata=?,row_version=?,updated_at=? WHERE id=? AND row_version=?")
            .bind(Value::Object(public_metadata).to_string()).bind(version).bind(&record.updated_at).bind(id).bind(installed.row_version)
            .execute(&mut *tx).await.map_err(|e|db_error("publicando conexiones de fuente",&e))?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "adapter.updated",
                resource_type: "adapter_installation",
                resource_id: id,
                resource_version: version,
                data: json!({"action":"source_binding_registered","source_adapter_id":record.id}),
            },
            ctx,
        )
        .await?;
        append(
            &mut tx,
            &NewEvent {
                event_type: "adapter.updated",
                resource_type: "model_source_adapter",
                resource_id: &record.id,
                resource_version: 1,
                data: json!({"action":"source_binding_created","adapter_installation_id":id}),
            },
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record)
    }
    async fn source_adapter(&self, id: &str) -> Result<Option<ModelSourceAdapter>, LarmError> {
        let row = sqlx::query("SELECT * FROM model_source_adapter WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error("adaptador de fuente", &e))?;
        row.as_ref().map(adapter_row).transpose()
    }
    async fn create_source(
        &self,
        request: &ModelSourceCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<ModelSource, LarmError> {
        request.validate()?;
        if !is_sha256(body_hash)
            || key.is_empty()
            || key.len() > 240
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            || ctx.actor_id.is_empty()
        {
            return Err(source_invalid("idempotency"));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let now = chrono::Utc::now();
        let replay:Option<(String,String)>=sqlx::query_as("SELECT body_hash,response_json FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at>?")
            .bind(&ctx.actor_id).bind(CREATE_SOURCE_OPERATION).bind(key).bind(timestamp(now)).fetch_optional(&mut *tx).await.map_err(|e|db_error("replay de fuente",&e))?;
        if let Some((hash, value)) = replay {
            if hash != body_hash {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            let original: ModelSource =
                serde_json::from_str(&value).map_err(|e| db_error("replay de fuente", &e))?;
            if original.configuration() != *request {
                return Err(LarmError::new(
                    ErrorCode::IdempotencyConflict,
                    "Idempotency-Key ya usada con otro cuerpo",
                    "idempotency_body_mismatch",
                ));
            }
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(original);
        }
        let id = uuid::Uuid::now_v7().to_string();
        let (_, guard) =
            validate_binding(&mut tx, &id, request, ctx, self.qualification.as_ref()).await?;
        let record = ModelSource {
            id,
            created_at: timestamp(now),
            updated_at: timestamp(now),
            row_version: 1,
            metadata: Map::new(),
            name: request.name.clone(),
            adapter_id: request.adapter_id.clone(),
            base_url: request.base_url.clone(),
            priority: request.priority,
            enabled: request.enabled,
            auth_mode: request.auth_mode.clone(),
            account_id: request.account_id.clone(),
            policy: request.policy.clone(),
            operational: SourceOperationalSnapshot::unknown(),
        };
        let auth_mode =
            serde_json::to_value(&record.auth_mode).map_err(|e| db_error("auth_mode", &e))?;
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES (?,?,?,1,'{}',?,?,?,?,?,?,?,?)")
            .bind(&record.id).bind(&record.created_at).bind(&record.updated_at).bind(&record.name).bind(&record.adapter_id).bind(&record.base_url).bind(record.priority as i64).bind(record.enabled).bind(auth_mode.as_str()).bind(&record.account_id).bind(Value::Object(record.policy.clone()).to_string()).execute(&mut *tx).await.map_err(|e|db_error("creando fuente",&e))?;
        sqlx::query("DELETE FROM idempotency WHERE principal_id=? AND operation=? AND key=? AND expires_at<=?").bind(&ctx.actor_id).bind(CREATE_SOURCE_OPERATION).bind(key).bind(timestamp(now)).execute(&mut *tx).await.map_err(|e|db_error("replay caducado",&e))?;
        sqlx::query("INSERT INTO idempotency(principal_id,operation,key,body_hash,status_code,response_json,expires_at) VALUES (?,?,?,?,201,?,?)")
            .bind(&ctx.actor_id).bind(CREATE_SOURCE_OPERATION).bind(key).bind(body_hash).bind(serde_json::to_string(&record).map_err(|e|db_error("replay",&e))?).bind(timestamp(now+chrono::Duration::days(30))).execute(&mut *tx).await.map_err(|e|db_error("guardando replay de fuente",&e))?;
        event(&mut tx, &record, "created", 1, ctx).await?;
        if let Some(guard) = &guard {
            guard.recheck().await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record)
    }
    async fn source(&self, id: &str) -> Result<Option<ModelSource>, LarmError> {
        let row = sqlx::query("SELECT s.*,COALESCE(o.status,'UNKNOWN') AS operational_status,o.last_sync_at AS operational_last_sync_at,o.rate_limit_json AS operational_rate_limit_json FROM model_source s LEFT JOIN model_source_operational_snapshot o ON o.source_id=s.id WHERE s.id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error("fuente", &e))?;
        row.as_ref().map(source_row).transpose()
    }
    async fn page_sources(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &SourceFilter,
    ) -> Result<Vec<ModelSource>, LarmError> {
        if !(1..=201).contains(&limit) {
            return Err(source_invalid("limit"));
        }
        let (at, id) = after.map_or((None, None), |(at, id)| (Some(at), Some(id)));
        let rows=sqlx::query("SELECT s.*,COALESCE(o.status,'UNKNOWN') AS operational_status,o.last_sync_at AS operational_last_sync_at,o.rate_limit_json AS operational_rate_limit_json FROM model_source s JOIN model_source_adapter a ON a.id=s.adapter_id LEFT JOIN model_source_operational_snapshot o ON o.source_id=s.id WHERE (? IS NULL OR (s.created_at,s.id)>(?,?)) AND (? IS NULL OR instr(lower(s.name),lower(?))>0 OR instr(lower(s.base_url),lower(?))>0) AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each(a.capabilities) WHERE value=?)) ORDER BY s.created_at,s.id LIMIT ?")
            .bind(&at).bind(&at).bind(id).bind(&filter.q).bind(&filter.q).bind(&filter.q).bind(&filter.capability).bind(&filter.capability).bind(i64::from(limit)).fetch_all(&self.pool).await.map_err(|e|db_error("listando fuentes",&e))?;
        rows.iter().map(source_row).collect()
    }
    async fn update_source(
        &self,
        id: &str,
        if_match: i64,
        request: &ModelSourceCreate,
        ctx: &MutationContext,
    ) -> Result<ModelSource, LarmError> {
        request.validate()?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let mut record = load(&mut tx, id).await?;
        if record.row_version != if_match {
            return Err(stale(id));
        }
        let (next_adapter, guard) =
            validate_binding(&mut tx, id, request, ctx, self.qualification.as_ref()).await?;
        let previous_adapter = adapter(&mut tx, &record.adapter_id).await?;
        if (record.base_url != request.base_url
            || previous_adapter.source_type != next_adapter.source_type)
            && references(&mut tx, id).await?
        {
            return Err(conflict(id, "source_identity_in_use"));
        }
        record.updated_at = timestamp(chrono::Utc::now());
        record.row_version = record
            .row_version
            .checked_add(1)
            .ok_or_else(|| source_invalid("row_version_overflow"))?;
        record.name = request.name.clone();
        record.adapter_id = request.adapter_id.clone();
        record.base_url = request.base_url.clone();
        record.priority = request.priority;
        record.enabled = request.enabled;
        record.auth_mode = request.auth_mode.clone();
        record.account_id = request.account_id.clone();
        record.policy = request.policy.clone();
        let auth_mode =
            serde_json::to_value(&record.auth_mode).map_err(|e| db_error("auth_mode", &e))?;
        let changed=sqlx::query("UPDATE model_source SET updated_at=?,row_version=?,name=?,adapter_id=?,base_url=?,priority=?,enabled=?,auth_mode=?,account_id=?,policy=? WHERE id=? AND row_version=?")
            .bind(&record.updated_at).bind(record.row_version).bind(&record.name).bind(&record.adapter_id).bind(&record.base_url).bind(record.priority as i64).bind(record.enabled).bind(auth_mode.as_str()).bind(&record.account_id).bind(Value::Object(record.policy.clone()).to_string()).bind(id).bind(if_match).execute(&mut *tx).await.map_err(|e|db_error("editando fuente",&e))?;
        if changed.rows_affected() != 1 {
            return Err(stale(id));
        }
        event(&mut tx, &record, "updated", record.row_version, ctx).await?;
        if let Some(guard) = &guard {
            guard.recheck().await?;
        }
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(record)
    }
    async fn delete_source(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx).await?;
        let record = load(&mut tx, id).await?;
        if record.row_version != if_match {
            return Err(stale(id));
        }
        if references(&mut tx, id).await? {
            return Err(conflict(id, "source_in_use"));
        }
        let version = record
            .row_version
            .checked_add(1)
            .ok_or_else(|| source_invalid("row_version_overflow"))?;
        sqlx::query("DELETE FROM model_source WHERE id=? AND row_version=?")
            .bind(id)
            .bind(if_match)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("borrando configuración de fuente", &e))?;
        event(&mut tx, &record, "deleted", version, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(())
    }
}
