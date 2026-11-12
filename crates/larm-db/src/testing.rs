//! Base temporal migrada para pruebas de cualquier crate (P004, checklist 5).

use crate::pool::Db;
use larm_domain::error::LarmError;

/// Base SQLite nueva en una carpeta temporal, con pragmas del contrato y
/// migraciones aplicadas. La carpeta se borra al soltar el `TempDir`.
///
/// # Errors
/// `INTERNAL` si no se puede crear la carpeta, abrir o migrar.
pub async fn temp_migrated() -> Result<(tempfile::TempDir, Db), LarmError> {
    let dir = tempfile::tempdir().map_err(|e| crate::pool::db_error("tempdir", &e))?;
    let db = Db::open(&dir.path().join("larm.sqlite")).await?;
    crate::migrate::migrate(&db.pool).await?;
    Ok((dir, db))
}
