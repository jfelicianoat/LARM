//! `AdapterManifest` (P020; contrato 03, schema `AdapterManifest`, tabla
//! `adapter-methods.json`).
//!
//! - Exactamente los campos del schema (`deny_unknown_fields`).
//! - `methods` solo admite las 40 firmas publicadas, de la familia del
//!   adaptador o comunes, e incluye las comunes obligatorias y las
//!   obligatorias de su familia. No hay un contrato público alternativo.
//! - `executable.relative_path` se resuelve bajo el paquete: nada absoluto,
//!   ni `..`, ni comandos de shell.
//! - `installation_strategies`: el enum canónico de §52 más las tres
//!   extensiones del contrato 11, sin repetidos. Una estrategia pedida que
//!   el manifiesto no anuncia se rechaza **antes** de invocar al worker.
//! - Capacidades extensibles: identificadores con espacio de nombres; las
//!   desconocidas se conservan tal cual.

use std::collections::BTreeSet;

use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};

/// Tabla de métodos del contrato (copia en `docs/contracts`).
pub const METHODS_JSON: &str = include_str!("../../../docs/contracts/adapter-methods.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Runtime,
    Source,
    Protocol,
    Transformation,
    Benchmark,
}

impl Family {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Source => "source",
            Self::Protocol => "protocol",
            Self::Transformation => "transformation",
            Self::Benchmark => "benchmark",
        }
    }
}

/// Un único enum para manifiesto, solicitud y persistencia; conserva esta ruta pública.
pub use larm_domain::runtime::InstallationStrategy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Executable {
    pub relative_path: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub network_origins: Vec<String>,
    pub process_allowlist: Vec<String>,
    /// Referencias de Credential Manager, nunca valores.
    pub secret_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub algorithm: SignatureAlgorithm,
    pub key_id: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureAlgorithm {
    Ed25519,
}

/// `AdapterManifest` (schema cerrado).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterManifest {
    pub id: String,
    pub version: String,
    pub family: Family,
    pub protocol_version: String,
    pub publisher: String,
    pub executable: Executable,
    pub platforms: Vec<String>,
    pub architectures: Vec<String>,
    pub methods: Vec<String>,
    pub permissions: Permissions,
    pub capabilities: Vec<String>,
    pub package_sha256: String,
    pub signature: Option<Signature>,
    pub installation_strategies: Vec<InstallationStrategy>,
}

/// Firma publicada de un método.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MethodSpec {
    pub method: String,
    pub family: String,
    pub request_schema: String,
    pub response_schema: String,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct MethodsFile {
    protocol: String,
    contract_version: String,
    mandatory_common: Vec<String>,
    methods: Vec<MethodSpec>,
}

/// Tabla de métodos del contrato.
#[derive(Debug, Clone)]
pub struct MethodTable {
    pub protocol: String,
    pub contract_version: String,
    pub mandatory_common: Vec<String>,
    pub methods: Vec<MethodSpec>,
}

impl MethodTable {
    /// La tabla publicada.
    ///
    /// # Panics
    /// Nunca con el fichero del paquete (se comprueba en las pruebas).
    #[must_use]
    pub fn contract() -> Self {
        #[allow(clippy::expect_used)]
        let f: MethodsFile =
            serde_json::from_str(METHODS_JSON).expect("adapter-methods.json del contrato");
        Self {
            protocol: f.protocol,
            contract_version: f.contract_version,
            mandatory_common: f.mandatory_common,
            methods: f.methods,
        }
    }

    #[must_use]
    pub fn get(&self, method: &str) -> Option<&MethodSpec> {
        self.methods.iter().find(|m| m.method == method)
    }

    /// Métodos obligatorios de una familia (sin los comunes).
    #[must_use]
    pub fn required_for(&self, family: Family) -> Vec<&str> {
        self.methods
            .iter()
            .filter(|m| m.required && m.family == family.as_str())
            .map(|m| m.method.as_str())
            .collect()
    }
}

fn invalid(reason: &str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Identificador de capacidad con espacio de nombres (`runtime.gguf`,
/// `vendor.acme.npu`): segmentos `[a-z][a-z0-9_-]*` separados por `.`, al
/// menos dos, como mucho 128 caracteres.
#[must_use]
pub fn is_namespaced(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    id.len() <= 128
        && segments.len() >= 2
        && segments.iter().all(|s| {
            let mut c = s.chars();
            c.next().is_some_and(|x| x.is_ascii_lowercase())
                && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_' || x == '-')
        })
}

fn check_relative(path: &str) -> bool {
    let unified = path.replace('\\', "/");
    !unified.is_empty()
        && !unified.starts_with('/')
        && !unified.contains(':')
        && unified
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
        && !unified
            .chars()
            .any(|c| c.is_control() || "<>|&;$`\"'*?%^".contains(c))
}

fn check_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    matches!(scheme, "https" | "http")
        && !rest.is_empty()
        && !rest.contains(['/', '?', '#', '@', ' '])
}

impl AdapterManifest {
    /// Parsea y valida un manifiesto.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` `schema_violation` si no cumple el schema; los de
    /// [`AdapterManifest::validate`].
    pub fn parse(text: &str) -> Result<Self, LarmError> {
        let m: Self = serde_json::from_str(text).map_err(|e| {
            invalid(
                "schema_violation",
                "El manifiesto no cumple el schema AdapterManifest",
            )
            .with_technical(e.to_string())
        })?;
        m.validate(&MethodTable::contract())?;
        Ok(m)
    }

    /// Reglas del contrato 03 que el schema no expresa.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` con el motivo.
    pub fn validate(&self, table: &MethodTable) -> Result<(), LarmError> {
        if self.protocol_version != "1.0" {
            return Err(invalid(
                "protocol_version_invalid",
                "protocol_version debe ser 1.0",
            ));
        }
        for (field, value) in [
            ("id", &self.id),
            ("version", &self.version),
            ("publisher", &self.publisher),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(
                    &format!("{field}_invalid"),
                    format!("{field} vacío"),
                ));
            }
        }
        if !check_relative(&self.executable.relative_path) {
            return Err(invalid(
                "executable_path_invalid",
                "El ejecutable debe ser una ruta relativa dentro del paquete, sin comandos de shell",
            ));
        }
        if !is_sha256(&self.package_sha256) {
            return Err(invalid(
                "package_sha256_invalid",
                "package_sha256 no es sha256",
            ));
        }
        let mut seen = BTreeSet::new();
        for m in &self.methods {
            let spec = table.get(m).ok_or_else(|| {
                invalid(
                    "method_unknown",
                    format!("{m} no es una firma publicada del contrato"),
                )
            })?;
            if spec.family != "all" && spec.family != self.family.as_str() {
                return Err(invalid(
                    "method_family_mismatch",
                    format!("{m} es de la familia {}", spec.family),
                ));
            }
            if !seen.insert(m.as_str()) {
                return Err(invalid("method_duplicate", format!("{m} repetido")));
            }
        }
        let required = table
            .mandatory_common
            .iter()
            .map(String::as_str)
            .chain(table.required_for(self.family));
        for r in required {
            if !seen.contains(r) {
                return Err(invalid(
                    "method_required_missing",
                    format!("Falta el método obligatorio {r}"),
                ));
            }
        }
        for c in &self.capabilities {
            if !is_namespaced(c) {
                return Err(invalid(
                    "capability_invalid",
                    format!("Capacidad sin espacio de nombres: {c}"),
                ));
            }
        }
        let strategies: BTreeSet<InstallationStrategy> =
            self.installation_strategies.iter().copied().collect();
        if strategies.len() != self.installation_strategies.len() {
            return Err(invalid(
                "installation_strategy_duplicate",
                "Estrategias de instalación repetidas",
            ));
        }
        if self.family != Family::Runtime && !self.installation_strategies.is_empty() {
            return Err(invalid(
                "installation_strategies_not_applicable",
                "Solo los adaptadores de runtime instalan; una familia que no instala declara lista vacía",
            ));
        }
        let p = &self.permissions;
        if p.network_origins.iter().any(|o| !check_origin(o)) {
            return Err(invalid(
                "network_origin_invalid",
                "network_origins solo admite orígenes http(s)://host[:puerto]",
            ));
        }
        for root in p.read_roots.iter().chain(&p.write_roots) {
            if root.trim().is_empty() {
                return Err(invalid("root_invalid", "Raíz de permisos vacía"));
            }
        }
        if p.process_allowlist.iter().any(|x| !check_relative(x)) {
            return Err(invalid(
                "process_allowlist_invalid",
                "process_allowlist solo admite rutas relativas del paquete",
            ));
        }
        if p.secret_refs
            .iter()
            .any(|r| !r.starts_with("larm/") || r.contains(char::is_whitespace))
        {
            return Err(invalid(
                "secret_ref_invalid",
                "secret_refs son referencias de Credential Manager (larm/…), nunca valores",
            ));
        }
        if let Some(s) = &self.signature {
            if s.key_id.trim().is_empty() || s.value.trim().is_empty() {
                return Err(invalid("signature_invalid", "Firma sin key_id o sin valor"));
            }
        }
        Ok(())
    }

    /// Comprueba, antes de invocar al worker, que el manifiesto anuncia la
    /// estrategia pedida.
    ///
    /// # Errors
    /// `UNSUPPORTED_CAPABILITY` (422) `installation_strategy_not_announced`.
    pub fn require_strategy(&self, strategy: InstallationStrategy) -> Result<(), LarmError> {
        if self.installation_strategies.contains(&strategy) {
            Ok(())
        } else {
            Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El adaptador no anuncia esa estrategia de instalación",
                "installation_strategy_not_announced",
            )
            .retryable(false))
        }
    }

    /// Comprueba que el manifiesto anuncia el método.
    ///
    /// # Errors
    /// `UNSUPPORTED_CAPABILITY` `method_not_announced`.
    pub fn require_method(&self, method: &str) -> Result<(), LarmError> {
        if self.methods.iter().any(|m| m == method) {
            Ok(())
        } else {
            Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El adaptador no anuncia ese método",
                "method_not_announced",
            ))
        }
    }
}

/// Capacidades de un adaptador de runtime (§10): conjunto extensible de
/// identificadores con espacio de nombres; las desconocidas se conservan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeAdapterCapabilities(pub BTreeSet<String>);

/// Capacidades de un adaptador de fuente de modelos (§12).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelSourceAdapterCapabilities(pub BTreeSet<String>);

/// Capacidades conocidas de runtime (referencia; no es una lista cerrada).
pub const KNOWN_RUNTIME_CAPABILITIES: [&str; 8] = [
    "runtime.discover",
    "runtime.install",
    "runtime.openai_compatible",
    "runtime.gguf",
    "runtime.onnx",
    "runtime.multi_instance",
    "runtime.streaming",
    "runtime.benchmark",
];

/// Capacidades conocidas de fuente (referencia; no es una lista cerrada).
pub const KNOWN_SOURCE_CAPABILITIES: [&str; 6] = [
    "source.search",
    "source.revisions",
    "source.resume",
    "source.sha256",
    "source.auth",
    "source.sync",
];

macro_rules! capability_set {
    ($t:ty, $known:expr) => {
        impl $t {
            /// # Errors
            /// Alguna capacidad sin espacio de nombres.
            pub fn new(ids: impl IntoIterator<Item = String>) -> Result<Self, LarmError> {
                let set: BTreeSet<String> = ids.into_iter().collect();
                if let Some(bad) = set.iter().find(|c| !is_namespaced(c)) {
                    return Err(invalid(
                        "capability_invalid",
                        format!("Capacidad sin espacio de nombres: {bad}"),
                    ));
                }
                Ok(Self(set))
            }
            #[must_use]
            pub fn has(&self, id: &str) -> bool {
                self.0.contains(id)
            }
            /// Las que LARM no conoce: se conservan, no se interpretan.
            #[must_use]
            pub fn unknown(&self) -> Vec<&str> {
                self.0
                    .iter()
                    .filter(|c| !$known.contains(&c.as_str()))
                    .map(String::as_str)
                    .collect()
            }
        }
    };
}
capability_set!(RuntimeAdapterCapabilities, KNOWN_RUNTIME_CAPABILITIES);
capability_set!(ModelSourceAdapterCapabilities, KNOWN_SOURCE_CAPABILITIES);

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub fn manifest_json() -> serde_json::Value {
        let table = MethodTable::contract();
        let mut methods: Vec<String> = table.mandatory_common.clone();
        methods.extend(
            table
                .required_for(Family::Runtime)
                .iter()
                .map(|s| (*s).to_string()),
        );
        serde_json::json!({
            "id": "larm.llama-cpp", "version": "0.1.0", "family": "runtime", "protocol_version": "1.0",
            "publisher": "LARM", "executable": {"relative_path": "bin/adapter.exe", "args": ["--stdio"]},
            "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
            "permissions": {"read_roots": ["$models"], "write_roots": ["$runtime"],
                "network_origins": ["https://github.com"], "process_allowlist": ["bin/llama-server.exe"],
                "secret_refs": ["larm/source/hf"]},
            "capabilities": ["runtime.gguf", "vendor.acme.npu"], "package_sha256": "a".repeat(64), "signature": null,
            "installation_strategies": ["GITHUB_RELEASE", "SOURCE_BUILD", "MSI_MANAGED"]
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::tests_support::manifest_json;
    use super::*;

    #[test]
    fn tabla_del_contrato() {
        let t = MethodTable::contract();
        assert_eq!(
            (t.protocol.as_str(), t.contract_version.as_str()),
            ("JSON-RPC 2.0 NDJSON", "1.1")
        );
        assert_eq!(t.methods.len(), 40, "las 40 firmas publicadas");
        assert_eq!(
            t.mandatory_common,
            vec!["core.handshake", "core.cancel", "core.heartbeat"]
        );
        assert!(t
            .required_for(Family::Runtime)
            .contains(&"runtime.launch_plan"));
        assert!(
            !t.required_for(Family::Runtime)
                .contains(&"runtime.list_models"),
            "opcional"
        );
    }

    #[test]
    fn manifiesto_valido_roundtrip_y_capacidades_desconocidas() {
        let text = manifest_json().to_string();
        let m = AdapterManifest::parse(&text).unwrap();
        assert!(
            m.capabilities.contains(&"vendor.acme.npu".to_string()),
            "la desconocida se conserva"
        );
        let back = serde_json::to_value(&m).unwrap();
        assert_eq!(back, manifest_json(), "roundtrip exacto");
        let caps = RuntimeAdapterCapabilities::new(m.capabilities.clone()).unwrap();
        assert_eq!(caps.unknown(), vec!["vendor.acme.npu"]);
        assert!(
            InstallationStrategy::MsiManaged.is_extension()
                && !InstallationStrategy::Uv.is_extension()
        );
    }

    #[test]
    fn estrategia_no_anunciada_antes_del_worker() {
        let m = AdapterManifest::parse(&manifest_json().to_string()).unwrap();
        assert!(m
            .require_strategy(InstallationStrategy::GithubRelease)
            .is_ok());
        let e = m
            .require_strategy(InstallationStrategy::Winget)
            .unwrap_err();
        assert_eq!(
            (e.code.clone(), e.details.reason.as_str(), e.http_status()),
            (
                ErrorCode::UnsupportedCapability,
                "installation_strategy_not_announced",
                422
            )
        );
        assert_eq!(
            m.require_method("runtime.benchmark")
                .unwrap_err()
                .details
                .reason,
            "method_not_announced"
        );
    }

    #[test]
    fn negativos_del_manifiesto() {
        let reason = |patch: &dyn Fn(&mut serde_json::Value)| {
            let mut v = manifest_json();
            patch(&mut v);
            AdapterManifest::parse(&v.to_string())
                .unwrap_err()
                .details
                .reason
        };
        assert_eq!(
            reason(&|v| {
                v["extra"] = 1.into();
            }),
            "schema_violation"
        );
        assert_eq!(
            reason(&|v| {
                v["family"] = "otra".into();
            }),
            "schema_violation"
        );
        assert_eq!(
            reason(&|v| {
                v["protocol_version"] = "2.0".into();
            }),
            "protocol_version_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["executable"]["relative_path"] = "C:/x.exe".into();
            }),
            "executable_path_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["executable"]["relative_path"] = "../x.exe".into();
            }),
            "executable_path_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["executable"]["relative_path"] = "run.cmd & del".into();
            }),
            "executable_path_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["methods"]
                    .as_array_mut()
                    .unwrap()
                    .push("runtime.teleport".into());
            }),
            "method_unknown"
        );
        assert_eq!(
            reason(&|v| {
                v["methods"]
                    .as_array_mut()
                    .unwrap()
                    .push("source.search".into());
            }),
            "method_family_mismatch"
        );
        assert_eq!(
            reason(&|v| {
                v["methods"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|m| m != "core.heartbeat");
            }),
            "method_required_missing"
        );
        assert_eq!(
            reason(&|v| {
                v["methods"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|m| m != "runtime.health");
            }),
            "method_required_missing"
        );
        assert_eq!(
            reason(&|v| {
                v["capabilities"] = serde_json::json!(["gguf"]);
            }),
            "capability_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["installation_strategies"] = serde_json::json!(["PIP", "PIP"]);
            }),
            "installation_strategy_duplicate"
        );
        assert_eq!(
            reason(&|v| {
                v["installation_strategies"] = serde_json::json!(["NPM"]);
            }),
            "schema_violation"
        );
        assert_eq!(
            reason(&|v| {
                v["permissions"]["secret_refs"] = serde_json::json!(["hf_abc123"]);
            }),
            "secret_ref_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["permissions"]["network_origins"] = serde_json::json!(["https://x.com/path"]);
            }),
            "network_origin_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["package_sha256"] = "x".into();
            }),
            "package_sha256_invalid"
        );
        assert_eq!(
            reason(&|v| {
                v["signature"] =
                    serde_json::json!({"algorithm": "RSA", "key_id": "k", "value": "v"});
            }),
            "schema_violation"
        );
    }

    #[test]
    fn familia_que_no_instala() {
        let t = MethodTable::contract();
        let mut v = manifest_json();
        let mut methods = t.mandatory_common.clone();
        methods.extend(
            t.required_for(Family::Benchmark)
                .iter()
                .map(|s| (*s).to_string()),
        );
        v["family"] = "benchmark".into();
        v["methods"] = serde_json::json!(methods);
        assert_eq!(
            AdapterManifest::parse(&v.to_string())
                .unwrap_err()
                .details
                .reason,
            "installation_strategies_not_applicable"
        );
        v["installation_strategies"] = serde_json::json!([]);
        assert!(AdapterManifest::parse(&v.to_string()).is_ok());
    }
}
