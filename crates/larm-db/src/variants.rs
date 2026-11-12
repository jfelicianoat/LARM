//! Variantes, artefactos y bundles sobre SQLite (P041).
//!
//! El alta de una variante escribe la variante, sus artefactos y su bundle
//! en una sola transacción con sus eventos `model.state_changed`: nunca
//! queda una variante sin artefactos ni un bundle sin variante.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    ArtifactSizeEstimate, BundleState, CanonicalModel, LifecycleEvidence, ModelArtifact,
    ModelBundle, ModelLifecycle, ModelVariant,
};
use larm_domain::ports::{
    ArtifactLocationRepository, ModelRepository, MutationContext, VariantFilter, VariantRepository,
};
use larm_domain::units::DecimalU64;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

const VARIANT: &str = "id,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,\
bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits,\
update_available,status";
const ARTIFACT: &str =
    "id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,\
expected_size,actual_size,sha256,source_url,required,size_estimate";
const BUNDLE: &str =
    "id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,\
optional_artifact_ids,manifest_hash,state";

fn text(row: &SqliteRow, c: &str) -> Result<String, LarmError> {
    row.try_get::<String, _>(c).map_err(|e| db_error(c, &e))
}
fn opt(row: &SqliteRow, c: &str) -> Result<Option<String>, LarmError> {
    row.try_get::<Option<String>, _>(c)
        .map_err(|e| db_error(c, &e))
}
fn json<T: serde::de::DeserializeOwned>(row: &SqliteRow, c: &str) -> Result<T, LarmError> {
    serde_json::from_str(&text(row, c)?).map_err(|e| db_error(c, &e))
}
fn int(row: &SqliteRow, c: &str) -> Result<i64, LarmError> {
    row.try_get::<i64, _>(c).map_err(|e| db_error(c, &e))
}
fn size(row: &SqliteRow, c: &str) -> Result<Option<DecimalU64>, LarmError> {
    row.try_get::<Option<i64>, _>(c)
        .map_err(|e| db_error(c, &e))?
        .map(|v| {
            u64::try_from(v)
                .ok()
                .and_then(DecimalU64::new)
                .ok_or_else(|| db_error(c, &"negativo"))
        })
        .transpose()
}
fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}
fn as_i64(v: Option<DecimalU64>) -> Option<i64> {
    v.map(|v| i64::try_from(v.get()).unwrap_or(i64::MAX))
}

pub(crate) fn to_variant(row: &SqliteRow) -> Result<ModelVariant, LarmError> {
    Ok(ModelVariant {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: int(row, "row_version")?,
        metadata: json(row, "metadata")?,
        revision_id: text(row, "revision_id")?,
        format: opt(row, "format")?,
        quantization: opt(row, "quantization")?,
        architecture: opt(row, "architecture")?,
        bundle_hash: opt(row, "bundle_hash")?,
        lifecycle: text(row, "lifecycle")?,
        availability: text(row, "availability")?,
        ownership: serde_json::from_value(serde_json::Value::String(text(row, "ownership")?))
            .map_err(|e| db_error("ownership", &e))?,
        promotion: text(row, "promotion")?,
        system_role: text(row, "system_role")?,
        capabilities: json(row, "capabilities")?,
        model_limits: json(row, "model_limits")?,
        system1_state: opt(row, "system1_state")?,
        update_available: int(row, "update_available")? != 0,
        status: larm_domain::model::ModelVariant::status_for(
            &text(row, "lifecycle")?,
            &text(row, "availability")?,
            int(row, "update_available")? != 0,
        )
        .to_string(),
    })
}

pub(crate) fn to_artifact(row: &SqliteRow) -> Result<ModelArtifact, LarmError> {
    Ok(ModelArtifact {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: int(row, "row_version")?,
        metadata: json(row, "metadata")?,
        variant_id: text(row, "variant_id")?,
        filename: text(row, "filename")?,
        format: opt(row, "format")?,
        quantization: opt(row, "quantization")?,
        expected_size: size(row, "expected_size")?,
        actual_size: size(row, "actual_size")?,
        size_estimate: json::<ArtifactSizeEstimate>(row, "size_estimate")?,
        sha256: opt(row, "sha256")?,
        source_url: opt(row, "source_url")?,
        required: int(row, "required")? != 0,
    })
}

pub(crate) fn to_bundle(row: &SqliteRow) -> Result<ModelBundle, LarmError> {
    Ok(ModelBundle {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: int(row, "row_version")?,
        metadata: json(row, "metadata")?,
        variant_id: text(row, "variant_id")?,
        required_artifact_ids: json(row, "required_artifact_ids")?,
        optional_artifact_ids: json(row, "optional_artifact_ids")?,
        manifest_hash: text(row, "manifest_hash")?,
        state: serde_json::from_value(serde_json::Value::String(text(row, "state")?))
            .map_err(|e| db_error("state", &e))?,
    })
}

async fn event(
    tx: &mut SqliteConnection,
    resource_type: &str,
    id: &str,
    version: i64,
    data: serde_json::Value,
    ctx: &MutationContext,
) -> Result<String, LarmError> {
    append(
        tx,
        &NewEvent {
            event_type: "model.state_changed",
            resource_type,
            resource_id: id,
            resource_version: version,
            data,
        },
        ctx,
    )
    .await
}

#[derive(Clone)]
pub struct SqlVariants {
    pub pool: SqlitePool,
}

impl SqlVariants {
    /// Resolve the same remote observation after its required physical hash has
    /// matured. A hash becoming known does not authorize a second Core UUID.
    pub async fn observed_variant(
        &self,
        manifest_source_id: &str,
        revision_id: &str,
        upstream_hash: &str,
        format: Option<&str>,
        quantization: Option<&str>,
        architecture: Option<&str>,
    ) -> Result<Option<ModelVariant>, LarmError> {
        let rows=sqlx::query("SELECT DISTINCT v.* FROM model_variant v JOIN model_acquisition_manifest m ON m.variant_id=v.id WHERE m.source_id=? AND m.revision_id=? AND m.upstream_manifest_hash=? AND v.format IS ? AND coalesce(v.quantization,'')=coalesce(?,'') AND v.architecture IS ? ORDER BY v.id LIMIT 2")
            .bind(manifest_source_id).bind(revision_id).bind(upstream_hash).bind(format)
            .bind(quantization).bind(architecture).fetch_all(&self.pool).await
            .map_err(|e|db_error("resolviendo identidad remota madura",&e))?;
        if rows.len() > 1 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La observación remota tiene varias identidades",
                "catalog_acquisition_identity_ambiguous",
            ));
        }
        rows.first().map(to_variant).transpose()
    }
}

async fn json_records(
    pool: &SqlitePool,
    query: &str,
    variant_id: &str,
    table: &str,
) -> Result<Vec<serde_json::Value>, LarmError> {
    let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_string()))
        .bind(variant_id)
        .fetch_all(pool)
        .await
        .map_err(|e| db_error(&format!("leyendo {table}"), &e))?;
    rows.iter()
        .map(|row| {
            serde_json::from_str(row).map_err(|e| db_error(&format!("decodificando {table}"), &e))
        })
        .collect()
}

#[async_trait]
impl VariantRepository for SqlVariants {
    async fn list_variants(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        f: &VariantFilter,
    ) -> Result<Vec<ModelVariant>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let cols = VARIANT
            .split(',')
            .map(|c| format!("v.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {cols} FROM model_variant v \
             LEFT JOIN model_revision r ON r.id = v.revision_id \
             LEFT JOIN model_distribution d ON d.id = r.distribution_id \
             WHERE (v.created_at, v.id) > (?, ?) \
             AND (? IS NULL OR v.lifecycle = ?) \
             AND (? IS NULL OR EXISTS (SELECT 1 FROM json_each(v.capabilities) WHERE value = ?)) \
             AND (? IS NULL OR d.canonical_model_id = ?) \
             ORDER BY v.created_at, v.id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(&f.state)
        .bind(&f.state)
        .bind(&f.capability)
        .bind(&f.capability)
        .bind(&f.model_id)
        .bind(&f.model_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando model_variant", &e))?;
        rows.iter().map(to_variant).collect()
    }

    async fn get_variant(&self, id: &str) -> Result<Option<ModelVariant>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {VARIANT} FROM model_variant WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_variant", &e))?
        .as_ref()
        .map(to_variant)
        .transpose()
    }

    async fn variant_detail(&self, id: &str) -> Result<Option<serde_json::Value>, LarmError> {
        let Some(variant) = self.get_variant(id).await? else {
            return Ok(None);
        };
        let models = super::models::SqlModels {
            pool: self.pool.clone(),
        };
        let revision = models
            .get_revision(&variant.revision_id)
            .await?
            .ok_or_else(|| db_error("model_revision de model_variant", &"referencia ausente"))?;
        let distribution = models
            .get_distribution(&revision.distribution_id)
            .await?
            .ok_or_else(|| {
                db_error(
                    "model_distribution de model_revision",
                    &"referencia ausente",
                )
            })?;
        let canonical_model = models
            .get_model(&distribution.canonical_model_id)
            .await?
            .ok_or_else(|| {
                db_error(
                    "canonical_model de model_distribution",
                    &"referencia ausente",
                )
            })?;

        let mut artifacts = self.artifacts_of(id).await?;
        let referenced: Vec<String> = sqlx::query_scalar(
            "SELECT artifact_id FROM artifact_reference WHERE variant_id = ? ORDER BY artifact_id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_reference de variante", &e))?;
        for artifact_id in referenced {
            if !artifacts.iter().any(|artifact| artifact.id == artifact_id) {
                if let Some(artifact) = self.get_artifact(&artifact_id).await? {
                    artifacts.push(artifact);
                }
            }
        }
        artifacts.sort_by(|a, b| (&a.filename, &a.id).cmp(&(&b.filename, &b.id)));

        let locations = super::artifacts::SqlArtifactLocations {
            pool: self.pool.clone(),
        }
        .for_variant(id)
        .await?
        .unwrap_or_default()
        .into_iter()
        .map(|item| item.location)
        .collect::<Vec<_>>();
        let mut provenance = Vec::new();
        for artifact in &artifacts {
            provenance.extend(models.provenance_for(&artifact.id).await?);
        }

        let compatibility = json_records(
            &self.pool,
            "SELECT json_object('id',id,'created_at',created_at,'updated_at',updated_at,'row_version',row_version,'metadata',json(metadata),'variant_id',variant_id,'runtime_version_id',runtime_version_id,'backend_id',backend_id,'hardware_fingerprint',hardware_fingerprint,'launch_profile_hash',launch_profile_hash,'status',status,'hard_gates',json(hard_gates),'limitations',json(limitations),'technical_fit',technical_fit,'evidence_refs',json(evidence_refs)) FROM compatibility_record WHERE variant_id = ? ORDER BY created_at,id",
            id,
            "compatibility_record",
        )
        .await?;
        let evaluation_references = json_records(
            &self.pool,
            "SELECT json_object('id',id,'created_at',created_at,'updated_at',updated_at,'row_version',row_version,'metadata',json(metadata),'source_system',source_system,'external_id',external_id,'source_version',source_version,'experiment_id',experiment_id,'suite_hash',suite_hash,'dataset_hash',dataset_hash,'variant_id',variant_id,'execution_fingerprint',execution_fingerprint,'score',score,'scale',json(scale),'status',status,'artifact_refs',json(artifact_refs),'issued_at',issued_at,'signature',signature) FROM evaluation_reference WHERE variant_id = ? ORDER BY created_at,id",
            id,
            "evaluation_reference",
        )
        .await?;
        let certifications = json_records(
            &self.pool,
            "SELECT json_object('id',id,'created_at',created_at,'updated_at',updated_at,'row_version',row_version,'metadata',json(metadata),'evaluation_reference_id',evaluation_reference_id,'variant_id',variant_id,'runtime_version_id',runtime_version_id,'backend_id',backend_id,'hardware_fingerprint',hardware_fingerprint,'launch_profile_hash',launch_profile_hash,'suite_hash',suite_hash,'status',status,'expires_at',expires_at) FROM certification WHERE variant_id = ? ORDER BY created_at,id",
            id,
            "certification",
        )
        .await?;
        let active_lease_ids: Vec<String> = sqlx::query_scalar(
            "SELECT l.id FROM runtime_lease l JOIN runtime_instance i ON i.id = l.instance_id WHERE i.variant_id = ? AND l.state = 'ACTIVE' ORDER BY l.id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo leases activos de variante", &e))?;

        Ok(Some(serde_json::json!({
            "variant": variant,
            "revision": revision,
            "distribution": distribution,
            "canonical_model": canonical_model,
            "bundle": self.bundle_of(id).await?,
            "artifacts": artifacts,
            "locations": locations,
            "provenance": provenance,
            "compatibility": compatibility,
            "evaluation_references": evaluation_references,
            "certifications": certifications,
            "active_lease_ids": active_lease_ids,
        })))
    }

    async fn select_variant(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<ModelVariant, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let current: Option<(i64, String, String, i64)> = sqlx::query_as(
            "SELECT row_version,lifecycle,availability,update_available FROM model_variant WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant para selección", &e))?;
        let Some((version, from, availability, update_available)) = current else {
            return Err(
                LarmError::new(ErrorCode::NotFound, "Variante desconocida", "not_found")
                    .with_resource(id.to_string()),
            );
        };
        if from == ModelLifecycle::Selected.as_str() {
            tx.rollback().await.map_err(|e| db_error("ROLLBACK", &e))?;
            return self.get_variant(id).await?.ok_or_else(|| {
                db_error("model_variant seleccionado", &"desapareció tras la lectura")
            });
        }
        let from_lifecycle = ModelLifecycle::parse(&from).ok_or_else(|| {
            LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Estado de ciclo de vida desconocido",
                "invalid_state_transition",
            )
            .with_resource(id.to_string())
        })?;
        larm_domain::model::validate_lifecycle_transition(
            from_lifecycle,
            ModelLifecycle::Selected,
            &LifecycleEvidence {
                selection_recorded: true,
                ..LifecycleEvidence::default()
            },
        )?;
        let status = ModelVariant::status_for(
            ModelLifecycle::Selected.as_str(),
            &availability,
            update_available != 0,
        );
        let updated_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let changed = sqlx::query(
            "UPDATE model_variant SET lifecycle='SELECTED',status=?,updated_at=?,row_version=row_version+1 WHERE id=? AND row_version=?",
        )
        .bind(status)
        .bind(updated_at)
        .bind(id)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("seleccionando model_variant", &e))?;
        if changed.rows_affected() != 1 {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió durante la selección",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        }
        event(
            &mut tx,
            "model_variant",
            id,
            version + 1,
            serde_json::json!({"action":"lifecycle","from":from,"to":"SELECTED"}),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        self.get_variant(id).await?.ok_or_else(|| {
            db_error(
                "model_variant seleccionado",
                &"desapareció tras la escritura",
            )
        })
    }

    async fn find_variant(
        &self,
        revision_id: &str,
        format: Option<&str>,
        quantization: Option<&str>,
        architecture: Option<&str>,
        bundle_hash: Option<&str>,
    ) -> Result<Option<ModelVariant>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {VARIANT} FROM model_variant WHERE revision_id = ? AND format IS ? \
             AND coalesce(quantization, '') = coalesce(?, '') AND architecture IS ? AND bundle_hash IS ?"
        )))
        .bind(revision_id)
        .bind(format)
        .bind(quantization)
        .bind(architecture)
        .bind(bundle_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_variant", &e))?
        .as_ref()
        .map(to_variant)
        .transpose()
    }

    async fn insert_variant(
        &self,
        v: &ModelVariant,
        artifacts: &[ModelArtifact],
        bundle: Option<&ModelBundle>,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        insert_variant_in(&mut tx, v, artifacts, bundle, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn list_artifacts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        q: Option<&str>,
    ) -> Result<Vec<ModelArtifact>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let like = q
            .filter(|q| !q.is_empty())
            .map(|q| format!("%{}%", q.to_lowercase()));
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ARTIFACT} FROM model_artifact WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR lower(filename) LIKE ?) ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(&like)
        .bind(&like)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando model_artifact", &e))?;
        rows.iter().map(to_artifact).collect()
    }

    async fn get_artifact(&self, id: &str) -> Result<Option<ModelArtifact>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ARTIFACT} FROM model_artifact WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_artifact", &e))?
        .as_ref()
        .map(to_artifact)
        .transpose()
    }

    async fn artifacts_of(&self, variant_id: &str) -> Result<Vec<ModelArtifact>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ARTIFACT} FROM model_artifact WHERE variant_id = ? ORDER BY filename, id"
        )))
        .bind(variant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_artifact", &e))?;
        rows.iter().map(to_artifact).collect()
    }

    async fn list_bundles(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        state: Option<BundleState>,
    ) -> Result<Vec<ModelBundle>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let state = state.map(BundleState::as_str);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {BUNDLE} FROM model_bundle WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR state = ?) ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(state)
        .bind(state)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando model_bundle", &e))?;
        rows.iter().map(to_bundle).collect()
    }

    async fn get_bundle(&self, id: &str) -> Result<Option<ModelBundle>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {BUNDLE} FROM model_bundle WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_bundle", &e))?
        .as_ref()
        .map(to_bundle)
        .transpose()
    }

    async fn bundle_of(&self, variant_id: &str) -> Result<Option<ModelBundle>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {BUNDLE} FROM model_bundle WHERE variant_id = ? ORDER BY created_at DESC, id DESC LIMIT 1"
        )))
        .bind(variant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_bundle", &e))?
        .as_ref()
        .map(to_bundle)
        .transpose()
    }

    async fn bundle_artifacts(
        &self,
        bundle: &ModelBundle,
    ) -> Result<Vec<ModelArtifact>, LarmError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN descriptores físicos", &e))?;
        let row = sqlx::query("SELECT * FROM model_bundle WHERE id=?")
            .bind(&bundle.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("leyendo bundle físico", &e))?
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Bundle desconocido", "bundle_missing")
            })?;
        if to_bundle(&row)? != *bundle {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El bundle cambió",
                "stale_revision",
            ));
        }
        let rows =
            sqlx::query("SELECT * FROM model_artifact WHERE variant_id=? ORDER BY filename,id")
                .bind(&bundle.variant_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo miembros físicos", &e))?;
        let artifacts = rows
            .iter()
            .map(to_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        let result = crate::download_bundle::physical_artifacts(&mut tx, bundle, artifacts).await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT descriptores físicos", &e))?;
        Ok(result)
    }

    async fn set_availability(
        &self,
        id: &str,
        expected: i64,
        availability: &str,
        patch: serde_json::Value,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        if !larm_domain::model::AVAILABILITY.contains(&availability) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "availability fuera del contrato",
                "availability_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let from: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT availability, lifecycle, update_available FROM model_variant WHERE id = ? AND row_version = ?",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant", &e))?;
        if let Some((_, lifecycle, update_available)) = &from {
            let status = larm_domain::model::ModelVariant::status_for(
                lifecycle,
                availability,
                *update_available != 0,
            );
            sqlx::query(
                "UPDATE model_variant SET availability = ?, status = ?, metadata = json_patch(metadata, ?), updated_at = ?, \
                 row_version = row_version + 1 WHERE id = ? AND row_version = ?",
            )
            .bind(availability)
            .bind(status)
            .bind(patch.to_string())
            .bind(&now)
            .bind(id)
            .bind(expected)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("actualizando model_variant", &e))?;
        }
        let Some((from, _, _)) = from else {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        };
        event(
            &mut tx,
            "model_variant",
            id,
            expected + 1,
            serde_json::json!({"action": "availability", "from": from, "to": availability}),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(expected + 1)
    }

    async fn set_promotion(
        &self,
        id: &str,
        expected: i64,
        to: &str,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        use larm_domain::model::{can_transition_promotion, PROMOTION};
        if !PROMOTION.contains(&to) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "promotion fuera del contrato",
                "promotion_invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let from: Option<(String, String, Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT promotion, revision_id, format, quantization, row_version              FROM model_variant WHERE id = ? AND row_version = ?",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant", &e))?;
        let Some((current, revision_id, format, quantization, _)) = from else {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        };
        if !can_transition_promotion(&current, to) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Transición de promoción no permitida",
                "promotion_transition_invalid",
            )
            .with_resource(id.to_string()));
        }
        // DEFAULT es exclusivo por (revision_id, format, quantization): la
        // DEFAULT previa de la misma combinación retrocede en la misma
        // transacción.
        if to == "DEFAULT" {
            let previous: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM model_variant                  WHERE revision_id = ? AND format IS ? AND coalesce(quantization, '') = coalesce(?, '')                  AND promotion = 'DEFAULT' AND id != ?",
            )
            .bind(&revision_id)
            .bind(&format)
            .bind(&quantization)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("buscando DEFAULT previa", &e))?;
            if let Some((previous_id,)) = previous {
                sqlx::query(
                    "UPDATE model_variant SET promotion = 'EVALUATION_APPROVED', updated_at = ?,                      row_version = row_version + 1 WHERE id = ? AND promotion = 'DEFAULT'",
                )
                .bind(&now)
                .bind(&previous_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("rebajando DEFAULT previa", &e))?;
                event(
                    &mut tx,
                    "model_variant",
                    &previous_id,
                    expected + 1,
                    serde_json::json!({"action": "promotion", "from": "DEFAULT", "to": "EVALUATION_APPROVED", "reason": "reemplazada"}),
                    ctx,
                )
                .await?;
            }
        }
        sqlx::query(
            "UPDATE model_variant SET promotion = ?, updated_at = ?,              row_version = row_version + 1 WHERE id = ? AND row_version = ?",
        )
        .bind(to)
        .bind(&now)
        .bind(id)
        .bind(expected)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando model_variant", &e))?;
        event(
            &mut tx,
            "model_variant",
            id,
            expected + 1,
            serde_json::json!({"action": "promotion", "from": current, "to": to}),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(expected + 1)
    }

    async fn mark_variant_corrupt(
        &self,
        id: &str,
        expected: i64,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let current: Option<(String, String)> = sqlx::query_as(
            "SELECT lifecycle, availability FROM model_variant WHERE id = ? AND row_version = ?",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant corrupta", &e))?;
        let Some((from_lifecycle, from_availability)) = current else {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        };
        if from_lifecycle == "CORRUPT" && from_availability == "CORRUPT" {
            tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
            return Ok(expected);
        }
        sqlx::query(
            "UPDATE model_variant SET lifecycle='CORRUPT', availability='CORRUPT', status='CORRUPT', \
             metadata=json_patch(metadata, ?), updated_at=?, row_version=row_version+1 \
             WHERE id=? AND row_version=?",
        )
        .bind(serde_json::json!({"availability_before_offline": null}).to_string())
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(id)
        .bind(expected)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("marcando variant corrupt", &e))?;
        event(
            &mut tx,
            "model_variant",
            id,
            expected + 1,
            serde_json::json!({
                "action": "lifecycle",
                "from": from_lifecycle,
                "to": "CORRUPT",
                "availability_from": from_availability,
                "availability_to": "CORRUPT"
            }),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(expected + 1)
    }

    async fn finish_variant_reverification(
        &self,
        id: &str,
        expected: i64,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let current: Option<(String, i64)> = sqlx::query_as(
            "SELECT lifecycle, update_available FROM model_variant WHERE id = ? AND row_version = ?",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant para reverificación", &e))?;
        let Some((from_lifecycle, update_available)) = current else {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        };
        if from_lifecycle != "CORRUPT" {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Sólo una variante CORRUPT puede volver a VERIFIED por reverificación",
                "invalid_state_transition",
            )
            .with_resource(id.to_string()));
        }
        let status = larm_domain::model::ModelVariant::status_for(
            "VERIFIED",
            "UNKNOWN",
            update_available != 0,
        );
        sqlx::query(
            "UPDATE model_variant SET lifecycle='VERIFIED', availability='UNKNOWN', status=?, \
             metadata=json_patch(metadata, ?), updated_at=?, row_version=row_version+1 \
             WHERE id=? AND row_version=?",
        )
        .bind(status)
        .bind(serde_json::json!({"availability_before_offline": null}).to_string())
        .bind(larm_domain::bridge::timestamp(chrono::Utc::now()))
        .bind(id)
        .bind(expected)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("finalizando reverificación de variant", &e))?;
        event(
            &mut tx,
            "model_variant",
            id,
            expected + 1,
            serde_json::json!({
                "action": "lifecycle",
                "from": from_lifecycle,
                "to": "VERIFIED",
                "availability_from": "CORRUPT",
                "availability_to": "UNKNOWN"
            }),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(expected + 1)
    }

    async fn delete_variant(
        &self,
        id: &str,
        expected: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        sqlx::query("UPDATE model_variant SET row_version = row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando model_variant", &e))?;
        let found: Option<i64> =
            sqlx::query_scalar("SELECT row_version FROM model_variant WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo model_variant", &e))?;
        match found {
            None => {
                return Err(LarmError::new(
                    ErrorCode::NotFound,
                    "Variante desconocida",
                    "not_found",
                )
                .with_resource(id.to_string()))
            }
            Some(v) if v != expected => {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "La variante cambió desde que la leíste",
                    "stale_revision",
                )
                .with_resource(id.to_string()))
            }
            Some(_) => {}
        }
        // Artefactos propios que no usa otra variante.
        let statements = [
            "DELETE FROM artifact_location WHERE artifact_id IN (SELECT id FROM model_artifact WHERE variant_id = ?1 \
             AND id NOT IN (SELECT artifact_id FROM artifact_reference WHERE variant_id <> ?1))",
            "DELETE FROM artifact_provenance WHERE artifact_id IN (SELECT id FROM model_artifact WHERE variant_id = ?1 \
             AND id NOT IN (SELECT artifact_id FROM artifact_reference WHERE variant_id <> ?1))",
            "DELETE FROM artifact_reference WHERE variant_id = ?1",
            "DELETE FROM model_bundle WHERE variant_id = ?1",
            "DELETE FROM model_artifact WHERE variant_id = ?1 \
             AND id NOT IN (SELECT artifact_id FROM artifact_reference WHERE variant_id <> ?1)",
            "DELETE FROM model_variant WHERE id = ?1",
        ];
        for sql in statements {
            sqlx::query(sql)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_error("borrando la variante", &e))?;
        }
        event(
            &mut tx,
            "model_variant",
            id,
            expected + 1,
            serde_json::json!({"action": "deleted"}),
            ctx,
        )
        .await?;
        // Las claves ajenas diferidas se comprueban aquí: si algo más usa la
        // variante (instancias, compatibilidad, benchmarks…), no se borra nada.
        tx.commit().await.map_err(|e| {
            LarmError::new(
                ErrorCode::Conflict,
                "La variante está en uso por otros registros",
                "variant_in_use",
            )
            .with_resource(id.to_string())
            .with_technical(e.to_string())
        })
    }

    async fn set_bundle_state(
        &self,
        id: &str,
        expected: i64,
        state: BundleState,
        evidence: serde_json::Value,
        ctx: &MutationContext,
    ) -> Result<i64, LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let from: Option<String> = sqlx::query_scalar(
            "UPDATE model_bundle SET state = ?, updated_at = ?, row_version = row_version + 1, \
             metadata = json_set(metadata, '$.verification', json(?)) \
             WHERE id = ? AND row_version = ? RETURNING state",
        )
        .bind(state.as_str())
        .bind(&now)
        .bind(evidence.to_string())
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("actualizando model_bundle", &e))?;
        if from.is_none() {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El bundle cambió o no existe",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        }
        let version = expected + 1;
        event(
            &mut tx,
            "model_bundle",
            id,
            version,
            serde_json::json!({"action": "verified", "state": state.as_str()}),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(version)
    }

    async fn models_with_artifact_hashes(
        &self,
        hashes: &[String],
    ) -> Result<Vec<CanonicalModel>, LarmError> {
        if hashes.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = vec!["?"; hashes.len()].join(",");
        let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT d.canonical_model_id FROM model_artifact a \
             JOIN model_variant v ON v.id = a.variant_id \
             JOIN model_revision r ON r.id = v.revision_id \
             JOIN model_distribution d ON d.id = r.distribution_id \
             WHERE a.sha256 IN ({placeholders}) ORDER BY d.canonical_model_id"
        )));
        for h in hashes {
            query = query.bind(h);
        }
        let ids = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error("buscando modelos por hash", &e))?;
        let models = super::models::SqlModels {
            pool: self.pool.clone(),
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(m) = larm_domain::ports::ModelRepository::get_model(&models, &id).await? {
                out.push(m);
            }
        }
        Ok(out)
    }
}

/// Shared write inside the caller-owned transaction; never commits.
pub(crate) async fn insert_variant_in(
    tx: &mut SqliteConnection,
    v: &ModelVariant,
    artifacts: &[ModelArtifact],
    bundle: Option<&ModelBundle>,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    v.validate()?;
    for a in artifacts {
        a.validate()?;
        if a.variant_id != v.id {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Artefacto de otra variante",
                "artifact_variant_mismatch",
            ));
        }
    }
    if let Some(hash) = &v.bundle_hash {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM model_variant WHERE revision_id = ? AND format IS ? \
             AND coalesce(quantization, '') = coalesce(?, '') AND architecture IS ? AND bundle_hash = ?",
        )
        .bind(&v.revision_id)
        .bind(&v.format)
        .bind(&v.quantization)
        .bind(&v.architecture)
        .bind(hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_variant", &e))?;
        if let Some(existing) = existing {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Esa variante ya está registrada",
                "variant_exists",
            )
            .with_resource(existing));
        }
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO model_variant ({VARIANT}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&v.id)
    .bind(&v.created_at)
    .bind(&v.updated_at)
    .bind(v.row_version)
    .bind(to_json(&v.metadata))
    .bind(&v.revision_id)
    .bind(&v.format)
    .bind(&v.quantization)
    .bind(&v.architecture)
    .bind(&v.bundle_hash)
    .bind(&v.lifecycle)
    .bind(&v.availability)
    .bind(v.ownership.as_str())
    .bind(&v.promotion)
    .bind(&v.system1_state)
    .bind(&v.system_role)
    .bind(to_json(&v.capabilities))
    .bind(to_json(&v.model_limits))
    .bind(i64::from(v.update_available))
    .bind(v.derived_status())
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("insertando model_variant", &e))?;
    event(
        &mut *tx,
        "model_variant",
        &v.id,
        v.row_version,
        serde_json::json!({"action": "created", "lifecycle": v.lifecycle, "revision_id": v.revision_id}),
        ctx,
    )
    .await?;
    for a in artifacts {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO model_artifact ({ARTIFACT}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&a.id)
        .bind(&a.created_at)
        .bind(&a.updated_at)
        .bind(a.row_version)
        .bind(to_json(&a.metadata))
        .bind(&a.variant_id)
        .bind(&a.filename)
        .bind(&a.format)
        .bind(&a.quantization)
        .bind(as_i64(a.expected_size))
        .bind(as_i64(a.actual_size))
        .bind(&a.sha256)
        .bind(&a.source_url)
        .bind(i64::from(a.required))
        .bind(to_json(&a.size_estimate))
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando model_artifact", &e))?;
    }
    if let Some(b) = bundle {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO model_bundle ({BUNDLE}) VALUES (?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&b.id)
        .bind(&b.created_at)
        .bind(&b.updated_at)
        .bind(b.row_version)
        .bind(to_json(&b.metadata))
        .bind(&b.variant_id)
        .bind(to_json(&b.required_artifact_ids))
        .bind(to_json(&b.optional_artifact_ids))
        .bind(&b.manifest_hash)
        .bind(b.state.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando model_bundle", &e))?;
        event(
            &mut *tx,
            "model_bundle",
            &b.id,
            b.row_version,
            serde_json::json!({"action": "created", "state": b.state.as_str(), "variant_id": b.variant_id}),
            ctx,
        )
        .await?;
    }
    Ok(())
}
