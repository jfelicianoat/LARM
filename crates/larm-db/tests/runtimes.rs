//! SQLite real con fixtures explícitas: el catálogo no cualifica ejecución.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::{pool::Db, runtimes::SqlRuntimes};
use larm_domain::{
    bridge::timestamp, error::ErrorCode, ports::MutationContext, runtime::*, runtime_ports::*,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;

const AT: &str = "2026-10-20T13:00:00.000Z";
fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "p030-fixture".into(),
        correlation_id: uid(),
    }
}
fn entity(fields: Value) -> Value {
    let mut base = json!({"id":uid(),"created_at":AT,"updated_at":AT,"row_version":1,"metadata":{"fixture_only":true}});
    base.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    base
}
fn digest<T: serde::Serialize>(value: &T) -> String {
    larm_domain::canonical::sha256_hex(&serde_json::to_value(value).unwrap()).unwrap()
}
async fn setup() -> (tempfile::TempDir, Db, SqlRuntimes, RuntimeCreate) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let repo = SqlRuntimes {
        pool: db.pool.clone(),
    };
    let installed = uid();
    sqlx::query("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{\"fixture_only\":true}','fixture.runtime','1.0.0','runtime',?,'fixture','UNTRUSTED',?,'INSTALLED')")
        .bind(&installed).bind(AT).bind(AT).bind("a".repeat(64)).bind(json!({"id":"fixture.runtime","version":"1.0.0","family":"runtime","capabilities":["vendor.future"]}).to_string()).execute(&db.pool).await.unwrap();
    let binding = repo
        .register_runtime_adapter(&installed, "fixture.engine", &ctx())
        .await
        .unwrap();
    let request = RuntimeCreate {
        name: "Runtime sin SemVer obligatorio".into(),
        adapter_id: binding.id,
        capabilities: vec!["vendor.future".into()],
        default_version_id: None,
        channel_policy: RuntimeChannelPolicy::default(),
        retention_policy: Default::default(),
    };
    (dir, db, repo, request)
}
async fn counts(pool: &SqlitePool) -> Vec<i64> {
    let mut result = Vec::new();
    for name in [
        "runtime",
        "runtime_adapter",
        "runtime_version",
        "runtime_installation",
        "runtime_backend",
        "external_host",
        "operation_journal",
        "event",
        "event_outbox",
        "idempotency",
        "volume_reservation",
        "memory_reservation",
    ] {
        result.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {name}")))
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    result
}
async fn create(repo: &SqlRuntimes, request: &RuntimeCreate) -> Runtime {
    repo.create_runtime(request, &uid(), &digest(request), &ctx())
        .await
        .unwrap()
}
fn version(runtime: &str, channel: RuntimeChannel) -> RuntimeVersion {
    serde_json::from_value(entity(json!({"runtime_id":runtime,"version_string":"b7142-local",
        "commit":null,"build":null,"channel":channel,"released_at":null,"artifact_hash":null,"promotion":"NONE"}))).unwrap()
}

#[tokio::test]
async fn roundtrip_cinco_canales_provider_abierto_host_separado_y_reapertura() {
    let (dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let mut versions = Vec::new();
    for channel in [
        RuntimeChannel::Stable,
        RuntimeChannel::Prerelease,
        RuntimeChannel::Nightly,
        RuntimeChannel::Commit,
        RuntimeChannel::Custom,
    ] {
        let mut v = version(&runtime.id, channel);
        if channel == RuntimeChannel::Custom {
            v.metadata.insert(
                "upstream_channel".into(),
                ObservedChannel::from_upstream("experimental.vendor/42").metadata(),
            );
        }
        repo.record_version(&v, &ctx()).await.unwrap();
        versions.push(v);
    }
    let host:ExternalHost=serde_json::from_value(entity(json!({"name":"fixture host","base_url":"http://127.0.0.1:23239","host_type":"vendor.unlisted.host","credential_reference_id":null,"state":"UNKNOWN","ownership":"EXTERNAL","observed_version":null,"registration_mode":"ADOPTED"}))).unwrap();
    repo.record_external_host(&host, &ctx()).await.unwrap();
    let installed:RuntimeInstallation=serde_json::from_value(entity(json!({"runtime_version_id":versions[0].id,"storage_location_id":null,"relative_path":null,"strategy":"EXTERNAL_ADOPTION","external_host_id":host.id,"execution_host_id":null,"state":"DISCOVERED","ownership":"EXTERNAL","lock_hash":null,"stage":"DISCOVERED","registration_mode":"ADOPTED"}))).unwrap();
    repo.record_installation(&installed, &ctx()).await.unwrap();
    let backend:RuntimeBackend=serde_json::from_value(entity(json!({"runtime_installation_id":installed.id,"backend_type":"vendor.future.provider/v42","device_id":null,"driver_version":null,"status":"UNKNOWN","evidence_refs":[],"limits":{"max_instances":1,"multi_model_support":false,"parallel_requests":1,"gpu_sharing":false}}))).unwrap();
    repo.record_backend(&backend, &ctx()).await.unwrap();
    db.pool.close().await;
    let reopened = Db::open(&dir.path().join("larm.sqlite")).await.unwrap();
    assert!(larm_db::migrate::migrate(&reopened.pool)
        .await
        .unwrap()
        .is_empty());
    let repo = SqlRuntimes {
        pool: reopened.pool.clone(),
    };
    assert_eq!(
        repo.runtime(&runtime.id).await.unwrap(),
        Some(runtime.clone())
    );
    for v in &versions {
        assert_eq!(repo.version(&v.id).await.unwrap(), Some(v.clone()));
    }
    assert_eq!(repo.external_host(&host.id).await.unwrap(), Some(host));
    assert_eq!(
        repo.installation(&installed.id).await.unwrap(),
        Some(installed.clone())
    );
    assert_eq!(
        repo.backend(&backend.id).await.unwrap(),
        Some(backend.clone())
    );
    let filter = RuntimeFilter {
        runtime_id: Some(runtime.id.clone()),
        ..Default::default()
    };
    assert_eq!(
        repo.versions(None, 201, &filter).await.unwrap().len(),
        5,
        "la política stable no oculta inventario"
    );
    assert_eq!(
        repo.installations(None, 201, &filter).await.unwrap(),
        vec![installed]
    );
    assert_eq!(
        repo.backends(None, 201, &filter).await.unwrap(),
        vec![backend]
    );
    assert!(reopened.health().await.unwrap().is_healthy());
}

#[tokio::test]
async fn replay_atomico_aislado_por_actor_cuerpo_y_caducidad_treinta_dias() {
    let (_dir, db, repo, request) = setup().await;
    let key = uid();
    let actor = ctx();
    let first = repo
        .create_runtime(&request, &key, &digest(&request), &actor)
        .await
        .unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.create_runtime(&request, &key, &digest(&request), &actor)
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&db.pool).await, before);
    let mut changed = request.clone();
    changed.name = "otro cuerpo".into();
    assert_eq!(
        repo.create_runtime(&changed, &key, &digest(&request), &actor)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(counts(&db.pool).await, before);
    let mut other = actor.clone();
    other.actor_id = "otro-actor".into();
    assert_ne!(
        repo.create_runtime(&request, &key, &digest(&request), &other)
            .await
            .unwrap()
            .id,
        first.id
    );
    let expiry: String =
        sqlx::query_scalar("SELECT expires_at FROM idempotency WHERE principal_id=? AND key=?")
            .bind(&actor.actor_id)
            .bind(&key)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let expiry = chrono::DateTime::parse_from_rfc3339(&expiry).unwrap();
    assert!((expiry.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days() >= 29);
    sqlx::query("UPDATE idempotency SET expires_at=? WHERE principal_id=? AND key=?")
        .bind(timestamp(chrono::Utc::now() - chrono::Duration::seconds(1)))
        .bind(&actor.actor_id)
        .bind(&key)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_ne!(
        repo.create_runtime(&request, &key, &digest(&request), &actor)
            .await
            .unwrap()
            .id,
        first.id
    );
}

#[tokio::test]
async fn politica_cas_concurrente_no_modifica_inventario_pins_default_ni_ownership() {
    let (_dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let mut pinned = version(&runtime.id, RuntimeChannel::Commit);
    repo.record_version(&pinned, &ctx()).await.unwrap();
    // Pin/default son estado previo explícito de fixture, no cualificación P117.
    sqlx::query("UPDATE runtime_version SET promotion='PINNED' WHERE id=?")
        .bind(&pinned.id)
        .execute(&db.pool)
        .await
        .unwrap();
    pinned.promotion = RuntimePromotion::Pinned;
    sqlx::query("UPDATE runtime SET default_version_id=? WHERE id=?")
        .bind(&pinned.id)
        .bind(&runtime.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let policy = RuntimeChannelPolicy {
        allowed_channels: vec![RuntimeChannel::Prerelease, RuntimeChannel::Commit],
    };
    let other = RuntimeChannelPolicy {
        allowed_channels: vec![RuntimeChannel::Nightly],
    };
    let before = counts(&db.pool).await;
    let first_actor = ctx();
    let second_actor = ctx();
    let (one, two) = tokio::join!(
        repo.change_channel_policy(&runtime.id, 1, &policy, &first_actor),
        repo.change_channel_policy(&runtime.id, 1, &other, &second_actor)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert_eq!(
        one.as_ref().err().or(two.as_ref().err()).unwrap().code,
        ErrorCode::StaleRevision
    );
    let value = repo.runtime(&runtime.id).await.unwrap().unwrap();
    assert_eq!(value.row_version, 2);
    assert_eq!(value.default_version_id, Some(pinned.id.clone()));
    assert_eq!(repo.version(&pinned.id).await.unwrap(), Some(pinned));
    let after = counts(&db.pool).await;
    for i in 0..after.len() {
        assert_eq!(after[i], before[i] + i64::from([6, 7, 8].contains(&i)));
    }
    let before = counts(&db.pool).await;
    assert!(repo
        .change_channel_policy(
            &runtime.id,
            2,
            &RuntimeChannelPolicy {
                allowed_channels: vec![]
            },
            &ctx()
        )
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn binding_actual_capacidades_y_default_ajeno_se_rechazan_sin_efectos() {
    let (_dir, db, repo, request) = setup().await;
    let bound = repo
        .runtime_adapter(&request.adapter_id)
        .await
        .unwrap()
        .unwrap();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.register_runtime_adapter(&bound.adapter_installation_id, "fixture.engine", &ctx())
            .await
            .unwrap(),
        bound
    );
    assert_eq!(counts(&db.pool).await, before);
    for bad in [
        RuntimeCreate {
            adapter_id: uid(),
            ..request.clone()
        },
        RuntimeCreate {
            capabilities: vec!["not.announced".into()],
            ..request.clone()
        },
        RuntimeCreate {
            default_version_id: Some(uid()),
            ..request.clone()
        },
    ] {
        assert!(repo
            .create_runtime(&bad, &uid(), &digest(&bad), &ctx())
            .await
            .is_err());
        assert_eq!(counts(&db.pool).await, before);
    }
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&bound.adapter_installation_id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo
        .create_runtime(&request, &uid(), &digest(&request), &ctx())
        .await
        .is_err());
    assert_eq!(counts(&db.pool).await, before);
}

#[tokio::test]
async fn fallos_abort_e_ignore_revierten_filas_journal_eventos_outbox_y_replay() {
    let (_dir, db, repo, request) = setup().await;
    for target in [
        "runtime",
        "operation_journal",
        "event_sequence_alloc",
        "event",
        "event_outbox",
        "idempotency",
    ] {
        for behavior in ["ABORT, 'injected'", "IGNORE"] {
            let before = counts(&db.pool).await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER inject_runtime_fault BEFORE INSERT ON {target} BEGIN SELECT RAISE({behavior}); END;"))).execute(&db.pool).await.unwrap();
            let key = uid();
            assert!(
                repo.create_runtime(&request, &key, &digest(&request), &ctx())
                    .await
                    .is_err(),
                "{target}/{behavior}"
            );
            assert_eq!(counts(&db.pool).await, before, "{target}/{behavior}");
            sqlx::raw_sql("DROP TRIGGER inject_runtime_fault")
                .execute(&db.pool)
                .await
                .unwrap();
            repo.create_runtime(&request, &key, &digest(&request), &ctx())
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn fallo_tardio_de_politica_conserva_revision_y_permite_reintento_unico() {
    let (_dir, db, repo, request) = setup().await;
    let original = create(&repo, &request).await;
    let policy = RuntimeChannelPolicy {
        allowed_channels: vec![RuntimeChannel::Nightly, RuntimeChannel::Commit],
    };
    for target in ["operation_journal", "event", "event_outbox"] {
        let before = counts(&db.pool).await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER inject_runtime_fault BEFORE INSERT ON {target} BEGIN SELECT RAISE(IGNORE); END;"))).execute(&db.pool).await.unwrap();
        assert!(repo
            .change_channel_policy(&original.id, 1, &policy, &ctx())
            .await
            .is_err());
        assert_eq!(
            repo.runtime(&original.id).await.unwrap(),
            Some(original.clone())
        );
        assert_eq!(counts(&db.pool).await, before);
        sqlx::raw_sql("DROP TRIGGER inject_runtime_fault")
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let updated = repo
        .change_channel_policy(&original.id, 1, &policy, &ctx())
        .await
        .unwrap();
    assert_eq!(updated.row_version, 2);
    assert_eq!(updated.channel_policy, policy);
}

#[tokio::test]
async fn fk_identidad_y_corrupcion_no_se_convierten_en_inventario_valido() {
    let (_dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let value = version(&runtime.id, RuntimeChannel::Nightly);
    repo.record_version(&value, &ctx()).await.unwrap();
    let mut duplicate = value.clone();
    duplicate.id = uid();
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.record_version(&duplicate, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(counts(&db.pool).await, before);
    let mut orphan = value.clone();
    orphan.id = uid();
    orphan.runtime_id = uid();
    assert!(repo.record_version(&orphan, &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    let mut promotion = value;
    promotion.id = uid();
    promotion.promotion = RuntimePromotion::Default;
    assert!(repo.record_version(&promotion, &ctx()).await.is_err());
    assert_eq!(counts(&db.pool).await, before);
    sqlx::query("UPDATE runtime SET channel_policy='{\"allowed_channels\":[]}' WHERE id=?")
        .bind(&runtime.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.runtime(&runtime.id).await.unwrap_err().code,
        ErrorCode::IntegrityFailed
    );
}

#[tokio::test]
async fn paginas_ordenadas_filtros_y_cursor_no_ocultan_canales_instalados() {
    let (_dir, _db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    for name in ["stable", "prerelease", "nightly", "commit", "custom"] {
        repo.record_version(
            &version(&runtime.id, serde_json::from_value(json!(name)).unwrap()),
            &ctx(),
        )
        .await
        .unwrap();
    }
    let filter = RuntimeFilter {
        runtime_id: Some(runtime.id.clone()),
        ..Default::default()
    };
    let first = repo.versions(None, 2, &filter).await.unwrap();
    assert_eq!(first.len(), 2);
    let last = first.last().unwrap();
    let second = repo
        .versions(
            Some((last.created_at.clone(), last.id.clone())),
            201,
            &filter,
        )
        .await
        .unwrap();
    assert_eq!(second.len(), 3);
    assert!(first.iter().all(|a| second.iter().all(|b| a.id != b.id)));
    let by_cap = RuntimeFilter {
        capability: Some("vendor.future".into()),
        ..Default::default()
    };
    assert_eq!(
        repo.runtimes(None, 201, &by_cap).await.unwrap(),
        vec![runtime]
    );
    assert!(repo.versions(None, 0, &filter).await.is_err());
    assert!(repo.versions(None, 201, &by_cap).await.is_err());
}

#[tokio::test]
async fn indice_publico_es_reparable_atomico_y_no_otorga_identidad_al_binding() {
    let (_dir, db, repo, request) = setup().await;
    let bound = repo
        .runtime_adapter(&request.adapter_id)
        .await
        .unwrap()
        .unwrap();
    let metadata: String =
        sqlx::query_scalar("SELECT metadata FROM adapter_installation WHERE id=?")
            .bind(&bound.adapter_installation_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let expected = json!([{"id":bound.id,"runtime_family":bound.runtime_family,"capabilities":bound.capabilities}]);
    assert_eq!(
        serde_json::from_str::<Value>(&metadata).unwrap()["runtime_bindings"],
        expected
    );
    for behavior in ["ABORT, 'injected'", "IGNORE"] {
        let before = counts(&db.pool).await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER inject_runtime_fault BEFORE UPDATE ON adapter_installation BEGIN SELECT RAISE({behavior}); END;"))).execute(&db.pool).await.unwrap();
        assert!(repo
            .register_runtime_adapter(&bound.adapter_installation_id, "other.engine", &ctx())
            .await
            .is_err());
        assert_eq!(counts(&db.pool).await, before);
        sqlx::raw_sql("DROP TRIGGER inject_runtime_fault")
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let invented = uid();
    sqlx::query("UPDATE adapter_installation SET metadata=?,row_version=row_version+1 WHERE id=?").bind(json!({"runtime_bindings":[{"id":invented,"runtime_family":"fake","capabilities":["vendor.future"]}]}).to_string()).bind(&bound.adapter_installation_id).execute(&db.pool).await.unwrap();
    let fake = RuntimeCreate {
        adapter_id: invented,
        ..request.clone()
    };
    assert!(repo
        .create_runtime(&fake, &uid(), &digest(&fake), &ctx())
        .await
        .is_err());
    let before = counts(&db.pool).await;
    assert_eq!(
        repo.register_runtime_adapter(&bound.adapter_installation_id, "fixture.engine", &ctx())
            .await
            .unwrap(),
        bound
    );
    let after = counts(&db.pool).await;
    assert_eq!(after[1], before[1]);
    assert_eq!(after[6], before[6]);
    assert_eq!(after[7], before[7] + 1);
    assert_eq!(after[8], before[8] + 1);
    assert_eq!(
        repo.register_runtime_adapter(&bound.adapter_installation_id, "fixture.engine", &ctx())
            .await
            .unwrap(),
        bound
    );
    assert_eq!(counts(&db.pool).await, after);
}

async fn validated_installation(repo: &SqlRuntimes, version_id: &str) -> RuntimeInstallation {
    let installation: RuntimeInstallation = serde_json::from_value(entity(json!({
        "runtime_version_id": version_id,
        "storage_location_id": null,
        "relative_path": null,
        "strategy": "CUSTOM",
        "external_host_id": null,
        "execution_host_id": null,
        "state": "VALIDATED",
        "ownership": "LARM_MANAGED",
        "lock_hash": null,
        "stage": "VALIDATED",
        "registration_mode": "MANAGED"
    })))
    .unwrap();
    repo.record_installation(&installation, &ctx())
        .await
        .unwrap();
    installation
}

async fn version_record(
    repo: &SqlRuntimes,
    runtime_id: &str,
    version_string: &str,
) -> RuntimeVersion {
    let mut value = version(runtime_id, RuntimeChannel::Stable);
    value.id = uid();
    value.version_string = version_string.into();
    repo.record_version(&value, &ctx()).await.unwrap();
    value
}

#[tokio::test]
async fn retencion_es_cas_idempotente_y_auditable() {
    let (_dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let policy = RuntimeRetentionPolicy {
        keep_last_validated: 5,
    };
    let body_hash = digest(&json!({
        "runtime_id": runtime.id,
        "if_match": 1,
        "policy": policy,
    }));
    let key = uid();
    let context = ctx();
    let updated = repo
        .change_retention_policy(&runtime.id, 1, &policy, &key, &body_hash, &context)
        .await
        .unwrap();
    assert_eq!(updated.row_version, 2);
    assert_eq!(updated.retention_policy, policy);
    assert_eq!(
        repo.change_retention_policy(&runtime.id, 1, &policy, &key, &body_hash, &context)
            .await
            .unwrap(),
        updated
    );
    let changed_hash = digest(&json!({
        "runtime_id": runtime.id,
        "if_match": 1,
        "policy": RuntimeRetentionPolicy::default(),
    }));
    assert_eq!(
        repo.change_retention_policy(
            &runtime.id,
            1,
            &RuntimeRetentionPolicy::default(),
            &key,
            &changed_hash,
            &context,
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        repo.change_retention_policy(&runtime.id, 1, &policy, &uid(), &body_hash, &context,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleRevision
    );
    let stored: String = sqlx::query_scalar("SELECT retention_policy FROM runtime WHERE id=?")
        .bind(&runtime.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<RuntimeRetentionPolicy>(&stored).unwrap(),
        policy
    );
}

#[tokio::test]
async fn rollback_cambia_default_con_cas_y_conserva_version_anterior() {
    let (_dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let previous = version_record(&repo, &runtime.id, "old").await;
    let target = version_record(&repo, &runtime.id, "target").await;
    validated_installation(&repo, &target.id).await;
    sqlx::query("UPDATE runtime SET default_version_id=? WHERE id=?")
        .bind(&previous.id)
        .bind(&runtime.id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runtime_version SET lifecycle='DEFAULT',promotion='DEFAULT' WHERE id=?")
        .bind(&previous.id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runtime_version SET lifecycle='VALIDATED' WHERE id=?")
        .bind(&target.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let request = RuntimeRollbackRequest {
        expected_default_version_id: Some(previous.id.clone()),
    };
    let body_hash = digest(&json!({"version_id":target.id,"request":request}));
    let key = uid();
    let context = ctx();
    let receipt = repo
        .rollback_version(&target.id, &request, &key, &body_hash, &context)
        .await
        .unwrap();
    assert_eq!(receipt.resource_id, target.id);
    assert!(
        repo.rollback_version(&target.id, &request, &key, &body_hash, &context)
            .await
            .unwrap()
            == receipt
    );
    assert_eq!(
        repo.runtime(&runtime.id)
            .await
            .unwrap()
            .unwrap()
            .default_version_id,
        Some(target.id.clone())
    );
    assert_eq!(
        repo.version(&target.id).await.unwrap().unwrap().lifecycle,
        RuntimeVersionLifecycle::Default
    );
    assert_eq!(
        repo.version(&previous.id).await.unwrap().unwrap().lifecycle,
        RuntimeVersionLifecycle::Retained
    );
    let stale = repo
        .rollback_version(
            &previous.id,
            &RuntimeRollbackRequest {
                expected_default_version_id: Some(previous.id.clone()),
            },
            &uid(),
            &digest(&json!({"stale":true})),
            &context,
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleRevision);
}

async fn seed_compatible_variant(db: &Db, runtime_id: &str, version_id: &str) -> (String, String) {
    let now = AT;
    let source_adapter_id = uid();
    let source_id = uid();
    let model_id = uid();
    let distribution_id = uid();
    let revision_id = uid();
    let variant_id = uid();
    let profile_id = uid();
    let backend_id = uid();
    let backend_installation_id: String = sqlx::query_scalar(
        "SELECT id FROM runtime_installation WHERE runtime_version_id=? LIMIT 1",
    )
    .bind(version_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let adapter_installation_id: String = sqlx::query_scalar(
        "SELECT adapter_installation_id FROM runtime_adapter WHERE id=(SELECT adapter_id FROM runtime WHERE id=?)",
    )
    .bind(runtime_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES(?,?,?,1,'{}',?,'fixture', '[]')")
        .bind(&source_adapter_id).bind(now).bind(now).bind(&adapter_installation_id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,account_id,policy) VALUES(?,?,?,1,'{}','fixture',?,'https://example.invalid',1,1,'NONE',NULL,'{}')")
        .bind(&source_id).bind(now).bind(now).bind(&source_adapter_id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,upstream_identity,capabilities,possible_duplicates) VALUES(?,?,?,1,'{}','fixture',NULL,'[]','[]')")
        .bind(&model_id).bind(now).bind(now).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id,license_id) VALUES(?,?,?,1,'{}',?,?,'fixture','fixture',NULL)")
        .bind(&distribution_id).bind(now).bind(now).bind(&model_id).bind(&source_id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,upstream_commit,manifest_hash,state) VALUES(?,?,?,1,'{}',?,'rev1',NULL,?,'CURRENT')")
        .bind(&revision_id).bind(now).bind(now).bind(&distribution_id).bind("a".repeat(64)).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits,update_available,status) VALUES(?,?,?,1,'{}',?,'GGUF','Q4_K_M','fixture',NULL,'AVAILABLE','AVAILABLE','LARM_MANAGED','NONE',NULL,'GENERAL','[]','{}',0,'AVAILABLE')")
        .bind(&variant_id).bind(now).bind(now).bind(&revision_id).execute(&db.pool).await.unwrap();
    let profile_hash = "b".repeat(64);
    sqlx::query("INSERT INTO launch_profile(id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id) VALUES(?,?,?,1,'{}','CUSTOM','{}',?,NULL)")
        .bind(&profile_id).bind(now).bind(now).bind(&profile_hash).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO runtime_backend(id,created_at,updated_at,row_version,metadata,runtime_installation_id,backend_type,device_id,driver_version,status,evidence_refs,limits) VALUES(?,?,?,1,'{}',?,'fixture.cpu',NULL,NULL,'AVAILABLE','[]','{}')")
        .bind(&backend_id).bind(now).bind(now).bind(&backend_installation_id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO compatibility_record(id,created_at,updated_at,row_version,metadata,variant_id,runtime_version_id,backend_id,hardware_fingerprint,launch_profile_hash,status,hard_gates,limitations,technical_fit,evidence_refs) VALUES(?,?,?,1,'{}',?,?,?,?,?,'COMPATIBLE','[{\"name\":\"fixture\",\"status\":\"PASS\"}]','[]',1.0,'[]')")
        .bind(uid()).bind(now).bind(now).bind(&variant_id).bind(version_id).bind(&backend_id).bind("c".repeat(64)).bind(&profile_hash).execute(&db.pool).await.unwrap();
    (variant_id, profile_id)
}

#[tokio::test]
async fn pin_requiere_compatibilidad_y_unpin_es_durable_e_idempotente() {
    let (_dir, db, repo, request) = setup().await;
    let runtime = create(&repo, &request).await;
    let target = version_record(&repo, &runtime.id, "pin-target").await;
    validated_installation(&repo, &target.id).await;
    sqlx::query("UPDATE runtime_version SET lifecycle='VALIDATED' WHERE id=?")
        .bind(&target.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let (variant_id, launch_profile_id) =
        seed_compatible_variant(&db, &runtime.id, &target.id).await;
    let pin = RuntimePinRequest {
        variant_id: variant_id.clone(),
        launch_profile_id: launch_profile_id.clone(),
    };
    let hash = digest(&json!({"version_id":target.id,"request":pin}));
    let key = uid();
    let context = ctx();
    let receipt = repo
        .pin_version(&target.id, &pin, &key, &hash, &context)
        .await
        .unwrap();
    assert_eq!(receipt.row_version, 1);
    assert_eq!(
        repo.pin_for(&variant_id, &launch_profile_id)
            .await
            .unwrap()
            .unwrap()
            .runtime_version_id,
        target.id
    );
    assert_eq!(
        repo.pin_version(&target.id, &pin, &key, &hash, &context)
            .await
            .unwrap(),
        receipt
    );
    let unpin_hash = digest(&json!({
        "variant_id": variant_id,
        "launch_profile_id": launch_profile_id,
    }));
    let unpin_key = uid();
    repo.unpin(
        &variant_id,
        &launch_profile_id,
        &unpin_key,
        &unpin_hash,
        &context,
    )
    .await
    .unwrap();
    repo.unpin(
        &variant_id,
        &launch_profile_id,
        &unpin_key,
        &unpin_hash,
        &context,
    )
    .await
    .unwrap();
    assert!(repo
        .pin_for(&variant_id, &launch_profile_id)
        .await
        .unwrap()
        .is_none());
}
