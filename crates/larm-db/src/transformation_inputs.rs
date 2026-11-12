//! One read transaction for P093 catalogue descriptors and physical-copy
//! candidates. This snapshot is not physical verification or execution rights.
use crate::pool::db_error;
use larm_domain::{
    artifacts::ArtifactLocation,
    bridge::is_uuid_v7,
    error::{ErrorCode, LarmError},
    locations::StorageLocation,
    model::{bundle_hash, BundleEntry, BundleState, ModelArtifact, ModelBundle, ModelVariant},
    transformations::{InputArtifact, TransformationInputIdentity},
};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};

pub struct TransformationInputCatalogue {
    pub identity: TransformationInputIdentity,
    pub variant: ModelVariant,
    pub bundle: ModelBundle,
    pub artifacts: Vec<InputArtifact>,
    pub copies: Vec<ArtifactLocation>,
    pub storage_locations: Vec<StorageLocation>,
}

#[derive(Clone)]
pub struct SqlTransformationInputs {
    /// Use Db::qualification_reader in production: callers may hold the writer
    /// and normal consumers may already be waiting for that writer's pool.
    pub pool: SqlitePool,
}
fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::InvalidStateTransition,
        "El modelo no tiene una entrada local verificada",
        reason,
    )
}

/// Membership is relative to the consuming bundle. A referenced artifact keeps
/// its source identity and declaration; its required/optional role here belongs
/// to this bundle, not to the variant which originally acquired it.
async fn physical_members(
    tx: &mut SqliteConnection,
    bundle: &ModelBundle,
    declared: Vec<ModelArtifact>,
) -> Result<Vec<ModelArtifact>, LarmError> {
    let required: BTreeSet<_> = bundle.required_artifact_ids.iter().collect();
    let optional: BTreeSet<_> = bundle.optional_artifact_ids.iter().collect();
    if required.is_empty()
        || required.len() != bundle.required_artifact_ids.len()
        || optional.len() != bundle.optional_artifact_ids.len()
        || !required.is_disjoint(&optional)
        || !required.iter().chain(&optional).all(|id| is_uuid_v7(id))
        || declared
            .iter()
            .any(|a| a.variant_id == bundle.variant_id && a.required && !required.contains(&a.id))
    {
        return Err(unavailable("transformation_input_membership_invalid"));
    }
    let mut chosen: Vec<_> = declared
        .into_iter()
        .filter(|a| required.contains(&a.id) || optional.contains(&a.id))
        .collect();
    if chosen.len() != required.len() + optional.len()
        || chosen
            .iter()
            .any(|a| a.variant_id == bundle.variant_id && a.required != required.contains(&a.id))
    {
        return Err(unavailable("transformation_input_membership_invalid"));
    }
    // Preserve the downloader's stricter owner and exact-acquisition checks.
    if chosen.iter().all(|a| a.variant_id == bundle.variant_id) {
        return crate::download_bundle::physical_artifacts(tx, bundle, chosen).await;
    }
    let mut owners = BTreeMap::new();
    for artifact in &mut chosen {
        if artifact.sha256.is_none() || artifact.expected_size.is_none() {
            if !owners.contains_key(&artifact.variant_id) {
                let rows = sqlx::query("SELECT * FROM model_bundle WHERE variant_id=? AND state='VERIFIED' ORDER BY created_at DESC,id DESC")
                    .bind(&artifact.variant_id).fetch_all(&mut *tx).await
                    .map_err(|e| db_error("bundles históricos de entrada", &e))?;
                let rows = rows
                    .iter()
                    .map(crate::variants::to_bundle)
                    .collect::<Result<Vec<_>, _>>()?;
                let declarations = sqlx::query("SELECT * FROM model_artifact WHERE variant_id=?")
                    .bind(&artifact.variant_id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|e| db_error("declaraciones de entrada compartida", &e))?;
                let declarations = declarations
                    .iter()
                    .map(crate::variants::to_artifact)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut projected = BTreeMap::new();
                for original in rows {
                    // A historical bundle need not contain newer declarations.
                    // Its own exact membership, seal and hash still must pass.
                    let members: Vec<_> = declarations
                        .iter()
                        .filter(|a| {
                            original.required_artifact_ids.contains(&a.id)
                                || original.optional_artifact_ids.contains(&a.id)
                        })
                        .cloned()
                        .collect();
                    if let Ok(physical) =
                        crate::download_bundle::physical_artifacts(tx, &original, members).await
                    {
                        for a in physical {
                            if let Some(previous) = projected.insert(a.id.clone(), a.clone()) {
                                if previous.sha256 != a.sha256
                                    || previous.expected_size != a.expected_size
                                {
                                    return Err(unavailable(
                                        "transformation_input_evidence_ambiguous",
                                    ));
                                }
                            }
                        }
                    }
                }
                owners.insert(artifact.variant_id.clone(), projected);
            }
            let physical = owners
                .get(&artifact.variant_id)
                .and_then(|m| m.get(&artifact.id))
                .ok_or_else(|| unavailable("transformation_input_evidence_missing"))?;
            artifact.sha256 = physical.sha256.clone();
            artifact.expected_size = physical.expected_size;
        }
        artifact.required = required.contains(&artifact.id);
    }
    let entries = chosen
        .iter()
        .map(|a| {
            Some(BundleEntry {
                relative_path: a.filename.clone(),
                sha256: a.sha256.clone()?,
                size_bytes: a.expected_size?.get(),
                required: a.required,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| unavailable("transformation_input_evidence_missing"))?;
    if bundle_hash(&entries)? != bundle.manifest_hash {
        return Err(unavailable("transformation_input_manifest_mismatch"));
    }
    Ok(chosen)
}

impl SqlTransformationInputs {
    pub async fn load(&self, variant_id: &str) -> Result<TransformationInputCatalogue, LarmError> {
        if !is_uuid_v7(variant_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "La variante no es válida",
                "transformation_input_variant_invalid",
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| db_error("BEGIN input de transformación", &e))?;
        let row = sqlx::query("SELECT * FROM model_variant WHERE id=?")
            .bind(variant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_error("variante de transformación", &e))?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Variante desconocida",
                    "transformation_input_missing",
                )
            })?;
        let variant = crate::variants::to_variant(&row)?;
        if !matches!(
            variant.lifecycle.as_str(),
            "VERIFIED" | "PREPARED" | "TECHNICALLY_VALIDATED" | "AVAILABLE" | "RUNNING"
        ) || matches!(variant.availability.as_str(), "OFFLINE" | "CORRUPT")
        {
            return Err(unavailable("transformation_input_not_verified"));
        }
        let row=sqlx::query("SELECT * FROM model_bundle WHERE variant_id=? ORDER BY created_at DESC,id DESC LIMIT 1")
            .bind(variant_id).fetch_optional(&mut *tx).await.map_err(|e|db_error("bundle de transformación",&e))?
            .ok_or_else(||unavailable("transformation_input_bundle_missing"))?;
        let bundle = crate::variants::to_bundle(&row)?;
        let identity = TransformationInputIdentity {
            variant_id: variant_id.into(),
            revision_id: variant.revision_id.clone(),
            variant_bundle_hash: variant
                .bundle_hash
                .clone()
                .ok_or_else(|| unavailable("transformation_input_identity_missing"))?,
            bundle_id: bundle.id.clone(),
            bundle_row_version: bundle.row_version,
            bundle_manifest_hash: bundle.manifest_hash.clone(),
        };
        if bundle.state != BundleState::Verified
            || bundle.row_version < 1
            || !is_uuid_v7(&bundle.id)
            || !is_uuid_v7(&variant.revision_id)
            || ![
                &identity.variant_bundle_hash,
                &identity.bundle_manifest_hash,
            ]
            .into_iter()
            .all(|h| larm_domain::adapter_updates::is_sha256(h))
        {
            return Err(unavailable("transformation_input_bundle_invalid"));
        }
        let rows=sqlx::query("SELECT a.* FROM model_artifact a WHERE a.variant_id=? OR EXISTS(SELECT 1 FROM artifact_reference r WHERE r.artifact_id=a.id AND r.variant_id=?) ORDER BY a.filename,a.id")
            .bind(variant_id).bind(variant_id).fetch_all(&mut *tx).await.map_err(|e|db_error("artefactos de transformación",&e))?;
        let declared = rows
            .iter()
            .map(crate::variants::to_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        // Reuse immutable completed acquisition evidence for unknown upstream
        // hashes; never promote a mutable location checksum to source truth.
        let physical = physical_members(&mut tx, &bundle, declared).await?;
        let entries = physical
            .iter()
            .map(|a| {
                Ok(larm_domain::model::BundleEntry {
                    relative_path: a.filename.clone(),
                    sha256: a
                        .sha256
                        .clone()
                        .ok_or_else(|| unavailable("transformation_input_hash_unknown"))?,
                    size_bytes: a
                        .expected_size
                        .ok_or_else(|| unavailable("transformation_input_size_unknown"))?
                        .get(),
                    required: a.required,
                })
            })
            .collect::<Result<Vec<_>, LarmError>>()?;
        if larm_domain::model::required_hash(&entries)? != identity.variant_bundle_hash {
            return Err(unavailable("transformation_input_bundle_identity_mismatch"));
        }
        let artifacts = physical
            .iter()
            .zip(&entries)
            .map(|(a, e)| {
                Ok(InputArtifact {
                    artifact_id: a.id.clone(),
                    relative_path: e.relative_path.clone(),
                    size_bytes: larm_domain::units::DecimalU64::new(e.size_bytes)
                        .ok_or_else(|| unavailable("transformation_input_size_unknown"))?,
                    sha256: e.sha256.clone(),
                })
            })
            .collect::<Result<Vec<_>, LarmError>>()?;
        larm_domain::transformations::verify_observed_input_hashes(&artifacts, &artifacts)
            .map_err(|_| unavailable("transformation_input_descriptors_invalid"))?;
        let ids =
            serde_json::to_string(&artifacts.iter().map(|a| &a.artifact_id).collect::<Vec<_>>())
                .map_err(|e| db_error("miembros de transformación", &e))?;
        let rows=sqlx::query("SELECT l.* FROM artifact_location l JOIN json_each(?) a ON a.value=l.artifact_id ORDER BY l.artifact_id,l.id")
            .bind(ids).fetch_all(&mut *tx).await.map_err(|e|db_error("copias de transformación",&e))?;
        let copies = rows
            .iter()
            .map(crate::artifacts::to_location)
            .collect::<Result<Vec<_>, _>>()?;
        let rows = sqlx::query("SELECT * FROM storage_location ORDER BY priority,id")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| db_error("raíces de transformación", &e))?;
        let storage_locations = rows
            .iter()
            .map(crate::locations::to_location)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()
            .await
            .map_err(|e| db_error("COMMIT input de transformación", &e))?;
        Ok(TransformationInputCatalogue {
            identity,
            variant,
            bundle,
            artifacts,
            copies,
            storage_locations,
        })
    }
}
