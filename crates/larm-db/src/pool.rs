//! Apertura del pool SQLite con los pragmas del contrato (P004).

use larm_domain::error::{ErrorCode, LarmError};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Duration;

/// Pool de base de datos de LARM.
#[derive(Clone)]
pub struct Db {
    pub pool: SqlitePool,
}

impl Db {
    /// Independent, read-only connections to the same database for physical
    /// qualification invoked inside a writing transaction. Ordinary workers
    /// can exhaust their pool while waiting for that caller's writer lock;
    /// borrowing one of those slots here would starve the lock's owner.
    /// Reads still observe current committed profiles, never cached authority.
    #[must_use]
    pub fn qualification_reader(&self) -> SqlitePool {
        let options = self
            .pool
            .connect_options()
            .as_ref()
            .clone()
            .create_if_missing(false)
            .read_only(true)
            .pragma("query_only", "ON");
        SqlitePoolOptions::new()
            .max_connections(2)
            .connect_lazy_with(options)
    }

    /// Abre el pool con WAL, `foreign_keys=ON` y `busy_timeout=5000`
    /// (contrato `persistence.sql`). No ejecuta migraciones.
    ///
    /// # Errors
    /// `INTERNAL` si no se puede crear la carpeta o abrir la base.
    pub async fn open(path: &Path) -> Result<Self, LarmError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| db_error("creando directorio de DB", &e))?;
        }
        // Es una ruta de filesystem, no una URL: el prefijo Windows \\?\ y
        // los caracteres %/# no deben convertirse en opciones ni escapes URI.
        let options = SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_millis(5000))
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .map_err(|e| db_error("abriendo pool SQLite", &e))?;
        Ok(Self { pool })
    }

    /// Versión de la biblioteca SQLite enlazada.
    ///
    /// # Errors
    /// `INTERNAL` si la consulta falla.
    pub async fn sqlite_version(&self) -> Result<String, LarmError> {
        sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("sqlite_version", &e))
    }

    /// Health check: `SELECT 1` + `PRAGMA quick_check` + modo journal.
    ///
    /// # Errors
    /// [`LarmError`] si la base no responde.
    pub async fn health(&self) -> Result<DbHealth, LarmError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("health SELECT 1", &e))?;
        let quick: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("health quick_check", &e))?;
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("health journal_mode", &e))?;
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("health foreign_keys", &e))?;
        let busy: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("health busy_timeout", &e))?;
        Ok(DbHealth {
            quick_check: quick,
            journal_mode: journal,
            foreign_keys_on: fk == 1,
            busy_timeout_ms: busy,
        })
    }
}

/// Resultado del health check de la base.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbHealth {
    pub quick_check: String,
    pub journal_mode: String,
    pub foreign_keys_on: bool,
    pub busy_timeout_ms: i64,
}

impl DbHealth {
    /// Pragmas del contrato (`persistence.sql`): WAL, FK y busy_timeout 5000, base íntegra.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.quick_check == "ok"
            && self.journal_mode.eq_ignore_ascii_case("wal")
            && self.foreign_keys_on
            && self.busy_timeout_ms == 5000
    }
}

/// Error de base de datos con el error SQLite redactado como `original_error`.
///
/// `SQLITE_BUSY`/`SQLITE_LOCKED` (otro proceso retiene la base más allá de
/// `busy_timeout`) es transitorio: `RESOURCE_UNAVAILABLE` reintentable. El
/// resto es `INTERNAL`. Se reconoce por el texto estable de `sqlite3_errstr`.
pub(crate) fn db_error(context: &str, e: &impl std::fmt::Display) -> LarmError {
    let text = e.to_string();
    let busy = text.contains("database is locked") || text.contains("database table is locked");
    let error = if busy {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "La base de datos está ocupada por otro proceso",
            "database_busy",
        )
        .retryable(true)
    } else {
        LarmError::new(
            ErrorCode::Internal,
            "Fallo de base de datos",
            "database_error",
        )
    };
    error
        .with_technical(format!("{context}: {text}"))
        .with_original("sqlite", context, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloqueo_es_reintentable_y_lo_demas_interno() {
        let busy = db_error(
            "x",
            &"error returned from database: (code: 5) database is locked",
        );
        assert_eq!(busy.code, ErrorCode::ResourceUnavailable);
        assert!(busy.retryable);
        let other = db_error("x", &"no such table: foo");
        assert_eq!(other.code, ErrorCode::Internal);
        assert!(!other.retryable);
    }
}
