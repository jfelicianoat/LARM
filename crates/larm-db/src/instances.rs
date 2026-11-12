//! Lecturas contractuales de `runtime_instance` para P033.

use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::{
    bridge::is_uuid_v7,
    error::{ErrorCode, LarmError},
    instances::{InstanceState, RuntimeInstance},
    ports::MutationContext,
    protocol::{EndpointQualification, InferenceEndpoint},
    runtime::RuntimeOwnership,
    runtime_ports::{RuntimeInstanceHealthRepository, RuntimeInstanceRepository},
};
use serde_json::{json, Map, Value};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};

#[derive(Clone)]
pub struct SqlRuntimeInstances {
    pub pool: SqlitePool,
}

impl SqlRuntimeInstances {
    async fn transition_instance_inner(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
        require_unleased: bool,
    ) -> Result<RuntimeInstance, LarmError> {
        instance.validate_record()?;
        if expected_row_version < 1
            || expected_row_version
                .checked_add(1)
                .is_none_or(|next| instance.row_version != next)
        {
            return Err(invalid("runtime_instance_revision_invalid"));
        }
        if require_unleased
            && (instance.state != InstanceState::Stopping
                || instance.ownership != RuntimeOwnership::LarmManaged)
        {
            return Err(invalid("runtime_instance_stop_transition_invalid"));
        }
        let generation = i64::try_from(instance.generation)
            .map_err(|_| invalid("runtime_instance_generation_invalid"))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando transición de runtime_instance", &error))?;
        let row = sqlx::query("SELECT * FROM runtime_instance WHERE id=?")
            .bind(&instance.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("leyendo runtime_instance para transición", &error))?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instancia de runtime desconocida",
                    "runtime_instance_not_found",
                )
                .with_resource(instance.id.clone())
            })?;
        let current = record(&row)?;
        if current.row_version != expected_row_version {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia cambió desde la última lectura",
                "runtime_instance_stale",
            )
            .with_resource(instance.id.clone()));
        }
        if current.created_at != instance.created_at
            || current.runtime_installation_id != instance.runtime_installation_id
            || current.variant_id != instance.variant_id
            || current.backend_id != instance.backend_id
            || current.launch_profile_id != instance.launch_profile_id
            || current.generation != instance.generation
            || current.ownership != instance.ownership
        {
            return Err(invalid("runtime_instance_identity_changed"));
        }
        if !current.state.can_transition_to(instance.state) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Transición de RuntimeInstance no permitida",
                "runtime_instance_transition_invalid",
            )
            .with_resource(instance.id.clone()));
        }
        if require_unleased {
            let active_lease: Option<String> = sqlx::query_scalar(
                "SELECT id FROM runtime_lease WHERE instance_id=? \
                 AND state NOT IN ('EXPIRED','RELEASED','FAILED') LIMIT 1",
            )
            .bind(&instance.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("comprobando leases antes de detener runtime", &error))?;
            if active_lease.is_some() {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "No se puede detener una instancia con leases no terminales",
                    "runtime_instance_has_active_leases",
                )
                .with_resource(instance.id.clone()));
            }
        }
        let updated = sqlx::query(
            "UPDATE runtime_instance SET updated_at=?,row_version=?,metadata=?,state=?,pid=?,\
             process_started_at=?,executable_hash=?,generation=?,execution_fingerprint=?,\
             last_used_at=? WHERE id=? AND row_version=?",
        )
        .bind(&instance.updated_at)
        .bind(instance.row_version)
        .bind(Value::Object(instance.metadata.clone()).to_string())
        .bind(state_name(instance.state))
        .bind(instance.pid.map(i64::from))
        .bind(&instance.process_started_at)
        .bind(&instance.executable_hash)
        .bind(generation)
        .bind(&instance.execution_fingerprint)
        .bind(&instance.last_used_at)
        .bind(&instance.id)
        .bind(expected_row_version)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("actualizando runtime_instance", &error))?;
        if updated.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia cambió desde la última lectura",
                "runtime_instance_stale",
            )
            .with_resource(instance.id.clone()));
        }
        super::events::append(
            &mut tx,
            &super::events::NewEvent {
                event_type: "instance.state_changed",
                resource_type: "runtime_instance",
                resource_id: &instance.id,
                resource_version: instance.row_version,
                data: json!({"from": current.state, "to": instance.state}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando transición de runtime_instance", &error))?;
        Ok(instance.clone())
    }
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Consulta de instancias no válida",
        reason,
    )
}

fn corrupt() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "La instancia persistida no supera la validación",
        "runtime_instance_record_corrupt",
    )
}

pub(crate) fn state_name(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Created => "CREATED",
        InstanceState::Starting => "STARTING",
        InstanceState::Loading => "LOADING",
        InstanceState::Ready => "READY",
        InstanceState::Busy => "BUSY",
        InstanceState::Degraded => "DEGRADED",
        InstanceState::Failed => "FAILED",
        InstanceState::Stopping => "STOPPING",
        InstanceState::Stopped => "STOPPED",
        InstanceState::Crashed => "CRASHED",
    }
}

fn ownership_name(ownership: RuntimeOwnership) -> &'static str {
    match ownership {
        RuntimeOwnership::LarmManaged => "LARM_MANAGED",
        RuntimeOwnership::External => "EXTERNAL",
    }
}

pub(crate) fn record(row: &SqliteRow) -> Result<RuntimeInstance, LarmError> {
    let get_text = |name: &str| row.try_get::<String, _>(name).map_err(|_| corrupt());
    let metadata: String = row.try_get("metadata").map_err(|_| corrupt())?;
    let metadata: Map<String, Value> = serde_json::from_str(&metadata).map_err(|_| corrupt())?;
    let state: InstanceState =
        serde_json::from_value(json!(get_text("state")?)).map_err(|_| corrupt())?;
    let ownership: RuntimeOwnership =
        serde_json::from_value(json!(get_text("ownership")?)).map_err(|_| corrupt())?;
    let pid: Option<i64> = row.try_get("pid").map_err(|_| corrupt())?;
    let generation: i64 = row.try_get("generation").map_err(|_| corrupt())?;
    let instance = RuntimeInstance {
        id: get_text("id")?,
        created_at: get_text("created_at")?,
        updated_at: get_text("updated_at")?,
        row_version: row.try_get("row_version").map_err(|_| corrupt())?,
        metadata,
        runtime_installation_id: get_text("runtime_installation_id")?,
        variant_id: get_text("variant_id")?,
        backend_id: get_text("backend_id")?,
        launch_profile_id: get_text("launch_profile_id")?,
        state,
        pid: pid
            .map(|value| u32::try_from(value).map_err(|_| corrupt()))
            .transpose()?,
        process_started_at: row.try_get("process_started_at").map_err(|_| corrupt())?,
        executable_hash: row.try_get("executable_hash").map_err(|_| corrupt())?,
        generation: u64::try_from(generation).map_err(|_| corrupt())?,
        execution_fingerprint: row
            .try_get("execution_fingerprint")
            .map_err(|_| corrupt())?,
        last_used_at: row.try_get("last_used_at").map_err(|_| corrupt())?,
        ownership,
    };
    instance.validate_record().map_err(|_| corrupt())?;
    Ok(instance)
}

#[async_trait]
impl RuntimeInstanceRepository for SqlRuntimeInstances {
    async fn create_instance(
        &self,
        instance: &RuntimeInstance,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        instance.validate_record()?;
        if instance.state != InstanceState::Created
            || instance.row_version != 1
            || instance.pid.is_some()
            || instance.process_started_at.is_some()
            || instance.generation != 0
        {
            return Err(invalid("runtime_instance_initial_record_invalid"));
        }
        let generation = i64::try_from(instance.generation)
            .map_err(|_| invalid("runtime_instance_generation_invalid"))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando alta de runtime_instance", &error))?;
        let inserted = sqlx::query(
            "INSERT INTO runtime_instance(id,created_at,updated_at,row_version,metadata,\
             runtime_installation_id,variant_id,backend_id,launch_profile_id,state,pid,\
             process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&instance.id)
        .bind(&instance.created_at)
        .bind(&instance.updated_at)
        .bind(instance.row_version)
        .bind(Value::Object(instance.metadata.clone()).to_string())
        .bind(&instance.runtime_installation_id)
        .bind(&instance.variant_id)
        .bind(&instance.backend_id)
        .bind(&instance.launch_profile_id)
        .bind(state_name(instance.state))
        .bind(instance.pid.map(i64::from))
        .bind(&instance.process_started_at)
        .bind(&instance.executable_hash)
        .bind(generation)
        .bind(&instance.execution_fingerprint)
        .bind(&instance.last_used_at)
        .bind(ownership_name(instance.ownership))
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("insertando runtime_instance", &error))?;
        if inserted.rows_affected() != 1 {
            return Err(db_error(
                "insertando runtime_instance",
                &"escritura no confirmada",
            ));
        }
        super::events::append(
            &mut tx,
            &super::events::NewEvent {
                event_type: "instance.state_changed",
                resource_type: "runtime_instance",
                resource_id: &instance.id,
                resource_version: instance.row_version,
                data: json!({"from": null, "to": "CREATED"}),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando alta de runtime_instance", &error))?;
        Ok(())
    }

    async fn register_external_qualified_instance(
        &self,
        instance: &RuntimeInstance,
        endpoint: &InferenceEndpoint,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        instance.validate_record()?;
        endpoint.validate_record()?;
        if ctx.actor_id.trim().is_empty()
            || instance.state != InstanceState::Ready
            || instance.ownership != RuntimeOwnership::External
            || instance.generation == 0
            || instance.pid.is_some()
            || instance.process_started_at.is_some()
            || instance.executable_hash.is_some()
            || instance.execution_fingerprint.is_none()
            || endpoint.instance_id != instance.id
            || endpoint.qualification != EndpointQualification::Qualified
        {
            return Err(invalid("runtime_external_qualified_record_invalid"));
        }
        let generation = i64::try_from(instance.generation)
            .map_err(|_| invalid("runtime_instance_generation_invalid"))?;
        let metadata = serde_json::to_string(&Value::Object(instance.metadata.clone()))
            .map_err(|error| db_error("serializando runtime_instance externo", &error))?;
        let endpoint_metadata = serde_json::to_string(&Value::Object(endpoint.metadata.clone()))
            .map_err(|error| db_error("serializando endpoint externo", &error))?;
        let capabilities = serde_json::to_string(&endpoint.capabilities)
            .map_err(|error| db_error("serializando capacidades del endpoint", &error))?;
        let evidence_refs = serde_json::to_string(&endpoint.evidence_refs)
            .map_err(|error| db_error("serializando evidencia del endpoint", &error))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando registro externo cualificado", &error))?;
        let inserted_instance = sqlx::query(
            "INSERT INTO runtime_instance(id,created_at,updated_at,row_version,metadata,\
             runtime_installation_id,variant_id,backend_id,launch_profile_id,state,pid,\
             process_started_at,executable_hash,generation,execution_fingerprint,last_used_at,ownership) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&instance.id)
        .bind(&instance.created_at)
        .bind(&instance.updated_at)
        .bind(instance.row_version)
        .bind(metadata)
        .bind(&instance.runtime_installation_id)
        .bind(&instance.variant_id)
        .bind(&instance.backend_id)
        .bind(&instance.launch_profile_id)
        .bind(state_name(instance.state))
        .bind(instance.pid.map(i64::from))
        .bind(&instance.process_started_at)
        .bind(&instance.executable_hash)
        .bind(generation)
        .bind(&instance.execution_fingerprint)
        .bind(&instance.last_used_at)
        .bind(ownership_name(instance.ownership))
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("insertando runtime_instance externo", &error))?;
        if inserted_instance.rows_affected() != 1 {
            return Err(db_error(
                "insertando runtime_instance externo",
                &"escritura no confirmada",
            ));
        }
        let inserted_endpoint = sqlx::query(
            "INSERT INTO inference_endpoint(id,created_at,updated_at,row_version,metadata,instance_id,\
             protocol,protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&endpoint.id)
        .bind(&endpoint.created_at)
        .bind(&endpoint.updated_at)
        .bind(endpoint.row_version)
        .bind(endpoint_metadata)
        .bind(&endpoint.instance_id)
        .bind(&endpoint.protocol)
        .bind(&endpoint.protocol_profile_hash)
        .bind(&endpoint.base_url)
        .bind(&endpoint.credential_reference_id)
        .bind(capabilities)
        .bind("QUALIFIED")
        .bind(evidence_refs)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("insertando inference_endpoint externo", &error))?;
        if inserted_endpoint.rows_affected() != 1 {
            return Err(db_error(
                "insertando inference_endpoint externo",
                &"escritura no confirmada",
            ));
        }
        super::events::append(
            &mut tx,
            &super::events::NewEvent {
                event_type: "instance.state_changed",
                resource_type: "runtime_instance",
                resource_id: &instance.id,
                resource_version: instance.row_version,
                data: json!({"from": null, "to": "READY", "ownership": "EXTERNAL"}),
            },
            ctx,
        )
        .await?;
        super::events::append(
            &mut tx,
            &super::events::NewEvent {
                event_type: "runtime.endpoint.registered",
                resource_type: "inference_endpoint",
                resource_id: &endpoint.id,
                resource_version: endpoint.row_version,
                data: json!({
                    "instance_id": endpoint.instance_id,
                    "protocol": endpoint.protocol,
                    "protocol_profile_hash": endpoint.protocol_profile_hash,
                    "qualification": "QUALIFIED"
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando registro externo cualificado", &error))?;
        Ok(())
    }

    async fn instance(&self, id: &str) -> Result<Option<RuntimeInstance>, LarmError> {
        if !is_uuid_v7(id) {
            return Err(invalid("runtime_instance_id_invalid"));
        }
        let row = sqlx::query("SELECT * FROM runtime_instance WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| db_error("leyendo runtime_instance", &error))?;
        row.as_ref().map(record).transpose()
    }

    async fn instances(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        runtime_id: Option<&str>,
    ) -> Result<Vec<RuntimeInstance>, LarmError> {
        if !(1..=201).contains(&limit)
            || runtime_id.is_some_and(|id| !is_uuid_v7(id))
            || after.as_ref().is_some_and(|(_, id)| !is_uuid_v7(id))
        {
            return Err(invalid("runtime_instance_list_query_invalid"));
        }

        let mut sql = String::from("SELECT i.* FROM runtime_instance i");
        if runtime_id.is_some() {
            sql.push_str(
                " JOIN runtime_installation ri ON ri.id=i.runtime_installation_id \
                 JOIN runtime_version rv ON rv.id=ri.runtime_version_id",
            );
        }
        let mut predicates = Vec::new();
        if runtime_id.is_some() {
            predicates.push("rv.runtime_id=?");
        }
        if after.is_some() {
            predicates.push("(i.created_at>? OR (i.created_at=? AND i.id>?))");
        }
        if !predicates.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&predicates.join(" AND "));
        }
        sql.push_str(" ORDER BY i.created_at,i.id LIMIT ?");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        if let Some(runtime_id) = runtime_id {
            query = query.bind(runtime_id);
        }
        if let Some((created_at, id)) = after {
            query = query.bind(&created_at).bind(created_at).bind(id);
        }
        let rows = query
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| db_error("listando runtime_instance", &error))?;
        rows.iter().map(record).collect()
    }

    async fn runtime_exists(&self, id: &str) -> Result<bool, LarmError> {
        if !is_uuid_v7(id) {
            return Err(invalid("runtime_id_invalid"));
        }
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runtime WHERE id=?)")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| db_error("comprobando runtime", &error))
    }

    async fn transition_instance(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError> {
        self.transition_instance_inner(instance, expected_row_version, ctx, false)
            .await
    }

    async fn begin_stop_instance(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError> {
        self.transition_instance_inner(instance, expected_row_version, ctx, true)
            .await
    }
}

#[async_trait]
impl RuntimeInstanceHealthRepository for SqlRuntimeInstances {
    async fn record_health_observation(
        &self,
        instance: &RuntimeInstance,
        expected_row_version: i64,
        ctx: &MutationContext,
    ) -> Result<RuntimeInstance, LarmError> {
        instance.validate_record()?;
        if expected_row_version < 1
            || expected_row_version
                .checked_add(1)
                .is_none_or(|next| instance.row_version != next)
        {
            return Err(invalid("runtime_instance_revision_invalid"));
        }
        let snapshot = instance
            .health_snapshot()?
            .ok_or_else(|| invalid("runtime_health_snapshot_missing"))?;
        if !health_state_consistent(instance.state, &snapshot) {
            return Err(invalid("runtime_health_snapshot_state_mismatch"));
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando observación de salud", &error))?;
        let row = sqlx::query("SELECT * FROM runtime_instance WHERE id=?")
            .bind(&instance.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| db_error("leyendo runtime_instance para health", &error))?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instancia de runtime desconocida",
                    "runtime_instance_not_found",
                )
                .with_resource(instance.id.clone())
            })?;
        let current = record(&row)?;
        if current.row_version != expected_row_version {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia cambió desde la última lectura",
                "runtime_instance_stale",
            )
            .with_resource(instance.id.clone()));
        }
        if current.created_at != instance.created_at
            || current.runtime_installation_id != instance.runtime_installation_id
            || current.variant_id != instance.variant_id
            || current.backend_id != instance.backend_id
            || current.launch_profile_id != instance.launch_profile_id
            || current.generation != instance.generation
            || current.ownership != instance.ownership
            || current.pid != instance.pid
            || current.process_started_at != instance.process_started_at
            || current.executable_hash != instance.executable_hash
            || current.execution_fingerprint != instance.execution_fingerprint
            || current.last_used_at != instance.last_used_at
            || !metadata_differs_only_by_health(&current, instance)
        {
            return Err(invalid("runtime_health_observation_identity_changed"));
        }
        if !matches!(
            current.state,
            InstanceState::Ready | InstanceState::Busy | InstanceState::Degraded
        ) || (current.state != instance.state
            && !current.state.can_transition_to(instance.state))
            || !matches!(
                instance.state,
                InstanceState::Ready
                    | InstanceState::Busy
                    | InstanceState::Degraded
                    | InstanceState::Failed
            )
        {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La observación de salud no permite esta transición",
                "runtime_health_state_invalid",
            )
            .with_resource(instance.id.clone()));
        }

        let updated = sqlx::query(
            "UPDATE runtime_instance SET updated_at=?,row_version=?,metadata=?,state=? \
             WHERE id=? AND row_version=?",
        )
        .bind(&instance.updated_at)
        .bind(instance.row_version)
        .bind(Value::Object(instance.metadata.clone()).to_string())
        .bind(state_name(instance.state))
        .bind(&instance.id)
        .bind(expected_row_version)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("guardando observación de salud", &error))?;
        if updated.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia cambió desde la última lectura",
                "runtime_instance_stale",
            )
            .with_resource(instance.id.clone()));
        }
        super::events::append(
            &mut tx,
            &super::events::NewEvent {
                event_type: "instance.health_observed",
                resource_type: "runtime_instance",
                resource_id: &instance.id,
                resource_version: instance.row_version,
                data: json!({
                    "from": current.state,
                    "to": instance.state,
                    "consecutive_failures": snapshot.consecutive_failures,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando observación de salud", &error))?;
        Ok(instance.clone())
    }
}

fn metadata_differs_only_by_health(current: &RuntimeInstance, proposed: &RuntimeInstance) -> bool {
    let mut current_metadata = current.metadata.clone();
    let mut proposed_metadata = proposed.metadata.clone();
    current_metadata.remove(larm_domain::instances::RUNTIME_HEALTH_SNAPSHOT_KEY);
    proposed_metadata.remove(larm_domain::instances::RUNTIME_HEALTH_SNAPSHOT_KEY);
    current_metadata == proposed_metadata
}

fn health_state_consistent(
    state: InstanceState,
    snapshot: &larm_domain::instances::HealthSnapshot,
) -> bool {
    use InstanceState::{Busy, Degraded, Failed, Ready};
    match state {
        Ready | Busy => snapshot.consecutive_failures <= 1 && snapshot.return_state.is_none(),
        Degraded => {
            (2..5).contains(&snapshot.consecutive_failures)
                && snapshot
                    .return_state
                    .is_some_and(|state| matches!(state, Ready | Busy))
        }
        Failed => snapshot.consecutive_failures == 5 && snapshot.return_state.is_none(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    async fn repo() -> SqlRuntimeInstances {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        for schema in [
            "CREATE TABLE runtime(id TEXT PRIMARY KEY)",
            "CREATE TABLE runtime_version(id TEXT PRIMARY KEY,runtime_id TEXT NOT NULL)",
            "CREATE TABLE runtime_installation(id TEXT PRIMARY KEY,runtime_version_id TEXT NOT NULL)",
            "CREATE TABLE model_variant(id TEXT PRIMARY KEY)",
            "CREATE TABLE runtime_backend(id TEXT PRIMARY KEY)",
            "CREATE TABLE launch_profile(id TEXT PRIMARY KEY)",
            "CREATE TABLE runtime_instance(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,runtime_installation_id TEXT NOT NULL REFERENCES runtime_installation(id),variant_id TEXT NOT NULL REFERENCES model_variant(id),backend_id TEXT NOT NULL REFERENCES runtime_backend(id),launch_profile_id TEXT NOT NULL REFERENCES launch_profile(id),state TEXT NOT NULL,pid INTEGER,process_started_at TEXT,executable_hash TEXT,generation INTEGER NOT NULL,execution_fingerprint TEXT,last_used_at TEXT,ownership TEXT NOT NULL)",
            "CREATE TABLE inference_endpoint(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,instance_id TEXT NOT NULL REFERENCES runtime_instance(id),protocol TEXT NOT NULL,protocol_profile_hash TEXT NOT NULL,base_url TEXT NOT NULL,credential_reference_id TEXT,capabilities TEXT NOT NULL,qualification TEXT NOT NULL,evidence_refs TEXT NOT NULL)",
            "CREATE TABLE runtime_lease(id TEXT PRIMARY KEY,instance_id TEXT,state TEXT NOT NULL)",
            "CREATE TABLE event_sequence_alloc(id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "CREATE TABLE event(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,event_id INTEGER NOT NULL UNIQUE,event_type TEXT NOT NULL,actor_id TEXT NOT NULL,resource_type TEXT NOT NULL,resource_id TEXT NOT NULL,resource_version INTEGER NOT NULL,correlation_id TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL)",
            "CREATE TABLE event_outbox(event_id INTEGER PRIMARY KEY,event_record_id TEXT NOT NULL UNIQUE,delivered_at TEXT)",
        ] {
            sqlx::query(schema).execute(&pool).await.unwrap();
        }
        for (runtime, version, installation) in [(id(10), id(11), id(12)), (id(20), id(21), id(22))]
        {
            sqlx::query("INSERT INTO runtime(id) VALUES(?)")
                .bind(&runtime)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO runtime_version(id,runtime_id) VALUES(?,?)")
                .bind(&version)
                .bind(&runtime)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO runtime_installation(id,runtime_version_id) VALUES(?,?)")
                .bind(installation)
                .bind(version)
                .execute(&pool)
                .await
                .unwrap();
        }
        for (table, value) in [
            ("model_variant", id(30)),
            ("runtime_backend", id(31)),
            ("launch_profile", id(32)),
        ] {
            let sql = format!("INSERT INTO {table}(id) VALUES(?)");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(value)
                .execute(&pool)
                .await
                .unwrap();
        }
        SqlRuntimeInstances { pool }
    }

    async fn insert(repo: &SqlRuntimeInstances, instance_id: u16, runtime_installation_id: &str) {
        sqlx::query("INSERT INTO runtime_instance VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(id(instance_id))
            .bind("2026-10-22T00:00:00.000Z")
            .bind("2026-10-22T00:00:00.000Z")
            .bind(1_i64)
            .bind("{}")
            .bind(runtime_installation_id)
            .bind(id(30))
            .bind(id(31))
            .bind(id(32))
            .bind("READY")
            .bind(Some(1234_i64))
            .bind(Some("2026-10-22T00:00:00.000Z"))
            .bind(Some("a".repeat(64)))
            .bind(0_i64)
            .bind(Some("b".repeat(64)))
            .bind(Option::<String>::None)
            .bind("LARM_MANAGED")
            .execute(&repo.pool)
            .await
            .unwrap();
    }

    fn created_instance(instance_id: u16, runtime_installation_id: &str) -> RuntimeInstance {
        RuntimeInstance {
            id: id(instance_id),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: runtime_installation_id.into(),
            variant_id: id(30),
            backend_id: id(31),
            launch_profile_id: id(32),
            state: InstanceState::Created,
            pid: None,
            process_started_at: None,
            executable_hash: Some("a".repeat(64)),
            generation: 0,
            execution_fingerprint: Some("b".repeat(64)),
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        }
    }

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: "p033-test".into(),
            correlation_id: id(90),
        }
    }

    fn launch_snapshot() -> larm_domain::instances::RuntimeLaunchSnapshot {
        let now = chrono::Utc::now();
        let parameters = larm_domain::launch::LaunchParameters::builtin(id(4), 4, None, 128);
        let mut plan = larm_domain::instances::LaunchPlan {
            plan_id: id(91),
            plan_hash: "0".repeat(64),
            executable: "C:/runtime/runtime.exe".into(),
            args: vec!["--threads".into(), "4".into()],
            cwd: "C:/runtime".into(),
            env_allowlist: parameters.environment.allowlist.clone(),
            port: 24000,
            memory_estimate: larm_domain::estimate::RuntimeMemoryEstimate {
                weights_bytes: None,
                kv_bytes: None,
                activation_bytes: None,
                workspace_bytes: None,
                overhead_bytes: None,
                total_bytes: None,
                per_pool: vec![],
                confidence: larm_domain::estimate::Confidence::Unknown,
                evidence_refs: vec![],
                multimodal_bytes: None,
                temporary_bytes: None,
                safety_margin_bytes: None,
            },
            artifact_ids: vec![],
            expires_at: larm_domain::bridge::timestamp(now + chrono::Duration::minutes(5)),
            effective_parameters: parameters.clone(),
            environment: parameters.environment.clone(),
            effective_memory_limit_bytes: None,
        };
        let mut unsigned = serde_json::to_value(&plan).unwrap();
        unsigned.as_object_mut().unwrap().remove("plan_hash");
        plan.plan_hash = larm_domain::canonical::sha256_hex(&unsigned).unwrap();
        larm_domain::instances::RuntimeLaunchSnapshot::from_plan(plan, now).unwrap()
    }

    #[tokio::test]
    async fn create_transition_and_outbox_are_atomic_and_use_cas() {
        let repo = repo().await;
        let mut created = created_instance(4, &id(12));
        let snapshot = launch_snapshot();
        created.store_launch_snapshot(&snapshot).unwrap();
        repo.create_instance(&created, &ctx()).await.unwrap();
        assert_eq!(
            repo.instance(&created.id).await.unwrap(),
            Some(created.clone())
        );
        assert_eq!(
            repo.instance(&created.id)
                .await
                .unwrap()
                .unwrap()
                .launch_snapshot()
                .unwrap(),
            Some(snapshot)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            1
        );

        let mut starting = created.transition_to(InstanceState::Starting).unwrap();
        starting.pid = Some(4321);
        starting.process_started_at = Some("2026-10-22T00:00:01.000Z".into());
        let persisted = repo
            .transition_instance(&starting, created.row_version, &ctx())
            .await
            .unwrap();
        assert_eq!(persisted.state, InstanceState::Starting);
        assert_eq!(persisted.row_version, 2);
        assert_eq!(
            repo.instance(&created.id).await.unwrap(),
            Some(starting.clone())
        );

        let stale = repo
            .transition_instance(&starting, created.row_version, &ctx())
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleRevision);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            2,
            "un CAS obsoleto no emite evento"
        );

        let mut invalid = starting.clone();
        invalid.state = InstanceState::Stopped;
        invalid.row_version = 3;
        assert_eq!(
            repo.transition_instance(&invalid, starting.row_version, &ctx())
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidStateTransition
        );
    }

    #[tokio::test]
    async fn external_ready_instance_and_qualified_endpoint_commit_together() {
        let repo = repo().await;
        let now = "2026-10-24T12:00:00.000Z";
        let instance = RuntimeInstance {
            id: id(40),
            created_at: now.into(),
            updated_at: now.into(),
            row_version: 1,
            metadata: Map::new(),
            runtime_installation_id: id(12),
            variant_id: id(30),
            backend_id: id(31),
            launch_profile_id: id(32),
            state: InstanceState::Ready,
            pid: None,
            process_started_at: None,
            executable_hash: None,
            generation: 7,
            execution_fingerprint: Some("b".repeat(64)),
            last_used_at: None,
            ownership: RuntimeOwnership::External,
        };
        let endpoint = InferenceEndpoint {
            id: id(41),
            created_at: now.into(),
            updated_at: now.into(),
            row_version: 1,
            metadata: Map::new(),
            instance_id: instance.id.clone(),
            protocol: "openai".into(),
            protocol_profile_hash: "a".repeat(64),
            base_url: "http://127.0.0.1:11434/v1".into(),
            credential_reference_id: None,
            capabilities: vec!["text.generate".into()],
            qualification: EndpointQualification::Qualified,
            evidence_refs: vec!["ollama:api-version:200".into(), "openai:models:200".into()],
        };

        repo.register_external_qualified_instance(&instance, &endpoint, &ctx())
            .await
            .unwrap();
        assert_eq!(repo.instance(&instance.id).await.unwrap(), Some(instance));
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT qualification FROM inference_endpoint WHERE id=?"
            )
            .bind(&endpoint.id)
            .fetch_one(&repo.pool)
            .await
            .unwrap(),
            "QUALIFIED"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn health_snapshot_cas_persiste_contadores_transicion_y_outbox() {
        let repo = repo().await;
        insert(&repo, 4, &id(12)).await;
        let current = repo.instance(&id(4)).await.unwrap().unwrap();
        let mut first = current.clone();
        first.row_version += 1;
        first.updated_at = "2026-10-22T00:00:01.000Z".into();
        first
            .store_health_snapshot(&larm_domain::instances::HealthSnapshot {
                consecutive_failures: 1,
                return_state: None,
            })
            .unwrap();
        let persisted = repo
            .record_health_observation(&first, current.row_version, &ctx())
            .await
            .unwrap();
        assert_eq!(
            persisted
                .health_snapshot()
                .unwrap()
                .unwrap()
                .consecutive_failures,
            1
        );
        assert_eq!(persisted.state, InstanceState::Ready);

        let stale = repo
            .record_health_observation(&first, current.row_version, &ctx())
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleRevision);

        let mut degraded = persisted.clone();
        degraded.row_version += 1;
        degraded.updated_at = "2026-10-22T00:00:02.000Z".into();
        degraded.state = InstanceState::Degraded;
        degraded
            .store_health_snapshot(&larm_domain::instances::HealthSnapshot {
                consecutive_failures: 2,
                return_state: Some(InstanceState::Ready),
            })
            .unwrap();
        let persisted = repo
            .record_health_observation(&degraded, persisted.row_version, &ctx())
            .await
            .unwrap();
        assert_eq!(persisted.state, InstanceState::Degraded);
        assert_eq!(
            persisted.health_snapshot().unwrap().unwrap().return_state,
            Some(InstanceState::Ready)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn health_cas_no_permite_editar_identidad_o_metadata_ajena() {
        let repo = repo().await;
        insert(&repo, 4, &id(12)).await;
        let current = repo.instance(&id(4)).await.unwrap().unwrap();
        let mut proposed = current.clone();
        proposed.row_version += 1;
        proposed.updated_at = "2026-10-22T00:00:01.000Z".into();
        proposed.backend_id = id(99);
        proposed
            .store_health_snapshot(&larm_domain::instances::HealthSnapshot {
                consecutive_failures: 1,
                return_state: None,
            })
            .unwrap();
        assert_eq!(
            repo.record_health_observation(&proposed, current.row_version, &ctx())
                .await
                .unwrap_err()
                .details
                .reason,
            "runtime_health_observation_identity_changed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn stop_cas_rechaza_toda_lease_no_terminal_y_confirma_sin_lease() {
        let repo = repo().await;
        insert(&repo, 4, &id(12)).await;
        let current = repo.instance(&id(4)).await.unwrap().unwrap();
        let stopping = current.transition_to(InstanceState::Stopping).unwrap();

        for (n, state) in ["REQUESTED", "ADMITTING", "ACTIVE", "EXPIRING"]
            .into_iter()
            .enumerate()
        {
            sqlx::query("INSERT INTO runtime_lease(id,instance_id,state) VALUES(?,?,?)")
                .bind(id(100 + n as u16))
                .bind(&current.id)
                .bind(state)
                .execute(&repo.pool)
                .await
                .unwrap();
            let error = repo
                .begin_stop_instance(&stopping, current.row_version, &ctx())
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(error.details.reason, "runtime_instance_has_active_leases");
            assert_eq!(
                repo.instance(&current.id).await.unwrap(),
                Some(current.clone())
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                    .fetch_one(&repo.pool)
                    .await
                    .unwrap(),
                0,
                "una solicitud de stop bloqueada no escribe evento"
            );
        }

        sqlx::query("UPDATE runtime_lease SET state='RELEASED'")
            .execute(&repo.pool)
            .await
            .unwrap();
        let stopped = repo
            .begin_stop_instance(&stopping, current.row_version, &ctx())
            .await
            .unwrap();
        assert_eq!(stopped.state, InstanceState::Stopping);
        assert_eq!(stopped.row_version, current.row_version + 1);
        assert_eq!(repo.instance(&current.id).await.unwrap(), Some(stopping));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn failed_outbox_insert_rolls_back_instance_and_event() {
        let repo = repo().await;
        sqlx::query(
            "CREATE TRIGGER fail_instance_outbox BEFORE INSERT ON event_outbox \
             BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END",
        )
        .execute(&repo.pool)
        .await
        .unwrap();
        let error = repo
            .create_instance(&created_instance(5, &id(12)), &ctx())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runtime_instance")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&repo.pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn detail_pagination_and_runtime_filter_return_contractual_rows() {
        let repo = repo().await;
        insert(&repo, 1, &id(12)).await;
        insert(&repo, 2, &id(22)).await;
        insert(&repo, 3, &id(12)).await;

        let first = repo.instances(None, 2, None).await.unwrap();
        assert_eq!(
            first.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            [id(1), id(2)]
        );
        let next = repo
            .instances(
                Some((first[1].created_at.clone(), first[1].id.clone())),
                2,
                None,
            )
            .await
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, id(3));
        assert_eq!(repo.instance(&id(2)).await.unwrap(), Some(first[1].clone()));
        assert_eq!(
            repo.instances(None, 10, Some(&id(10))).await.unwrap().len(),
            2
        );
        assert!(repo.runtime_exists(&id(10)).await.unwrap());
        assert!(!repo.runtime_exists(&id(99)).await.unwrap());
    }

    #[tokio::test]
    async fn corruption_and_invalid_queries_fail_closed() {
        let repo = repo().await;
        insert(&repo, 1, &id(12)).await;
        sqlx::query("UPDATE runtime_instance SET state='READYISH' WHERE id=?")
            .bind(id(1))
            .execute(&repo.pool)
            .await
            .unwrap();
        assert_eq!(
            repo.instance(&id(1)).await.unwrap_err().code,
            ErrorCode::IntegrityFailed
        );
        assert_eq!(
            repo.instance("bad-id").await.unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            repo.instances(None, 0, None).await.unwrap_err().code,
            ErrorCode::ValidationFailed
        );
    }
}
