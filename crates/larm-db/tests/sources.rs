//! Catálogo P043 con SQLite real: CAS, replay y fallos dentro de transacción.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::sources::SqlSources;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::credential::{CredentialReference, SecretType, BACKEND};
use larm_domain::error::ErrorCode;
use larm_domain::ports::{
    AdapterInstallationRepository, CredentialRepository, MutationContext, SourceAccountRepository,
    SourceCatalogRepository,
};
use larm_domain::source_accounts::{CheckSourceLicense, ModelSourceAccount};
use larm_domain::sources::*;
use serde_json::{json, Map, Value};

#[path = "../../../tests/fixtures/adapters/conformance_persistence.rs"]
mod persistence;

fn ctx() -> MutationContext {
    MutationContext {
        actor_id: "source-test-actor".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    }
}
fn request(adapter: &str) -> ModelSourceCreate {
    serde_json::from_value(json!({"name":"fixture","adapter_id":adapter,"base_url":"https://fixture.invalid/models","priority":1,"enabled":false,"auth_mode":"NONE","account_id":null,"policy":{"vendor":{"unknown":[1,true,null]}}})).unwrap()
}
async fn setup() -> (
    tempfile::TempDir,
    larm_db::pool::Db,
    SqlSources,
    ModelSourceAdapter,
    AdapterInstallation,
) {
    let (dir, db) = larm_db::testing::temp_migrated().await.unwrap();
    let table: Value =
        serde_json::from_str(include_str!("../../../docs/contracts/adapter-methods.json")).unwrap();
    let mut methods = vec![
        json!("core.handshake"),
        json!("core.cancel"),
        json!("core.heartbeat"),
    ];
    methods.extend(
        table["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["family"] == "source")
            .map(|m| m["method"].clone()),
    );
    let installed = AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-16T00:00:00.000Z".into(),
        updated_at: "2026-10-16T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        adapter_id: "fixture.source".into(),
        version: "1.0.0".into(),
        family: "source".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.source","version":"1.0.0","family":"source","protocol_version":"1.0","publisher":"fixture","executable":{"relative_path":"bin/fixture.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":methods,"permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":["source.search"],"package_sha256":"a".repeat(64),"signature":null,"installation_strategies":[]}),
    };
    larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .insert(&installed, &ctx())
    .await
    .unwrap();
    let repo = SqlSources {
        pool: db.pool.clone(),
        qualification: persistence::physical_gate_fixture(),
    };
    let binding = repo
        .register_source_adapter(&installed.id, "HTTP", &Map::new(), &ctx())
        .await
        .unwrap();
    (dir, db, repo, binding, installed)
}

async fn source(
    repo: &SqlSources,
    adapter: &ModelSourceAdapter,
) -> larm_domain::sources::ModelSource {
    repo.create_source(
        &request(&adapter.id),
        &uuid::Uuid::now_v7().to_string(),
        &"b".repeat(64),
        &ctx(),
    )
    .await
    .unwrap()
}

async fn basic_source_account(
    repo: &SqlSources,
    adapter: &ModelSourceAdapter,
) -> (
    larm_domain::sources::ModelSource,
    ModelSourceAccount,
    larm_db::source_accounts::SqlSourceAccounts,
) {
    let mut config = request(&adapter.id);
    config.auth_mode = AuthMode::Basic;
    // Las pruebas de credenciales no necesitan una fuente ejecutable ni una
    // cualificación del worker; el vínculo Basic sí se persiste deshabilitada.
    config.enabled = false;
    let source = repo
        .create_source(
            &config,
            &uuid::Uuid::now_v7().to_string(),
            &"f".repeat(64),
            &ctx(),
        )
        .await
        .unwrap();
    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: None,
        display_name: "Cuenta Basic".into(),
        accepted_license_hashes: vec![],
    };
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: repo.pool.clone(),
    };
    accounts
        .create_source_account(&account, &ctx())
        .await
        .unwrap();
    let mut selected = source.configuration();
    selected.account_id = Some(account.id.clone());
    let source = repo
        .update_source(&source.id, source.row_version, &selected, &ctx())
        .await
        .unwrap();
    (source, account, accounts)
}

#[tokio::test]
async fn cuentas_de_fuente_persisten_referencia_licencias_y_eventos_sin_secreto() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let source = source(&repo, &adapter).await;
    let credential = CredentialReference {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        service: format!("larm/source/{}", source.id),
        username: "tecnico-local".into(),
        backend: BACKEND.into(),
        secret_type: SecretType::Token,
        scopes: vec![],
        expires_at: None,
    };
    larm_db::security::SqlCredentials {
        pool: db.pool.clone(),
    }
    .insert(&credential, &ctx())
    .await
    .unwrap();
    let hash = "c".repeat(64);
    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: Some(credential.id.clone()),
        display_name: "Cuenta de prueba".into(),
        accepted_license_hashes: vec![hash.clone()],
    };
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: db.pool.clone(),
    };
    let created = accounts
        .create_source_account(&account, &ctx())
        .await
        .unwrap();
    assert_eq!(created, account);
    assert_eq!(
        accounts.source_account(&created.id).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        accounts.page_source_accounts(None, 2).await.unwrap(),
        vec![created.clone()]
    );

    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.event_type='source_account.created' AND e.resource_id=?",
    )
    .bind(&created.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(outbox_count, 1);
    let stored_json: String = sqlx::query_scalar(
        "SELECT json_object('metadata',metadata,'display_name',display_name,'licenses',accepted_license_hashes) FROM model_source_account WHERE id=?",
    )
    .bind(&created.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!stored_json.contains("secret"));

    accounts
        .delete_source_account(&created.id, &ctx())
        .await
        .unwrap();
    assert!(accounts
        .source_account(&created.id)
        .await
        .unwrap()
        .is_none());
    let deleted_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.event_type='source_account.deleted' AND e.resource_id=?",
    )
    .bind(&created.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(deleted_events, 1);
}

#[tokio::test]
async fn cuenta_rechaza_credencial_de_otra_fuente_y_borrado_si_esta_seleccionada() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let source = source(&repo, &adapter).await;
    let credential = CredentialReference {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        service: "larm/source/0192aaaa-0000-7000-8000-000000000009".into(),
        username: "otra-cuenta".into(),
        backend: BACKEND.into(),
        secret_type: SecretType::Token,
        scopes: vec![],
        expires_at: None,
    };
    larm_db::security::SqlCredentials {
        pool: db.pool.clone(),
    }
    .insert(&credential, &ctx())
    .await
    .unwrap();
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: db.pool.clone(),
    };
    let wrong_reference = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: Some(credential.id.clone()),
        display_name: "Otra fuente".into(),
        accepted_license_hashes: vec![],
    };
    assert_eq!(
        accounts
            .create_source_account(&wrong_reference, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );

    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: None,
        display_name: "Seleccionada".into(),
        accepted_license_hashes: vec![],
    };
    accounts
        .create_source_account(&account, &ctx())
        .await
        .unwrap();
    let mut selected = source.configuration();
    selected.account_id = Some(account.id.clone());
    repo.update_source(&source.id, source.row_version, &selected, &ctx())
        .await
        .unwrap();
    assert_eq!(
        accounts
            .delete_source_account(&account.id, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(accounts
        .source_account(&account.id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn credencial_basic_se_vincula_y_rotacion_conserva_solo_la_referencia() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let (source, account, accounts) = basic_source_account(&repo, &adapter).await;
    let now = larm_domain::bridge::timestamp(chrono::Utc::now());
    let reference = CredentialReference {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: now.clone(),
        updated_at: now,
        row_version: 1,
        metadata: Map::new(),
        service: format!("larm/source/{}", source.id),
        username: account.id.clone(),
        backend: BACKEND.into(),
        secret_type: SecretType::Password,
        scopes: vec![],
        expires_at: None,
    };
    let first = accounts
        .set_basic_credential(&account.id, &reference, &ctx())
        .await
        .unwrap();
    assert_eq!(first.id, reference.id);
    assert_eq!(first.row_version, 1);
    let attached = accounts.source_account(&account.id).await.unwrap().unwrap();
    assert_eq!(
        attached.credential_reference_id.as_deref(),
        Some(first.id.as_str())
    );
    assert_eq!(attached.row_version, 2);

    let rotated_request = CredentialReference {
        updated_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        row_version: 2,
        ..first.clone()
    };
    let rotated = accounts
        .set_basic_credential(&account.id, &rotated_request, &ctx())
        .await
        .unwrap();
    assert_eq!(rotated.id, first.id);
    assert_eq!(rotated.row_version, 2);
    assert_eq!(
        accounts
            .source_account(&account.id)
            .await
            .unwrap()
            .unwrap()
            .row_version,
        2,
        "rotar una credencial no cambia el vínculo de cuenta"
    );

    let persisted: (String, String, String, i64) = sqlx::query_as(
        "SELECT service,username,secret_type,row_version FROM credential_reference WHERE id=?",
    )
    .bind(&first.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        persisted,
        (
            format!("larm/source/{}", source.id),
            account.id.clone(),
            "PASSWORD".into(),
            2,
        )
    );
    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT event_type,payload FROM event WHERE resource_id IN (?,?) ORDER BY event_id",
    )
    .bind(&account.id)
    .bind(&first.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.0.as_str())
            .collect::<Vec<_>>(),
        [
            "source_account.created",
            "credential.created",
            "source_account.credential_updated",
            "credential.rotated"
        ]
    );
    assert!(events
        .iter()
        .all(|event| !event.1.contains("password") && !event.1.contains("usuario")));
}

#[tokio::test]
async fn fallo_de_outbox_revierte_referencia_y_vinculo_basic() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let (_source, account, accounts) = basic_source_account(&repo, &adapter).await;
    let reference = CredentialReference {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        service: format!("larm/source/{}", account.source_id),
        username: account.id.clone(),
        backend: BACKEND.into(),
        secret_type: SecretType::Password,
        scopes: vec![],
        expires_at: None,
    };
    sqlx::query(
        "CREATE TRIGGER reject_basic_credential_outbox BEFORE INSERT ON event_outbox \
         WHEN (SELECT event_type FROM event WHERE event.event_id=NEW.event_id)='source_account.credential_updated' \
         BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(accounts
        .set_basic_credential(&account.id, &reference, &ctx())
        .await
        .is_err());
    sqlx::query("DROP TRIGGER reject_basic_credential_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let (references, attached, events): (i64, Option<String>, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM credential_reference WHERE id=?), \
         (SELECT credential_reference_id FROM model_source_account WHERE id=?), \
         (SELECT COUNT(*) FROM event WHERE resource_id IN (?,?) \
          AND event_type IN ('credential.created','source_account.credential_updated'))",
    )
    .bind(&reference.id)
    .bind(&account.id)
    .bind(&account.id)
    .bind(&reference.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((references, attached, events), (0, None, 0));
    assert_eq!(
        accounts
            .source_account(&account.id)
            .await
            .unwrap()
            .unwrap()
            .row_version,
        1
    );
}
#[tokio::test]
async fn fallo_del_outbox_restaura_la_cuenta_y_el_evento_en_la_misma_transaccion() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let source = source(&repo, &adapter).await;
    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id,
        credential_reference_id: None,
        display_name: "Rollback fixture".into(),
        accepted_license_hashes: vec!["d".repeat(64)],
    };
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: db.pool.clone(),
    };
    sqlx::query(
        "CREATE TRIGGER reject_source_account_outbox BEFORE INSERT ON event_outbox \
         WHEN (SELECT event_type FROM event WHERE event.event_id=NEW.event_id)='source_account.created' \
         BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(accounts
        .create_source_account(&account, &ctx())
        .await
        .is_err());
    sqlx::query("DROP TRIGGER reject_source_account_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_source_account WHERE id=?")
        .bind(&account.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event WHERE event_type='source_account.created' AND resource_id=?",
    )
    .bind(&account.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((rows, events), (0, 0));
}

#[tokio::test]
async fn aceptacion_y_retirada_de_licencia_actualizan_cuenta_y_outbox_atomicamente() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let source = source(&repo, &adapter).await;
    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: None,
        display_name: "Licencia fixture".into(),
        accepted_license_hashes: vec![],
    };
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: db.pool.clone(),
    };
    accounts
        .create_source_account(&account, &ctx())
        .await
        .unwrap();
    let hash = "e".repeat(64);
    let accepted = CheckSourceLicense {
        license_hash: hash.clone(),
        source_account_id: account.id.clone(),
        revision_key: "fixture:revision-1".into(),
        accepted: true,
    };
    let accepted_receipt = accounts
        .set_source_license(
            &source.id,
            &accepted,
            "license-accept-once",
            &"f".repeat(64),
            &ctx(),
        )
        .await
        .unwrap();
    let version = accepted_receipt.row_version;
    assert_eq!(version, 2);
    assert!(!accepted_receipt.event_id.is_empty());
    let stored = accounts.source_account(&account.id).await.unwrap().unwrap();
    assert_eq!(stored.row_version, 2);
    assert_eq!(stored.accepted_license_hashes, vec![hash]);
    let replay = accounts
        .set_source_license(
            &source.id,
            &accepted,
            "license-accept-once",
            &"f".repeat(64),
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(replay.row_version, version);
    assert_eq!(replay.event_id, accepted_receipt.event_id);
    assert_eq!(
        accounts
            .source_account(&account.id)
            .await
            .unwrap()
            .unwrap()
            .row_version,
        2
    );
    assert_eq!(
        accounts
            .set_source_license(
                &source.id,
                &accepted,
                "license-accept-once",
                &"0".repeat(64),
                &ctx(),
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );

    let revoked = CheckSourceLicense {
        accepted: false,
        ..accepted
    };
    let revoked_receipt = accounts
        .set_source_license(
            &source.id,
            &revoked,
            "license-revoke",
            &"a".repeat(64),
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_receipt.row_version, 3);
    let stored = accounts.source_account(&account.id).await.unwrap().unwrap();
    assert_eq!(stored.row_version, 3);
    assert!(stored.accepted_license_hashes.is_empty());
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event WHERE event_type='source.license_checked' AND resource_id=?",
    )
    .bind(&account.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);
    let idempotency_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency WHERE operation='P045_post__sources_id_license' AND principal_id=?",
    )
    .bind("source-test-actor")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(idempotency_rows, 2);
}

#[tokio::test]
async fn fallo_del_outbox_en_licencia_revierte_hash_version_evento_e_idempotencia() {
    let (_dir, db, repo, adapter, _installed) = setup().await;
    let source = source(&repo, &adapter).await;
    let account = ModelSourceAccount {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-23T00:00:00.000Z".into(),
        updated_at: "2026-10-23T00:00:00.000Z".into(),
        row_version: 1,
        metadata: Map::new(),
        source_id: source.id.clone(),
        credential_reference_id: None,
        display_name: "License rollback".into(),
        accepted_license_hashes: vec![],
    };
    let accounts = larm_db::source_accounts::SqlSourceAccounts {
        pool: db.pool.clone(),
    };
    accounts
        .create_source_account(&account, &ctx())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_source_license_outbox BEFORE INSERT ON event_outbox \
         WHEN (SELECT event_type FROM event WHERE event.event_id=NEW.event_id)='source.license_checked' \
         BEGIN SELECT RAISE(ABORT, 'injected license outbox failure'); END",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let request = CheckSourceLicense {
        license_hash: "f".repeat(64),
        source_account_id: account.id.clone(),
        revision_key: "fixture:rev".into(),
        accepted: true,
    };
    assert!(accounts
        .set_source_license(
            &source.id,
            &request,
            "license-fail-once",
            &"9".repeat(64),
            &ctx()
        )
        .await
        .is_err());
    sqlx::query("DROP TRIGGER reject_source_license_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let stored = accounts.source_account(&account.id).await.unwrap().unwrap();
    assert_eq!(stored.row_version, 1);
    assert!(stored.accepted_license_hashes.is_empty());
    let (events, keys): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM event WHERE event_type='source.license_checked' AND resource_id=?), \
                (SELECT COUNT(*) FROM idempotency WHERE operation='P045_post__sources_id_license' AND key='license-fail-once')",
    )
    .bind(&account.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((events, keys), (0, 0));
}

async fn snapshot(db: &larm_db::pool::Db) -> Value {
    let mut result = Map::new();
    for table in [
        "model_source",
        "model_source_adapter",
        "event",
        "event_outbox",
        "idempotency",
        "operation_journal",
        "volume_reservation",
        "resource_lock",
    ] {
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT json_object('count',count(*)) FROM {table}"
        )))
        .fetch_all(&db.pool)
        .await
        .unwrap();
        result.insert(table.into(), json!(rows));
    }
    result.insert(
        "installations".into(),
        json!(sqlx::query_as::<_, (String, i64, String)>(
            "SELECT id,row_version,metadata FROM adapter_installation ORDER BY id"
        )
        .fetch_all(&db.pool)
        .await
        .unwrap()),
    );
    result.insert(
        "sources".into(),
        json!(sqlx::query_as::<_, (String, i64, String, String)>(
            "SELECT id,row_version,name,policy FROM model_source ORDER BY id"
        )
        .fetch_all(&db.pool)
        .await
        .unwrap()),
    );
    Value::Object(result)
}

#[tokio::test]
async fn indice_publico_de_conexiones_y_binding_se_publican_atomicamente() {
    let (_dir, db, repo, binding, installed) = setup().await;
    let current = larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .get(&installed.id)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        current.metadata["source_bindings"],
        json!([{"id":binding.id,"source_type":"HTTP"}])
    );
    assert_eq!(current.row_version, 2);
    assert_eq!(current.state, installed.state);
    assert_eq!(current.trust, installed.trust);
    assert_eq!(current.package_hash, installed.package_hash);
    let before = snapshot(&db).await;
    sqlx::query("CREATE TRIGGER fail_binding_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(ABORT,'fixture fail'); END")
        .execute(&db.pool).await.unwrap();
    assert!(repo
        .register_source_adapter(&installed.id, "LOCAL", &Map::new(), &ctx())
        .await
        .is_err());
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn alta_concurrente_replay_original_y_retencion_de_treinta_dias() {
    let (_dir, db, repo, binding, _) = setup().await;
    let req = request(&binding.id);
    let hash = "a".repeat(64);
    let mutation = ctx();
    let (a, b) = tokio::join!(
        repo.create_source(&req, "key", &hash, &mutation),
        repo.create_source(&req, "key", &hash, &mutation)
    );
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(repo.source(&a.id).await.unwrap(), Some(a.clone()));
    let original = snapshot(&db).await;
    let mut changed = req.clone();
    changed.name = "different".into();
    assert_eq!(
        repo.create_source(&changed, "key", &hash, &mutation)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        repo.create_source(&req, "key", &"b".repeat(64), &mutation)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(snapshot(&db).await, original);
    changed = req.clone();
    changed.name = "new name".into();
    repo.update_source(&a.id, 1, &changed, &mutation)
        .await
        .unwrap();
    repo.delete_source(&a.id, 2, &mutation).await.unwrap();
    assert_eq!(
        repo.create_source(&req, "key", &hash, &mutation)
            .await
            .unwrap(),
        a
    );
    assert!(
        repo.source(&a.id).await.unwrap().is_none(),
        "replay no recrea una fuente borrada"
    );
    let expiry: String = sqlx::query_scalar("SELECT expires_at FROM idempotency")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let days = chrono::DateTime::parse_from_rfc3339(&expiry)
        .unwrap()
        .signed_duration_since(chrono::Utc::now())
        .num_days();
    assert_eq!(days, 29);
    sqlx::query("UPDATE idempotency SET expires_at='2020-01-01T00:00:00.000Z'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_ne!(
        repo.create_source(&req, "key", &hash, &mutation)
            .await
            .unwrap()
            .id,
        a.id
    );
}

#[tokio::test]
async fn actores_y_cas_separados_conservan_politica_y_eventos() {
    let (_dir, db, repo, binding, _) = setup().await;
    let req = request(&binding.id);
    let hash = "a".repeat(64);
    let a = repo
        .create_source(&req, "key", &hash, &ctx())
        .await
        .unwrap();
    let mut other = ctx();
    other.actor_id = "other".into();
    let b = repo
        .create_source(&req, "key", &hash, &other)
        .await
        .unwrap();
    assert_ne!(a.id, b.id);
    let mut changed = req.clone();
    changed.name = "winner".into();
    let mutation = ctx();
    let (one, two) = tokio::join!(
        repo.update_source(&a.id, 1, &changed, &mutation),
        repo.update_source(&a.id, 1, &changed, &mutation)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let error = one.err().or(two.err()).unwrap();
    assert_eq!(error.code, ErrorCode::StaleRevision);
    let updated = repo.source(&a.id).await.unwrap().unwrap();
    assert_eq!(updated.row_version, 2);
    assert_eq!(updated.policy, req.policy);
    let before = snapshot(&db).await;
    assert_eq!(
        repo.delete_source(&a.id, 1, &ctx()).await.unwrap_err().code,
        ErrorCode::StaleRevision
    );
    assert_eq!(snapshot(&db).await, before);
    let events:Vec<(String,i64)>=sqlx::query_as("SELECT resource_id,resource_version FROM event WHERE resource_type='model_source' ORDER BY event_id").fetch_all(&db.pool).await.unwrap();
    assert_eq!(events, vec![(a.id.clone(), 1), (b.id, 1), (a.id, 2)]);
}

#[tokio::test]
async fn registro_de_binding_no_concede_validated_y_rechaza_revocacion() {
    let (_dir, db, repo, binding, installed) = setup().await;
    let before = snapshot(&db).await;
    assert_eq!(
        repo.register_source_adapter(&installed.id, "HTTP", &Map::new(), &ctx())
            .await
            .unwrap(),
        binding
    );
    assert_eq!(snapshot(&db).await, before);
    let mut metadata = Map::new();
    metadata.insert("changed".into(), json!(true));
    assert_eq!(
        repo.register_source_adapter(&installed.id, "HTTP", &metadata, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let state: (String, String) =
        sqlx::query_as("SELECT state,trust FROM adapter_installation WHERE id=?")
            .bind(&installed.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(state, ("INSTALLED".into(), "UNTRUSTED".into()));
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&installed.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut enabled = request(&binding.id);
    enabled.enabled = true;
    assert_eq!(
        repo.create_source(&enabled, "key", &"a".repeat(64), &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidStateTransition
    );
    let mut disabled = request(&binding.id);
    disabled.enabled = false;
    let source = repo
        .create_source(&disabled, "disabled", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    let mut enable = disabled;
    enable.enabled = true;
    assert_eq!(
        repo.update_source(&source.id, 1, &enable, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidStateTransition
    );
}

#[tokio::test]
async fn habilitar_exige_validated_y_revocar_no_impide_desactivar() {
    let (_dir, db, repo, binding, installed) = setup().await;
    let disabled = request(&binding.id);
    let source = repo
        .create_source(&disabled, "create", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    let mut enabled = disabled.clone();
    enabled.enabled = true;
    let before = snapshot(&db).await;
    assert_eq!(
        repo.update_source(&source.id, 1, &enabled, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "source_adapter_unqualified"
    );
    assert_eq!(snapshot(&db).await, before);
    // Recibo sintético de persistencia; no acredita ejecutar un worker.
    persistence::qualify_fixture(&db.pool, &installed, &ctx()).await;
    assert!(
        repo.update_source(&source.id, 1, &enabled, &ctx())
            .await
            .unwrap()
            .enabled
    );
    sqlx::query("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?")
        .bind(&installed.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo
        .update_source(&source.id, 2, &enabled, &ctx())
        .await
        .is_err());
    let stopped = repo
        .update_source(&source.id, 2, &disabled, &ctx())
        .await
        .unwrap();
    assert!(!stopped.enabled);
    assert_eq!(stopped.row_version, 3);
}

#[tokio::test]
async fn habilitar_rechaza_validated_sin_recibo_y_recibo_obsoleto_sin_efectos() {
    for qualified in [false, true] {
        let (_dir, db, repo, binding, installed) = setup().await;
        if qualified {
            persistence::qualify_fixture(&db.pool, &installed, &ctx()).await;
            sqlx::query("UPDATE adapter_installation SET manifest=json_set(manifest,'$.permissions.read_roots',json('[\"changed\"]'))")
                .execute(&db.pool).await.unwrap();
        } else {
            sqlx::query("UPDATE adapter_installation SET state='VALIDATED'")
                .execute(&db.pool)
                .await
                .unwrap();
        }
        let disabled = request(&binding.id);
        let source = repo
            .create_source(&disabled, "disabled", &"a".repeat(64), &ctx())
            .await
            .unwrap();
        let mut enabled = disabled;
        enabled.enabled = true;
        let before = snapshot(&db).await;
        assert!(repo
            .create_source(&enabled, "enabled", &"a".repeat(64), &ctx())
            .await
            .is_err());
        assert!(repo
            .update_source(&source.id, 1, &enabled, &ctx())
            .await
            .is_err());
        assert_eq!(snapshot(&db).await, before);
    }
}

#[tokio::test]
async fn resolver_execution_binding_exige_fuente_habilitada_y_binding_validated_vigente() {
    let (_dir, db, repo, binding, installed) = setup().await;
    let disabled = repo
        .create_source(&request(&binding.id), "disabled", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    assert_eq!(
        repo.execution_binding(&disabled.id)
            .await
            .unwrap_err()
            .details
            .reason,
        "source_disabled"
    );

    persistence::qualify_fixture(&db.pool, &installed, &ctx()).await;
    let mut enabled = request(&binding.id);
    enabled.enabled = true;
    let source = repo
        .create_source(&enabled, "enabled", &"b".repeat(64), &ctx())
        .await
        .unwrap();
    let snapshot = repo.execution_binding(&source.id).await.unwrap();
    assert_eq!(snapshot.source, source);
    assert_eq!(snapshot.adapter, binding);
    assert_eq!(snapshot.installation.state, AdapterState::Validated);

    sqlx::query("UPDATE model_source_adapter SET capabilities='[]' WHERE id=?")
        .bind(&binding.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        repo.execution_binding(&source.id)
            .await
            .unwrap_err()
            .details
            .reason,
        "source_adapter_binding_stale"
    );
}

#[tokio::test]
async fn binding_de_capacidades_alterado_no_habilita_aunque_instalacion_tenga_recibo_vigente() {
    let (_dir, db, repo, binding, installed) = setup().await;
    persistence::qualify_fixture(&db.pool, &installed, &ctx()).await;
    sqlx::query("UPDATE model_source_adapter SET capabilities='[\"source.sync\"]'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut enabled = request(&binding.id);
    enabled.enabled = true;
    let before = snapshot(&db).await;
    assert_eq!(
        repo.create_source(&enabled, "enabled", &"a".repeat(64), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "source_adapter_binding_stale"
    );
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn cuentas_ajenas_referencias_y_retarget_no_tienen_efectos() {
    let (_dir, db, repo, binding, _) = setup().await;
    let req = request(&binding.id);
    let a = repo
        .create_source(&req, "a", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    let b = repo
        .create_source(&req, "b", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    let account = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO model_source_account(id,created_at,updated_at,row_version,metadata,source_id,credential_reference_id,display_name,accepted_license_hashes) VALUES (?,'2026-10-16T00:00:00.000Z','2026-10-16T00:00:00.000Z',1,'{}',?,NULL,'fixture','[]')").bind(&account).bind(&a.id).execute(&db.pool).await.unwrap();
    let before = snapshot(&db).await;
    let mut changed = req.clone();
    changed.account_id = Some(account.clone());
    assert_eq!(
        repo.update_source(&b.id, 1, &changed, &ctx())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        repo.delete_source(&a.id, 1, &ctx()).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    changed = req.clone();
    changed.base_url = "https://another.invalid".into();
    assert_eq!(
        repo.update_source(&a.id, 1, &changed, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "source_identity_in_use"
    );
    assert_eq!(snapshot(&db).await, before);
    changed = req;
    changed.account_id = Some(account);
    changed.enabled = false;
    assert!(
        !repo
            .update_source(&a.id, 1, &changed, &ctx())
            .await
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn outbox_e_idempotencia_fallidos_revierten_toda_mutacion() {
    let (_dir, db, repo, binding, _) = setup().await;
    let req = request(&binding.id);
    let source = repo
        .create_source(&req, "initial", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    let before = snapshot(&db).await;
    sqlx::query("CREATE TRIGGER source_test_outbox BEFORE INSERT ON event_outbox BEGIN SELECT RAISE(FAIL,'source_test_outbox'); END").execute(&db.pool).await.unwrap();
    assert!(repo
        .create_source(&req, "failed", &"a".repeat(64), &ctx())
        .await
        .is_err());
    assert_eq!(snapshot(&db).await, before);
    let mut update = req.clone();
    update.name = "not saved".into();
    assert!(repo
        .update_source(&source.id, 1, &update, &ctx())
        .await
        .is_err());
    assert_eq!(snapshot(&db).await, before);
    assert!(repo.delete_source(&source.id, 1, &ctx()).await.is_err());
    assert_eq!(snapshot(&db).await, before);
    sqlx::query("DROP TRIGGER source_test_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER source_test_replay BEFORE INSERT ON idempotency BEGIN SELECT RAISE(FAIL,'source_test_replay'); END").execute(&db.pool).await.unwrap();
    assert!(repo
        .create_source(&req, "failed-replay", &"a".repeat(64), &ctx())
        .await
        .is_err());
    assert_eq!(snapshot(&db).await, before);
}

#[tokio::test]
async fn paginacion_filtros_literal_y_reapertura() {
    let (dir, db, repo, binding, _) = setup().await;
    let mut req = request(&binding.id);
    req.name = "name_%".into();
    let first = repo
        .create_source(&req, "1", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    req.name = "another".into();
    let second = repo
        .create_source(&req, "2", &"b".repeat(64), &ctx())
        .await
        .unwrap();
    let filter = SourceFilter {
        q: None,
        capability: Some("source.search".into()),
    };
    let page = repo.page_sources(None, 1, &filter).await.unwrap();
    assert_eq!(page.as_slice(), std::slice::from_ref(&first));
    assert_eq!(
        repo.page_sources(
            Some((first.created_at.clone(), first.id.clone())),
            1,
            &filter
        )
        .await
        .unwrap(),
        [second]
    );
    assert_eq!(
        repo.page_sources(
            None,
            200,
            &SourceFilter {
                q: Some("_%".into()),
                capability: Some("source.search".into())
            }
        )
        .await
        .unwrap()
        .as_slice(),
        std::slice::from_ref(&first)
    );
    assert!(repo
        .page_sources(
            None,
            200,
            &SourceFilter {
                q: None,
                capability: Some("' OR 1=1".into())
            }
        )
        .await
        .unwrap()
        .is_empty());
    db.pool.close().await;
    let reopened = larm_db::pool::Db::open(&dir.path().join("larm.sqlite"))
        .await
        .unwrap();
    assert_eq!(
        SqlSources {
            pool: reopened.pool.clone(),
            qualification: persistence::physical_gate_fixture()
        }
        .source(&first.id)
        .await
        .unwrap(),
        Some(first)
    );
}

#[tokio::test]
async fn activar_fuente_exige_gate_fisico_aunque_el_recibo_y_journal_sean_validos() {
    let (_dir, db, mut repo, binding, installed) = setup().await;
    persistence::qualify_fixture(&db.pool, &installed, &ctx()).await;
    let mut req = request(&binding.id);
    let source = repo
        .create_source(&req, "physical-disabled", &"a".repeat(64), &ctx())
        .await
        .unwrap();
    repo.qualification = None;
    req.enabled = true;
    let before = snapshot(&db).await;
    assert_eq!(
        repo.create_source(&req, "physical-enabled", &"b".repeat(64), &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_physical_gate_unavailable"
    );
    assert_eq!(
        repo.update_source(&source.id, source.row_version, &req, &ctx())
            .await
            .unwrap_err()
            .details
            .reason,
        "conformance_physical_gate_unavailable"
    );
    assert_eq!(snapshot(&db).await, before);
    req.enabled = false;
    let changed = repo
        .update_source(&source.id, source.row_version, &req, &ctx())
        .await
        .unwrap();
    assert!(!changed.enabled);
    repo.qualification = persistence::physical_gate_fixture();
    req.enabled = true;
    assert!(
        repo.update_source(&source.id, changed.row_version, &req, &ctx())
            .await
            .unwrap()
            .enabled
    );
}
