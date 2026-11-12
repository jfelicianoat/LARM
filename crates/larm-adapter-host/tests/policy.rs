//! Permisos, firma y confianza (P022) con procesos reales: el adaptador falso
//! en un paquete temporal con su `package_sha256` real, firmado o no.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration as Ttl, Utc};
use ed25519_dalek::SigningKey;
use larm_adapter_host::policy::{
    ElevationProbe, GuardedWorker, Policy, TrustedOnlyGrant, UpgradeDecision, WhoAmI, MANIFEST_FILE,
};
use larm_adapter_host::worker::{HostConfig, TreeKill, WorkerSpec, WorkerState};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{
    adapter_resource, elevation_digest, execution_digest, package_hash, sign, Access,
    PermissionDelta, PinnedKey, RootMap, Trust, TrustStore,
};
use larm_domain::approval::{ApprovalRecord, ApprovalRequest};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::{json, Value};

const PUBLISHER: &str = "LARM (pruebas)";
const ACTOR: &str = "user:operador";

fn fake_exe() -> PathBuf {
    let exe =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/examples/fake_adapter.exe");
    assert!(
        exe.exists(),
        "falta {} (lo compila cargo test)",
        exe.display()
    );
    exe
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn store() -> TrustStore {
    TrustStore {
        keys: vec![PinnedKey {
            key_id: "pruebas-2026".into(),
            publisher: PUBLISHER.into(),
            public_key: hex::encode(key().verifying_key().to_bytes()),
            official: true,
            revoked: false,
        }],
        revoked_packages: vec![],
    }
}

struct NotElevated;
impl ElevationProbe for NotElevated {
    fn elevated(&self) -> Result<bool, LarmError> {
        Ok(false)
    }
}
struct Elevated;
impl ElevationProbe for Elevated {
    fn elevated(&self) -> Result<bool, LarmError> {
        Ok(true)
    }
}

struct Package {
    dir: tempfile::TempDir,
    pkg: PathBuf,
    manifest: AdapterManifest,
}

impl Package {
    fn models(&self) -> PathBuf {
        self.dir.path().join("models")
    }

    fn spec(&self) -> WorkerSpec {
        WorkerSpec {
            package_dir: self.pkg.clone(),
            manifest: self.manifest.clone(),
            environment: vec![],
            work_dir: self.dir.path().join("work"),
        }
    }

    fn policy(&self, trust: TrustStore, elevation: Arc<dyn ElevationProbe>) -> Policy {
        Policy {
            trust,
            roots: RootMap(vec![("$models".into(), self.models())]),
            elevation,
        }
    }
}

/// Paquete real con permisos de lectura en `$models`, un origen y un secreto
/// declarado; `signed` lo firma con la clave fijada.
fn package(signed: bool) -> Package {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(dir.path().join("models").join("m.gguf"), b"GGUF").unwrap();
    std::fs::write(dir.path().join("fuera.txt"), b"x").unwrap();
    std::fs::copy(fake_exe(), pkg.join("bin").join("fake_adapter.exe")).unwrap();
    let manifest_path = pkg.join(MANIFEST_FILE);
    let mut m: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/fake_adapter/manifest.json"
    ))
    .unwrap();
    m["executable"]["args"] = json!([
        "--manifest",
        manifest_path.display().to_string(),
        "--mode",
        "ok"
    ]);
    m["permissions"] = json!({
        "read_roots": ["$models"], "write_roots": [], "network_origins": ["https://huggingface.co"],
        "process_allowlist": [], "secret_refs": ["larm/source/hf"]
    });
    m["package_sha256"] = json!(package_hash(&pkg, &[MANIFEST_FILE]).unwrap());
    let mut manifest = AdapterManifest::parse(&m.to_string()).unwrap();
    if signed {
        manifest.signature = Some(sign(&manifest, "pruebas-2026", &key()).unwrap());
    }
    // El fake adapter lee este fichero para el handshake.
    std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
    Package { dir, pkg, manifest }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn approval(digest: &str, resource: &str, code_hash: &str, permissions: Value) -> ApprovalRecord {
    ApprovalRequest {
        operation_digest: digest.into(),
        resource_ids: vec![resource.into()],
        code_hash: Some(code_hash.into()),
        permissions: permissions.as_object().cloned().unwrap_or_default(),
        approved: true,
    }
    .grant("appr-1", ACTOR, now(), Ttl::seconds(900))
    .unwrap()
}

fn execution_approval(m: &AdapterManifest, extra: Value) -> ApprovalRecord {
    let mut p = json!({
        "publisher": m.publisher,
        "permissions": serde_json::to_value(&m.permissions).unwrap(),
        "expires_at": (now() + Ttl::days(30)).to_rfc3339(),
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    approval(
        &execution_digest(m).unwrap(),
        &adapter_resource(m),
        &m.package_sha256,
        p,
    )
}

fn reason(e: &LarmError) -> &str {
    &e.details.reason
}

fn fast() -> HostConfig {
    HostConfig {
        handshake_timeout: std::time::Duration::from_secs(5),
        ..HostConfig::default()
    }
}

#[tokio::test]
async fn permiso_denegado_en_cada_operacion() {
    let p = package(true);
    let policy = p.policy(store(), Arc::new(NotElevated));
    let auth = policy
        .authorize(&p.pkg, &p.manifest, false, None, now())
        .unwrap();
    assert_eq!(auth.assessment.trust, Trust::Verified);
    assert!(auth.approval_id.is_none());
    let w = GuardedWorker::start(p.spec(), &auth, fast(), 1, Arc::new(TreeKill))
        .await
        .unwrap();
    assert_eq!(w.state(), WorkerState::Ready);

    // Dentro de los permisos: llega al adaptador.
    let ok = Access {
        reads: vec![p.models().join("m.gguf")],
        urls: vec!["https://huggingface.co/api/models".into()],
        secrets: vec!["larm/source/hf".into()],
        ..Access::default()
    };
    w.call("benchmark.describe", json!({}), &ok, None, None)
        .await
        .unwrap();

    // Fuera: FORBIDDEN sin enviar nada; el worker sigue sano.
    let cases = [
        (
            Access {
                reads: vec![p.dir.path().join("fuera.txt")],
                ..Access::default()
            },
            "read_denied",
        ),
        (
            Access {
                reads: vec![p.models().join("..").join("fuera.txt")],
                ..Access::default()
            },
            "read_denied",
        ),
        (
            Access {
                writes: vec![p.models().join("nuevo.bin")],
                ..Access::default()
            },
            "write_denied",
        ),
        (
            Access {
                urls: vec!["https://huggingface.co.evil.example/".into()],
                ..Access::default()
            },
            "network_denied",
        ),
        (
            Access {
                processes: vec!["C:/Windows/System32/cmd.exe".into()],
                ..Access::default()
            },
            "process_denied",
        ),
        (
            Access {
                secrets: vec!["larm/source/github".into()],
                ..Access::default()
            },
            "secret_denied",
        ),
    ];
    for (access, expected) in cases {
        let e = w
            .call("benchmark.describe", json!({}), &access, None, None)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Forbidden);
        assert_eq!(reason(&e), expected);
    }
    assert_eq!(w.state(), WorkerState::Ready);
    w.stop().await;

    // Un manifiesto distinto del autorizado no arranca.
    let mut other = p.spec();
    other
        .manifest
        .permissions
        .network_origins
        .push("https://evil.example".into());
    let e = GuardedWorker::start(other, &auth, fast(), 2, Arc::new(TreeKill))
        .await
        .err()
        .unwrap();
    assert_eq!(reason(&e), "authorization_mismatch");
}

#[tokio::test]
async fn sin_firma_solo_con_concesion_trusted_only() {
    let p = package(false);
    let policy = p.policy(store(), Arc::new(NotElevated));

    // Sin concesión: APPROVAL_REQUIRED, nada se lanza.
    let e = policy
        .authorize(&p.pkg, &p.manifest, false, None, now())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ApprovalRequired);
    assert_eq!(reason(&e), "untrusted_requires_approval");
    let e = policy
        .authorize(&p.pkg, &p.manifest, true, None, now())
        .unwrap_err();
    assert_eq!(
        reason(&e),
        "untrusted_requires_approval",
        "CUSTOM tampoco se ejecuta sin aprobación"
    );

    // Aprobaciones que no cubren exactamente esto.
    let m = &p.manifest;
    let wrong = [
        (
            execution_approval(m, json!({"publisher": "Otro"})),
            "approval_publisher_mismatch",
        ),
        (
            execution_approval(
                m,
                json!({"permissions": {"read_roots": [], "write_roots": [], "network_origins": [], "process_allowlist": [], "secret_refs": []}}),
            ),
            "approval_permissions_mismatch",
        ),
        (
            execution_approval(
                m,
                json!({"expires_at": (now() - Ttl::minutes(1)).to_rfc3339()}),
            ),
            "approval_expires_at_invalid",
        ),
        (
            execution_approval(m, json!({"secret_refs": ["larm/source/github"]})),
            "approval_secret_not_declared",
        ),
        (
            approval(
                &execution_digest(m).unwrap(),
                &adapter_resource(m),
                &"b".repeat(64),
                json!({}),
            ),
            "approval_code_hash_mismatch",
        ),
        (
            approval(
                &"c".repeat(64),
                &adapter_resource(m),
                &m.package_sha256,
                json!({}),
            ),
            "approval_digest_mismatch",
        ),
    ];
    for (record, expected) in wrong {
        let e = TrustedOnlyGrant::from_approval(&record, m, ACTOR, now()).unwrap_err();
        assert_eq!(e.code, ErrorCode::ApprovalRequired);
        assert_eq!(reason(&e), expected);
    }
    let e =
        TrustedOnlyGrant::from_approval(&execution_approval(m, json!({})), m, "user:otro", now())
            .unwrap_err();
    assert_eq!(reason(&e), "approval_actor_mismatch");

    // Concesión correcta sin secretos: ejecuta, pero sin el secreto declarado.
    let grant = TrustedOnlyGrant::from_approval(&execution_approval(m, json!({})), m, ACTOR, now())
        .unwrap();
    let auth = policy
        .authorize(&p.pkg, m, false, Some(&grant), now())
        .unwrap();
    assert_eq!(auth.assessment.trust, Trust::Untrusted);
    assert_eq!(auth.approval_id.as_deref(), Some("appr-1"));
    assert!(
        auth.grants.secret_refs.is_empty(),
        "UNTRUSTED no recibe secret_refs por defecto"
    );
    let w = GuardedWorker::start(p.spec(), &auth, fast(), 1, Arc::new(TreeKill))
        .await
        .unwrap();
    let secret = Access {
        secrets: vec!["larm/source/hf".into()],
        ..Access::default()
    };
    assert_eq!(
        reason(
            &w.call("benchmark.describe", json!({}), &secret, None, None)
                .await
                .unwrap_err()
        ),
        "secret_denied"
    );
    w.call(
        "benchmark.describe",
        json!({}),
        &Access::default(),
        None,
        None,
    )
    .await
    .unwrap();
    w.stop().await;

    // Concesión explícita del secreto.
    let grant_s = TrustedOnlyGrant::from_approval(
        &execution_approval(m, json!({"secret_refs": ["larm/source/hf"]})),
        m,
        ACTOR,
        now(),
    )
    .unwrap();
    let auth = policy
        .authorize(&p.pkg, m, false, Some(&grant_s), now())
        .unwrap();
    assert!(auth.grants.secret_refs.contains("larm/source/hf"));

    // La concesión caduca y no vale para otra revisión del paquete.
    let e = policy
        .authorize(&p.pkg, m, false, Some(&grant), now() + Ttl::days(31))
        .unwrap_err();
    assert_eq!(reason(&e), "grant_expired");
    let mut next = m.clone();
    next.version = "0.2.0".into();
    assert_eq!(
        reason(
            &policy
                .authorize(&p.pkg, &next, false, Some(&grant), now())
                .unwrap_err()
        ),
        "grant_mismatch"
    );

    // Paquete manipulado tras aprobar: el hash ya no coincide.
    std::fs::write(p.pkg.join("bin").join("extra.dll"), b"MZ").unwrap();
    let e = policy
        .authorize(&p.pkg, m, false, Some(&grant), now())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Forbidden);
    assert_eq!(reason(&e), "package_hash_mismatch");
}

#[tokio::test]
async fn revocado_firma_invalida_y_elevado_bloquean() {
    let p = package(true);
    let mut revoked = store();
    revoked.keys[0].revoked = true;
    let e = p
        .policy(revoked, Arc::new(NotElevated))
        .authorize(&p.pkg, &p.manifest, false, None, now())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Forbidden);
    assert_eq!(reason(&e), "revoked");
    assert_eq!(
        store_with_revoked_package(&p.manifest)
            .assess(&p.manifest, false)
            .unwrap()
            .trust,
        Trust::Revoked
    );

    // Firma de otra clave con el mismo key_id.
    let mut forged = p.manifest.clone();
    forged.signature =
        Some(sign(&forged, "pruebas-2026", &SigningKey::from_bytes(&[1u8; 32])).unwrap());
    let e = p
        .policy(store(), Arc::new(NotElevated))
        .authorize(&p.pkg, &forged, false, None, now())
        .unwrap_err();
    assert_eq!(reason(&e), "signature_invalid");

    // LARM elevado: no lanza ni un adaptador firmado.
    let e = p
        .policy(store(), Arc::new(Elevated))
        .authorize(&p.pkg, &p.manifest, false, None, now())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Forbidden);
    assert_eq!(reason(&e), "elevated_process");
}

fn store_with_revoked_package(m: &AdapterManifest) -> TrustStore {
    let mut s = store();
    s.revoked_packages.push(m.package_sha256.clone());
    s
}

#[test]
fn elevacion_de_permisos_requiere_aprobacion() {
    let p = package(true);
    let accepted = p.manifest.permissions.clone();
    let mut upgraded = p.manifest.clone();
    upgraded.version = "0.2.0".into();
    upgraded.permissions.write_roots.push("$models".into());
    upgraded
        .permissions
        .network_origins
        .push("https://github.com".into());

    // Menos o igual: se acepta.
    assert_eq!(
        Policy::review_upgrade(&accepted, &p.manifest, None, now()).unwrap(),
        UpgradeDecision::Accepted { approval_id: None }
    );
    // Más: estado de aprobación pendiente con el delta exacto.
    let UpgradeDecision::ApprovalRequired {
        delta,
        operation_digest,
        resource_id,
    } = Policy::review_upgrade(&accepted, &upgraded, None, now()).unwrap()
    else {
        panic!("una elevación debe pedir aprobación");
    };
    assert_eq!(delta.write_roots, vec!["$models".to_string()]);
    assert_eq!(
        delta.network_origins,
        vec!["https://github.com".to_string()]
    );
    assert!(delta.read_roots.is_empty() && delta.secret_refs.is_empty());
    assert_eq!(resource_id, "adapter:larm.fake-adapter@0.2.0");
    assert_eq!(
        serde_json::to_value(Policy::review_upgrade(&accepted, &upgraded, None, now()).unwrap())
            .unwrap()["state"],
        "APPROVAL_REQUIRED"
    );

    // La aprobación de este delta lo acepta; la de otro delta, no.
    let good = approval(
        &operation_digest,
        &resource_id,
        &upgraded.package_sha256,
        json!({}),
    );
    assert_eq!(
        Policy::review_upgrade(&accepted, &upgraded, Some((&good, ACTOR)), now()).unwrap(),
        UpgradeDecision::Accepted {
            approval_id: Some("appr-1".into())
        }
    );
    let mut smaller = PermissionDelta::between(&accepted, &upgraded.permissions);
    smaller.network_origins.clear();
    let other = approval(
        &elevation_digest(&upgraded, &smaller).unwrap(),
        &resource_id,
        &upgraded.package_sha256,
        json!({}),
    );
    let e = Policy::review_upgrade(&accepted, &upgraded, Some((&other, ACTOR)), now()).unwrap_err();
    assert_eq!(reason(&e), "approval_digest_mismatch");
    let e = Policy::review_upgrade(&accepted, &upgraded, Some((&good, "user:otro")), now())
        .unwrap_err();
    assert_eq!(reason(&e), "approval_actor_mismatch");
}

#[test]
fn nivel_de_integridad_real_de_este_proceso() {
    // En el PC IA las pruebas corren sin elevar (integridad media).
    let elevated = WhoAmI.elevated().unwrap();
    let expected = std::env::var("LARM_EXPECT_ELEVATED").is_ok_and(|v| v == "1");
    assert_eq!(
        elevated, expected,
        "whoami /groups debe reflejar el nivel real"
    );
}
