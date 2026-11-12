//! Checkpoints durables de artefactos P051.

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::pool::db_error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadControl {
    Continue,
    Pause,
    Cancel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCheckpoint {
    pub durable_offset: DecimalU64,
    pub resume_validator: Option<Value>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub revision_key: Option<String>,
}

#[derive(Clone)]
pub struct SqlDownloadProgress {
    pub pool: SqlitePool,
}

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "No se pudo guardar el progreso de descarga",
        reason,
    )
}

fn validate_identity(
    job_id: &str,
    artifact_id: &str,
    offset: Option<u64>,
) -> Result<(), LarmError> {
    if !larm_domain::bridge::is_uuid_v7(job_id)
        || !larm_domain::bridge::is_uuid_v7(artifact_id)
        || offset.is_some_and(|value| DecimalU64::new(value).is_none())
    {
        return Err(invalid("download_checkpoint_identity_or_offset_invalid"));
    }
    Ok(())
}

fn approved_artifact(metadata: &str, artifact_id: &str) -> Result<bool, LarmError> {
    let metadata: Value =
        serde_json::from_str(metadata).map_err(|e| db_error("metadata de descarga", &e))?;
    Ok(metadata
        .get("artifacts")
        .and_then(Value::as_array)
        .is_some_and(|artifacts| {
            artifacts.iter().any(|artifact| {
                artifact.get("artifact_id").and_then(Value::as_str) == Some(artifact_id)
            })
        }))
}

fn checkpoint_from_progress(
    progress: &str,
    artifact_id: &str,
) -> Result<ArtifactCheckpoint, LarmError> {
    let progress: Value =
        serde_json::from_str(progress).map_err(|e| db_error("progreso de descarga", &e))?;
    let Some(value) = progress
        .get("artifacts")
        .and_then(Value::as_object)
        .and_then(|artifacts| artifacts.get(artifact_id))
    else {
        return Ok(ArtifactCheckpoint::default());
    };
    serde_json::from_value(value.clone()).map_err(|e| db_error("checkpoint de artefacto", &e))
}

async fn lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    sqlx::query("UPDATE download_job SET updated_at=updated_at WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando checkpoint de descarga", &e))?;
    Ok(())
}

impl SqlDownloadProgress {
    /// Recupera el último offset y su validador como un único checkpoint.
    pub async fn load_checkpoint(
        &self,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<ArtifactCheckpoint, LarmError> {
        validate_identity(job_id, artifact_id, None)?;
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT metadata,progress FROM download_job WHERE id=?")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| db_error("leyendo checkpoint de descarga", &e))?;
        let Some((metadata, progress)) = row else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "No se encontró el trabajo de descarga",
                "download_job_not_found",
            )
            .with_resource(job_id));
        };
        if !approved_artifact(&metadata, artifact_id)? {
            return Err(
                invalid("download_checkpoint_artifact_not_selected").with_resource(artifact_id)
            );
        }
        checkpoint_from_progress(&progress, artifact_id)
    }

    /// Guarda offset y validador en la misma escritura SQLite, después de que
    /// staging haya sincronizado esos bytes a disco.
    pub async fn checkpoint(
        &self,
        job_id: &str,
        artifact_id: &str,
        durable_offset: u64,
        resume_validator: Option<Value>,
    ) -> Result<DownloadControl, LarmError> {
        validate_identity(job_id, artifact_id, Some(durable_offset))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN checkpoint de descarga", &e))?;
        lock(&mut tx).await?;
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT state,metadata,progress FROM download_job WHERE id=?")
                .bind(job_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo trabajo para checkpoint", &e))?;
        let Some((state, metadata, progress)) = row else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "No se encontró el trabajo de descarga",
                "download_job_not_found",
            )
            .with_resource(job_id));
        };
        if !approved_artifact(&metadata, artifact_id)? {
            return Err(
                invalid("download_checkpoint_artifact_not_selected").with_resource(artifact_id)
            );
        }
        let control = match state.as_str() {
            "RUNNING" => DownloadControl::Continue,
            "PAUSING" => DownloadControl::Pause,
            "CANCELLING" => DownloadControl::Cancel,
            _ => return Err(invalid("download_checkpoint_job_not_active").with_resource(job_id)),
        };
        let previous = checkpoint_from_progress(&progress, artifact_id)?;
        let previous_offset = previous.durable_offset.get();
        if durable_offset > 0
            && (durable_offset < previous_offset
                || (previous_offset > 0 && previous.resume_validator != resume_validator))
        {
            return Err(
                invalid("download_checkpoint_regression_or_validator_change")
                    .with_resource(artifact_id),
            );
        }

        let mut progress: Value =
            serde_json::from_str(&progress).map_err(|e| db_error("progreso de descarga", &e))?;
        let Some(progress_object) = progress.as_object_mut() else {
            return Err(db_error(
                "progreso de descarga",
                &"progress must be an object",
            ));
        };
        if !progress_object
            .get("artifacts")
            .is_some_and(Value::is_object)
        {
            progress_object.insert("artifacts".into(), Value::Object(Default::default()));
        }
        let checkpoint = ArtifactCheckpoint {
            durable_offset: DecimalU64::new(durable_offset)
                .ok_or_else(|| invalid("download_checkpoint_offset_invalid"))?,
            resume_validator,
            source_id: previous.source_id,
            revision_key: previous.revision_key,
        };
        progress["artifacts"][artifact_id] = serde_json::to_value(&checkpoint)
            .map_err(|e| db_error("serializando checkpoint de artefacto", &e))?;
        let changed = sqlx::query(
            "UPDATE download_job SET progress=?,updated_at=?,row_version=row_version+1 \
             WHERE id=? AND state=?",
        )
        .bind(progress.to_string())
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(job_id)
        .bind(&state)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando checkpoint de descarga", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_checkpoint_state_conflict").with_resource(job_id));
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT checkpoint de descarga", &e))?;
        Ok(control)
    }

    /// Liga los checkpoints del archivo a una fuente y revisión aprobadas.
    /// Una identidad distinta sólo se admite desde offset cero, después de un
    /// evento durable de failover.
    pub async fn set_source_identity(
        &self,
        job_id: &str,
        artifact_id: &str,
        source_id: &str,
        revision_key: &str,
    ) -> Result<(), LarmError> {
        validate_identity(job_id, artifact_id, None)?;
        if !larm_domain::bridge::is_uuid_v7(source_id)
            || revision_key.trim().is_empty()
            || revision_key.len() > 4096
        {
            return Err(invalid("download_checkpoint_source_invalid"));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN procedencia de checkpoint", &e))?;
        lock(&mut tx).await?;
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT state,metadata,progress FROM download_job WHERE id=?")
                .bind(job_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo trabajo para fijar fuente", &e))?;
        let Some((state, metadata, progress)) = row else {
            return Err(LarmError::new(
                ErrorCode::NotFound,
                "No se encontró el trabajo de descarga",
                "download_job_not_found",
            )
            .with_resource(job_id));
        };
        if state != "RUNNING" {
            return Err(invalid("download_checkpoint_job_not_active").with_resource(job_id));
        }
        if !approved_artifact(&metadata, artifact_id)? {
            return Err(
                invalid("download_checkpoint_artifact_not_selected").with_resource(artifact_id)
            );
        }
        let mut progress: Value =
            serde_json::from_str(&progress).map_err(|e| db_error("progreso de descarga", &e))?;
        let checkpoint = checkpoint_from_progress(&progress.to_string(), artifact_id)?;
        if checkpoint.durable_offset.get() > 0
            && (checkpoint.source_id.as_deref() != Some(source_id)
                || checkpoint.revision_key.as_deref() != Some(revision_key))
        {
            return Err(invalid("download_checkpoint_source_change_with_partial")
                .with_resource(artifact_id));
        }
        let checkpoint = ArtifactCheckpoint {
            source_id: Some(source_id.to_owned()),
            revision_key: Some(revision_key.to_owned()),
            ..checkpoint
        };
        let Some(progress_object) = progress.as_object_mut() else {
            return Err(db_error(
                "progreso de descarga",
                &"progress must be an object",
            ));
        };
        if !progress_object
            .get("artifacts")
            .is_some_and(Value::is_object)
        {
            progress_object.insert("artifacts".into(), Value::Object(Default::default()));
        }
        progress["artifacts"][artifact_id] = serde_json::to_value(&checkpoint)
            .map_err(|e| db_error("serializando procedencia de checkpoint", &e))?;
        let changed = sqlx::query(
            "UPDATE download_job SET progress=?,updated_at=?,row_version=row_version+1 WHERE id=? AND state='RUNNING'",
        )
        .bind(progress.to_string())
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("guardando procedencia de checkpoint", &e))?;
        if changed.rows_affected() != 1 {
            return Err(invalid("download_checkpoint_state_conflict").with_resource(job_id));
        }
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT procedencia de checkpoint", &e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use crate::testing::temp_migrated;

    const JOB: &str = "0192f000-0000-7000-8000-00000000d101";
    const ARTIFACT: &str = "0192f000-0000-7000-8000-00000000d102";
    const VARIANT: &str = "0192f000-0000-7000-8000-00000000d103";
    const LOCATION: &str = "0192f000-0000-7000-8000-00000000d104";

    async fn seeded() -> (tempfile::TempDir, SqlDownloadProgress) {
        let (dir, db) = temp_migrated().await.unwrap();
        let pool = &db.pool;
        sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Model','[]','[]')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','Source','msa','https://example.invalid',0,1,'NONE','{}')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md','t','t',1,'{}','cm','ms','ns','model')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr','t','t',1,'{}','md','r1','hash','CURRENT')")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]','{}',0)")
            .bind(VARIANT).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,'t','t',1,'{}','Destination','D:\\models','D:\\models','NTFS','LOCAL',1000000,1000000,0,0,0,'ONLINE','volume-test','SSD')")
            .bind(LOCATION).execute(pool).await.unwrap();
        let metadata = serde_json::json!({"artifacts":[{"artifact_id":ARTIFACT}]});
        sqlx::query("INSERT INTO download_job(id,created_at,updated_at,row_version,metadata,priority,variant_id,storage_location_id,plan_hash,state,attempt,progress,error) VALUES(?,?,?,1,?,'NORMAL',?,?,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','RUNNING',1,'{}',NULL)")
            .bind(JOB).bind("t").bind("t").bind(metadata.to_string()).bind(VARIANT).bind(LOCATION).execute(pool).await.unwrap();
        (dir, SqlDownloadProgress { pool: db.pool })
    }

    fn etag(value: &str) -> Value {
        serde_json::json!({"kind":"STRONG_ETAG","value":value})
    }

    #[tokio::test]
    async fn checkpoint_offset_y_validador_son_atomicos_y_reanudables() {
        let (_dir, progress) = seeded().await;
        assert_eq!(
            progress
                .checkpoint(JOB, ARTIFACT, 7, Some(etag("\"v1\"")))
                .await
                .unwrap(),
            DownloadControl::Continue
        );
        assert_eq!(
            progress.load_checkpoint(JOB, ARTIFACT).await.unwrap(),
            ArtifactCheckpoint {
                durable_offset: DecimalU64::new(7).unwrap(),
                resume_validator: Some(etag("\"v1\"")),
                source_id: None,
                revision_key: None,
            }
        );
        assert!(progress
            .checkpoint(JOB, ARTIFACT, 8, Some(etag("\"v2\"")))
            .await
            .is_err());
        assert!(progress
            .checkpoint(JOB, ARTIFACT, 6, Some(etag("\"v1\"")))
            .await
            .is_err());
        assert_eq!(
            progress
                .checkpoint(JOB, ARTIFACT, 0, Some(etag("\"v2\"")))
                .await
                .unwrap(),
            DownloadControl::Continue
        );
        assert_eq!(
            progress
                .checkpoint(JOB, ARTIFACT, 3, Some(etag("\"v2\"")))
                .await
                .unwrap(),
            DownloadControl::Continue
        );
        assert_eq!(
            progress
                .load_checkpoint(JOB, ARTIFACT)
                .await
                .unwrap()
                .durable_offset
                .get(),
            3
        );
    }

    #[tokio::test]
    async fn checkpoint_respeta_seleccion_y_senales_de_pausa_y_cancelacion() {
        let (_dir, progress) = seeded().await;
        let other = "0192f000-0000-7000-8000-00000000d105";
        assert!(progress.load_checkpoint(JOB, other).await.is_err());
        assert!(progress.checkpoint(JOB, other, 0, None).await.is_err());

        sqlx::query("UPDATE download_job SET state='PAUSING' WHERE id=?")
            .bind(JOB)
            .execute(&progress.pool)
            .await
            .unwrap();
        assert_eq!(
            progress.checkpoint(JOB, ARTIFACT, 0, None).await.unwrap(),
            DownloadControl::Pause
        );
        sqlx::query("UPDATE download_job SET state='CANCELLING' WHERE id=?")
            .bind(JOB)
            .execute(&progress.pool)
            .await
            .unwrap();
        assert_eq!(
            progress.checkpoint(JOB, ARTIFACT, 0, None).await.unwrap(),
            DownloadControl::Cancel
        );
        sqlx::query("UPDATE download_job SET state='COMPLETED' WHERE id=?")
            .bind(JOB)
            .execute(&progress.pool)
            .await
            .unwrap();
        assert!(progress.checkpoint(JOB, ARTIFACT, 0, None).await.is_err());
    }
}
