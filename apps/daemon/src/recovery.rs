//! Updater y recuperación de LARM (P133; spec §132-§134) sobre la base real:
//!
//! - guard del updater: leases (`REQUESTED`/`ADMITTING`/`ACTIVE`) y jobs del
//!   journal (`QUEUED`/`RUNNING`) activas exigen confirmación;
//! - `update_larm`: verificación del staged (sha256 del fichero en
//!   `<data_root>/updates/staging`), backup previo de la base con
//!   verificación (`larm_db::backup`) y bundle determinista con el manifiesto
//!   de digests (`larm_core::backup`) — el reinicio y la migración los aplica
//!   el arranque siguiente (P132);
//! - `recovery_rebuild`: rebuild del registro desde las raíces de las
//!   ubicaciones de almacenamiento hacia una base de staging (`P018`), con el
//!   informe persistido para el operador.

use larm_db::jobs::SqlJournalJobs;
use larm_domain::bridge::{JobPhase, JobReceipt};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{JournalJobRepository, MutationContext, RecoveryOps};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

/// Tipos de job del journal (uno activo por tipo).
pub const UPDATE_JOB_KIND: &str = "larm.update";
pub const REBUILD_JOB_KIND: &str = "recovery.rebuild";

pub struct DaemonRecovery {
    pub pool: sqlx::SqlitePool,
    pub settings: larm_core::config::Settings,
    pub data_root: PathBuf,
}

impl DaemonRecovery {
    fn update_journal(&self) -> SqlJournalJobs {
        SqlJournalJobs {
            pool: self.pool.clone(),
            kind: UPDATE_JOB_KIND,
            resource_type: "larm",
        }
    }

    fn rebuild_journal(&self) -> SqlJournalJobs {
        SqlJournalJobs {
            pool: self.pool.clone(),
            kind: REBUILD_JOB_KIND,
            resource_type: "larm",
        }
    }

    fn receipt(job: &larm_domain::ports::JournalJob) -> JobReceipt {
        JobReceipt {
            job_id: job.state.job_id.clone(),
            status_url: format!("/api/v1/jobs/{}", job.state.job_id),
            state: JobPhase::Running,
        }
    }

    /// Verifica el artefacto staged del updater, si el operador lo dejó en
    /// `<data_root>/updates/staging`: `larm-update.bin` junto a su sha256 en
    /// `larm-update.bin.sha256`.
    fn verify_staged(&self) -> Result<Option<String>, LarmError> {
        let staging = self.data_root.join("updates").join("staging");
        let artifact = staging.join("larm-update.bin");
        let digest_file = staging.join("larm-update.bin.sha256");
        if !artifact.is_file() {
            return Ok(None);
        }
        let expected = std::fs::read_to_string(&digest_file)
            .map_err(|e| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "El staged no tiene su sha256 de referencia",
                    "update_staged_digest_missing",
                )
                .with_technical(e.to_string())
            })?
            .trim()
            .to_ascii_lowercase();
        let bytes = std::fs::read(&artifact).map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Leyendo el staged", "io_failed")
                .with_technical(e.to_string())
        })?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != expected {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El staged no coincide con su sha256 de referencia",
                "update_staged_hash_mismatch",
            ));
        }
        Ok(Some(actual))
    }

    /// Raíces de las ubicaciones de almacenamiento registradas.
    async fn storage_roots(&self) -> Result<Vec<PathBuf>, LarmError> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT root_path FROM storage_location")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                LarmError::new(ErrorCode::Internal, "Leyendo ubicaciones", "io_failed")
                    .with_technical(e.to_string())
            })?;
        Ok(rows.into_iter().map(PathBuf::from).collect())
    }
}

#[async_trait::async_trait]
impl RecoveryOps for DaemonRecovery {
    async fn update_guard(&self, confirmed: bool) -> Result<(), LarmError> {
        let active_leases: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runtime_lease WHERE state IN ('REQUESTED','ADMITTING','ACTIVE')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Consultando leases", "io_failed")
                .with_technical(e.to_string())
        })?;
        let active_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM operation_journal WHERE phase IN ('QUEUED','RUNNING')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Consultando jobs", "io_failed")
                .with_technical(e.to_string())
        })?;
        let to_u64 = |v: i64| u64::try_from(v).unwrap_or(u64::MAX);
        larm_core::backup::restart_decision(to_u64(active_leases), to_u64(active_jobs), confirmed)
    }

    async fn update_larm(
        &self,
        plan_id: &str,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(JobReceipt, larm_domain::ports::BackgroundTask), LarmError> {
        let journal = self.update_journal();
        let (job, started) = journal.enqueue(ctx, None).await.map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo encolar el update",
                "io_failed",
            )
            .with_technical(e.details.reason.clone())
        })?;
        if !started {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya hay un update en curso",
                "update_already_running",
            ));
        }
        let id = job.state.job_id.clone();
        journal.running(&id).await.map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Journal del update", "io_failed")
                .with_technical(e.details.reason.clone())
        })?;
        let receipt = Self::receipt(&job);
        let this = Arc::new(Self {
            pool: self.pool.clone(),
            settings: self.settings.clone(),
            data_root: self.data_root.clone(),
        });
        let (plan_id, plan_hash) = (plan_id.to_string(), plan_hash.to_string());
        let task = Box::pin(async move {
            let result = run_update(&this, &plan_id, &plan_hash).await;
            match result {
                Ok(value) => {
                    let _ = journal.complete(&id, "larm", value, &[]).await;
                }
                Err(error) => {
                    let _ = journal.fail(&id, &error).await;
                }
            }
        });
        Ok((receipt, task))
    }

    async fn recovery_rebuild(
        &self,
        plan_id: &str,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(JobReceipt, larm_domain::ports::BackgroundTask), LarmError> {
        let journal = self.rebuild_journal();
        let (job, started) = journal.enqueue(ctx, None).await.map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo encolar el rebuild",
                "io_failed",
            )
            .with_technical(e.details.reason.clone())
        })?;
        if !started {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya hay un rebuild en curso",
                "rebuild_already_running",
            ));
        }
        let id = job.state.job_id.clone();
        journal.running(&id).await.map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Journal del rebuild", "io_failed")
                .with_technical(e.details.reason.clone())
        })?;
        let receipt = Self::receipt(&job);
        let this = Arc::new(Self {
            pool: self.pool.clone(),
            settings: self.settings.clone(),
            data_root: self.data_root.clone(),
        });
        let (plan_id, plan_hash) = (plan_id.to_string(), plan_hash.to_string());
        let task = Box::pin(async move {
            let result = run_rebuild(&this, &plan_id, &plan_hash).await;
            match result {
                Ok(value) => {
                    let _ = journal.complete(&id, "larm", value, &[]).await;
                }
                Err(error) => {
                    let _ = journal.fail(&id, &error).await;
                }
            }
        });
        Ok((receipt, task))
    }
}

/// Trabajo del updater: staged → backup verificado → bundle determinista.
async fn run_update(
    this: &DaemonRecovery,
    plan_id: &str,
    plan_hash: &str,
) -> Result<serde_json::Value, LarmError> {
    let staged = this.verify_staged()?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let backups = this.data_root.join("backups");
    let db_backup = backups.join(format!("pre-update-{timestamp}.sqlite"));
    larm_db::backup::backup(&this.pool, &db_backup).await?;
    let verification = larm_db::backup::verify_backup(&this.pool, &db_backup).await?;
    if !verification.is_valid() {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "El backup previo al update no verificó",
            "update_backup_failed",
        ));
    }
    // Config y manifiestos del operador junto al snapshot de la base.
    let mut files: Vec<(String, PathBuf)> = vec![("db.sqlite".to_string(), db_backup.clone())];
    if let Ok(entries) = std::fs::read_dir(&this.data_root) {
        let mut locks: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains("lock"))
            })
            .collect();
        locks.sort();
        for lock in locks {
            let name = lock
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            files.push((format!("config/{name}"), lock));
        }
    }
    let bundle_dir = backups.join(format!("bundle-{timestamp}"));
    let manifest = larm_core::backup::write_bundle(
        &bundle_dir,
        &larm_domain::bridge::timestamp(chrono::Utc::now()),
        &files,
    )?;
    tracing::info!(
        plan_id = %plan_id,
        plan_hash = %plan_hash,
        backup = %db_backup.display(),
        "update preparado: el reinicio y la migración los aplica el arranque siguiente"
    );
    Ok(serde_json::json!({
        "backup": db_backup.display().to_string(),
        "bundle": bundle_dir.display().to_string(),
        "bundle_files": manifest.files.keys().cloned().collect::<Vec<_>>(),
        "staged_verified": staged,
        "note": "el reinicio y la migración los aplica el arranque siguiente",
    }))
}

/// Trabajo del rebuild: staging desde las raíces registradas (P018).
async fn run_rebuild(
    this: &DaemonRecovery,
    plan_id: &str,
    plan_hash: &str,
) -> Result<serde_json::Value, LarmError> {
    let roots = this.storage_roots().await?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let out = this
        .data_root
        .join("recovery")
        .join(format!("rebuild-staging-{timestamp}.sqlite"));
    let report = crate::registry_rebuild(&this.settings, &roots, &out).await?;
    tracing::info!(
        plan_id = %plan_id,
        plan_hash = %plan_hash,
        staging = %out.display(),
        "rebuild del registro completado en staging"
    );
    Ok(serde_json::json!({
        "staging": out.display().to_string(),
        "roots": report.get("roots").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

/// Estado para montar las rutas de recuperación con el impl real del daemon.
pub fn state(
    pool: sqlx::SqlitePool,
    settings: larm_core::config::Settings,
    data_root: PathBuf,
) -> Arc<dyn RecoveryOps> {
    Arc::new(DaemonRecovery {
        pool,
        settings,
        data_root,
    })
}
