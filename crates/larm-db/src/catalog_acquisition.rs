//! One initial, unverified catalog graph and its remote snapshot, atomically.
//! P041 must resolve canonical identities before calling; conflicts never remap
//! IDs or merge models. No physical bundle, location, job or reservation is made.

use crate::acquisition::{record_in, validate_links};
use crate::models::{
    add_possible_duplicate_in, insert_distribution_in, insert_model_in, insert_revision_in,
};
use crate::pool::db_error;
use crate::variants::insert_variant_in;
use larm_domain::acquisition::ModelAcquisitionManifest;
use larm_domain::bridge::is_uuid_v7;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    CanonicalModel, ModelArtifact, ModelDistribution, ModelRevision, ModelVariant,
};
use larm_domain::ports::{AdapterQualification, MutationContext};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Internal persistence input, not a public import DTO. Existing graph rows
/// must be current rows read by P041. Only the explicit reciprocal duplicate
/// annotations may update existing models; the rest of the graph is reused.
pub struct CatalogAcquisition<'a> {
    /// Read before P041 resolves candidate identities. A newly created model
    /// cannot commit if the catalog changed during that resolution.
    pub expected_catalog_revision: i64,
    /// Source configuration read before RPC; changing it requires fresh IO.
    pub expected_source_revision: i64,
    /// Current candidate rows that need the reciprocal P041 duplicate marker.
    pub identity_updates: &'a [CanonicalModel],
    pub model: &'a CanonicalModel,
    pub distribution: &'a ModelDistribution,
    pub revision: &'a ModelRevision,
    pub variant: &'a ModelVariant,
    pub artifacts: &'a [ModelArtifact],
    pub manifest: &'a ModelAcquisitionManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogAcquisitionReceipt {
    pub model_created: bool,
    pub distribution_created: bool,
    pub revision_created: bool,
    pub variant_created: bool,
    pub snapshot_created: bool,
}

#[derive(Clone)]
pub struct SqlCatalogAcquisitions {
    pub pool: SqlitePool,
    pub qualification: Option<Arc<dyn AdapterQualification>>,
}

fn conflict(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::Conflict,
        "El catálogo cambió; vuelve a resolver su identidad",
        reason,
    )
}

impl CatalogAcquisition<'_> {
    fn validate(&self) -> Result<(), LarmError> {
        self.model.validate()?;
        self.distribution.validate()?;
        self.revision.validate()?;
        self.variant.validate()?;
        self.manifest.validate()?;
        let m = self.manifest;
        let reference = &m.source_reference;
        if self.expected_catalog_revision < 0
            || self.expected_source_revision < 1
            || [
                self.model.id.as_str(),
                self.distribution.id.as_str(),
                self.revision.id.as_str(),
                self.variant.id.as_str(),
            ]
            .iter()
            .any(|id| !is_uuid_v7(id))
            || self.model.name.trim() != self.model.name
            || self.distribution.canonical_model_id != self.model.id
            || self.revision.distribution_id != self.distribution.id
            || self.variant.revision_id != self.revision.id
            || m.variant_id != self.variant.id
            || m.revision_id != self.revision.id
            || m.source_id != self.distribution.source_id
            || reference.upstream_namespace != self.distribution.upstream_namespace
            || reference.upstream_model_id != self.distribution.upstream_model_id
            || reference.revision_key != self.revision.revision_key
            || m.upstream_manifest_hash != self.revision.manifest_hash
            || self.artifacts.len() != m.artifacts.len()
        {
            return Err(conflict("catalog_acquisition_graph_mismatch"));
        }
        let mut ids = BTreeSet::new();
        let mut duplicate_ids = BTreeSet::new();
        for other in self.identity_updates {
            other.validate()?;
            if other.id == self.model.id
                || !duplicate_ids.insert(&other.id)
                || !self.model.possible_duplicates.contains(&other.id)
            {
                return Err(conflict("catalog_acquisition_identity_updates_invalid"));
            }
        }
        for artifact in self.artifacts {
            artifact.validate()?;
            let observed = m
                .artifacts
                .iter()
                .find(|row| row.artifact_id == artifact.id)
                .ok_or_else(|| conflict("catalog_acquisition_artifacts_mismatch"))?;
            if !ids.insert(artifact.id.as_str())
                || artifact.variant_id != self.variant.id
                || artifact.filename != observed.relative_path
                || artifact.expected_size != observed.size_bytes
                || artifact.sha256 != observed.sha256
                || artifact.required != observed.required
                || artifact.role().as_str() != observed.role
                || artifact.size_estimate != observed.size_estimate
            {
                return Err(conflict("catalog_acquisition_artifacts_mismatch"));
            }
        }
        Ok(())
    }
}

async fn existing(
    conn: &mut SqliteConnection,
    table: &str,
    id: &str,
) -> Result<Option<sqlx::sqlite::SqliteRow>, LarmError> {
    // Call sites use only the four literal catalog table names below.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT * FROM {table} WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(|e| db_error("leyendo grafo de catálogo", &e))
}

impl SqlCatalogAcquisitions {
    /// Read this before resolving P041 identities. The transaction checks the
    /// same durable event watermark under the writer lock before creating a
    /// canonical model. Unrelated committed events can conservatively conflict;
    /// the caller resolves again. Pruning delivered events cannot reset it.
    pub async fn resolution_revision(&self) -> Result<i64, LarmError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| db_error("leyendo versión del catálogo", &e))?;
        resolution_revision_in(&mut conn).await
    }
    /// Commits a prepared graph, snapshot and all event/outbox rows together.
    /// Exact replay adds nothing. A different prepared identity fails closed;
    /// callers must resolve again, never treat a UUID conflict as a merge.
    pub async fn record(
        &self,
        graph: &CatalogAcquisition<'_>,
        ctx: &MutationContext,
    ) -> Result<CatalogAcquisitionReceipt, LarmError> {
        graph.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN catálogo remoto", &e))?;
        // Take the writer lock before reading identities, including absent rows.
        sqlx::query("UPDATE canonical_model SET row_version=row_version WHERE 0")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_error("bloqueando catálogo remoto", &e))?;
        let source: Option<(i64, bool)> =
            sqlx::query_as("SELECT row_version,enabled FROM model_source WHERE id=?")
                .bind(&graph.manifest.source_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("revalidando configuración de fuente", &e))?;
        if source != Some((graph.expected_source_revision, true)) {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La fuente cambió durante la consulta",
                "catalog_acquisition_source_changed",
            ));
        }
        let model_created =
            if let Some(row) = existing(&mut tx, "canonical_model", &graph.model.id).await? {
                if crate::models::to_model(&row)? != *graph.model {
                    return Err(conflict("catalog_acquisition_model_changed"));
                }
                false
            } else {
                if resolution_revision_in(&mut tx).await? != graph.expected_catalog_revision {
                    return Err(LarmError::new(
                        ErrorCode::StaleRevision,
                        "El catálogo cambió durante la resolución de identidad",
                        "catalog_acquisition_resolution_changed",
                    ));
                }
                insert_model_in(&mut tx, graph.model, ctx).await?;
                true
            };
        let distribution_created = if let Some(row) =
            existing(&mut tx, "model_distribution", &graph.distribution.id).await?
        {
            if crate::models::to_distribution(&row)? != *graph.distribution {
                return Err(conflict("catalog_acquisition_distribution_changed"));
            }
            false
        } else {
            insert_distribution_in(&mut tx, graph.distribution, ctx).await?;
            true
        };
        let revision_created =
            if let Some(row) = existing(&mut tx, "model_revision", &graph.revision.id).await? {
                if crate::models::to_revision(&row)? != *graph.revision {
                    return Err(conflict("catalog_acquisition_revision_changed"));
                }
                false
            } else {
                insert_revision_in(&mut tx, graph.revision, ctx).await?;
                true
            };

        // A second pre-resolved UUID for this same observation must be resolved
        // by P041 after a concurrent commit, not silently inserted as a duplicate.
        let prior: Option<String> = sqlx::query_scalar(
            "SELECT m.variant_id FROM model_acquisition_manifest m JOIN model_variant v ON v.id=m.variant_id WHERE m.source_id=? AND m.revision_id=? AND m.upstream_manifest_hash=? AND m.variant_id<>? AND v.format IS ? AND coalesce(v.quantization,'')=coalesce(?,'') AND v.architecture IS ? LIMIT 1")
            .bind(&graph.manifest.source_id).bind(&graph.revision.id)
            .bind(&graph.manifest.upstream_manifest_hash).bind(&graph.variant.id)
            .bind(&graph.variant.format).bind(&graph.variant.quantization).bind(&graph.variant.architecture)
            .fetch_optional(&mut *tx).await.map_err(|e| db_error("leyendo identidad remota concurrente", &e))?;
        if prior.is_some() {
            return Err(conflict("catalog_acquisition_identity_changed"));
        }
        let variant_created = if let Some(row) =
            existing(&mut tx, "model_variant", &graph.variant.id).await?
        {
            if crate::variants::to_variant(&row)? != *graph.variant {
                return Err(conflict("catalog_acquisition_variant_changed"));
            }
            let rows =
                sqlx::query("SELECT * FROM model_artifact WHERE variant_id=? ORDER BY filename,id")
                    .bind(&graph.variant.id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|e| db_error("leyendo artefactos de catálogo", &e))?;
            let stored = rows
                .iter()
                .map(crate::variants::to_artifact)
                .collect::<Result<Vec<_>, _>>()?;
            let mut expected = graph.artifacts.to_vec();
            expected.sort_by(|a, b| (&a.filename, &a.id).cmp(&(&b.filename, &b.id)));
            if stored != expected {
                return Err(conflict("catalog_acquisition_artifacts_changed"));
            }
            false
        } else {
            if graph.variant.lifecycle != "DISCOVERED"
                || graph.variant.availability != "UNKNOWN"
                || graph.variant.bundle_hash.is_some()
                || graph.artifacts.iter().any(|a| a.actual_size.is_some())
            {
                return Err(conflict(
                    "catalog_acquisition_new_variant_has_physical_facts",
                ));
            }
            insert_variant_in(&mut tx, graph.variant, graph.artifacts, None, ctx).await?;
            true
        };
        if model_created {
            let declared: BTreeSet<_> = graph.model.possible_duplicates.iter().collect();
            let updates: BTreeSet<_> = graph.identity_updates.iter().map(|m| &m.id).collect();
            if declared != updates {
                return Err(conflict("catalog_acquisition_identity_updates_missing"));
            }
            for other in graph.identity_updates {
                let row = existing(&mut tx, "canonical_model", &other.id)
                    .await?
                    .ok_or_else(|| conflict("catalog_acquisition_identity_candidate_changed"))?;
                if crate::models::to_model(&row)? != *other {
                    return Err(conflict("catalog_acquisition_identity_candidate_changed"));
                }
                add_possible_duplicate_in(&mut tx, &other.id, &graph.model.id, ctx).await?;
            }
        } else if !graph.identity_updates.is_empty() {
            return Err(conflict("catalog_acquisition_identity_updates_invalid"));
        }
        let guard =
            validate_links(&mut tx, graph.manifest, ctx, self.qualification.as_ref()).await?;
        let snapshot_created = record_in(&mut tx, graph.manifest, ctx).await?;
        guard.recheck().await?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT catálogo remoto", &e))?;
        Ok(CatalogAcquisitionReceipt {
            model_created,
            distribution_created,
            revision_created,
            variant_created,
            snapshot_created,
        })
    }
}

async fn resolution_revision_in(conn: &mut SqliteConnection) -> Result<i64, LarmError> {
    sqlx::query_scalar("SELECT coalesce((SELECT max(id) FROM event_sequence_alloc),0)")
        .fetch_one(conn)
        .await
        .map_err(|e| db_error("leyendo versión del catálogo", &e))
}
