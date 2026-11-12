//! SQLite registry for protocol-qualified instance endpoints (P036).

use std::collections::BTreeSet;

use async_trait::async_trait;
use larm_domain::{
    bridge::{is_uuid_v7, timestamp},
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    protocol::{EndpointQualification, InferenceEndpoint},
    runtime_ports::InferenceEndpointRepository,
};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::{
    events::{append, NewEvent},
    pool::db_error,
};

#[derive(Clone)]
pub struct SqlInferenceEndpoints {
    pub pool: SqlitePool,
}

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Endpoint o contexto de mutación no válido",
        reason,
    )
}

fn corrupt() -> LarmError {
    LarmError::new(
        ErrorCode::IntegrityFailed,
        "El endpoint persistido no supera la verificación",
        "inference_endpoint_corrupt",
    )
}

fn validate_context(ctx: &MutationContext) -> Result<(), LarmError> {
    if ctx.actor_id.trim().is_empty() || !is_uuid_v7(&ctx.correlation_id) {
        return Err(invalid("inference_endpoint_actor_invalid"));
    }
    Ok(())
}

fn qualification(value: &str) -> Option<EndpointQualification> {
    match value {
        "UNTESTED" => Some(EndpointQualification::Untested),
        "QUALIFIED" => Some(EndpointQualification::Qualified),
        "FAILED" => Some(EndpointQualification::Failed),
        _ => None,
    }
}

async fn reserve_writer(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE inference_endpoint SET id=id WHERE 0")
        .execute(&mut *conn)
        .await
        .map_err(|error| db_error("reservando escritura inference_endpoint", &error))?;
    Ok(())
}

async fn read_row(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<InferenceEndpoint>, LarmError> {
    let row = sqlx::query(
        "SELECT id,created_at,updated_at,row_version,metadata,instance_id,protocol,\
         protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs \
         FROM inference_endpoint WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|error| db_error("leyendo inference_endpoint", &error))?;
    row.map(parse_row).transpose()
}

fn parse_row(row: sqlx::sqlite::SqliteRow) -> Result<InferenceEndpoint, LarmError> {
    let endpoint = InferenceEndpoint {
        id: row.try_get("id").map_err(|_| corrupt())?,
        created_at: row.try_get("created_at").map_err(|_| corrupt())?,
        updated_at: row.try_get("updated_at").map_err(|_| corrupt())?,
        row_version: row.try_get("row_version").map_err(|_| corrupt())?,
        metadata: parse_json(row.try_get("metadata").map_err(|_| corrupt())?)?,
        instance_id: row.try_get("instance_id").map_err(|_| corrupt())?,
        protocol: row.try_get("protocol").map_err(|_| corrupt())?,
        protocol_profile_hash: row
            .try_get("protocol_profile_hash")
            .map_err(|_| corrupt())?,
        base_url: row.try_get("base_url").map_err(|_| corrupt())?,
        credential_reference_id: row
            .try_get("credential_reference_id")
            .map_err(|_| corrupt())?,
        capabilities: parse_json(row.try_get("capabilities").map_err(|_| corrupt())?)?,
        qualification: qualification(
            &row.try_get::<String, _>("qualification")
                .map_err(|_| corrupt())?,
        )
        .ok_or_else(corrupt)?,
        evidence_refs: parse_json(row.try_get("evidence_refs").map_err(|_| corrupt())?)?,
    };
    endpoint.validate_record().map_err(|_| corrupt())?;
    Ok(endpoint)
}

fn parse_json<T: serde::de::DeserializeOwned>(value: String) -> Result<T, LarmError> {
    serde_json::from_str(&value).map_err(|_| corrupt())
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<String, LarmError> {
    serde_json::to_string(value).map_err(|_| invalid("inference_endpoint_serialization_invalid"))
}

#[async_trait]
impl InferenceEndpointRepository for SqlInferenceEndpoints {
    async fn register_endpoint(
        &self,
        endpoint: &InferenceEndpoint,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        endpoint.validate_record()?;
        if endpoint.qualification == EndpointQualification::Untested
            || endpoint.evidence_refs.is_empty()
        {
            return Err(invalid("inference_endpoint_probe_required"));
        }
        validate_context(ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando register inference_endpoint", &error))?;
        reserve_writer(&mut tx).await?;
        let inserted = sqlx::query(
            "INSERT INTO inference_endpoint(id,created_at,updated_at,row_version,metadata,instance_id,\
             protocol,protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&endpoint.id)
        .bind(&endpoint.created_at)
        .bind(&endpoint.updated_at)
        .bind(endpoint.row_version)
        .bind(encode_json(&endpoint.metadata)?)
        .bind(&endpoint.instance_id)
        .bind(&endpoint.protocol)
        .bind(&endpoint.protocol_profile_hash)
        .bind(&endpoint.base_url)
        .bind(&endpoint.credential_reference_id)
        .bind(encode_json(&endpoint.capabilities)?)
        .bind(match endpoint.qualification {
            EndpointQualification::Untested => "UNTESTED",
            EndpointQualification::Qualified => "QUALIFIED",
            EndpointQualification::Failed => "FAILED",
        })
        .bind(encode_json(&endpoint.evidence_refs)?)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("insertando inference_endpoint", &error))?;
        if inserted.rows_affected() != 1 {
            return Err(db_error(
                "insertando inference_endpoint",
                &"escritura no confirmada",
            ));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.endpoint.registered",
                resource_type: "inference_endpoint",
                resource_id: &endpoint.id,
                resource_version: endpoint.row_version,
                data: serde_json::json!({
                    "instance_id": endpoint.instance_id,
                    "protocol": endpoint.protocol,
                    "protocol_profile_hash": endpoint.protocol_profile_hash,
                    "qualification": match endpoint.qualification {
                        EndpointQualification::Qualified => "QUALIFIED",
                        EndpointQualification::Failed => "FAILED",
                        EndpointQualification::Untested => "UNTESTED",
                    }
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando register inference_endpoint", &error))?;
        Ok(())
    }

    async fn endpoint(&self, id: &str) -> Result<Option<InferenceEndpoint>, LarmError> {
        if !is_uuid_v7(id) {
            return Err(invalid("inference_endpoint_id_invalid"));
        }
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| db_error("abriendo inference_endpoint", &error))?;
        read_row(&mut conn, id).await
    }

    async fn endpoints_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<InferenceEndpoint>, LarmError> {
        if !is_uuid_v7(instance_id) {
            return Err(invalid("inference_endpoint_instance_id_invalid"));
        }
        let rows = sqlx::query(
            "SELECT id,created_at,updated_at,row_version,metadata,instance_id,protocol,\
             protocol_profile_hash,base_url,credential_reference_id,capabilities,qualification,evidence_refs \
             FROM inference_endpoint WHERE instance_id=? ORDER BY created_at,id",
        )
        .bind(instance_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("listando inference_endpoint", &error))?;
        rows.into_iter().map(parse_row).collect()
    }

    async fn fail_endpoint(
        &self,
        id: &str,
        expected_row_version: i64,
        evidence_refs: &[String],
        ctx: &MutationContext,
    ) -> Result<InferenceEndpoint, LarmError> {
        if !is_uuid_v7(id)
            || expected_row_version < 1
            || evidence_refs.is_empty()
            || evidence_refs
                .iter()
                .any(|reference| reference.trim().is_empty())
        {
            return Err(invalid("inference_endpoint_failure_invalid"));
        }
        validate_context(ctx)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("iniciando fail inference_endpoint", &error))?;
        reserve_writer(&mut tx).await?;
        let mut current = read_row(&mut tx, id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Endpoint desconocido",
                "inference_endpoint_not_found",
            )
            .with_resource(id.to_owned())
        })?;
        if current.row_version != expected_row_version {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El endpoint cambió desde que se leyó",
                "stale_revision",
            )
            .with_resource(id.to_owned()));
        }
        if current.qualification == EndpointQualification::Failed {
            tx.commit()
                .await
                .map_err(|error| db_error("confirmando endpoint ya fallido", &error))?;
            return Ok(current);
        }
        let mut merged = current
            .evidence_refs
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        merged.extend(evidence_refs.iter().cloned());
        current.evidence_refs = merged.into_iter().collect();
        current.qualification = EndpointQualification::Failed;
        current.row_version += 1;
        current.updated_at = timestamp(chrono::Utc::now());
        current.validate_record()?;
        let updated = sqlx::query(
            "UPDATE inference_endpoint SET updated_at=?,row_version=?,qualification='FAILED',evidence_refs=? \
             WHERE id=? AND row_version=?",
        )
        .bind(&current.updated_at)
        .bind(current.row_version)
        .bind(encode_json(&current.evidence_refs)?)
        .bind(id)
        .bind(expected_row_version)
        .execute(&mut *tx)
        .await
        .map_err(|error| db_error("actualizando inference_endpoint", &error))?;
        if updated.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El endpoint cambió desde que se leyó",
                "stale_revision",
            )
            .with_resource(id.to_owned()));
        }
        append(
            &mut tx,
            &NewEvent {
                event_type: "runtime.endpoint.failed",
                resource_type: "inference_endpoint",
                resource_id: id,
                resource_version: current.row_version,
                data: serde_json::json!({
                    "instance_id": current.instance_id,
                    "protocol": current.protocol,
                    "reason": "endpoint_probe_failed",
                    "evidence_refs": current.evidence_refs,
                }),
            },
            ctx,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| db_error("confirmando fail inference_endpoint", &error))?;
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use serde_json::Map;
    use sqlx::sqlite::SqlitePoolOptions;

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    async fn repo() -> SqlInferenceEndpoints {
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
            "CREATE TABLE runtime_instance(id TEXT PRIMARY KEY)",
            "CREATE TABLE inference_endpoint(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,instance_id TEXT NOT NULL REFERENCES runtime_instance(id),protocol TEXT NOT NULL,protocol_profile_hash TEXT NOT NULL,base_url TEXT NOT NULL,credential_reference_id TEXT,capabilities TEXT NOT NULL,qualification TEXT NOT NULL,evidence_refs TEXT NOT NULL)",
            "CREATE TABLE event_sequence_alloc(id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "CREATE TABLE event(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,event_id INTEGER NOT NULL UNIQUE,event_type TEXT NOT NULL,actor_id TEXT NOT NULL,resource_type TEXT NOT NULL,resource_id TEXT NOT NULL,resource_version INTEGER NOT NULL,correlation_id TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL)",
            "CREATE TABLE event_outbox(event_id INTEGER PRIMARY KEY,event_record_id TEXT NOT NULL UNIQUE,delivered_at TEXT)",
        ] {
            sqlx::query(schema).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO runtime_instance(id) VALUES(?)")
            .bind(id(1))
            .execute(&pool)
            .await
            .unwrap();
        SqlInferenceEndpoints { pool }
    }

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: "p036-test".into(),
            correlation_id: id(90),
        }
    }

    fn endpoint(n: u16, protocol: &str) -> InferenceEndpoint {
        InferenceEndpoint {
            id: id(n),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            instance_id: id(1),
            protocol: protocol.into(),
            protocol_profile_hash: "a".repeat(64),
            base_url: format!("http://127.0.0.1:18{n:03}/v1"),
            credential_reference_id: None,
            capabilities: vec!["text.generate".into()],
            qualification: EndpointQualification::Qualified,
            evidence_refs: vec![format!("probe:{n}")],
        }
    }

    #[tokio::test]
    async fn permite_multiples_protocolos_y_mantiene_lectura_solo_de_calificados() {
        let repo = repo().await;
        repo.register_endpoint(&endpoint(2, "openai"), &ctx())
            .await
            .unwrap();
        repo.register_endpoint(&endpoint(3, "vendor.protocol/v7"), &ctx())
            .await
            .unwrap();
        let rows = repo.endpoints_for_instance(&id(1)).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].protocol, "openai");
        assert_eq!(rows[1].protocol, "vendor.protocol/v7");
        assert_eq!(
            repo.qualified_endpoints_for_instance(&id(1))
                .await
                .unwrap()
                .len(),
            2
        );
        let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(event_count, 2);
    }

    #[tokio::test]
    async fn endpoint_muerto_pasa_a_failed_con_cas_y_sale_de_la_lectura_del_broker() {
        let repo = repo().await;
        repo.register_endpoint(&endpoint(2, "openai"), &ctx())
            .await
            .unwrap();
        let failed = repo
            .fail_endpoint(&id(2), 1, &["probe:connection-refused".into()], &ctx())
            .await
            .unwrap();
        assert_eq!(failed.qualification, EndpointQualification::Failed);
        assert_eq!(failed.row_version, 2);
        assert_eq!(
            repo.qualified_endpoints_for_instance(&id(1))
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            repo.fail_endpoint(&id(2), 1, &["probe:stale".into()], &ctx())
                .await
                .unwrap_err()
                .code,
            ErrorCode::StaleRevision
        );
    }

    #[tokio::test]
    async fn endpoint_sin_probe_y_fallo_de_outbox_no_dejan_estado_parcial() {
        let repo = repo().await;
        let mut untested = endpoint(2, "openai");
        untested.qualification = EndpointQualification::Untested;
        untested.evidence_refs.clear();
        assert_eq!(
            repo.register_endpoint(&untested, &ctx())
                .await
                .unwrap_err()
                .details
                .reason,
            "inference_endpoint_probe_required"
        );
        sqlx::query("CREATE TRIGGER reject_endpoint_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'injected outbox failure'); END")
            .execute(&repo.pool)
            .await
            .unwrap();
        assert!(repo
            .register_endpoint(&endpoint(2, "openai"), &ctx())
            .await
            .is_err());
        let endpoints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inference_endpoint")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(endpoints, 0);
        assert_eq!(events, 0);
    }
}
