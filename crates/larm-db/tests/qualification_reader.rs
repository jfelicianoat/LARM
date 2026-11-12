//! Qualification must be able to resolve committed profiles while its caller
//! holds SQLite's writer and every ordinary pool slot has a waiting writer.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use larm_db::pool::Db;
use std::time::Duration;

async fn database() -> (tempfile::TempDir, Db) {
    let root = tempfile::tempdir().unwrap();
    let db = Db::open(&root.path().join("qualification.sqlite"))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE qualification_fact(value INTEGER NOT NULL)")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO qualification_fact VALUES(7)")
        .execute(&db.pool)
        .await
        .unwrap();
    (root, db)
}

#[tokio::test]
async fn qualification_reads_committed_state_with_all_ordinary_slots_waiting_for_the_writer() {
    let (_root, db) = database().await;
    let reader = db.qualification_reader();
    let mut writer = db.pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *writer)
        .await
        .unwrap();
    sqlx::query("UPDATE qualification_fact SET value=8")
        .execute(&mut *writer)
        .await
        .unwrap();
    let (ready, mut acquired) = tokio::sync::mpsc::channel(7);
    let mut pending = Vec::new();
    for _ in 0..7 {
        let pool = db.pool.clone();
        let ready = ready.clone();
        pending.push(tokio::spawn(async move {
            let mut connection = pool.acquire().await.unwrap();
            ready.send(()).await.unwrap();
            sqlx::query("UPDATE qualification_fact SET value=value WHERE 0")
                .execute(&mut *connection)
                .await
                .unwrap();
        }));
    }
    drop(ready);
    for _ in 0..7 {
        tokio::time::timeout(Duration::from_secs(2), acquired.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(db.pool.num_idle(), 0);
    let read = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_scalar::<_, i64>("SELECT value FROM qualification_fact").fetch_one(&reader),
    )
    .await;
    // Always release owned locks and join waiters, including the RED case.
    sqlx::query("ROLLBACK").execute(&mut *writer).await.unwrap();
    drop(writer);
    for waiter in pending {
        waiter.await.unwrap();
    }
    assert_eq!(
        read.expect("qualification reader starved behind its caller's writer")
            .unwrap(),
        7
    );
    reader.close().await;
    db.pool.close().await;
}

#[tokio::test]
async fn qualification_reader_rejects_writes() {
    let (_root, db) = database().await;
    let reader = db.qualification_reader();
    let result = sqlx::query("UPDATE qualification_fact SET value=8")
        .execute(&reader)
        .await;
    assert!(result.is_err(), "qualification pool must be read-only");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT value FROM qualification_fact")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        7
    );
    reader.close().await;
    db.pool.close().await;
}

#[tokio::test]
async fn qualification_reader_observes_new_commits_without_caching_authority() {
    let (_root, db) = database().await;
    let reader = db.qualification_reader();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT value FROM qualification_fact")
            .fetch_one(&reader)
            .await
            .unwrap(),
        7
    );
    sqlx::query("UPDATE qualification_fact SET value=9")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT value FROM qualification_fact")
            .fetch_one(&reader)
            .await
            .unwrap(),
        9
    );
    reader.close().await;
    db.pool.close().await;
}
