//! Configuración local de recetas compiladas. No admite código, Proof ni planes
//! de ejecución del plugin. El endpoint Probe no deserializa este documento.
use super::*;
use larm_adapter_sdk::permissions::RootMap;
use larm_domain::conformance_profiles::ConformanceProfile;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "recipe", content = "parameters", deny_unknown_fields)]
pub enum Recipe {
    #[serde(rename = "reference.runtime.echo.v1")]
    Runtime(runtime::RuntimeConfig),
    #[serde(rename = "reference.source.catalog.v1")]
    Source(source::SourceConfig),
    #[serde(rename = "source.http.chunks.v1")]
    SourceHttp(source_http::HttpSourceConfig),
    #[serde(rename = "reference.protocol.echo.v1")]
    Protocol(protocol::ProtocolConfig),
    #[serde(rename = "reference.transformation.base64.v1")]
    Transformation(transformation::TransformationConfig),
    #[serde(rename = "product.transformation.gguf.v1")]
    Gguf(gguf::GgufConfig),
    #[serde(rename = "product.transformation.onnx.v1")]
    Onnx(onnx::OnnxConfig),
    #[serde(rename = "reference.benchmark.echo.v1")]
    Benchmark(benchmark::BenchmarkConfig),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProfile {
    pub schema_version: String,
    pub package_dir: PathBuf,
    pub work_dir: PathBuf,
    pub roots: Vec<(String, PathBuf)>,
    pub custom: bool,
    pub capability_profile: Option<PathBuf>,
    pub immutable_inputs: Vec<PathBuf>,
    pub timeout_seconds: u64,
    pub suite: Recipe,
}
struct BoundSuite {
    profile_sha256: String,
    inner: Arc<dyn NativeSuite>,
}
#[async_trait]
impl NativeSuite for BoundSuite {
    fn descriptor(&self) -> Value {
        let mut d = self.inner.descriptor();
        d["registered_profile_sha256"] = json!(self.profile_sha256);
        d
    }
    fn protocol_probes_first(&self) -> bool {
        self.inner.protocol_probes_first()
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        self.inner.execute(run, session).await
    }
}
impl LocalProfile {
    fn suite(&self, installation: &AdapterInstallation) -> Result<Arc<dyn NativeSuite>, LarmError> {
        let (family, suite): (&str, Arc<dyn NativeSuite>) = match &self.suite {
            Recipe::Runtime(c) => (
                "runtime",
                Arc::new(runtime::ReferenceRuntimeSuite::new(c.clone())?),
            ),
            Recipe::Source(c) => (
                "source",
                Arc::new(source::ReferenceSourceSuite::new(c.clone())?),
            ),
            Recipe::SourceHttp(c) => (
                "source",
                Arc::new(source_http::HttpSourceSuite::new(c.clone())?),
            ),
            Recipe::Protocol(c) => (
                "protocol",
                Arc::new(protocol::ReferenceProtocolSuite::new(c.clone())?),
            ),
            Recipe::Transformation(c) => (
                "transformation",
                Arc::new(transformation::ReferenceTransformationSuite::new(
                    c.clone(),
                )?),
            ),
            Recipe::Gguf(c) => (
                "transformation",
                Arc::new(gguf::GgufTransformationSuite::new(c.clone())?),
            ),
            Recipe::Onnx(c) => (
                "transformation",
                Arc::new(onnx::OnnxTransformationSuite::new(c.clone())?),
            ),
            Recipe::Benchmark(c) => (
                "benchmark",
                Arc::new(benchmark::ReferenceBenchmarkSuite::new(c.clone())?),
            ),
        };
        if installation.family != family {
            return Err(invalid("conformance_profile_family_mismatch"));
        }
        Ok(suite)
    }
    pub fn target(
        &self,
        profile: &ConformanceProfile,
        installation: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<NativeTarget, LarmError> {
        profile.check_installation(installation)?;
        if hash(self)? != hash(&profile.configuration)? {
            return Err(invalid("conformance_profile_configuration_mismatch"));
        }
        if profile.actor_id != ctx.actor_id {
            return Err(error(
                ErrorCode::Forbidden,
                "conformance_profile_actor_mismatch",
            ));
        }
        if self.schema_version != "1.0"
            || self.timeout_seconds == 0
            // Los conversores de producto reales (cuantización K en Python
            // stdlib) legítimamente superan los 10 minutos por suite: el
            // timeout sigue acotado y es un detector de cuelgue.
            || self.timeout_seconds > 2400
            || self.immutable_inputs.len() > 128
            || self.roots.len() > 32
        {
            return Err(invalid("conformance_profile_configuration_invalid"));
        }
        for path in [&self.package_dir, &self.work_dir] {
            if !path.is_absolute() {
                return Err(invalid("conformance_profile_path_invalid"));
            }
            check_managed_path(path)?;
        }
        let mut names = BTreeSet::new();
        for (name, path) in &self.roots {
            if name.is_empty()
                || name.len() > 256
                || name.chars().any(char::is_control)
                || !names.insert(name.to_ascii_lowercase())
                || !path.is_absolute()
            {
                return Err(invalid("conformance_profile_roots_invalid"));
            }
            check_managed_path(path)?;
        }
        let manifest =
            larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())?;
        // La aprobación de un uso se comprueba al emitir la concesión. Su TTL
        // corto no reemplaza expires_at de la concesión persistida; P022 vuelve
        // a comprobar esa caducidad efectiva antes de cada ejecución/recheck.
        let issued = chrono::DateTime::parse_from_rfc3339(&profile.created_at)
            .map_err(|_| invalid("conformance_profile_invalid"))?
            .with_timezone(&chrono::Utc);
        let grant =
            TrustedOnlyGrant::from_approval(&profile.approval, &manifest, &ctx.actor_id, issued)?;
        if chrono::DateTime::parse_from_rfc3339(&grant.expires_at)
            .is_ok_and(|expiry| expiry.with_timezone(&chrono::Utc) <= chrono::Utc::now())
        {
            return Err(error(ErrorCode::ApprovalRequired, "grant_expired"));
        }
        let mut inputs = self.immutable_inputs.clone();
        match &self.suite {
            Recipe::Runtime(c) => {
                inputs.push(c.forbidden_model.clone());
                for key in ["model_path", "config_path"] {
                    if let Some(p) =
                        c.launch_request["launch_profile"]["adapter_options"][key].as_str()
                    {
                        inputs.push(p.into());
                    }
                }
            }
            Recipe::Source(c) => {
                inputs.extend(c.expected_files.clone());
                inputs.extend([
                    c.forbidden.clone(),
                    self.work_dir.join("profile.json"),
                    self.work_dir.join("catalog/catalog.json"),
                ]);
            }
            Recipe::SourceHttp(c) => {
                inputs.extend([c.expected_file.clone(), self.work_dir.join("profile.json")]);
            }
            Recipe::Protocol(c) => {
                inputs.extend([
                    c.forbidden.clone(),
                    c.backend_binary.clone(),
                    self.work_dir.join("profile.json"),
                ]);
            }
            Recipe::Gguf(c) => {
                inputs.extend([
                    c.forbidden.clone(),
                    c.expected_input.clone(),
                    self.work_dir.join("input/fixture.safetensors"),
                    self.work_dir.join("profile/profile.json"),
                ]);
            }
            Recipe::Onnx(c) => {
                inputs.extend([
                    c.forbidden.clone(),
                    c.expected_input.clone(),
                    self.work_dir.join("input/fixture.safetensors"),
                    self.work_dir.join("profile/profile.json"),
                ]);
            }
            Recipe::Transformation(c) => {
                inputs.extend([
                    c.forbidden.clone(),
                    c.expected_input.clone(),
                    self.work_dir.join("input/fixture.bin"),
                    self.work_dir.join("profile/profile.json"),
                ]);
            }
            Recipe::Benchmark(c) => {
                inputs.extend([
                    c.forbidden.clone(),
                    c.backend_binary.clone(),
                    self.work_dir.join("profile/profile.json"),
                ]);
            }
        }
        inputs.sort();
        inputs.dedup();
        if inputs.len() > 128 || inputs.iter().any(|p| !p.is_absolute()) {
            return Err(invalid("conformance_inputs_invalid"));
        }
        Ok(NativeTarget {
            spec: WorkerSpec {
                package_dir: self.package_dir.clone(),
                manifest,
                environment: vec![],
                work_dir: self.work_dir.clone(),
            },
            roots: Some(RootMap(self.roots.clone())),
            custom: self.custom,
            grant: Some(grant),
            capability_profile: self.capability_profile.clone(),
            immutable_inputs: inputs,
            suite: Arc::new(BoundSuite {
                profile_sha256: hash(profile)?,
                inner: self.suite(installation)?,
            }),
            timeout: Duration::from_secs(self.timeout_seconds),
        })
    }
}
