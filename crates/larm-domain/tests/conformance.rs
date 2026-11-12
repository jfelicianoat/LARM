//! Invariantes de evidencia, sin simular una ejecución de adaptador.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::conformance::{BindingHashes, Observation, Outcome, Proof, Run};
use larm_domain::error::ErrorCode;
use serde_json::json;

fn installed() -> AdapterInstallation {
    AdapterInstallation {
        id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-10-18T00:00:00.000Z".into(),
        updated_at: "2026-10-18T00:00:00.000Z".into(),
        row_version: 1,
        metadata: json!({"fixture_only":true}).as_object().unwrap().clone(),
        adapter_id: "fixture.transport".into(),
        version: "1.0.0".into(),
        family: "runtime".into(),
        package_hash: "a".repeat(64),
        publisher: "unit fixture".into(),
        trust: AdapterTrust::Untrusted,
        state: AdapterState::Installed,
        manifest: json!({"id":"fixture.transport","version":"1.0.0","family":"runtime","package_sha256":"a".repeat(64),"publisher":"unit fixture","capabilities":["vendor.reference.echo"],"permissions":{"read_roots":[]}}),
    }
}
fn bindings() -> BindingHashes {
    BindingHashes {
        suite_sha256: "b".repeat(64),
        capability_profile_sha256: "c".repeat(64),
        execution_context_sha256: "d".repeat(64),
    }
}
fn checks(run: &Run) -> Vec<Observation> {
    run.required_checks()
        .into_iter()
        .map(|id| Observation {
            check_id: id,
            evidence_sha256: "e".repeat(64),
            passed: true,
        })
        .collect()
}

#[test]
fn cobertura_exacta_por_familia_y_capacidad_sin_dependencia_del_orden() {
    for family in [
        "runtime",
        "source",
        "protocol",
        "transformation",
        "benchmark",
    ] {
        let mut a = installed();
        a.family = family.into();
        a.manifest["family"] = json!(family);
        let run = Run::new(&a, bindings(), "unit fixture actor").unwrap();
        let first = Proof::evaluate(&run, checks(&run)).unwrap();
        let mut reverse = checks(&run);
        reverse.reverse();
        let second = Proof::evaluate(&run, reverse).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.digest().unwrap(), second.digest().unwrap());
        assert!(run.required_checks().contains(&format!("family:{family}")));
        assert!(run
            .required_checks()
            .contains("capability:vendor.reference.echo"));
        assert_eq!(run.required_checks().len(), 17);
    }
}
#[test]
fn no_pasa_si_falta_falla_se_repite_o_sobra_un_check_o_su_evidencia() {
    let run = Run::new(&installed(), bindings(), "unit fixture actor").unwrap();
    for case in 0..5 {
        let mut observations = checks(&run);
        match case {
            0 => {
                observations.pop();
            }
            1 => observations[0].passed = false,
            2 => observations.push(observations[0].clone()),
            3 => observations.push(Observation {
                check_id: "invented".into(),
                evidence_sha256: "f".repeat(64),
                passed: true,
            }),
            _ => observations[0].evidence_sha256 = "not-a-sha".into(),
        }
        assert!(Proof::evaluate(&run, observations).is_err());
    }
}
#[test]
fn binding_incluye_paquete_manifiesto_permisos_metadata_y_contexto() {
    let original = installed();
    let run = Run::new(&original, bindings(), "unit fixture actor").unwrap();
    for case in 0..5 {
        let mut changed = original.clone();
        match case {
            0 => changed.package_hash = "f".repeat(64),
            1 => changed.manifest["protocol_version"] = json!("2.0"),
            2 => changed.manifest["permissions"]["read_roots"] = json!(["other"]),
            3 => changed
                .metadata
                .insert("unexpected".into(), json!(true))
                .map(|_| ())
                .unwrap_or(()),
            _ => changed.row_version += 1,
        }
        assert!(!run.matches(&changed).unwrap());
    }
    let proof = Proof::evaluate(&run, checks(&run)).unwrap();
    let mut other = run.clone();
    other.bindings.execution_context_sha256 = "e".repeat(64);
    assert!(proof.check_run(&other).is_err());
    other = run.clone();
    other.id = uuid::Uuid::now_v7().to_string();
    assert!(proof.check_run(&other).is_err());
    other = run.clone();
    other.actor_id = "other actor".into();
    assert!(proof.check_run(&other).is_err());
}
#[test]
fn revisiones_grandes_son_exactas_y_no_hay_overflow_ni_ceros_iniciales() {
    let mut a = installed();
    a.row_version = i64::MAX - 1;
    let mut run = Run::new(&a, bindings(), "unit fixture actor").unwrap();
    assert!(run.digest().is_ok());
    assert!(run.matches(&a).unwrap());
    a.row_version = i64::MAX;
    assert!(Run::new(&a, bindings(), "unit fixture actor").is_err());
    run.installation_revision = "01".into();
    assert!(run.validate().is_err());
}
#[test]
fn manifiesto_ambiguo_alias_y_estados_no_ejecutables_se_rechazan() {
    for case in 0..5 {
        let mut a = installed();
        match case {
            0 => a.manifest["publisher"] = json!("another publisher"),
            1 => {
                a.manifest["capabilities"] =
                    json!(["vendor.reference.echo", "vendor.reference.echo"])
            }
            2 => a.manifest["capabilities"] = json!(["embedding"]),
            3 => a.trust = AdapterTrust::Revoked,
            _ => a.state = AdapterState::Retired,
        }
        assert!(Run::new(&a, bindings(), "unit fixture actor").is_err());
    }
}
#[test]
fn fallo_no_persiste_texto_arbitrario_del_plugin() {
    let run = Run::new(&installed(), bindings(), "unit fixture actor").unwrap();
    let outcome = Outcome::failed(&ErrorCode::Other("private-marker".into()));
    outcome.check_run(&run).unwrap();
    let value = serde_json::to_string(&outcome).unwrap();
    assert!(!value.contains("private-marker"));
    assert!(value.contains("INTERNAL"));
    assert!(Outcome::Failed {
        error_code: "private-marker".into()
    }
    .check_run(&run)
    .is_err());
}
