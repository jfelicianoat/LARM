//! Máquina de estados de promoción P120: transición transaccional con
//! exclusividad de DEFAULT por (revision, format, quantization).
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use larm_db::testing::temp_migrated;
use larm_domain::model::can_transition_promotion;
use larm_domain::ports::{MutationContext, VariantRepository};

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}

#[tokio::test]
async fn promotion_transitions_follow_the_state_machine() {
    // Tabla completa: el ascendente legal, y los movimientos prohibidos.
    for (from, to, allowed) in [
        ("NONE", "CANDIDATE", true),
        ("CANDIDATE", "TECHNICALLY_APPROVED", true),
        ("TECHNICALLY_APPROVED", "EVALUATION_APPROVED", true),
        ("EVALUATION_APPROVED", "DEFAULT", true),
        ("NONE", "TECHNICALLY_APPROVED", false),
        ("CANDIDATE", "DEFAULT", false),
        ("DEFAULT", "EVALUATION_APPROVED", false),
        ("DEFAULT", "REJECTED", false),
        ("NONE", "REJECTED", true),
        ("EVALUATION_APPROVED", "REJECTED", true),
        ("DEFAULT", "PINNED", true),
        ("NONE", "PINNED", true),
    ] {
        assert_eq!(can_transition_promotion(from, to), allowed, "{from}->{to}");
    }
}

async fn seed_lineage(pool: &sqlx::SqlitePool) {
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('ai','t','t',1,'{}','src-adapter','1.0','source','ph','pub','UNTRUSTED','{}','INSTALLED')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('msa','t','t',1,'{}','ai','HTTP','{}')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('ms','t','t',1,'{}','source','msa','https://example.invalid',0,1,'NONE','{}')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm-1','t','t',1,'{}','model','[]','[]')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('md-1','t','t',1,'{}','cm-1','ms','ns','model')")
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('mr-1','t','t',1,'{}','md-1','r1','{}','CURRENT')")
        .execute(pool).await.unwrap();
}

async fn seed_variant(
    pool: &sqlx::SqlitePool,
    id: &str,
    format: &str,
    quantization: Option<&str>,
    promotion: &str,
) {
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,format,quantization,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,'t','t',1,'{}','mr-1',?,?,?,'DOWNLOADED','UNKNOWN','LARM_MANAGED',?,'GENERAL','[]','{}',0)")
        .bind(id)
        .bind(format)
        .bind(quantization)
        .bind("DOWNLOADED")
        .bind(promotion)
        .execute(pool)
        .await
        .unwrap();
}

async fn promotion_of(pool: &sqlx::SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT promotion FROM model_variant WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn promotion_ascends_one_step_and_rejects_skips() {
    let (_dir, db) = temp_migrated().await.unwrap();
    seed_lineage(&db.pool).await;
    let store = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    seed_variant(
        &db.pool,
        "0192aaaa-0000-7000-8000-0000000000a1",
        "GGUF",
        None,
        "NONE",
    )
    .await;
    let ctx = ctx();

    let version = store
        .set_promotion("0192aaaa-0000-7000-8000-0000000000a1", 1, "CANDIDATE", &ctx)
        .await
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(
        promotion_of(&db.pool, "0192aaaa-0000-7000-8000-0000000000a1").await,
        "CANDIDATE"
    );

    // Saltar dos escalones falla cerrado y no cambia nada.
    let error = store
        .set_promotion(
            "0192aaaa-0000-7000-8000-0000000000a1",
            version,
            "DEFAULT",
            &ctx,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "INVALID_STATE_TRANSITION");
    assert_eq!(
        promotion_of(&db.pool, "0192aaaa-0000-7000-8000-0000000000a1").await,
        "CANDIDATE"
    );
}

#[tokio::test]
async fn default_is_exclusive_per_revision_format_quantization() {
    let (_dir, db) = temp_migrated().await.unwrap();
    seed_lineage(&db.pool).await;
    let store = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let ctx = ctx();
    seed_variant(
        &db.pool,
        "0192aaaa-0000-7000-8000-0000000000b1",
        "GGUF",
        Some("Q8_0"),
        "NONE",
    )
    .await;
    seed_variant(
        &db.pool,
        "0192aaaa-0000-7000-8000-0000000000b2",
        "GGUF",
        Some("Q8_0"),
        "NONE",
    )
    .await;
    seed_variant(
        &db.pool,
        "0192aaaa-0000-7000-8000-0000000000b3",
        "ONNX",
        None,
        "NONE",
    )
    .await;

    // Ascender la primera hasta DEFAULT en tres pasos.
    let a = "0192aaaa-0000-7000-8000-0000000000b1";
    for (expected, to) in [
        (1, "CANDIDATE"),
        (2, "TECHNICALLY_APPROVED"),
        (3, "EVALUATION_APPROVED"),
        (4, "DEFAULT"),
    ] {
        store.set_promotion(a, expected, to, &ctx).await.unwrap();
    }
    assert_eq!(promotion_of(&db.pool, a).await, "DEFAULT");

    // Ascender la segunda (misma revisión+formato+cuantización) a DEFAULT
    // rebaja la primera en la misma transacción.
    let b = "0192aaaa-0000-7000-8000-0000000000b2";
    for (expected, to) in [
        (1, "CANDIDATE"),
        (2, "TECHNICALLY_APPROVED"),
        (3, "EVALUATION_APPROVED"),
        (4, "DEFAULT"),
    ] {
        store.set_promotion(b, expected, to, &ctx).await.unwrap();
    }
    assert_eq!(promotion_of(&db.pool, b).await, "DEFAULT");
    assert_eq!(promotion_of(&db.pool, a).await, "EVALUATION_APPROVED");

    // La variante de otra combinación (formato distinto) no se toca.
    assert_eq!(
        promotion_of(&db.pool, "0192aaaa-0000-7000-8000-0000000000b3").await,
        "NONE"
    );
}

#[tokio::test]
async fn stale_row_version_fails_closed() {
    let (_dir, db) = temp_migrated().await.unwrap();
    seed_lineage(&db.pool).await;
    let store = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    seed_variant(
        &db.pool,
        "0192aaaa-0000-7000-8000-0000000000c1",
        "GGUF",
        None,
        "NONE",
    )
    .await;
    let error = store
        .set_promotion(
            "0192aaaa-0000-7000-8000-0000000000c1",
            9,
            "CANDIDATE",
            &ctx(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "STALE_REVISION");
}
