#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::{
    pool::Db,
    runtime_profiles::{SqlRuntimeProfileLocks, SqlRuntimeProfileQualifications},
};
use larm_domain::{
    error::ErrorCode,
    ports::MutationContext,
    runtime_profiles::{
        RuntimeProfileKey, RuntimeProfileLock, RuntimeProfileLockRepository,
        RuntimeProfileQualification, RuntimeProfileQualificationRepository, LOCK_SCHEMA,
        QUALIFICATION_SCHEMA, REQUIRED_FIELDS,
    },
};
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;

fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn qualification(lock: &RuntimeProfileLock) -> RuntimeProfileQualification {
    let mut result = RuntimeProfileQualification {
        schema_version: QUALIFICATION_SCHEMA.into(),
        lock_sha256: lock.lock_sha256.clone(),
        host_id: "A9_MEGA".into(),
        qualified_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        fixture_hashes: [("positive".into(), "b".repeat(64))].into(),
        strategy_test_ids: [(
            "PREBUILT_BINARY".into(),
            vec!["ollama-version-smoke".into()],
        )]
        .into(),
        evidence_sha256: "c".repeat(64),
        qualification_sha256: String::new(),
    };
    result.qualification_sha256 = result.computed_hash().unwrap();
    result
}

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "p099-fixture".into(),
        correlation_id: uid(),
    }
}

fn lock() -> RuntimeProfileLock {
    let key = RuntimeProfileKey {
        runtime: "llama.cpp".into(),
        source_url: "https://example.org/llama-server/1.0".into(),
        source_revision: "0123456789abcdef0123456789abcdef01234567".into(),
        source_sha256: "a".repeat(64),
        upstream_version: "1.0.0".into(),
        os: "Windows".into(),
        os_build: "10.0.26100".into(),
        backend: "CPU".into(),
        protocol: "openai-compatible-v1".into(),
    };
    let mut fields = Map::new();
    for name in REQUIRED_FIELDS {
        let value = match *name {
            "upstream_version" => json!(key.upstream_version),
            "install_strategy" => json!("PREBUILT_BINARY"),
            "installation_strategies" => json!(["PREBUILT_BINARY"]),
            "fixture_hashes" => json!({"positive": "b".repeat(64)}),
            "evidence_refs" => json!(["fixture://p099/profile"]),
            _ => Value::Null,
        };
        fields.insert((*name).to_owned(), value);
    }
    let mut result = RuntimeProfileLock {
        schema_version: LOCK_SCHEMA.into(),
        key,
        fields: Value::Object(fields),
        qualification: "UNQUALIFIED".into(),
        lock_sha256: String::new(),
    };
    result.lock_sha256 = result.computed_hash().unwrap();
    result
}

async fn counts(pool: &SqlitePool) -> Vec<i64> {
    let mut values = Vec::new();
    for table in [
        "runtime_profile_lock",
        "operation_journal",
        "event",
        "event_outbox",
    ] {
        values.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    values
}

#[tokio::test]
async fn acquire_is_idempotent_and_read_revalidates_the_persisted_lock() {
    let (_dir, db): (_, Db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let profile = lock();

    assert!(repo.acquire(&profile, &ctx()).await.unwrap());
    assert!(!repo.acquire(&profile, &ctx()).await.unwrap());
    assert_eq!(
        repo.get_by_hash(&profile.lock_sha256).await.unwrap(),
        Some(profile.clone())
    );
    assert_eq!(counts(&db.pool).await, [1, 1, 1, 1]);

    let event: (String, String, String) =
        sqlx::query_as("SELECT event_type,actor_id,resource_type FROM event")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        event,
        (
            "runtime.profile_lock.acquired".into(),
            "p099-fixture".into(),
            "runtime_profile_lock".into()
        )
    );
    let journal: (String, String, String) =
        sqlx::query_as("SELECT kind,phase,plan_hash FROM operation_journal")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        journal,
        (
            "runtime.profile_lock".into(),
            "COMPLETED".into(),
            profile.lock_sha256
        )
    );
}

#[tokio::test]
async fn identity_cannot_be_replaced_promoted_updated_or_deleted() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let original = lock();
    repo.acquire(&original, &ctx()).await.unwrap();

    let mut changed = original.clone();
    changed.fields["install_strategy"] = json!("SOURCE_BUILD");
    changed.lock_sha256 = changed.computed_hash().unwrap();
    let error = repo.acquire(&changed, &ctx()).await.unwrap_err();
    assert_eq!(
        error.details.reason,
        "runtime_profile_lock_identity_conflict"
    );

    let mut promoted = original.clone();
    promoted.qualification = "QUALIFIED".into();
    promoted.lock_sha256 = promoted.computed_hash().unwrap();
    assert_eq!(
        promoted.validate().unwrap_err().details.reason,
        "runtime_profile_lock_status_invalid"
    );

    assert!(
        sqlx::query("UPDATE runtime_profile_lock SET qualification='QUALIFIED'")
            .execute(&db.pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("DELETE FROM runtime_profile_lock")
        .execute(&db.pool)
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, [1, 1, 1, 1]);
}

#[tokio::test]
async fn qualification_is_separate_immutable_and_idempotent() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let locks = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let qualifications = SqlRuntimeProfileQualifications {
        pool: db.pool.clone(),
    };
    let profile = lock();
    let evidence = qualification(&profile);
    locks.acquire(&profile, &ctx()).await.unwrap();

    assert!(qualifications
        .record(&profile, &evidence, &ctx())
        .await
        .unwrap());
    assert!(!qualifications
        .record(&profile, &evidence, &ctx())
        .await
        .unwrap());
    assert_eq!(
        qualifications
            .get_by_lock_hash(&profile.lock_sha256)
            .await
            .unwrap(),
        Some(evidence.clone())
    );
    assert_eq!(
        locks.get_by_hash(&profile.lock_sha256).await.unwrap(),
        Some(profile.clone())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runtime_profile_qualification")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        1
    );

    let mut changed = evidence;
    changed
        .strategy_test_ids
        .get_mut("PREBUILT_BINARY")
        .unwrap()
        .push("extra-test".into());
    changed.qualification_sha256 = changed.computed_hash().unwrap();
    assert_eq!(
        qualifications
            .record(&profile, &changed, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_profile_qualification_conflict"
    );
    assert!(
        sqlx::query("UPDATE runtime_profile_qualification SET actor_id='changed'")
            .execute(&db.pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("DELETE FROM runtime_profile_qualification")
        .execute(&db.pool)
        .await
        .is_err());
}

#[tokio::test]
async fn qualification_rejects_fixtures_and_strategies_outside_lock() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let locks = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let qualifications = SqlRuntimeProfileQualifications {
        pool: db.pool.clone(),
    };
    let profile = lock();
    locks.acquire(&profile, &ctx()).await.unwrap();

    let mut bad_fixtures = qualification(&profile);
    bad_fixtures
        .fixture_hashes
        .insert("unknown".into(), "d".repeat(64));
    bad_fixtures.qualification_sha256 = bad_fixtures.computed_hash().unwrap();
    assert_eq!(
        qualifications
            .record(&profile, &bad_fixtures, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_profile_qualification_fixture_mismatch"
    );

    let mut bad_strategy = qualification(&profile);
    bad_strategy
        .strategy_test_ids
        .insert("SOURCE_BUILD".into(), vec!["unannounced".into()]);
    bad_strategy.qualification_sha256 = bad_strategy.computed_hash().unwrap();
    assert_eq!(
        qualifications
            .record(&profile, &bad_strategy, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "runtime_profile_qualification_strategy_mismatch"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runtime_profile_qualification")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn qualification_outbox_failure_rolls_back_evidence_and_journal() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let locks = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let qualifications = SqlRuntimeProfileQualifications {
        pool: db.pool.clone(),
    };
    let profile = lock();
    locks.acquire(&profile, &ctx()).await.unwrap();
    let evidence = qualification(&profile);
    sqlx::query("CREATE TRIGGER qualification_outbox_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture outbox fault'); END")
        .execute(&db.pool)
        .await
        .unwrap();

    assert!(qualifications
        .record(&profile, &evidence, &ctx())
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runtime_profile_qualification")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operation_journal")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query("DROP TRIGGER qualification_outbox_fault")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(qualifications
        .record(&profile, &evidence, &ctx())
        .await
        .unwrap());
}

#[tokio::test]
async fn outbox_failure_rolls_back_lock_and_journal() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    sqlx::query("CREATE TRIGGER runtime_profile_lock_outbox_fault BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture outbox fault'); END")
        .execute(&db.pool)
        .await
        .unwrap();

    assert!(repo.acquire(&lock(), &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, [0, 0, 0, 0]);

    sqlx::query("DROP TRIGGER runtime_profile_lock_outbox_fault")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo.acquire(&lock(), &ctx()).await.unwrap());
    assert_eq!(counts(&db.pool).await, [1, 1, 1, 1]);
}

#[tokio::test]
async fn corrupt_persisted_payload_fails_closed() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let profile = lock();
    repo.acquire(&profile, &ctx()).await.unwrap();
    sqlx::query("DROP TRIGGER runtime_profile_lock_immutable_update")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE runtime_profile_lock SET payload=json_set(payload,'$.qualification','QUALIFIED')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let error = repo.get_by_hash(&profile.lock_sha256).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::IntegrityFailed);
}

#[tokio::test]
async fn invalid_actor_and_non_hex_lookup_are_rejected() {
    let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimeProfileLocks {
        pool: db.pool.clone(),
    };
    let mut invalid_context = ctx();
    invalid_context.correlation_id = "not-a-uuid".into();
    let error = repo.acquire(&lock(), &invalid_context).await.unwrap_err();
    assert_eq!(error.details.reason, "runtime_profile_lock_actor_invalid");
    assert_eq!(
        repo.get_by_hash(&"G".repeat(64)).await.unwrap_err().code,
        ErrorCode::ValidationFailed
    );
    assert_eq!(counts(&db.pool).await, [0, 0, 0, 0]);
}

#[test]
fn catalog_and_fields_are_contract_bound() {
    let profile = lock();
    profile.validate().unwrap();
    let mut missing = profile.clone();
    missing
        .fields
        .as_object_mut()
        .unwrap()
        .remove("request_schema");
    missing.lock_sha256 = missing.computed_hash().unwrap();
    assert_eq!(
        missing.validate().unwrap_err().details.reason,
        "runtime_profile_required_fields_mismatch"
    );

    let mut unknown = profile;
    unknown.key.runtime = "made-up-runtime".into();
    unknown.lock_sha256 = unknown.computed_hash().unwrap();
    assert_eq!(
        unknown.validate().unwrap_err().details.reason,
        "runtime_profile_runtime_unknown"
    );
}
