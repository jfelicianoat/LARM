//! Repositorio SQLite de `hardware_profile` (P010): una fila por
//! configuración. Un sondeo con la misma huella que el perfil más reciente lo
//! actualiza; una huella distinta (driver, build, RAM…) crea otro perfil y el
//! anterior queda como historia.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::hardware::{HardwareProbe, HardwareProfile};
use larm_domain::ports::HardwareRepository;
use sqlx::{Row, SqlitePool};

use super::pool::db_error;

const COLUMNS: &str =
    "id,created_at,updated_at,row_version,metadata,hardware_fingerprint,snapshot,evidence_refs,probed_at";

fn invalid(reason: &str, detail: impl Into<String>) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Sondeo de hardware incoherente",
        reason,
    )
    .with_technical(detail)
}

fn text<T: serde::Serialize>(value: &T) -> Result<String, LarmError> {
    serde_json::to_string(value).map_err(|e| db_error("serializando", &e))
}

fn to_profile(row: &sqlx::sqlite::SqliteRow) -> Result<HardwareProfile, LarmError> {
    let get = |name: &str| -> Result<String, LarmError> {
        row.try_get::<String, _>(name)
            .map_err(|e| db_error(name, &e))
    };
    let json = |name: &str| -> Result<serde_json::Value, LarmError> {
        serde_json::from_str(&get(name)?).map_err(|e| db_error(name, &e))
    };
    Ok(HardwareProfile {
        id: get("id")?,
        created_at: get("created_at")?,
        updated_at: get("updated_at")?,
        row_version: row
            .try_get("row_version")
            .map_err(|e| db_error("row_version", &e))?,
        metadata: serde_json::from_value(json("metadata")?)
            .map_err(|e| db_error("metadata", &e))?,
        hardware_fingerprint: get("hardware_fingerprint")?,
        snapshot: serde_json::from_value(json("snapshot")?)
            .map_err(|e| db_error("snapshot fuera del schema", &e))?,
        evidence_refs: serde_json::from_value(json("evidence_refs")?)
            .map_err(|e| db_error("evidence_refs", &e))?,
        probed_at: get("probed_at")?,
    })
}

/// `hardware_profile` sobre SQLite.
#[derive(Clone)]
pub struct SqlHardware {
    pub pool: SqlitePool,
}

impl SqlHardware {
    async fn by_id(&self, id: &str) -> Result<HardwareProfile, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM hardware_profile WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| db_error("leyendo hardware_profile", &e))?;
        to_profile(&row)
    }
}

#[async_trait]
impl HardwareRepository for SqlHardware {
    async fn latest(&self) -> Result<Option<HardwareProfile>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM hardware_profile ORDER BY created_at DESC, id DESC LIMIT 1"
        )))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo el último hardware_profile", &e))?;
        row.as_ref().map(to_profile).transpose()
    }

    async fn record(&self, probe: &HardwareProbe) -> Result<HardwareProfile, LarmError> {
        probe
            .snapshot
            .validate()
            .map_err(|d| invalid("hardware_snapshot_invalid", d))?;
        let snapshot = probe.snapshot.clone().canonical();
        let fingerprint = snapshot
            .fingerprint()
            .map_err(|d| invalid("hardware_snapshot_invalid", d))?;
        let snapshot_text = text(&snapshot)?;
        let metadata_text = text(&probe.metadata)?;
        let evidence_text = text(&probe.evidence_refs)?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());

        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        // El UPDATE toma el bloqueo de escritura antes de decidir: dos sondeos
        // simultáneos de la misma configuración no crean dos perfiles.
        // Misma configuración: se conserva el diff que se calculó cuando apareció.
        let updated = sqlx::query(
            "UPDATE hardware_profile SET updated_at = ?, row_version = row_version + 1, \
             metadata = CASE WHEN json_type(metadata, '$.fingerprint') IS NULL THEN ? \
                        ELSE json_set(?, '$.fingerprint', json_extract(metadata, '$.fingerprint')) END, \
             snapshot = ?, evidence_refs = ?, probed_at = ? \
             WHERE id = (SELECT id FROM hardware_profile ORDER BY created_at DESC, id DESC LIMIT 1) \
             AND hardware_fingerprint = ? RETURNING id",
        )
        .bind(&now)
        .bind(&metadata_text)
        .bind(&metadata_text)
        .bind(&snapshot_text)
        .bind(&evidence_text)
        .bind(&probe.probed_at)
        .bind(&fingerprint)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando hardware_profile", &e))?;
        let id = if let Some(row) = updated {
            row.try_get::<String, _>("id")
                .map_err(|e| db_error("id", &e))?
        } else {
            let id = uuid::Uuid::now_v7().to_string();
            // Configuración nueva: versión de la huella y diff con la anterior.
            let previous = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLUMNS} FROM hardware_profile ORDER BY created_at DESC, id DESC LIMIT 1"
            )))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("leyendo el perfil anterior", &e))?
            .map(|row| to_profile(&row))
            .transpose()?;
            let mut metadata = probe.metadata.clone();
            metadata.insert(
                "fingerprint".into(),
                serde_json::json!({
                    "version": larm_domain::hardware::FINGERPRINT_VERSION,
                    "previous_profile_id": previous.as_ref().map(|p| p.id.clone()),
                    "previous_fingerprint": previous.as_ref().map(|p| p.hardware_fingerprint.clone()),
                    "changes": previous
                        .as_ref()
                        .map(|p| larm_domain::hardware::fingerprint_diff(&p.snapshot, &snapshot))
                        .unwrap_or_default(),
                }),
            );
            let metadata_text = text(&metadata)?;
            sqlx::query(
                "INSERT INTO hardware_profile (id, created_at, updated_at, row_version, metadata, \
                 hardware_fingerprint, snapshot, evidence_refs, probed_at) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&now)
            .bind(&now)
            .bind(&metadata_text)
            .bind(&fingerprint)
            .bind(&snapshot_text)
            .bind(&evidence_text)
            .bind(&probe.probed_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("insertando hardware_profile", &e))?;
            id
        };
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        self.by_id(&id).await
    }

    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
    ) -> Result<Vec<HardwareProfile>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM hardware_profile WHERE (created_at, id) > (?, ?) \
             ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando hardware_profile", &e))?;
        rows.iter().map(to_profile).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<HardwareProfile>, LarmError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM hardware_profile WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo hardware_profile", &e))?;
        row.as_ref().map(to_profile).transpose()
    }
}
