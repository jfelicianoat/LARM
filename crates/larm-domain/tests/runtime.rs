//! P030: oráculos de dominio. Ningún test declara un backend operable por nombre.
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_domain::error::ErrorCode;
use larm_domain::runtime::*;
use serde_json::{json, Value};

const AT: &str = "2026-10-20T13:00:00.000Z";
fn uid() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn entity(fields: Value) -> Value {
    let mut base =
        json!({"id":uid(),"created_at":AT,"updated_at":AT,"row_version":1,"metadata":{}});
    base.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    base
}
fn version() -> Value {
    entity(
        json!({"runtime_id":uid(),"version_string":"b7142-avx2-custom+local",
        "commit":null,"build":null,"channel":"stable","released_at":null,
        "artifact_hash":null,"promotion":"NONE","lifecycle":"CANDIDATE"}),
    )
}
fn installation() -> Value {
    entity(
        json!({"runtime_version_id":uid(),"storage_location_id":null,"relative_path":null,
        "strategy":"EXTERNAL_ADOPTION","external_host_id":uid(),"execution_host_id":null,"state":"DISCOVERED",
        "ownership":"EXTERNAL","lock_hash":null,"stage":"DISCOVERED","registration_mode":"ADOPTED"}),
    )
}
fn backend() -> Value {
    entity(
        json!({"runtime_installation_id":uid(),"backend_type":"vendor.future.accelerator/v9",
        "device_id":null,"driver_version":null,"status":"UNKNOWN","evidence_refs":[],
        "limits":{"max_instances":1,"multi_model_support":false,"parallel_requests":1,"gpu_sharing":false}}),
    )
}
fn host() -> Value {
    entity(
        json!({"name":"Host separado del engine","base_url":"http://127.0.0.1:23231",
        "host_type":"vendor.open.host","credential_reference_id":null,"state":"UNKNOWN",
        "ownership":"EXTERNAL","observed_version":null,"registration_mode":"ADOPTED"}),
    )
}

#[test]
fn cinco_canales_distintos_sin_semver_y_roundtrip_portable() {
    for name in ["stable", "prerelease", "nightly", "commit", "custom"] {
        let mut document = version();
        document["channel"] = json!(name);
        let value: RuntimeVersion = serde_json::from_value(document.clone()).unwrap();
        value.validate().unwrap();
        assert_eq!(serde_json::to_value(&value).unwrap(), document);
        let policy = RuntimeChannelPolicy {
            allowed_channels: vec![value.channel],
        };
        policy.validate().unwrap();
        for candidate in ["stable", "prerelease", "nightly", "commit", "custom"] {
            let observed = ObservedChannel::from_upstream(&candidate.to_uppercase());
            assert_eq!(
                observed.automatic_selection_allowed(&policy),
                candidate == name
            );
        }
    }
}

#[test]
fn politica_cerrada_sin_vacios_duplicados_o_canales_desconocidos() {
    assert_eq!(
        serde_json::to_value(RuntimeChannelPolicy::default()).unwrap(),
        json!({"allowed_channels":["stable"]})
    );
    for raw in [
        json!({"allowed_channels":[]}),
        json!({"allowed_channels":["stable","stable"]}),
    ] {
        assert!(serde_json::from_value::<RuntimeChannelPolicy>(raw)
            .unwrap()
            .validate()
            .is_err());
    }
    for raw in [
        json!({"allowed_channels":["experimental"]}),
        json!({"allowed_channels":["STABLE"]}),
        json!({"allowed_channels":["stable"],"fallback":true}),
        json!({}),
    ] {
        assert!(serde_json::from_value::<RuntimeChannelPolicy>(raw).is_err());
    }
}

#[test]
fn canal_desconocido_conserva_original_unknown_y_no_se_autopromueve() {
    let policy = RuntimeChannelPolicy {
        allowed_channels: vec![RuntimeChannel::Custom, RuntimeChannel::Stable],
    };
    for raw in ["experimental", "release/vendor-42", "", " stable "] {
        let observed = ObservedChannel::from_upstream(raw);
        assert_eq!(observed.channel, RuntimeChannel::Custom);
        assert_eq!(observed.upstream_value, raw);
        assert_eq!(observed.reason, Some("UNKNOWN"));
        assert!(!observed.automatic_selection_allowed(&policy));
        let mut portable = version();
        portable["channel"] = json!(observed.channel);
        portable["metadata"]["upstream_channel"] = observed.metadata();
        let value: RuntimeVersion = serde_json::from_value(portable.clone()).unwrap();
        value.validate().unwrap();
        assert_eq!(value.observed_channel().unwrap(), observed);
        assert_eq!(serde_json::to_value(value).unwrap(), portable);
        assert!(serde_json::from_value::<RuntimeChannel>(json!(raw)).is_err());
    }
    assert!(ObservedChannel::from_upstream("custom").automatic_selection_allowed(&policy));
}

#[test]
fn canal_unknown_del_adaptador_se_normaliza_y_metadata_contradictoria_se_rechaza() {
    let mut legacy = version();
    legacy["channel"] = json!("custom");
    legacy["metadata"]["channel_reason"] = json!("UNKNOWN");
    let mut value: RuntimeVersion = serde_json::from_value(legacy).unwrap();
    let observed = value.observed_channel().unwrap();
    assert_eq!(observed.channel, RuntimeChannel::Custom);
    assert_eq!(observed.upstream_value, "");
    assert_eq!(observed.reason, Some("UNKNOWN"));
    value.normalize_observed_channel_metadata().unwrap();
    assert_eq!(
        value.metadata["upstream_channel"],
        json!({"value":"", "reason":"UNKNOWN"})
    );
    assert!(!value
        .observed_channel()
        .unwrap()
        .automatic_selection_allowed(&RuntimeChannelPolicy {
            allowed_channels: vec![RuntimeChannel::Custom],
        }));

    let mut stable_claim = version();
    stable_claim["metadata"]["upstream_channel"] =
        json!({"value":"experimental/vendor-42", "reason":"UNKNOWN"});
    let stable_claim: RuntimeVersion = serde_json::from_value(stable_claim).unwrap();
    assert_eq!(
        stable_claim.observed_channel().unwrap_err().details.reason,
        "runtime_upstream_channel_mismatch"
    );

    let mut recognized_as_unknown = version();
    recognized_as_unknown["channel"] = json!("custom");
    recognized_as_unknown["metadata"]["upstream_channel"] =
        json!({"value":"stable", "reason":"UNKNOWN"});
    let recognized_as_unknown: RuntimeVersion =
        serde_json::from_value(recognized_as_unknown).unwrap();
    assert_eq!(
        recognized_as_unknown
            .observed_channel()
            .unwrap_err()
            .details
            .reason,
        "runtime_upstream_channel_mismatch"
    );
}

#[test]
fn providers_abiertos_y_host_separado_sin_heredar_estado_o_propiedad() {
    let raw = backend();
    let mut value: RuntimeBackend = serde_json::from_value(raw.clone()).unwrap();
    value.validate().unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), raw);
    assert_eq!(value.status, BackendStatus::Unknown);
    value.limits.max_instances = 0;
    assert!(value.validate().is_err());
    value.limits.max_instances = 1;
    value.limits.parallel_requests = 0;
    assert!(value.validate().is_err());
    let raw = host();
    let value: ExternalHost = serde_json::from_value(raw.clone()).unwrap();
    value.validate().unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), raw);
    assert_eq!(value.ownership, ExternalOwnership::External);
    assert_eq!(value.registration_mode, RegistrationMode::Adopted);
    assert_eq!(value.state, ExternalHostState::Unknown);
    let raw = installation();
    let value: RuntimeInstallation = serde_json::from_value(raw.clone()).unwrap();
    value.validate().unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), raw);
    assert_eq!(value.ownership, RuntimeOwnership::External);
    assert_eq!(value.registration_mode, RegistrationMode::Adopted);
    assert_eq!(value.stage, InstallationStage::Discovered);
}

#[test]
fn dtos_cerrados_y_todos_los_nullable_obligatorios() {
    for field in ["commit", "build", "released_at", "artifact_hash"] {
        let mut raw = version();
        raw.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RuntimeVersion>(raw).is_err(),
            "{field}"
        );
    }
    for field in [
        "storage_location_id",
        "relative_path",
        "external_host_id",
        "execution_host_id",
        "lock_hash",
    ] {
        let mut raw = installation();
        raw.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RuntimeInstallation>(raw).is_err(),
            "{field}"
        );
    }
    for field in ["device_id", "driver_version"] {
        let mut raw = backend();
        raw.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RuntimeBackend>(raw).is_err(),
            "{field}"
        );
    }
    for field in ["credential_reference_id", "observed_version"] {
        let mut raw = host();
        raw.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ExternalHost>(raw).is_err(),
            "{field}"
        );
    }
    let mut raw = version();
    raw["verified"] = json!(true);
    assert!(serde_json::from_value::<RuntimeVersion>(raw).is_err());
    let mut raw = backend();
    raw["provider"] = json!("cpu");
    assert!(serde_json::from_value::<RuntimeBackend>(raw).is_err());
    let mut raw = host();
    raw["ownership"] = json!("LARM_MANAGED");
    assert!(serde_json::from_value::<ExternalHost>(raw).is_err());
    let mut raw = installation();
    raw["ownership"] = json!("HOST_MANAGED");
    assert!(serde_json::from_value::<RuntimeInstallation>(raw).is_err());
}

#[test]
fn validacion_rechaza_identidades_fechas_hashes_y_rutas_invalidas() {
    for (field, bad) in [
        ("id", json!("uuid")),
        ("row_version", json!(0)),
        ("version_string", json!("")),
        ("commit", json!("")),
        ("build", json!("")),
        ("artifact_hash", json!("A".repeat(64))),
        ("created_at", json!("2026-02-30T00:00:00.000Z")),
        ("released_at", json!("2026-10-20T13:00:00Z")),
        ("updated_at", json!("2026-10-20T13:00:00.000+00:00")),
    ] {
        let mut raw = version();
        raw[field] = bad;
        assert!(
            serde_json::from_value::<RuntimeVersion>(raw)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
    for path in ["../outside", "C:/absolute", "foo:ads", "safe/../outside"] {
        let mut raw = installation();
        raw["relative_path"] = json!(path);
        assert!(serde_json::from_value::<RuntimeInstallation>(raw)
            .unwrap()
            .validate()
            .is_err());
    }
    let mut raw = installation();
    raw["state"] = json!("ACTIVE");
    assert!(serde_json::from_value::<RuntimeInstallation>(raw)
        .unwrap()
        .validate()
        .is_err());
}

#[test]
fn transiciones_secuenciales_y_retiro_protegido_sin_saltar_validacion() {
    use InstallationStage::*;
    let path = [
        Discovered,
        InstallPlanned,
        Installing,
        Installed,
        Validating,
        Validated,
    ];
    for pair in path.windows(2) {
        pair[0].transition(pair[1], false).unwrap();
    }
    for (from, to) in [
        (Discovered, Validated),
        (Installing, Validated),
        (Installed, Discovered),
        (Failed, Validated),
        (Retired, InstallPlanned),
        (Validated, Failed),
    ] {
        assert_eq!(
            from.transition(to, false).unwrap_err().code,
            ErrorCode::InvalidStateTransition
        );
    }
    Installing.transition(Failed, false).unwrap();
    assert_eq!(
        Validated.transition(Retired, true).unwrap_err().code,
        ErrorCode::ActiveLease
    );
    Validated.transition(Retired, false).unwrap();
}

#[test]
fn estado_publico_conserva_stage_y_prioriza_hechos_observados() {
    let facts = |failed, operable, active, rolled_back| InstallationActivity {
        failed_instances: failed,
        operable_instances: operable,
        active_instances: active,
        rolled_back,
    };
    let state = |x| InstallationState::observed(InstallationStage::Validated, x).unwrap();
    assert_eq!(state(facts(1, 0, 0, true)), InstallationState::Broken);
    assert_eq!(state(facts(1, 1, 1, true)), InstallationState::Degraded);
    assert_eq!(state(facts(0, 1, 1, true)), InstallationState::RolledBack);
    assert_eq!(state(facts(0, 1, 1, false)), InstallationState::Active);
    assert_eq!(state(facts(0, 0, 0, false)), InstallationState::Validated);
    assert_eq!(
        InstallationState::observed(InstallationStage::Installing, facts(0, 0, 0, false)).unwrap(),
        InstallationState::Installing
    );
    assert!(
        InstallationState::observed(InstallationStage::Validated, facts(0, 0, 1, false)).is_err()
    );
    let mut raw = installation();
    raw["stage"] = json!("VALIDATED");
    for public in ["VALIDATED", "ACTIVE", "DEGRADED", "BROKEN", "ROLLED_BACK"] {
        raw["state"] = json!(public);
        serde_json::from_value::<RuntimeInstallation>(raw.clone())
            .unwrap()
            .validate()
            .unwrap();
    }
}

#[test]
fn estrategias_distintas_y_alias_planned_solo_se_escribe_canonico() {
    let names = [
        "PREBUILT_BINARY",
        "GITHUB_RELEASE",
        "SOURCE_BUILD",
        "WINGET",
        "PIP",
        "UV",
        "CONDA",
        "MAMBA",
        "NUGET",
        "WSL",
        "CONTAINER",
        "CUSTOM",
        "MSI_MANAGED",
        "PYTHON_VENV",
        "EXTERNAL_ADOPTION",
    ];
    let values: std::collections::BTreeSet<InstallationStrategy> = names
        .iter()
        .map(|name| serde_json::from_value(json!(name)).unwrap())
        .collect();
    assert_eq!(values.len(), 15);
    for value in values {
        assert!(names.contains(&value.as_str()));
        assert_eq!(serde_json::to_value(value).unwrap(), json!(value.as_str()));
    }
    assert!(serde_json::from_value::<InstallationStrategy>(json!("ARCHIVE")).is_err());
    let mut raw = installation();
    raw["state"] = json!("PLANNED");
    raw["stage"] = json!("PLANNED");
    let value: RuntimeInstallation = serde_json::from_value(raw).unwrap();
    value.validate().unwrap();
    let canonical = serde_json::to_value(value).unwrap();
    assert_eq!(canonical["stage"], "INSTALL_PLANNED");
    assert_eq!(canonical["state"], "INSTALL_PLANNED");
}

#[test]
fn runtime_create_exacto_y_binding_es_una_referencia_no_el_plugin_id() {
    let raw = json!({"name":"llama.cpp","adapter_id":uid(),"capabilities":["vendor.future"],"default_version_id":null,"channel_policy":{"allowed_channels":["nightly","commit"]},"retention_policy":{"keep_last_validated":3}});
    let create: RuntimeCreate = serde_json::from_value(raw.clone()).unwrap();
    create.validate().unwrap();
    let full = entity(raw.clone());
    let runtime: Runtime = serde_json::from_value(full.clone()).unwrap();
    runtime.validate().unwrap();
    assert_eq!(runtime.configuration(), create);
    assert_eq!(serde_json::to_value(runtime).unwrap(), full);
    let mut missing = raw.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("default_version_id");
    assert!(serde_json::from_value::<RuntimeCreate>(missing).is_err());
    let mut bad = raw;
    bad["adapter_id"] = json!("reference.runtime");
    assert!(serde_json::from_value::<RuntimeCreate>(bad)
        .unwrap()
        .validate()
        .is_err());
    let binding:RuntimeAdapter=serde_json::from_value(entity(json!({"adapter_installation_id":uid(),"runtime_family":"vendor.engine","capabilities":["vendor.future"]}))).unwrap();
    binding.validate().unwrap();
    assert_ne!(binding.id, binding.adapter_installation_id);
}
