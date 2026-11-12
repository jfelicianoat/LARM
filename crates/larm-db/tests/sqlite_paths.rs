//! Las rutas locales son literales, incluidas las rutas canónicas de Windows.
#![allow(clippy::unwrap_used)]
use larm_db::{backup, migrate, pool::Db};

#[tokio::test]
async fn ruta_canonica_con_unicode_porcentaje_y_fragmento_reabre_el_mismo_archivo() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("datos español #100% 'literal'.sqlite");
    let db = Db::open(&path).await.unwrap();
    assert!(db.health().await.unwrap().is_healthy());
    sqlx::query("CREATE TABLE fixture_path(value TEXT NOT NULL)")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO fixture_path VALUES('conservado')")
        .execute(&db.pool)
        .await
        .unwrap();
    db.pool.close().await;
    assert!(path.is_file());
    let reopened = Db::open(&path).await.unwrap();
    let value: String = sqlx::query_scalar("SELECT value FROM fixture_path")
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
    assert_eq!(value, "conservado");
    assert!(reopened.health().await.unwrap().is_healthy());
    reopened.pool.close().await;
}

#[tokio::test]
async fn backup_y_restore_conservan_rutas_canonicas_literales() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let live = root.join("origen #100%.sqlite");
    let snapshot = root.join("copia 'íntegra' #100%.sqlite");
    let restored_path = root.join("restaurada #100%.sqlite");
    let db = Db::open(&live).await.unwrap();
    migrate::migrate(&db.pool).await.unwrap();
    sqlx::query("CREATE TABLE fixture_path(value TEXT NOT NULL)")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO fixture_path VALUES('conservado')")
        .execute(&db.pool)
        .await
        .unwrap();
    backup::backup(&db.pool, &snapshot).await.unwrap();
    let verified = backup::verify_backup(&db.pool, &snapshot).await.unwrap();
    assert_eq!(verified.integrity, "ok");
    assert_eq!(verified.live_rows, verified.backup_rows);
    db.pool.close().await;
    backup::restore(&snapshot, &restored_path).await.unwrap();
    let restored = Db::open(&restored_path).await.unwrap();
    let value: String = sqlx::query_scalar("SELECT value FROM fixture_path")
        .fetch_one(&restored.pool)
        .await
        .unwrap();
    assert_eq!(value, "conservado");
    assert!(restored.health().await.unwrap().is_healthy());
    restored.pool.close().await;
}
