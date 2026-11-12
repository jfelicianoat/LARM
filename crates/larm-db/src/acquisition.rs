//! Persistencia append-only de manifiestos remotos (contrato 3.12.0).
//! La cualificación física permanece adquirida hasta COMMIT; no verifica bytes.

use crate::events::{append, NewEvent};
use crate::pool::db_error;
use async_trait::async_trait;
use larm_domain::acquisition::{AcquisitionPlanBinding, ModelAcquisitionManifest};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AcquisitionManifestCatalog, AdapterQualification, AdapterQualificationGuard, MutationContext,
};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::sync::Arc;

#[derive(Clone)]
pub struct SqlAcquisitionManifests {
    pub pool: SqlitePool,
    pub qualification: Option<Arc<dyn AdapterQualification>>,
}

#[derive(sqlx::FromRow)]
struct CatalogBinding {
    revision_id: String,
    revision_key: String,
    manifest_hash: String,
    source_id: String,
    upstream_namespace: String,
    upstream_model_id: String,
    enabled: i64,
    adapter_installation_id: String,
}

fn conflict(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "El catálogo remoto cambió o perdió su cualificación",
        reason,
    )
}

pub(crate) fn decode(row: &sqlx::sqlite::SqliteRow) -> Result<ModelAcquisitionManifest, LarmError> {
    let mut value = serde_json::Map::new();
    for column in [
        "id",
        "variant_id",
        "revision_id",
        "source_id",
        "source_reference",
        "upstream_manifest_hash",
        "snapshot_hash",
        "observed_at",
        "required_artifact_ids",
        "optional_artifact_ids",
        "artifacts",
        "adapter_installation_id",
        "adapter_installation_revision",
        "adapter_installation_sha256",
        "conformance_run_id",
        "conformance_run_sha256",
    ] {
        let text: String = row
            .try_get(column)
            .map_err(|e| db_error("leyendo adquisición", &e))?;
        let field = if matches!(
            column,
            "source_reference" | "required_artifact_ids" | "optional_artifact_ids" | "artifacts"
        ) {
            serde_json::from_str(&text).map_err(|e| db_error("decodificando adquisición", &e))?
        } else {
            Value::String(text)
        };
        value.insert(column.into(), field);
    }
    let manifest: ModelAcquisitionManifest = serde_json::from_value(Value::Object(value))
        .map_err(|e| db_error("decodificando adquisición", &e))?;
    manifest.validate()?;
    Ok(manifest)
}

impl SqlAcquisitionManifests {
    pub async fn get(&self, id: &str) -> Result<Option<ModelAcquisitionManifest>, LarmError> {
        sqlx::query("SELECT * FROM model_acquisition_manifest WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error("leyendo manifiesto de adquisición", &e))?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn latest(
        &self,
        variant_id: &str,
    ) -> Result<Option<ModelAcquisitionManifest>, LarmError> {
        sqlx::query("SELECT * FROM model_acquisition_manifest WHERE variant_id=? ORDER BY observed_at DESC,id DESC LIMIT 1")
            .bind(variant_id).fetch_optional(&self.pool).await
            .map_err(|e| db_error("leyendo adquisición actual", &e))?
            .as_ref().map(decode).transpose()
    }

    /// Registra el snapshot sobre un catálogo existente y cualificado. La
    /// ingesta completa reutiliza record_in dentro de su propia transacción.
    /// Devuelve false para replay exacto sin duplicar evento/outbox.
    pub async fn record(
        &self,
        manifest: &ModelAcquisitionManifest,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError> {
        manifest.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN adquisición", &e))?;
        sqlx::query("UPDATE model_variant SET row_version=row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando catálogo", &e))?;
        let guard = validate_links(&mut tx, manifest, ctx, self.qualification.as_ref()).await?;
        let inserted = record_in(&mut tx, manifest, ctx).await?;
        guard.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT adquisición", &e))?;
        Ok(inserted)
    }
}

async fn read_lock(conn: &mut SqliteConnection) -> Result<(), LarmError> {
    // Consistent current relations while qualification is checked. No row,
    // lifecycle, event, reservation or job is changed by this read operation.
    sqlx::query("UPDATE model_variant SET row_version=row_version WHERE 0")
        .execute(conn)
        .await
        .map_err(|e| db_error("bloqueando lectura de adquisición", &e))?;
    Ok(())
}

async fn bound_in(
    conn: &mut SqliteConnection,
    variant_id: &str,
    binding: &AcquisitionPlanBinding,
) -> Result<ModelAcquisitionManifest, LarmError> {
    let row = sqlx::query("SELECT * FROM model_acquisition_manifest WHERE id=?")
        .bind(&binding.manifest_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| db_error("leyendo adquisición ligada al plan", &e))?
        .ok_or_else(|| conflict("acquisition_snapshot_missing"))?;
    let manifest = decode(&row)?;
    if manifest.variant_id != variant_id || manifest.binding() != *binding {
        return Err(conflict("acquisition_plan_binding_mismatch"));
    }
    Ok(manifest)
}

/// Revalida dentro de la transacción que persiste el plan/job. El caller
/// conserva el guard y lo comprueba justo antes del COMMIT de nuevas escrituras.
pub(crate) async fn validate_download_in(
    conn: &mut SqliteConnection,
    record: &larm_domain::download::DownloadPlanRecord,
    ctx: &MutationContext,
    gate: Option<&Arc<dyn AdapterQualification>>,
) -> Result<Option<Box<dyn AdapterQualificationGuard>>, LarmError> {
    let Some(binding) = &record.response.acquisition else {
        return Ok(None);
    };
    let manifest = bound_in(conn, &record.response.variant_id, binding).await?;
    manifest.validate_download_record(record)?;
    Ok(Some(validate_links(conn, &manifest, ctx, gate).await?))
}

#[async_trait]
impl AcquisitionManifestCatalog for SqlAcquisitionManifests {
    async fn latest_current(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<Option<ModelAcquisitionManifest>, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN lectura de adquisición", &e))?;
        read_lock(&mut tx).await?;
        let row = sqlx::query("SELECT * FROM model_acquisition_manifest WHERE variant_id=? ORDER BY observed_at DESC,id DESC LIMIT 1")
            .bind(variant_id).fetch_optional(&mut *tx).await
            .map_err(|e| db_error("leyendo adquisición actual", &e))?;
        let Some(row) = row else {
            tx.commit()
                .await
                .map_err(|e| db_error("COMMIT lectura de adquisición", &e))?;
            return Ok(None);
        };
        let manifest = decode(&row)?;
        let guard = validate_links(&mut tx, &manifest, ctx, self.qualification.as_ref()).await?;
        guard.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT lectura de adquisición", &e))?;
        Ok(Some(manifest))
    }

    async fn bound_current(
        &self,
        variant_id: &str,
        binding: &AcquisitionPlanBinding,
        ctx: &MutationContext,
    ) -> Result<ModelAcquisitionManifest, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN lectura de adquisición", &e))?;
        read_lock(&mut tx).await?;
        let manifest = bound_in(&mut tx, variant_id, binding).await?;
        let guard = validate_links(&mut tx, &manifest, ctx, self.qualification.as_ref()).await?;
        guard.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT lectura de adquisición", &e))?;
        Ok(manifest)
    }
}

/// Debe conservarse el guard hasta confirmar la transacción completa.
pub(crate) async fn validate_links(
    conn: &mut SqliteConnection,
    manifest: &ModelAcquisitionManifest,
    ctx: &MutationContext,
    gate: Option<&Arc<dyn AdapterQualification>>,
) -> Result<Box<dyn AdapterQualificationGuard>, LarmError> {
    manifest.validate()?;
    let binding: Option<CatalogBinding> = sqlx::query_as(
        "SELECT v.revision_id,r.revision_key,r.manifest_hash,d.source_id,d.upstream_namespace,d.upstream_model_id,s.enabled,a.adapter_installation_id FROM model_variant v JOIN model_revision r ON r.id=v.revision_id JOIN model_distribution d ON d.id=r.distribution_id JOIN model_source s ON s.id=d.source_id JOIN model_source_adapter a ON a.id=s.adapter_id WHERE v.id=?"
    ).bind(&manifest.variant_id).fetch_optional(&mut *conn).await
        .map_err(|e| db_error("leyendo identidad de adquisición",&e))?;
    let Some(binding) = binding else {
        return Err(conflict("acquisition_catalog_missing"));
    };
    let reference = &manifest.source_reference;
    if binding.revision_id != manifest.revision_id
        || binding.revision_key != reference.revision_key
        || binding.manifest_hash != manifest.upstream_manifest_hash
        || binding.source_id != manifest.source_id
        || binding.upstream_namespace != reference.upstream_namespace
        || binding.upstream_model_id != reference.upstream_model_id
        || binding.enabled != 1
        || binding.adapter_installation_id != manifest.adapter_installation_id
    {
        return Err(conflict("acquisition_catalog_mismatch"));
    }
    let artifacts=sqlx::query("SELECT id,metadata,filename,expected_size,sha256,required,size_estimate FROM model_artifact WHERE variant_id=? ORDER BY filename,id")
        .bind(&manifest.variant_id).fetch_all(&mut *conn).await
        .map_err(|e| db_error("leyendo artefactos de adquisición",&e))?;
    if artifacts.len() != manifest.artifacts.len() {
        return Err(conflict("acquisition_artifacts_mismatch"));
    }
    for (row, artifact) in artifacts.iter().zip(&manifest.artifacts) {
        let id: String = row.try_get("id").map_err(|e| db_error("artifact id", &e))?;
        let path: String = row
            .try_get("filename")
            .map_err(|e| db_error("artifact path", &e))?;
        let size: Option<i64> = row
            .try_get("expected_size")
            .map_err(|e| db_error("artifact size", &e))?;
        let hash: Option<String> = row
            .try_get("sha256")
            .map_err(|e| db_error("artifact hash", &e))?;
        let required: i64 = row
            .try_get("required")
            .map_err(|e| db_error("artifact required", &e))?;
        let estimate: String = row
            .try_get("size_estimate")
            .map_err(|e| db_error("artifact estimate", &e))?;
        let estimate: larm_domain::model::ArtifactSizeEstimate =
            serde_json::from_str(&estimate).map_err(|e| db_error("artifact estimate", &e))?;
        let metadata: String = row
            .try_get("metadata")
            .map_err(|e| db_error("artifact metadata", &e))?;
        let metadata: serde_json::Map<String, Value> =
            serde_json::from_str(&metadata).map_err(|e| db_error("artifact metadata", &e))?;
        let role: larm_domain::model::ArtifactRole = metadata
            .get("role")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| db_error("artifact role", &e))?
            .unwrap_or(larm_domain::model::ArtifactRole::Other);
        if id != artifact.artifact_id
            || path != artifact.relative_path
            || size != artifact.size_bytes.map(|v| v.get() as i64)
            || hash != artifact.sha256
            || (required == 1) != artifact.required
            || estimate != artifact.size_estimate
            || role.as_str() != artifact.role
        {
            return Err(conflict("acquisition_artifacts_mismatch"));
        }
    }
    let row = sqlx::query("SELECT * FROM adapter_installation WHERE id=?")
        .bind(&manifest.adapter_installation_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo instalación source", &e))?
        .ok_or_else(|| conflict("acquisition_installation_missing"))?;
    let installation = crate::adapters::to_installation(&row)?;
    let run = crate::conformance::require_consumer_current(&mut *conn, &installation).await?;
    if installation.family != "source"
        || installation.row_version.to_string() != manifest.adapter_installation_revision
        || larm_domain::conformance::committed_installation_hash(&installation)?
            != manifest.adapter_installation_sha256
        || run.id != manifest.conformance_run_id
        || run.digest()? != manifest.conformance_run_sha256
    {
        return Err(conflict("acquisition_qualification_mismatch"));
    }
    crate::conformance::qualify_current(conn, &installation, ctx, gate).await
}

/// Sólo tras validate_links; el caller conserva su guard y recheck antes de COMMIT.
pub(crate) async fn record_in(
    conn: &mut SqliteConnection,
    manifest: &ModelAcquisitionManifest,
    ctx: &MutationContext,
) -> Result<bool, LarmError> {
    let existing = sqlx::query("SELECT * FROM model_acquisition_manifest WHERE id=?")
        .bind(&manifest.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_error("leyendo replay adquisición", &e))?;
    if let Some(row) = existing {
        if decode(&row)? != *manifest {
            return Err(conflict("acquisition_snapshot_immutable"));
        }
        return Ok(false);
    }
    sqlx::query("INSERT INTO model_acquisition_manifest(id,variant_id,revision_id,source_id,source_reference,upstream_manifest_hash,snapshot_hash,observed_at,required_artifact_ids,optional_artifact_ids,artifacts,adapter_installation_id,adapter_installation_revision,adapter_installation_sha256,conformance_run_id,conformance_run_sha256) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&manifest.id).bind(&manifest.variant_id).bind(&manifest.revision_id).bind(&manifest.source_id)
        .bind(json!(manifest.source_reference).to_string()).bind(&manifest.upstream_manifest_hash)
        .bind(&manifest.snapshot_hash).bind(&manifest.observed_at)
        .bind(json!(manifest.required_artifact_ids).to_string()).bind(json!(manifest.optional_artifact_ids).to_string())
        .bind(json!(manifest.artifacts).to_string()).bind(&manifest.adapter_installation_id)
        .bind(&manifest.adapter_installation_revision).bind(&manifest.adapter_installation_sha256)
        .bind(&manifest.conformance_run_id).bind(&manifest.conformance_run_sha256)
        .execute(&mut *conn).await.map_err(|e|db_error("registrando adquisición",&e))?;
    append(
        conn,
        &NewEvent {
            event_type: "model.acquisition.recorded",
            resource_type: "model_acquisition_manifest",
            resource_id: &manifest.id,
            resource_version: 1,
            data: json!({"variant_id":manifest.variant_id,"revision_id":manifest.revision_id,
            "source_id":manifest.source_id,"snapshot_hash":manifest.snapshot_hash}),
        },
        ctx,
    )
    .await?;
    Ok(true)
}
