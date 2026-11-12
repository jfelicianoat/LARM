//! P031: resolución de contexto y publicación atómica del descubrimiento.
use super::*;
use larm_domain::adapters::AdapterInstallation;
use larm_domain::runtime_execution::{
    RuntimeExecutionContext, RuntimeExecutionRepository, RuntimeSessionGuard,
};
use std::collections::BTreeSet;

const DISCOVERY: &str = "runtime.discovery";

fn context_valid(ctx: &MutationContext) -> Result<(), LarmError> {
    if ctx.actor_id.is_empty()
        || ctx.actor_id.len() > 256
        || ctx.actor_id.chars().any(char::is_control)
        || !is_uuid_v7(&ctx.correlation_id)
    {
        return Err(invalid("runtime_execution_context_invalid"));
    }
    Ok(())
}
async fn read_context(
    conn: &mut SqliteConnection,
    runtime_id: &str,
    operation: String,
    ctx: &MutationContext,
) -> Result<RuntimeExecutionContext, LarmError> {
    context_valid(ctx)?;
    if !is_uuid_v7(runtime_id) || !is_uuid_v7(&operation) {
        return Err(invalid("runtime_execution_id_invalid"));
    }
    let runtime = load::<Runtime>(conn, runtime_id)
        .await?
        .ok_or_else(|| missing(runtime_id))?;
    let adapter = binding(conn, &runtime.adapter_id).await?;
    let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(&adapter.adapter_installation_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error("instalación runtime", &e))?;
    let installation: AdapterInstallation = crate::adapters::to_installation(&row)?;
    let run = crate::conformance::require_consumer_current(conn, &installation).await?;
    Ok(RuntimeExecutionContext {
        operation_id: operation,
        runtime,
        adapter,
        installation,
        run,
        actor_id: ctx.actor_id.clone(),
    })
}
fn identity(
    value: &RuntimeVersion,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    RuntimeChannel,
) {
    (
        value.version_string.clone(),
        value.commit.clone(),
        value.build.clone(),
        value.artifact_hash.clone(),
        value.channel,
    )
}
fn resources(context: &RuntimeExecutionContext, versions: &[RuntimeVersion]) -> Vec<String> {
    let mut values = vec![context.runtime.id.clone(), context.installation.id.clone()];
    values.extend(versions.iter().map(|v| v.id.clone()));
    values.sort();
    values.dedup();
    values
}
fn body_hash(
    context: &RuntimeExecutionContext,
    observations: &[RuntimeVersion],
) -> Result<String, LarmError> {
    let mut values =
        serde_json::to_value(observations).map_err(|_| invalid("runtime_discovery_invalid"))?;
    if let Value::Array(values) = &mut values {
        for (value, version) in values.iter_mut().zip(observations) {
            value["row_version"] = json!(version.row_version.to_string());
        }
    }
    larm_domain::conformance::hash(
        &json!({"context_sha256":context.digest()?,"observations":values}),
    )
}
fn corrupt() -> LarmError {
    error(
        ErrorCode::IntegrityFailed,
        "runtime_discovery_journal_corrupt",
    )
}
/// El hash cubre también el resultado asignado por el servidor. Las revisiones
/// SQLite son i64: se normalizan explícitamente, sin redondearlas a f64 en JCS.
fn journal_hash(payload: &Value) -> Result<String, LarmError> {
    let mut canonical = payload.clone();
    for field in ["observations", "result"] {
        let records: Vec<RuntimeVersion> =
            serde_json::from_value(payload[field].clone()).map_err(|_| corrupt())?;
        let values = canonical[field].as_array_mut().ok_or_else(corrupt)?;
        for (value, record) in values.iter_mut().zip(records) {
            value["row_version"] = json!(record.row_version.to_string());
        }
    }
    larm_domain::conformance::hash(&canonical).map_err(|_| corrupt())
}
fn validate_observations(
    context: &RuntimeExecutionContext,
    observations: &[RuntimeVersion],
) -> Result<(), LarmError> {
    let mut identities = BTreeSet::new();
    for version in observations {
        version.validate_record()?;
        version.observed_channel()?;
        if version.runtime_id != context.runtime.id
            || !context
                .runtime
                .channel_policy
                .allowed_channels
                .contains(&version.channel)
        {
            return Err(error(
                ErrorCode::ProtocolError,
                "runtime_discovery_binding_mismatch",
            ));
        }
        if version.promotion != RuntimePromotion::None {
            return Err(error(
                ErrorCode::ProtocolError,
                "runtime_discovery_promotion_not_authoritative",
            ));
        }
        if !identities.insert(identity(version)) {
            return Err(error(
                ErrorCode::ProtocolError,
                "runtime_discovery_identity_repeated",
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl RuntimeExecutionRepository for SqlRuntimes {
    async fn prepare_execution(
        &self,
        runtime: &str,
        ctx: &MutationContext,
    ) -> Result<RuntimeExecutionContext, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN contexto runtime", &e))?;
        let context = read_context(&mut tx, runtime, uuid::Uuid::now_v7().to_string(), ctx).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT contexto runtime", &e))?;
        Ok(context)
    }
    async fn commit_discovery(
        &self,
        context: &RuntimeExecutionContext,
        observations: &[RuntimeVersion],
        session: &dyn RuntimeSessionGuard,
        ctx: &MutationContext,
    ) -> Result<Vec<RuntimeVersion>, LarmError> {
        context_valid(ctx)?;
        if !is_uuid_v7(&context.operation_id) || !is_uuid_v7(&context.runtime.id) {
            return Err(invalid("runtime_execution_id_invalid"));
        }
        if ctx.actor_id != context.actor_id {
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_discovery_actor_mismatch",
            ));
        }
        if session.binding() != &context.binding()? {
            return Err(error(
                ErrorCode::Forbidden,
                "runtime_discovery_session_mismatch",
            ));
        }
        validate_observations(context, observations)?;
        let digest = body_hash(context, observations)?;
        session.recheck().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN descubrimiento runtime", &e))?;
        lock(&mut tx).await?;
        let current = read_context(
            &mut tx,
            &context.runtime.id,
            context.operation_id.clone(),
            ctx,
        )
        .await?;
        if current.digest()? != context.digest()? {
            return Err(error(
                ErrorCode::StaleRevision,
                "runtime_discovery_context_changed",
            ));
        }
        let replay: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT kind,phase,plan_hash,payload,resource_ids FROM operation_journal WHERE id=?",
        )
        .bind(&context.operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("replay de descubrimiento", &e))?;
        if let Some((kind, phase, recorded, payload, ids)) = replay {
            if kind != DISCOVERY {
                return Err(error(
                    ErrorCode::IdempotencyConflict,
                    "runtime_discovery_operation_mismatch",
                ));
            }
            let payload: Value = serde_json::from_str(&payload).map_err(|_| corrupt())?;
            let stored: Vec<RuntimeVersion> =
                serde_json::from_value(payload["observations"].clone()).map_err(|_| corrupt())?;
            let versions: Vec<RuntimeVersion> =
                serde_json::from_value(payload["result"].clone()).map_err(|_| corrupt())?;
            let ids: Vec<String> = serde_json::from_str(&ids).map_err(|_| corrupt())?;
            validate_observations(context, &stored).map_err(|_| corrupt())?;
            let stored_request = body_hash(context, &stored).map_err(|_| corrupt())?;
            if phase != "COMPLETED"
                || payload["schema_version"] != "1.0"
                || payload["actor_id"] != ctx.actor_id
                || payload["context_sha256"] != context.digest()?
                || payload["request_sha256"] != stored_request
                || journal_hash(&payload)? != recorded
                || versions.len() != stored.len()
                || ids != resources(context, &versions)
            {
                return Err(corrupt());
            }
            for (version, observation) in versions.iter().zip(&stored) {
                version.validate_record().map_err(|_| corrupt())?;
                if version.runtime_id != context.runtime.id
                    || identity(version) != identity(observation)
                {
                    return Err(corrupt());
                }
            }
            if stored_request != digest {
                return Err(error(
                    ErrorCode::IdempotencyConflict,
                    "runtime_discovery_operation_mismatch",
                ));
            }
            session.recheck().await?;
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT replay descubrimiento", &e))?;
            return Ok(versions);
        }
        let at = timestamp(chrono::Utc::now());
        let mut versions = Vec::new();
        for observation in observations {
            let found = sqlx::query("SELECT * FROM runtime_version WHERE runtime_id=? AND version_string=? AND coalesce(\"commit\",'')=? AND coalesce(build,'')=? AND coalesce(artifact_hash,'')=? AND channel=?")
                .bind(&context.runtime.id).bind(&observation.version_string).bind(observation.commit.as_deref().unwrap_or("")).bind(observation.build.as_deref().unwrap_or("")).bind(observation.artifact_hash.as_deref().unwrap_or("")).bind(observation.channel.as_str()).fetch_optional(&mut *tx).await.map_err(|e| db_error("identidad descubierta", &e))?;
            let value = if let Some(existing) = found {
                row::<RuntimeVersion>(&existing)?
            } else {
                let mut metadata = observation.metadata.clone();
                metadata.insert("runtime_discovery".into(), json!({"operation_id":context.operation_id,"adapter_installation_id":context.installation.id,"source_record_id":observation.id}));
                let value = RuntimeVersion {
                    id: uuid::Uuid::now_v7().to_string(),
                    created_at: at.clone(),
                    updated_at: at.clone(),
                    row_version: 1,
                    metadata,
                    runtime_id: context.runtime.id.clone(),
                    version_string: observation.version_string.clone(),
                    commit: observation.commit.clone(),
                    build: observation.build.clone(),
                    channel: observation.channel,
                    released_at: observation.released_at.clone(),
                    artifact_hash: observation.artifact_hash.clone(),
                    promotion: RuntimePromotion::None,
                    lifecycle: RuntimeVersionLifecycle::Candidate,
                };
                insert(&mut tx, &value).await?;
                mutation(&mut tx, &value, "discovered", ctx).await?;
                value
            };
            versions.push(value);
        }
        let payload = json!({"schema_version":"1.0","actor_id":ctx.actor_id,"context_sha256":context.digest()?,"request_sha256":digest,"observations":observations,"result":versions});
        let changed = sqlx::query("INSERT INTO operation_journal(id,kind,phase,plan_hash,resource_ids,payload,updated_at) VALUES(?,?,'COMPLETED',?,?,?,?)")
            .bind(&context.operation_id).bind(DISCOVERY).bind(journal_hash(&payload)?).bind(text_json(&resources(context,&versions))?).bind(text_json(&payload)?).bind(&at).execute(&mut *tx).await.map_err(|e| db_error("journal descubrimiento", &e))?.rows_affected();
        if changed != 1 {
            return Err(error(
                ErrorCode::Conflict,
                "runtime_discovery_journal_unconfirmed",
            ));
        }
        session.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT descubrimiento runtime", &e))?;
        Ok(versions)
    }
}
