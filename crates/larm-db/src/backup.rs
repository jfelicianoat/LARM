//! Backup atómico y restauración verificada (P004, ADR-0006, spec §132).
//!
//! `VACUUM INTO` produce una copia consistente aunque la base esté en WAL. Se
//! escribe en un temporal de la misma carpeta y se renombra al destino: un
//! fallo nunca deja un backup a medias ni destruye el anterior. La
//! verificación compara integridad y número de filas tabla a tabla, y la
//! restauración conserva la base previa hasta comprobar la restaurada.

use larm_domain::error::{ErrorCode, LarmError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Row, SqlitePool};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::pool::db_error;

/// Snapshot atómico de `pool` hacia `dest`. Sustituye un backup previo solo si el nuevo se completó.
///
/// # Errors
/// `INTERNAL` en fallo de IO o SQL; el destino previo queda intacto.
pub async fn backup(pool: &SqlitePool, dest: &Path) -> Result<(), LarmError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| db_error("creando directorio de backup", &e))?;
    }
    let partial = with_suffix(dest, "partial");
    if partial.exists() {
        std::fs::remove_file(&partial)
            .map_err(|e| db_error("eliminando un backup parcial anterior", &e))?;
    }
    // VACUUM INTO no admite parámetros; la ruta la genera LARM y se escapa.
    let target = partial
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "''");
    let result = sqlx::query(AssertSqlSafe(format!("VACUUM INTO '{target}'")))
        .execute(pool)
        .await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(db_error("VACUUM INTO", &e));
    }
    std::fs::rename(&partial, dest).map_err(|e| db_error("publicando el backup", &e))?;
    Ok(())
}

/// Resultado de comparar un backup con su origen.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupVerification {
    pub integrity: String,
    pub live_rows: BTreeMap<String, i64>,
    pub backup_rows: BTreeMap<String, i64>,
}

impl BackupVerification {
    /// Íntegro y con las mismas tablas y filas.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.integrity == "ok" && self.live_rows == self.backup_rows
    }
}

/// Verifica `backup_path` contra la base viva: `integrity_check` y filas por tabla.
///
/// # Errors
/// `INTERNAL` si el fichero no es una base SQLite legible.
pub async fn verify_backup(
    live: &SqlitePool,
    backup_path: &Path,
) -> Result<BackupVerification, LarmError> {
    let snapshot = open_read_only(backup_path).await?;
    let integrity = integrity(&snapshot).await;
    let backup_rows = row_counts(&snapshot).await;
    snapshot.close().await;
    Ok(BackupVerification {
        integrity: integrity?,
        live_rows: row_counts(live).await?,
        backup_rows: backup_rows?,
    })
}

/// Informe de una restauración.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreReport {
    pub integrity: String,
    pub rows: BTreeMap<String, i64>,
    /// Base que había antes de restaurar (se conserva hasta que el operador la borre).
    pub previous: Option<PathBuf>,
}

/// Restaura `backup_path` sobre `live_path`. La base viva debe estar cerrada.
///
/// Comprueba el backup antes de tocar nada; aparta la base actual (y sus
/// `-wal`/`-shm`) como `*.pre-restore`, coloca la copia y verifica que la
/// restaurada tenga las mismas filas que el backup. Si algo falla, devuelve
/// la base previa a su sitio.
///
/// # Errors
/// `INTEGRITY_FAILED` si el backup no es íntegro o la restaurada no coincide; `INTERNAL` en IO.
pub async fn restore(backup_path: &Path, live_path: &Path) -> Result<RestoreReport, LarmError> {
    let snapshot = open_read_only(backup_path).await?;
    let expected_integrity = integrity(&snapshot).await;
    let expected_rows = row_counts(&snapshot).await;
    snapshot.close().await;
    let expected_integrity = expected_integrity?;
    let expected_rows = expected_rows?;
    if expected_integrity != "ok" || !expected_rows.contains_key("schema_metadata") {
        return Err(LarmError::new(
            ErrorCode::IntegrityFailed,
            "El backup no es una base de LARM íntegra",
            "backup_not_restorable",
        )
        .with_technical(format!("integrity_check={expected_integrity}")));
    }

    // Una instalación nueva (la base se perdió) todavía no tiene carpeta.
    if let Some(parent) = live_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| db_error("creando la carpeta de la base", &e))?;
    }
    let staged = with_suffix(live_path, "restoring");
    std::fs::copy(backup_path, &staged).map_err(|e| db_error("copiando el backup", &e))?;

    // Apartar la base viva con sus -wal/-shm. Si un fichero está abierto por
    // otro proceso (Windows no deja renombrarlo) se deshace lo movido y la
    // base queda exactamente como estaba.
    let aside = with_suffix(live_path, "pre-restore");
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        let from = companion(live_path, ext);
        if !from.exists() {
            continue;
        }
        let to = companion(&aside, ext);
        if let Err(e) = std::fs::rename(&from, &to) {
            for (orig, now) in moved.iter().rev() {
                let _ = std::fs::rename(now, orig);
            }
            let _ = std::fs::remove_file(&staged);
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La base está en uso; detén el daemon antes de restaurar",
                "database_in_use",
            )
            .retryable(true)
            .with_technical(format!("renombrando {}: {e}", from.display()))
            .with_original("io", &format!("{:?}", e.kind()), &e.to_string()));
        }
        moved.push((from, to));
    }
    let previous = (!moved.is_empty()).then(|| aside.clone());

    let outcome = async {
        std::fs::rename(&staged, live_path)
            .map_err(|e| db_error("colocando la base restaurada", &e))?;
        let restored = open_read_only(live_path).await?;
        let got_integrity = integrity(&restored).await;
        let got_rows = row_counts(&restored).await;
        restored.close().await;
        let (got_integrity, got_rows) = (got_integrity?, got_rows?);
        if got_integrity != "ok" || got_rows != expected_rows {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "La base restaurada no coincide con el backup",
                "restore_mismatch",
            ));
        }
        Ok(RestoreReport {
            integrity: got_integrity,
            rows: got_rows,
            previous: previous.clone(),
        })
    }
    .await;

    if outcome.is_err() {
        let _ = std::fs::remove_file(&staged);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(companion(live_path, ext));
        }
        for (orig, now) in moved.iter().rev() {
            let _ = std::fs::rename(now, orig);
        }
    }
    outcome
}

/// `base` con el sufijo de un fichero compañero de SQLite (`-wal`, `-shm`).
fn companion(base: &Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}{ext}", base.display()))
}

/// Filas por tabla de usuario (incluida `schema_metadata`).
///
/// # Errors
/// `INTERNAL` si falla una consulta.
pub async fn row_counts(pool: &SqlitePool) -> Result<BTreeMap<String, i64>, LarmError> {
    let names: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| db_error("listando tablas", &e))?
    .iter()
    .filter_map(|r| r.try_get::<String, _>(0).ok())
    .collect();
    let mut counts = BTreeMap::new();
    for name in names {
        // Nombres del propio esquema; se rechaza cualquier otro carácter antes de citarlos.
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(db_error("nombre de tabla inesperado", &name));
        }
        let count: i64 =
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{name}\"")))
                .fetch_one(pool)
                .await
                .map_err(|e| db_error("contando filas", &e))?;
        counts.insert(name, count);
    }
    Ok(counts)
}

async fn integrity(pool: &SqlitePool) -> Result<String, LarmError> {
    sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .map_err(|e| db_error("integrity_check", &e))
}

async fn open_read_only(path: &Path) -> Result<SqlitePool, LarmError> {
    if !path.is_file() {
        return Err(LarmError::new(
            ErrorCode::NotFound,
            "No existe el fichero de base de datos",
            "database_file_missing",
        ));
    }
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| db_error("abriendo en solo lectura", &e))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", path.display()))
}
