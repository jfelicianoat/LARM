//! Repositorio SQLite de `adapter_installation` (P023). El alta escribe
//! `adapter.updated` en el outbox en la misma transacción.

use async_trait::async_trait;
use larm_domain::adapters::{AdapterFilter, AdapterInstallation};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterCatalogRepository, AdapterInstallationRepository, MutationContext,
};
use sqlx::{Row, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

pub(crate) const COLUMNS: &str =
    "id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state";

pub(crate) fn to_installation(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<AdapterInstallation, LarmError> {
    let text = |c: &str| row.try_get::<String, _>(c).map_err(|e| db_error(c, &e));
    let json = |c: &str| -> Result<serde_json::Value, LarmError> {
        serde_json::from_str(&text(c)?).map_err(|e| db_error(c, &e))
    };
    let enum_of = |c: &str| -> Result<serde_json::Value, LarmError> {
        Ok(serde_json::Value::String(text(c)?))
    };
    Ok(AdapterInstallation {
        id: text("id")?,
        created_at: text("created_at")?,
        updated_at: text("updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: serde_json::from_value(json("metadata")?)
            .map_err(|e| db_error("metadata", &e))?,
        adapter_id: text("adapter_id")?,
        version: text("version")?,
        family: text("family")?,
        package_hash: text("package_hash")?,
        publisher: text("publisher")?,
        trust: serde_json::from_value(enum_of("trust")?).map_err(|e| db_error("trust", &e))?,
        manifest: json("manifest")?,
        state: serde_json::from_value(enum_of("state")?).map_err(|e| db_error("state", &e))?,
    })
}

/// Alta y evento reutilizables en la transacción que completa un job.
pub(crate) async fn insert_in(
    conn: &mut sqlx::SqliteConnection,
    a: &AdapterInstallation,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    insert_row_in(conn, a).await?;
    append(
        &mut *conn,
        &NewEvent {
            event_type: "adapter.updated",
            resource_type: "adapter_installation",
            resource_id: &a.id,
            resource_version: a.row_version,
            data: serde_json::json!({
                "action": "created",
                "adapter_id": a.adapter_id,
                "version": a.version,
                "state": a.state.as_str(),
                "trust": a.trust.as_str(),
            }),
        },
        ctx,
    )
    .await?;
    Ok(())
}

/// Inserta sólo la fila. Flujos con un evento propio (P028) lo enlazan a su
/// journal dentro de la misma transacción sin emitir un evento de alta P023.
pub(crate) async fn insert_row_in(
    conn: &mut sqlx::SqliteConnection,
    a: &AdapterInstallation,
) -> Result<(), LarmError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO adapter_installation ({COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&a.id)
    .bind(&a.created_at)
    .bind(&a.updated_at)
    .bind(a.row_version)
    .bind(serde_json::to_string(&a.metadata).unwrap_or_else(|_| "{}".into()))
    .bind(&a.adapter_id)
    .bind(&a.version)
    .bind(&a.family)
    .bind(&a.package_hash)
    .bind(&a.publisher)
    .bind(a.trust.as_str())
    .bind(serde_json::to_string(&a.manifest).unwrap_or_else(|_| "{}".into()))
    .bind(a.state.as_str())
    .execute(&mut *conn)
    .await
    .map_err(|e| {
        if e.as_database_error()
            .is_some_and(|e| e.is_unique_violation())
        {
            LarmError::new(
                ErrorCode::Conflict,
                "La versión del adaptador ya está registrada",
                "adapter_version_exists",
            )
            .with_resource(&a.adapter_id)
        } else {
            db_error("insertando adapter_installation", &e)
        }
    })?;
    Ok(())
}

#[derive(Clone)]
pub struct SqlAdapterInstallations {
    pub pool: SqlitePool,
}

#[async_trait]
impl AdapterCatalogRepository for SqlAdapterInstallations {
    async fn page(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &AdapterFilter,
    ) -> Result<Vec<AdapterInstallation>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let state = filter.state.map(|s| s.as_str());
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM adapter_installation WHERE (created_at,id) > (?,?) \
             AND (? IS NULL OR instr(lower(adapter_id || ' ' || version || ' ' || publisher),lower(?)) > 0) \
             AND (? IS NULL OR state = ?) ORDER BY created_at,id LIMIT ?"
        )))
        .bind(created).bind(id).bind(&filter.q).bind(&filter.q).bind(state).bind(state).bind(i64::from(limit))
        .fetch_all(&self.pool).await.map_err(|e| db_error("listando adapter_installation", &e))?;
        rows.iter().map(to_installation).collect()
    }
}

#[async_trait]
impl AdapterInstallationRepository for SqlAdapterInstallations {
    async fn insert(
        &self,
        a: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        insert_in(&mut tx, a, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn get(&self, id: &str) -> Result<Option<AdapterInstallation>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM adapter_installation WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo adapter_installation", &e))?;
        row.as_ref().map(to_installation).transpose()
    }

    async fn find(
        &self,
        adapter_id: &str,
        version: &str,
    ) -> Result<Option<AdapterInstallation>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM adapter_installation WHERE adapter_id = ? AND version = ?"
        )))
        .bind(adapter_id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo adapter_installation", &e))?;
        row.as_ref().map(to_installation).transpose()
    }
}
