//! Lectura de evaluaciones de compatibilidad persistidas (P090).

use async_trait::async_trait;
use larm_domain::compatibility::CompatibilityRecord;
use larm_domain::error::LarmError;
use larm_domain::ports::CompatibilityRepository;
use sqlx::SqlitePool;

use crate::pool::db_error;

#[derive(Clone)]
pub struct SqlCompatibilityRepository {
    pub pool: SqlitePool,
}

#[async_trait]
impl CompatibilityRepository for SqlCompatibilityRepository {
    async fn list_all_compatibility(&self) -> Result<Vec<CompatibilityRecord>, LarmError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT json_object('id',id,'created_at',created_at,'updated_at',updated_at,'row_version',row_version,'metadata',json(metadata),'variant_id',variant_id,'runtime_version_id',runtime_version_id,'backend_id',backend_id,'hardware_fingerprint',hardware_fingerprint,'launch_profile_hash',launch_profile_hash,'status',status,'hard_gates',json(hard_gates),'limitations',json(limitations),'technical_fit',technical_fit,'evidence_refs',json(evidence_refs)) FROM compatibility_record ORDER BY created_at,id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("leyendo compatibility_record", &error))?;

        rows.into_iter()
            .map(|row| {
                let record: CompatibilityRecord = serde_json::from_str(&row)
                    .map_err(|error| db_error("decodificando compatibility_record", &error))?;
                record
                    .validate()
                    .map_err(|error| db_error("validando compatibility_record", &error))?;
                Ok(record)
            })
            .collect()
    }
}
