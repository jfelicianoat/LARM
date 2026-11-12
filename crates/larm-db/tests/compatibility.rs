#![allow(clippy::unwrap_used)]

use larm_domain::ports::CompatibilityRepository;

#[tokio::test]
async fn compatibility_list_reads_empty_migrated_database() {
    let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repository = larm_db::compatibility::SqlCompatibilityRepository {
        pool: db.pool.clone(),
    };
    assert!(repository
        .list_all_compatibility()
        .await
        .unwrap()
        .is_empty());
}
