//! Fixture nativa compartida CODE_PASS: proceso/P022 reales y observaciones
//! sintéticas. Nunca se registra en la composición del daemon.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use async_trait::async_trait;
use larm_adapter_host::conformance::{
    NativeConformanceRunner, NativeSession, NativeSuite, NativeTarget, NativeTargets,
};
use larm_adapter_host::policy::{ElevationProbe, Policy, TrustedOnlyGrant};
use larm_adapter_host::worker::{HostConfig, WorkerSpec};
use larm_adapter_sdk::manifest::{AdapterManifest, MethodTable};
use larm_adapter_sdk::permissions::{
    adapter_resource, execution_digest, package_hash, Access, RootMap, TrustStore,
};
use larm_adapter_sdk::protocol::Request;
use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use larm_domain::bridge::timestamp;
use larm_domain::conformance::{
    committed_installation_hash, hash, Observation, Outcome, Receipt, Run, METADATA_KEY,
};
use larm_domain::error::LarmError;
use larm_domain::ports::{AdapterConformanceRunner, MutationContext};
use larm_domain::runtime::RuntimeChannel;
use larm_domain::runtime_install::VersionQuery;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const RID: &str = "0192aaaa-0000-7000-8000-000000000001";
struct NotElevated;
impl ElevationProbe for NotElevated {
    fn elevated(&self) -> Result<bool, LarmError> {
        Ok(false)
    }
}
struct Suite;
#[async_trait]
impl NativeSuite for Suite {
    fn descriptor(&self) -> Value {
        json!({"family":"runtime","capabilities":[],"fixture_only":"CODE_PASS synthetic qualification","version":"p031-native-client-1"})
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let query = VersionQuery {
            runtime_id: RID.into(),
            channel: RuntimeChannel::Stable,
            cursor: None,
        };
        let access = Access {
            writes: vec![session.work_dir().into()],
            ..Access::default()
        };
        session
            .call(
                Request::new(
                    "runtime.list_versions",
                    serde_json::to_value(query).unwrap(),
                ),
                &access,
                Some(Duration::from_secs(3)),
                None,
            )
            .await?;
        // Sintético para probar el motor de consumo; no se registra en producción.
        Ok(run
            .required_checks()
            .into_iter()
            .map(|id| Observation {
                evidence_sha256: hash(&json!({"CODE_PASS":id,"run":run.id})).unwrap(),
                check_id: id,
                passed: true,
            })
            .collect())
    }
}
#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) spec: WorkerSpec,
    pub(crate) grant: TrustedOnlyGrant,
}
#[async_trait]
impl NativeTargets for Target {
    async fn target(
        &self,
        _: &AdapterInstallation,
        _: &MutationContext,
    ) -> Result<NativeTarget, LarmError> {
        Ok(NativeTarget {
            spec: self.spec.clone(),
            roots: None,
            custom: false,
            grant: Some(self.grant.clone()),
            capability_profile: None,
            immutable_inputs: vec![],
            suite: Arc::new(Suite),
            timeout: Duration::from_secs(20),
        })
    }
}
pub(crate) struct Fixture {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) target: Target,
    pub(crate) installation: AdapterInstallation,
    pub(crate) context: MutationContext,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("package");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/examples/fake_runtime_worker.exe"),
            package.join("bin/worker.exe"),
        )
        .unwrap();
        let context = MutationContext {
            actor_id: "p031-native-fixture".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let methods: Vec<_> = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "all" || m.family == "runtime")
            .map(|m| m.method.clone())
            .collect();
        let value = json!({"id":"fixture.p031.runtime","version":"1.0.0","family":"runtime","protocol_version":"1.0","publisher":"CODE_PASS fixture","executable":{"relative_path":"bin/worker.exe","args":["--manifest",package.join("manifest.json")]},"platforms":["windows"],"architectures":["x86_64"],"methods":methods,"permissions":{"read_roots":[],"write_roots":["$work"],"network_origins":[],"process_allowlist":["bin/worker.exe"],"secret_refs":[]},"capabilities":[],"installation_strategies":["PREBUILT_BINARY"],"package_sha256":package_hash(&package, &["manifest.json"]).unwrap(),"signature":null});
        let manifest = AdapterManifest::parse(&value.to_string()).unwrap();
        std::fs::write(
            package.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let at = timestamp(chrono::Utc::now());
        let installation = AdapterInstallation {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: at.clone(),
            updated_at: at,
            row_version: 1,
            metadata: Default::default(),
            adapter_id: manifest.id.clone(),
            version: manifest.version.clone(),
            family: "runtime".into(),
            package_hash: manifest.package_sha256.clone(),
            publisher: manifest.publisher.clone(),
            trust: AdapterTrust::Untrusted,
            manifest: value,
            state: AdapterState::Installed,
        };
        let grant = TrustedOnlyGrant {
            approval_id: uuid::Uuid::now_v7().to_string(),
            actor_id: context.actor_id.clone(),
            resource: adapter_resource(&manifest),
            execution_digest: execution_digest(&manifest).unwrap(),
            package_sha256: manifest.package_sha256.clone(),
            publisher: manifest.publisher.clone(),
            secret_refs: vec![],
            expires_at: timestamp(chrono::Utc::now() + chrono::Duration::minutes(10)),
        };
        Self {
            target: Target {
                spec: WorkerSpec {
                    package_dir: package,
                    manifest,
                    environment: vec![],
                    work_dir: dir.path().join("work"),
                },
                grant,
            },
            installation,
            context,
            _dir: dir,
        }
    }
    pub(crate) fn runner(&self) -> Arc<NativeConformanceRunner> {
        Arc::new(NativeConformanceRunner::new(
            Arc::new(Policy {
                trust: TrustStore::default(),
                roots: RootMap(vec![("$work".into(), self.target.spec.work_dir.clone())]),
                elevation: Arc::new(NotElevated),
            }),
            Arc::new(self.target.clone()),
            HostConfig {
                handshake_timeout: Duration::from_secs(3),
                cancel_grace: Duration::from_millis(400),
                ..HostConfig::default()
            },
        ))
    }
    pub(crate) fn access(&self) -> Access {
        Access {
            writes: vec![self.target.spec.work_dir.clone()],
            ..Access::default()
        }
    }
    pub(crate) fn calls(&self) -> usize {
        std::fs::read_to_string(self.target.spec.work_dir.join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    pub(crate) async fn validated(
        &self,
        runner: &NativeConformanceRunner,
    ) -> (AdapterInstallation, Run) {
        let execution = runner
            .execute_conformance(&self.installation, &self.context)
            .await
            .unwrap();
        let mut installed = self.installation.clone();
        installed.state = AdapterState::Validated;
        installed.row_version += 1;
        installed.updated_at = timestamp(chrono::Utc::now());
        let proof = execution.proof.unwrap();
        let receipt = Receipt {
            run_id: execution.run.id.clone(),
            installation_id: installed.id.clone(),
            installation_revision: installed.row_version.to_string(),
            committed_installation_sha256: committed_installation_hash(&installed).unwrap(),
            package_sha256: installed.package_hash.clone(),
            run_sha256: execution.run.digest().unwrap(),
            outcome_sha256: hash(&Outcome::Passed {
                proof: proof.clone(),
            })
            .unwrap(),
            evidence_sha256: Some(proof.digest().unwrap()),
            passed: true,
            completed_at: installed.updated_at.clone(),
        };
        installed
            .metadata
            .insert(METADATA_KEY.into(), serde_json::to_value(receipt).unwrap());
        (installed, execution.run)
    }
}
