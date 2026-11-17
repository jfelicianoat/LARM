#![allow(clippy::unwrap_used, clippy::expect_used)]
use chrono::{DateTime, Utc};
use larm_domain::approval::ApprovalRequest;
use larm_domain::artifacts::Plan;
use larm_domain::error::ErrorCode;
use larm_domain::runtime::{InstallationStrategy, RegistrationMode, RuntimeOwnership};
use larm_domain::runtime_install::InstallRequest;
use larm_runtime::install::{check_execution, plan_hash, spec, InstallSpec};
use serde_json::{json, Value};

const RID: &str = "0192aaaa-0000-7000-8000-000000000001";
const VID: &str = "0192aaaa-0000-7000-8000-000000000002";
const STORAGE: &str = "0192aaaa-0000-7000-8000-000000000003";
fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-20T14:00:00.000Z")
        .unwrap()
        .with_timezone(&Utc)
}
fn request(strategy: InstallationStrategy) -> InstallRequest {
    InstallRequest {
        runtime_id: RID.into(),
        upstream_version: "b7142+vendor/42".into(),
        strategy,
        storage_location_id: STORAGE.into(),
        profile_lock_hash: "a".repeat(64),
        external_host_id: None,
        // Las estrategias virtualizadas exigen host de ejecución referenciado.
        execution_host_id: matches!(
            strategy,
            InstallationStrategy::Wsl | InstallationStrategy::Container
        )
        .then(|| "0199aaaa-0000-7000-8000-0000000000e1".to_string()),
    }
}
fn binding(strategy: InstallationStrategy) -> InstallSpec {
    let adopted = strategy == InstallationStrategy::ExternalAdoption;
    let isolated = matches!(
        strategy,
        InstallationStrategy::Pip
            | InstallationStrategy::Uv
            | InstallationStrategy::Conda
            | InstallationStrategy::Mamba
            | InstallationStrategy::PythonVenv
    );
    let locked = isolated || strategy == InstallationStrategy::Nuget;
    let destination = format!("runtimes/{RID}/{VID}/{}", "a".repeat(64));
    InstallSpec {
        request: request(strategy),
        runtime_version_id: VID.into(),
        adapter_plan_hash: template().plan_hash,
        adapter_installation_id: "0192aaaa-0000-7000-8000-000000000004".into(),
        adapter_package_hash: "b".repeat(64),
        registration_mode: if adopted {
            RegistrationMode::Adopted
        } else {
            RegistrationMode::Managed
        },
        ownership: if adopted {
            RuntimeOwnership::External
        } else {
            RuntimeOwnership::LarmManaged
        },
        relative_destination: (!adopted).then_some(destination.clone()),
        environment_prefix: isolated.then(|| format!("{destination}/env")),
        dependencies_lock_hash: locked.then(|| "c".repeat(64)),
        global_effects: false,
        reboot_required: false,
    }
}
fn template() -> Plan {
    // Estimate de fixture, sin atribuirle reserva física o cálculo real.
    let mut plan: Plan = serde_json::from_value(json!({"plan_id":"0192aaaa-0000-7000-8000-000000000005", "plan_hash":"0".repeat(64), "operation":"runtime.install", "resource_ids":[RID,VID,STORAGE], "steps":[{"phase":"ADAPTER_STEP","adapter_id":"fixture.runtime","request":{"profile_command_reference":"fixture-only-no-execution"}}], "expires_at":"2026-10-20T14:15:00.000Z", "approval_required":false,"storage_estimate":{"download_bytes":null,"final_artifact_bytes":null,"installed_bytes":null,"derived_cache_bytes":null,"temporary_peak_bytes":null,"existing_bytes":null,"peak_extra_bytes":null,"required_free_bytes":null,"upper_bound_bytes":null,"confidence":"UNKNOWN","reason":"fixture_only","evidence_refs":[],"per_volume":[]}})).unwrap();
    plan.plan_hash = plan_hash(&plan).unwrap();
    plan
}
fn plan(strategy: InstallationStrategy) -> Plan {
    let binding = binding(strategy);
    binding
        .bind(&binding.request, &[strategy], template(), "fixture.runtime")
        .unwrap()
}

#[test]
fn quince_estrategias_quedan_en_plan_y_roundtrip_sin_sustitucion() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/schemas/larm.schema.json"
    ))
    .unwrap();
    for name in schema["$defs"]["InstallRequest"]["properties"]["strategy"]["enum"]
        .as_array()
        .unwrap()
    {
        let strategy: InstallationStrategy = serde_json::from_value(name.clone()).unwrap();
        let original = template();
        let plan = plan(strategy);
        let restored: Plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(plan, restored);
        assert_eq!(spec(&restored).unwrap().request, request(strategy));
        assert_eq!(&plan.steps[..original.steps.len()], original.steps);
        assert_eq!(plan.storage_estimate, original.storage_estimate);
        assert_eq!(plan_hash(&plan).unwrap(), plan.plan_hash);
    }
}

#[test]
fn estrategia_no_anunciada_no_genera_plan_ni_fallback() {
    let binding = binding(InstallationStrategy::Uv);
    let error = binding
        .bind(
            &binding.request,
            &[InstallationStrategy::PythonVenv],
            template(),
            "fixture.runtime",
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedCapability);
}

#[test]
fn hash_original_del_adaptador_debe_coincidir_antes_de_sellar() {
    let mut binding = binding(InstallationStrategy::PrebuiltBinary);
    binding.adapter_plan_hash = "c".repeat(64);
    let error = binding
        .bind(
            &binding.request,
            &[InstallationStrategy::PrebuiltBinary],
            template(),
            "fixture.runtime",
        )
        .unwrap_err();
    assert_eq!(error.details.reason, "runtime_install_plan_invalid");
}

#[test]
fn version_se_anade_despues_de_validar_hash_original_del_adaptador() {
    let mut adapter_plan = template();
    adapter_plan.resource_ids.retain(|id| id != VID);
    adapter_plan.plan_hash = plan_hash(&adapter_plan).unwrap();
    let mut binding = binding(InstallationStrategy::PrebuiltBinary);
    binding.adapter_plan_hash = adapter_plan.plan_hash.clone();

    let sealed = binding
        .bind(
            &binding.request,
            &[InstallationStrategy::PrebuiltBinary],
            adapter_plan.clone(),
            "fixture.runtime",
        )
        .unwrap();

    assert!(sealed.resource_ids.iter().any(|id| id == VID));
    assert_eq!(
        spec(&sealed).unwrap().adapter_plan_hash,
        adapter_plan.plan_hash
    );
    assert_ne!(sealed.plan_hash, adapter_plan.plan_hash);
}

#[test]
fn entornos_uv_conda_mamba_y_pip_exigen_prefijo_propio_y_lock() {
    for strategy in [
        InstallationStrategy::Uv,
        InstallationStrategy::Conda,
        InstallationStrategy::Mamba,
        InstallationStrategy::Pip,
        InstallationStrategy::PythonVenv,
    ] {
        let good = binding(strategy);
        good.validate_for(&good.request).unwrap();
        for variant in 0..3 {
            let mut bad = good.clone();
            match variant {
                0 => bad.environment_prefix = None,
                1 => bad.environment_prefix = Some("base".into()),
                _ => bad.dependencies_lock_hash = None,
            }
            assert_eq!(
                bad.validate_for(&good.request).unwrap_err().details.reason,
                "runtime_isolated_environment_required"
            );
        }
    }
}

#[test]
fn nuget_exige_lock_sin_crear_entorno_python_y_sella_su_hash_en_el_plan() {
    let good = binding(InstallationStrategy::Nuget);
    assert!(good.environment_prefix.is_none());
    good.validate_for(&good.request).unwrap();

    let mut missing = good.clone();
    missing.dependencies_lock_hash = None;
    assert_eq!(
        missing
            .validate_for(&missing.request)
            .unwrap_err()
            .details
            .reason,
        "runtime_dependencies_lock_required"
    );

    let first = good
        .bind(
            &good.request,
            &[InstallationStrategy::Nuget],
            template(),
            "fixture.runtime",
        )
        .unwrap();
    let mut changed = good;
    changed.dependencies_lock_hash = Some("d".repeat(64));
    let second = changed
        .bind(
            &changed.request,
            &[InstallationStrategy::Nuget],
            template(),
            "fixture.runtime",
        )
        .unwrap();
    assert_ne!(first.plan_hash, second.plan_hash);
}

#[test]
fn destino_side_by_side_cambia_con_version_o_lock_y_rechaza_rutas_ajenas() {
    let good = binding(InstallationStrategy::PrebuiltBinary);
    for path in [
        "runtimes/latest",
        "../outside",
        "C:/global",
        "/absolute",
        "runtimes/old/stable",
    ] {
        let mut bad = good.clone();
        bad.relative_destination = Some(path.into());
        assert!(bad.validate_for(&good.request).is_err());
    }
    let mut changed = good.clone();
    changed.runtime_version_id = "0192aaaa-0000-7000-8000-000000000009".into();
    assert!(changed.validate_for(&good.request).is_err());
    let mut changed = good.clone();
    changed.request.profile_lock_hash = "f".repeat(64);
    assert!(changed.validate_for(&changed.request).is_err());
}

#[test]
fn adopcion_externa_no_crea_destino_ni_entorno_ni_efecto_global() {
    let good = binding(InstallationStrategy::ExternalAdoption);
    good.validate_for(&good.request).unwrap();
    for variant in 0..5 {
        let mut bad = good.clone();
        match variant {
            0 => bad.relative_destination = Some("owned".into()),
            1 => bad.environment_prefix = Some("env".into()),
            2 => bad.global_effects = true,
            3 => bad.reboot_required = true,
            _ => bad.ownership = RuntimeOwnership::LarmManaged,
        }
        assert_eq!(
            bad.validate_for(&good.request).unwrap_err().details.reason,
            "runtime_adoption_must_be_read_only"
        );
    }
}

#[test]
fn efecto_global_exige_aprobacion_exacta_actor_hash_codigo_recursos_y_caducidad() {
    let mut binding = binding(InstallationStrategy::MsiManaged);
    binding.global_effects = true;
    let plan = binding
        .bind(
            &binding.request,
            &[InstallationStrategy::MsiManaged],
            template(),
            "fixture.runtime",
        )
        .unwrap();
    assert!(plan.approval_required);
    assert_eq!(
        check_execution(&plan, &binding.request, "operator", None, now())
            .unwrap_err()
            .code,
        ErrorCode::ApprovalRequired
    );
    let approval = ApprovalRequest {
        operation_digest: plan.plan_hash.clone(),
        resource_ids: plan.resource_ids.clone(),
        code_hash: Some(binding.adapter_package_hash.clone()),
        permissions: Default::default(),
        approved: true,
    }
    .grant(
        "0192aaaa-0000-7000-8000-000000000006",
        "operator",
        now(),
        chrono::Duration::seconds(900),
    )
    .unwrap();
    check_execution(&plan, &binding.request, "operator", Some(&approval), now()).unwrap();
    assert_eq!(
        check_execution(&plan, &binding.request, "another", Some(&approval), now())
            .unwrap_err()
            .details
            .reason,
        "approval_actor_mismatch"
    );
    let mut used = approval.clone();
    used.consumed_at = Some("2026-10-20T14:00:00.000Z".into());
    assert_eq!(
        check_execution(&plan, &binding.request, "operator", Some(&used), now())
            .unwrap_err()
            .details
            .reason,
        "approval_consumed"
    );
    let mut wrong = approval.clone();
    wrong.operation_digest = "f".repeat(64);
    assert_eq!(
        check_execution(&plan, &binding.request, "operator", Some(&wrong), now())
            .unwrap_err()
            .details
            .reason,
        "approval_digest_mismatch"
    );
    let mut wrong = approval.clone();
    wrong.code_hash = None;
    assert_eq!(
        check_execution(&plan, &binding.request, "operator", Some(&wrong), now())
            .unwrap_err()
            .details
            .reason,
        "approval_code_hash_mismatch"
    );
    let mut wrong = approval;
    wrong.resource_ids.clear();
    assert_eq!(
        check_execution(&plan, &binding.request, "operator", Some(&wrong), now())
            .unwrap_err()
            .details
            .reason,
        "approval_resources_mismatch"
    );
}

#[test]
fn plan_manipulado_vencido_o_biding_ambiguo_no_se_acepta() {
    let request = request(InstallationStrategy::PrebuiltBinary);
    let original = plan(request.strategy);
    let mut changed = original.clone();
    changed.steps[0]
        .request
        .insert("different".into(), json!(true));
    assert_eq!(
        check_execution(&changed, &request, "operator", None, now())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_plan_hash_mismatch"
    );
    assert_eq!(
        check_execution(
            &original,
            &request,
            "operator",
            None,
            now() + chrono::Duration::seconds(900)
        )
        .unwrap_err()
        .details
        .reason,
        "runtime_install_plan_expired"
    );
    let mut ambiguous = original.clone();
    ambiguous.steps.push(original.steps.last().unwrap().clone());
    assert_eq!(
        spec(&ambiguous).unwrap_err().details.reason,
        "runtime_install_spec_ambiguous"
    );
    let mut different = request.clone();
    different.upstream_version = "another build".into();
    assert_eq!(
        check_execution(&original, &different, "operator", None, now())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_request_mismatch"
    );
}

#[test]
fn planificar_no_inventa_espacio_permisos_comandos_o_efectos() {
    let plan = plan(InstallationStrategy::PrebuiltBinary);
    assert_eq!(plan.storage_estimate, template().storage_estimate);
    assert_eq!(plan.steps[0], template().steps[0]);
    assert!(!plan.approval_required);
    let mut reboot = binding(InstallationStrategy::Winget);
    reboot.reboot_required = true;
    let reviewed = reboot
        .bind(
            &reboot.request,
            &[InstallationStrategy::Winget],
            template(),
            "fixture.runtime",
        )
        .unwrap();
    assert!(reviewed.approval_required);
}

#[test]
fn un_plan_resellado_no_puede_ocultar_efecto_global_ni_perder_recursos() {
    let mut binding = binding(InstallationStrategy::Winget);
    binding.global_effects = true;
    let mut hidden = binding
        .bind(
            &binding.request,
            &[InstallationStrategy::Winget],
            template(),
            "fixture.runtime",
        )
        .unwrap();
    hidden.approval_required = false;
    hidden.plan_hash = plan_hash(&hidden).unwrap();
    assert_eq!(
        check_execution(&hidden, &binding.request, "operator", None, now())
            .unwrap_err()
            .details
            .reason,
        "runtime_install_approval_flag_mismatch"
    );
    let good = plan(InstallationStrategy::PrebuiltBinary);
    for variant in 0..3 {
        let mut bad = good.clone();
        match variant {
            0 => bad.plan_id = "not-an-id".into(),
            1 => bad.resource_ids.retain(|id| id != VID),
            _ => bad.expires_at = "2026-10-20T14:15:00Z".into(),
        }
        bad.plan_hash = plan_hash(&bad).unwrap();
        assert!(check_execution(
            &bad,
            &request(InstallationStrategy::PrebuiltBinary),
            "operator",
            None,
            now()
        )
        .is_err());
    }
}
