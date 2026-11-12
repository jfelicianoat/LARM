//! Registro de modelos sobre SQLite (P040): `canonical_model`,
//! `model_distribution`, `model_revision` y `artifact_provenance`.
//!
//! Cada alta valida el dominio y comprueba la identidad del contrato 01 en
//! una transacción que toma el bloqueo de escritura antes de leer, y escribe
//! su evento `model.state_changed` en el outbox de la misma transacción.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::model::{
    ArtifactProvenance, CanonicalModel, ModelDistribution, ModelRevision, RevisionState,
};
use larm_domain::ports::{
    DistributionFilter, ModelFilter, ModelRepository, MutationContext, RevisionFilter,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::events::{append, NewEvent};
use super::pool::db_error;

const MODEL: &str =
    "id,created_at,updated_at,row_version,metadata,name,upstream_identity,capabilities,possible_duplicates";
const DISTRIBUTION: &str =
    "id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,\
upstream_namespace,upstream_model_id,license_id";
const REVISION: &str =
    "id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,\
upstream_commit,manifest_hash,state";
const PROVENANCE: &str =
    "id,created_at,updated_at,row_version,metadata,artifact_id,source_id,revision_key,\
parents,tool_id,tool_version,parameters,license_id,license_hash,trust,actor_id,evidence_refs";

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
fn version(row: &SqliteRow) -> Result<i64, LarmError> {
    row.try_get("row_version")
        .map_err(|e| db_error("row_version", &e))
}
fn enum_from<T: serde::de::DeserializeOwned>(row: &SqliteRow, c: &str) -> Result<T, LarmError> {
    serde_json::from_value(serde_json::Value::String(text(row, c)?)).map_err(|e| db_error(c, &e))
}
fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub(crate) fn to_model(row: &SqliteRow) -> Result<CanonicalModel, LarmError> {
    Ok(CanonicalModel {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: version(row)?,
        metadata: json(row, "metadata")?,
        name: text(row, "name")?,
        upstream_identity: opt(row, "upstream_identity")?,
        capabilities: json(row, "capabilities")?,
        possible_duplicates: json(row, "possible_duplicates")?,
    })
}

pub(crate) fn to_distribution(row: &SqliteRow) -> Result<ModelDistribution, LarmError> {
    Ok(ModelDistribution {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: version(row)?,
        metadata: json(row, "metadata")?,
        canonical_model_id: text(row, "canonical_model_id")?,
        source_id: text(row, "source_id")?,
        upstream_namespace: text(row, "upstream_namespace")?,
        upstream_model_id: text(row, "upstream_model_id")?,
        license_id: opt(row, "license_id")?,
    })
}

pub(crate) fn to_revision(row: &SqliteRow) -> Result<ModelRevision, LarmError> {
    Ok(ModelRevision {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: version(row)?,
        metadata: json(row, "metadata")?,
        distribution_id: text(row, "distribution_id")?,
        revision_key: text(row, "revision_key")?,
        upstream_commit: opt(row, "upstream_commit")?,
        manifest_hash: text(row, "manifest_hash")?,
        state: enum_from(row, "state")?,
    })
}

pub(crate) fn to_provenance(row: &SqliteRow) -> Result<ArtifactProvenance, LarmError> {
    Ok(ArtifactProvenance {
        id: text(row, "id")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        row_version: version(row)?,
        metadata: json(row, "metadata")?,
        artifact_id: text(row, "artifact_id")?,
        source_id: opt(row, "source_id")?,
        revision_key: opt(row, "revision_key")?,
        parents: json(row, "parents")?,
        tool_id: opt(row, "tool_id")?,
        tool_version: opt(row, "tool_version")?,
        parameters: json(row, "parameters")?,
        license_id: opt(row, "license_id")?,
        license_hash: opt(row, "license_hash")?,
        trust: enum_from(row, "trust")?,
        actor_id: text(row, "actor_id")?,
        evidence_refs: json(row, "evidence_refs")?,
    })
}

async fn write_lock(tx: &mut SqliteConnection, table: &str) -> Result<(), LarmError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {table} SET row_version = row_version WHERE 0"
    )))
    .execute(&mut *tx)
    .await
    .map(|_| ())
    .map_err(|e| db_error("bloqueo de escritura", &e))
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

/// Guarda procedencia en la transacción que confirma una descarga y su
/// ubicación, para que ningún artefacto visible pierda su origen.
pub(crate) async fn insert_provenance_in(
    tx: &mut SqliteConnection,
    provenance: &ArtifactProvenance,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    provenance.validate()?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO artifact_provenance ({PROVENANCE}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&provenance.id)
    .bind(&provenance.created_at)
    .bind(&provenance.updated_at)
    .bind(provenance.row_version)
    .bind(to_json(&provenance.metadata))
    .bind(&provenance.artifact_id)
    .bind(&provenance.source_id)
    .bind(&provenance.revision_key)
    .bind(to_json(&provenance.parents))
    .bind(&provenance.tool_id)
    .bind(&provenance.tool_version)
    .bind(to_json(&provenance.parameters))
    .bind(&provenance.license_id)
    .bind(&provenance.license_hash)
    .bind(provenance.trust.as_str())
    .bind(&provenance.actor_id)
    .bind(to_json(&provenance.evidence_refs))
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("insertando artifact_provenance", &e))?;
    event(
        tx,
        "artifact_provenance",
        &provenance.id,
        provenance.row_version,
        serde_json::json!({
            "action": "created",
            "artifact_id": provenance.artifact_id,
            "trust": provenance.trust.as_str(),
        }),
        ctx,
    )
    .await?;
    Ok(())
}

fn conflict(reason: &str, message: &str, existing: &str) -> LarmError {
    LarmError::new(ErrorCode::Conflict, message, reason).with_resource(existing.to_string())
}

fn like(q: Option<&String>) -> Option<String> {
    q.filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q.to_lowercase()))
}

#[derive(Clone)]
pub struct SqlModels {
    pub pool: SqlitePool,
}

#[async_trait]
impl ModelRepository for SqlModels {
    async fn list_models(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &ModelFilter,
    ) -> Result<Vec<CanonicalModel>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let q = like(filter.q.as_ref());
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MODEL} FROM canonical_model WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR lower(name) LIKE ? OR lower(coalesce(upstream_identity, '')) LIKE ?) \
             AND (? IS NULL OR EXISTS (SELECT 1 FROM json_each(capabilities) WHERE value = ?)) \
             ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(&q)
        .bind(&q)
        .bind(&q)
        .bind(&filter.capability)
        .bind(&filter.capability)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando canonical_model", &e))?;
        rows.iter().map(to_model).collect()
    }

    async fn get_model(&self, id: &str) -> Result<Option<CanonicalModel>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MODEL} FROM canonical_model WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo canonical_model", &e))?
        .as_ref()
        .map(to_model)
        .transpose()
    }

    async fn model_by_upstream(&self, upstream: &str) -> Result<Option<CanonicalModel>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MODEL} FROM canonical_model WHERE upstream_identity = ? ORDER BY created_at, id LIMIT 1"
        )))
        .bind(upstream)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo canonical_model", &e))?
        .as_ref()
        .map(to_model)
        .transpose()
    }

    async fn models_named(&self, name: &str) -> Result<Vec<CanonicalModel>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MODEL} FROM canonical_model WHERE lower(name) = lower(?) ORDER BY created_at, id"
        )))
        .bind(name.trim())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo canonical_model", &e))?;
        rows.iter().map(to_model).collect()
    }

    async fn models_by_evidence(
        &self,
        field: &str,
        value: &str,
    ) -> Result<Vec<CanonicalModel>, LarmError> {
        if !larm_domain::ports::EVIDENCE_FIELDS.contains(&field) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Campo de evidencia no previsto",
                "evidence_field_invalid",
            ));
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MODEL} FROM canonical_model \
             WHERE lower(json_extract(metadata, ?)) = lower(?) ORDER BY created_at, id"
        )))
        .bind(format!("$.identity_evidence.{field}"))
        .bind(value)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("buscando por evidencia", &e))?;
        rows.iter().map(to_model).collect()
    }

    async fn insert_model(
        &self,
        m: &CanonicalModel,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "canonical_model").await?;
        insert_model_in(&mut tx, m, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn add_possible_duplicate(
        &self,
        id: &str,
        duplicate: &str,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "canonical_model").await?;
        add_possible_duplicate_in(&mut tx, id, duplicate, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn list_distributions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &DistributionFilter,
    ) -> Result<Vec<ModelDistribution>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let q = like(filter.q.as_ref());
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DISTRIBUTION} FROM model_distribution WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR lower(upstream_namespace || '/' || upstream_model_id) LIKE ?) \
             AND (? IS NULL OR source_id = ?) ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(&q)
        .bind(&q)
        .bind(&filter.source_id)
        .bind(&filter.source_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando model_distribution", &e))?;
        rows.iter().map(to_distribution).collect()
    }

    async fn get_distribution(&self, id: &str) -> Result<Option<ModelDistribution>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DISTRIBUTION} FROM model_distribution WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_distribution", &e))?
        .as_ref()
        .map(to_distribution)
        .transpose()
    }

    async fn distribution_by_identity(
        &self,
        source_id: &str,
        namespace: &str,
        model_id: &str,
    ) -> Result<Option<ModelDistribution>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DISTRIBUTION} FROM model_distribution WHERE source_id = ? AND upstream_namespace = ? AND upstream_model_id = ?"
        )))
        .bind(source_id)
        .bind(namespace)
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_distribution", &e))?
        .as_ref()
        .map(to_distribution)
        .transpose()
    }

    async fn insert_distribution(
        &self,
        d: &ModelDistribution,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "model_distribution").await?;
        insert_distribution_in(&mut tx, d, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn list_revisions(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RevisionFilter,
    ) -> Result<Vec<ModelRevision>, LarmError> {
        let (created, id) = after.unwrap_or_default();
        let state = filter.state.map(RevisionState::as_str);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION} FROM model_revision WHERE (created_at, id) > (?, ?) \
             AND (? IS NULL OR state = ?) ORDER BY created_at, id LIMIT ?"
        )))
        .bind(created)
        .bind(id)
        .bind(state)
        .bind(state)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("listando model_revision", &e))?;
        rows.iter().map(to_revision).collect()
    }

    async fn get_revision(&self, id: &str) -> Result<Option<ModelRevision>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION} FROM model_revision WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_revision", &e))?
        .as_ref()
        .map(to_revision)
        .transpose()
    }

    async fn revision_by_key(
        &self,
        distribution_id: &str,
        key: &str,
    ) -> Result<Option<ModelRevision>, LarmError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION} FROM model_revision WHERE distribution_id = ? AND revision_key = ?"
        )))
        .bind(distribution_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("leyendo model_revision", &e))?
        .as_ref()
        .map(to_revision)
        .transpose()
    }

    async fn insert_revision(
        &self,
        r: &ModelRevision,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "model_revision").await?;
        insert_revision_in(&mut tx, r, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn replace_current_revision(
        &self,
        r: &ModelRevision,
        ctx: &MutationContext,
    ) -> Result<Option<String>, LarmError> {
        r.validate()?;
        if r.state != RevisionState::Current {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Solo una revisión CURRENT sustituye a otra",
                "replacement_not_current",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "model_revision").await?;
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let old: Option<(String, i64)> = sqlx::query_as(
            "UPDATE model_revision SET state = 'SUPERSEDED', updated_at = ?, row_version = row_version + 1 \
             WHERE distribution_id = ? AND state = 'CURRENT' AND revision_key <> ? RETURNING id, row_version",
        )
        .bind(&now)
        .bind(&r.distribution_id)
        .bind(&r.revision_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("sustituyendo model_revision", &e))?;
        if let Some((id, version)) = &old {
            event(
                &mut tx,
                "model_revision",
                id,
                *version,
                serde_json::json!({"action": "superseded", "by": r.id}),
                ctx,
            )
            .await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO model_revision ({REVISION}) VALUES (?,?,?,?,?,?,?,?,?,?)"
        )))
        .bind(&r.id)
        .bind(&r.created_at)
        .bind(&r.updated_at)
        .bind(r.row_version)
        .bind(to_json(&r.metadata))
        .bind(&r.distribution_id)
        .bind(&r.revision_key)
        .bind(&r.upstream_commit)
        .bind(&r.manifest_hash)
        .bind(r.state.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error("insertando model_revision", &e))?;
        event(
            &mut tx,
            "model_revision",
            &r.id,
            r.row_version,
            serde_json::json!({"action": "created", "state": r.state.as_str(), "supersedes": old.as_ref().map(|o| &o.0)}),
            ctx,
        )
        .await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))?;
        Ok(old.map(|o| o.0))
    }

    async fn insert_provenance(
        &self,
        p: &ArtifactProvenance,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let mut tx = self.pool.begin().await.map_err(|e| db_error("BEGIN", &e))?;
        write_lock(&mut tx, "artifact_provenance").await?;
        insert_provenance_in(&mut tx, p, ctx).await?;
        tx.commit().await.map_err(|e| db_error("COMMIT", &e))
    }

    async fn provenance_for(
        &self,
        artifact_id: &str,
    ) -> Result<Vec<ArtifactProvenance>, LarmError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {PROVENANCE} FROM artifact_provenance WHERE artifact_id = ? ORDER BY created_at, id"
        )))
        .bind(artifact_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error("leyendo artifact_provenance", &e))?;
        rows.iter().map(to_provenance).collect()
    }
}

/// Marcadores de las fuentes citadas por manifiestos durante una
/// reconstrucción (P018): deshabilitadas, sin adaptador ni credenciales.
/// No se inventa su configuración; el operador las completa después.
///
/// # Errors
/// Fallo de la base.
pub async fn insert_recovered_sources(pool: &SqlitePool, ids: &[String]) -> Result<(), LarmError> {
    let mut conn = pool.acquire().await.map_err(|e| db_error("acquire", &e))?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("pragma", &e))?;
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let mut result = Ok(());
    for id in ids {
        if let Err(e) = sqlx::query(
            "INSERT OR IGNORE INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,             priority,enabled,auth_mode,account_id,policy) VALUES (?1,?2,?2,1,'{\"recovered\":true}','recuperada','unknown',             'unknown:',0,0,'NONE',NULL,'{}')",
        )
        .bind(id)
        .bind(&now)
        .execute(&mut *conn)
        .await
        {
            result = Err(db_error("insertando model_source", &e));
            break;
        }
    }
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error("pragma", &e))?;
    result
}

/// Shared write inside the caller-owned transaction; never commits.
pub(crate) async fn insert_model_in(
    tx: &mut SqliteConnection,
    m: &CanonicalModel,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    m.validate()?;
    if let Some(upstream) = &m.upstream_identity {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM canonical_model WHERE upstream_identity = ?")
                .bind(upstream)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_error("leyendo canonical_model", &e))?;
        if let Some(existing) = existing {
            return Err(conflict(
                "canonical_model_exists",
                "Ya hay un modelo con esa identidad upstream",
                &existing,
            ));
        }
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO canonical_model ({MODEL}) VALUES (?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&m.id)
    .bind(&m.created_at)
    .bind(&m.updated_at)
    .bind(m.row_version)
    .bind(to_json(&m.metadata))
    .bind(m.name.trim())
    .bind(&m.upstream_identity)
    .bind(to_json(&m.capabilities))
    .bind(to_json(&m.possible_duplicates))
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("insertando canonical_model", &e))?;
    event(
        &mut *tx,
        "canonical_model",
        &m.id,
        m.row_version,
        serde_json::json!({"action": "created", "possible_duplicates": m.possible_duplicates}),
        ctx,
    )
    .await?;
    Ok(())
}

/// Shared write inside the caller-owned transaction; never commits.
pub(crate) async fn insert_distribution_in(
    tx: &mut SqliteConnection,
    d: &ModelDistribution,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    d.validate()?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM model_distribution WHERE source_id = ? AND upstream_namespace = ? AND upstream_model_id = ?",
    )
    .bind(&d.source_id)
    .bind(&d.upstream_namespace)
    .bind(&d.upstream_model_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error("leyendo model_distribution", &e))?;
    if let Some(existing) = existing {
        return Err(conflict(
            "distribution_exists",
            "Esa publicación ya está registrada",
            &existing,
        ));
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO model_distribution ({DISTRIBUTION}) VALUES (?,?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&d.id)
    .bind(&d.created_at)
    .bind(&d.updated_at)
    .bind(d.row_version)
    .bind(to_json(&d.metadata))
    .bind(&d.canonical_model_id)
    .bind(&d.source_id)
    .bind(&d.upstream_namespace)
    .bind(&d.upstream_model_id)
    .bind(&d.license_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("insertando model_distribution", &e))?;
    event(
        &mut *tx,
        "model_distribution",
        &d.id,
        d.row_version,
        serde_json::json!({"action": "created", "canonical_model_id": d.canonical_model_id}),
        ctx,
    )
    .await?;
    Ok(())
}

/// Shared write inside the caller-owned transaction; never commits.
pub(crate) async fn insert_revision_in(
    tx: &mut SqliteConnection,
    r: &ModelRevision,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    r.validate()?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM model_revision WHERE distribution_id = ? AND revision_key = ?",
    )
    .bind(&r.distribution_id)
    .bind(&r.revision_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error("leyendo model_revision", &e))?;
    if let Some(existing) = existing {
        return Err(conflict(
            "revision_exists",
            "Esa revisión ya está registrada",
            &existing,
        ));
    }
    if r.state == RevisionState::Current {
        let current: Option<String> = sqlx::query_scalar(
            "SELECT id FROM model_revision WHERE distribution_id = ? AND state = 'CURRENT'",
        )
        .bind(&r.distribution_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_error("leyendo model_revision", &e))?;
        if let Some(current) = current {
            return Err(conflict(
                "current_revision_exists",
                "La distribución ya tiene una revisión actual",
                &current,
            ));
        }
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO model_revision ({REVISION}) VALUES (?,?,?,?,?,?,?,?,?,?)"
    )))
    .bind(&r.id)
    .bind(&r.created_at)
    .bind(&r.updated_at)
    .bind(r.row_version)
    .bind(to_json(&r.metadata))
    .bind(&r.distribution_id)
    .bind(&r.revision_key)
    .bind(&r.upstream_commit)
    .bind(&r.manifest_hash)
    .bind(r.state.as_str())
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("insertando model_revision", &e))?;
    event(
        &mut *tx,
        "model_revision",
        &r.id,
        r.row_version,
        serde_json::json!({"action": "created", "state": r.state.as_str(), "distribution_id": r.distribution_id}),
        ctx,
    )
    .await?;
    Ok(())
}

/// Adds the reciprocal P041 duplicate annotation without committing.
pub(crate) async fn add_possible_duplicate_in(
    tx: &mut SqliteConnection,
    id: &str,
    duplicate: &str,
    ctx: &MutationContext,
) -> Result<(), LarmError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {MODEL} FROM canonical_model WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error("leyendo canonical_model", &e))?
    .ok_or_else(|| {
        LarmError::new(ErrorCode::NotFound, "Modelo desconocido", "not_found")
            .with_resource(id.to_string())
    })?;
    let mut model = to_model(&row)?;
    if model.possible_duplicates.iter().any(|d| d == duplicate) {
        return Ok(());
    }
    model.possible_duplicates.push(duplicate.to_string());
    model.possible_duplicates.sort();
    model.validate()?;
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let next = model.row_version + 1;
    sqlx::query(
        "UPDATE canonical_model SET possible_duplicates = ?, updated_at = ?, row_version = ? WHERE id = ? AND row_version = ?",
    )
    .bind(to_json(&model.possible_duplicates))
    .bind(&now)
    .bind(next)
    .bind(id)
    .bind(model.row_version)
    .execute(&mut *tx)
    .await
    .map_err(|e| db_error("actualizando canonical_model", &e))?;
    event(
        &mut *tx,
        "canonical_model",
        id,
        next,
        serde_json::json!({"action": "possible_duplicate", "duplicate": duplicate}),
        ctx,
    )
    .await?;
    Ok(())
}
