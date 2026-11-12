//! Forma/binding de las recetas locales, sin ejecutar workers ni emitir Proof.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_adapter_host::conformance::{
    profiles::{LocalProfile, Recipe},
    runtime::RuntimeConfig,
};
use larm_adapter_sdk::{
    manifest::MethodTable,
    permissions::{adapter_resource, execution_digest},
};
use larm_domain::{
    adapters::{AdapterInstallation, AdapterState, AdapterTrust},
    approval::ApprovalRequest,
    bridge::timestamp,
    conformance::hash,
    conformance_profiles::ConformanceProfile,
    ports::MutationContext,
};
use serde_json::json;
fn fixture() -> (
    tempfile::TempDir,
    LocalProfile,
    ConformanceProfile,
    AdapterInstallation,
    MutationContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now();
    let id = uuid::Uuid::now_v7().to_string();
    let ctx = MutationContext {
        actor_id: uuid::Uuid::now_v7().to_string(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let methods = MethodTable::contract()
        .methods
        .iter()
        .filter(|m| m.family == "all" || m.family == "runtime")
        .map(|m| m.method.clone())
        .collect::<Vec<_>>();
    let manifest = json!({"id":"reference.echo","version":"1.0.0","family":"runtime","protocol_version":"1.0","publisher":"fixture","executable":{"relative_path":"bin/worker.exe","args":[]},"platforms":["windows"],"architectures":["x86_64"],"methods":methods,"installation_strategies":[],"capabilities":["vendor.reference.echo"],"permissions":{"read_roots":["reference-package"],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"package_sha256":"a".repeat(64),"signature":null});
    let a = AdapterInstallation {
        id: id.clone(),
        created_at: timestamp(now),
        updated_at: timestamp(now),
        row_version: 1,
        metadata: Default::default(),
        adapter_id: "reference.echo".into(),
        version: "1.0.0".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: manifest.clone(),
    };
    let mut parameters = serde_json::to_value(larm_domain::launch::LaunchParameters::builtin(
        &id,
        2,
        Some(2048),
        512,
    ))
    .unwrap();
    parameters["adapter_options"] =
        json!({"model_path":dir.path().join("package/model.bin"),"port":23891});
    let request = json!({"installation_id":id,"variant_id":id,"launch_profile":parameters,"constraints":{"runtime_version_id":null,"backend_id":null,"execution_fingerprint":null,"allow_backend_fallback":false,"max_memory_bytes":null},"resource_requirements":null});
    let config = LocalProfile {
        schema_version: "1.0".into(),
        package_dir: dir.path().join("package"),
        work_dir: dir.path().join("work"),
        roots: vec![("reference-package".into(), dir.path().join("package"))],
        custom: false,
        capability_profile: Some("reference-capabilities.json".into()),
        immutable_inputs: vec![],
        timeout_seconds: 120,
        suite: Recipe::Runtime(RuntimeConfig {
            runtime_id: id.clone(),
            launch_request: request,
            expected_version: "reference-echo 1.0.0".into(),
            forbidden_model: dir.path().join("oracle/forbidden.bin"),
            evidence_root: dir.path().join("reports"),
        }),
    };
    let m = larm_adapter_sdk::manifest::AdapterManifest::parse(&manifest.to_string()).unwrap();
    let approval=ApprovalRequest{operation_digest:execution_digest(&m).unwrap(),resource_ids:vec![adapter_resource(&m)],code_hash:Some(m.package_sha256.clone()),permissions:json!({"publisher":m.publisher,"permissions":m.permissions,"expires_at":timestamp(now+chrono::Duration::hours(1)),"secret_refs":[]}).as_object().unwrap().clone(),approved:true}.grant(uuid::Uuid::now_v7().to_string(),&ctx.actor_id,now,chrono::Duration::minutes(15)).unwrap();
    let profile = ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: id,
        actor_id: ctx.actor_id.clone(),
        manifest_sha256: hash(&manifest).unwrap(),
        configuration: serde_json::to_value(&config).unwrap(),
        approval,
        created_at: timestamp(now),
    };
    (dir, config, profile, a, ctx)
}
#[test]
fn roundtrip_reconstruye_receta_compilada_y_binding_completo_sin_proof() {
    let (_dir, config, p, a, ctx) = fixture();
    let decoded: LocalProfile = serde_json::from_value(p.configuration.clone()).unwrap();
    let target = decoded.target(&p, &a, &ctx).unwrap();
    assert_eq!(target.roots.unwrap().0, config.roots);
    let d = target.suite.descriptor();
    assert_eq!(d["family"], "runtime");
    assert_eq!(d["capabilities"], json!(["vendor.reference.echo"]));
    assert_eq!(d["registered_profile_sha256"], hash(&p).unwrap());
    assert_eq!(target.immutable_inputs.len(), 2);
    for mode in 0..3 {
        let mut value = p.configuration.clone();
        match mode {
            0 => value["suite"]["recipe"] = json!("external.code"),
            1 => value["passed"] = json!(true),
            _ => value["suite"]["parameters"]["proof"] = json!({"status":"PASS"}),
        };
        assert!(serde_json::from_value::<LocalProfile>(value).is_err());
    }
}
#[test]
fn datos_cambiados_actor_ajeno_raices_ambiguas_y_limites_invalidos_se_rechazan() {
    let (_dir, config, p, a, ctx) = fixture();
    let mut foreign = ctx.clone();
    foreign.actor_id = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        config
            .target(&p, &a, &foreign)
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_profile_actor_mismatch"
    );
    let mut changed = p.clone();
    changed.configuration["timeout_seconds"] = json!(119);
    assert_eq!(
        config
            .target(&changed, &a, &ctx)
            .err()
            .unwrap()
            .details
            .reason,
        "conformance_profile_configuration_mismatch"
    );
    for mode in 0..5 {
        let mut c = config.clone();
        match mode {
            0 => c.timeout_seconds = 0,
            // El techo del perfil subió a 2400 s con los conversores de
            // producto reales (cuantización K en Python stdlib); 2401 sigue
            // siendo inválido para todos los consumos.
            1 => c.timeout_seconds = 2401,
            2 => c
                .roots
                .push(("REFERENCE-PACKAGE".into(), c.package_dir.clone())),
            3 => c.package_dir = "relative".into(),
            _ => c.schema_version = "2.0".into(),
        };
        let mut record = p.clone();
        record.configuration = serde_json::to_value(&c).unwrap();
        assert!(c.target(&record, &a, &ctx).is_err());
    }
    let mut revoked = a.clone();
    revoked.trust = AdapterTrust::Revoked;
    assert!(config.target(&p, &revoked, &ctx).is_err());
}
#[test]
fn aprobacion_consumida_emite_concesion_con_su_caducidad_no_con_el_ttl_corto() {
    let (_dir, config, mut p, a, ctx) = fixture();
    let now = chrono::Utc::now();
    p.approval.created_at = timestamp(now - chrono::Duration::minutes(20));
    p.approval.expires_at = timestamp(now - chrono::Duration::minutes(5));
    p.created_at = timestamp(now - chrono::Duration::minutes(10));
    assert!(config.target(&p, &a, &ctx).is_ok());
    p.approval.permissions.insert(
        "expires_at".into(),
        json!(timestamp(now - chrono::Duration::minutes(1))),
    );
    assert_eq!(
        config.target(&p, &a, &ctx).err().unwrap().details.reason,
        "grant_expired"
    );
}
#[test]
fn renovar_perfil_o_concesion_cambia_identidad_de_suite_aunque_los_inputs_coincidan() {
    let (_dir, config, p, a, ctx) = fixture();
    let before = config.target(&p, &a, &ctx).unwrap().suite.descriptor();
    let mut next = p.clone();
    next.id = uuid::Uuid::now_v7().to_string();
    next.approval.id = uuid::Uuid::now_v7().to_string();
    let after = config.target(&next, &a, &ctx).unwrap().suite.descriptor();
    assert_eq!(before["family"], after["family"]);
    assert_eq!(before["launch_request"], after["launch_request"]);
    assert_ne!(
        before["registered_profile_sha256"],
        after["registered_profile_sha256"]
    );
    assert_ne!(hash(&before).unwrap(), hash(&after).unwrap());
}
