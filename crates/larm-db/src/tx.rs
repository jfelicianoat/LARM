//! Helper transaccional para operaciones multi-repositorio (P004).
//!
//! Toda escritura que toque más de un repositorio corre aquí: si la
//! operación devuelve `Err`, la transacción se revierte íntegramente.

use larm_domain::error::LarmError;
use sqlx::SqlitePool;
use std::future::Future;
use std::pin::Pin;

/// Futuro de una operación transaccional sobre la conexión prestada.
pub type TxFuture<'c, T> = Pin<Box<dyn Future<Output = Result<T, LarmError>> + Send + 'c>>;

/// Ejecuta `f` dentro de una transacción sobre `pool`; `Err` revierte todo.
///
/// # Errors
/// [`LarmError`] del propio `f` o de BEGIN/COMMIT/ROLLBACK.
pub async fn with_transaction<T, F>(pool: &SqlitePool, f: F) -> Result<T, LarmError>
where
    F: for<'c> FnOnce(&'c mut sqlx::SqliteConnection) -> TxFuture<'c, T> + Send,
{
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| super::pool::db_error("BEGIN", &e))?;
    match f(&mut tx).await {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|e| super::pool::db_error("COMMIT", &e))?;
            Ok(value)
        }
        Err(err) => {
            tx.rollback()
                .await
                .map_err(|e| super::pool::db_error("ROLLBACK", &e))?;
            Err(err)
        }
    }
}
